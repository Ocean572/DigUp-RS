"""The index-job contract (Stage 5): one writer, answered busy, honest states, recovery.

Runs in the `server-test` container over the **scratch corpus** (conftest: 4 photos + the
clip + notes.txt, already indexed): a new file arrives, `POST /api/v1/index` runs the real
`index-service` under the server's flock, the job's counts are the engine's own events,
and after `done` the searcher answers for the new file — the `reload → prewarm` sequence
the Stage-3 suite proved, wired by the server itself. No mocks.

Plan P3 (python-free plan) made the whole module drive the **shipping binary**, which
sharpened the recovery contracts into the deployment's own verbs:

* `app.stop()` is SIGTERM — `docker stop`'s stop contract: the writer is asked to close at
  the engine's safe point, the job records *interrupted* with its partial counters inside
  the grace, the process ends on the signal (143 / `-15` in Python's exit-code shape);
* `app.stop(sig=SIGKILL)` is the kill path: the log's last `idle` line carries the totals,
  and the next boot replays the running record as *interrupted* with those totals;
* `app.restart()` is the next boot — the replay the recovery tests read over HTTP.

Four tests reached into `JobRunner`'s internals and retired with the Python wrapper
(`test_an_interrupted_record_always_carries_the_counts_it_saw`,
`test_progress_lands_on_the_live_record_and_survives_the_final_mark`,
`test_shutdown_mid_job_finishes_and_marks_the_job_interrupted`,
`test_a_crashing_job_runner_leaves_indexing_alive`,
`test_a_prewarm_timeout_after_done_keeps_the_job_done`): their rules are the binary's own
code paths (`crates/server/src/jobs.rs` — progress merges onto the live record, the mark
takes the state lock and releases it before appending, `after_done`'s refusal is a note on
a *done* job), and every one of them has an observable twin pinned below: the interrupted
record carries its counters, a stop inside the grace answers 143 in bounded time, a job
whose sync timed out leaves the runner answering the next job, and a done job keeps
`state == "done"` in the log no matter what the refresh says.
"""

from __future__ import annotations

import json
import os
import signal
import time
from pathlib import Path

import pytest

from conftest import _cfg
from engine import EngineConfig
from support import LiveApp, live_server

NEW_SENTENCE = "A lighthouse stands above a stormy sea at dusk, its beam sweeping the waves.\n"
CANARY = "A red canoe rests on a glass-still lake under the northern lights.\n"


@pytest.fixture(scope="module")
def jobs_client(scratch, scratch_index, tmp_path_factory):
    """The server over the scratch corpus, with its own job log folder."""
    folder, _ = scratch_index
    with live_server(_cfg(folder, (scratch,)),
                     config_dir=str(tmp_path_factory.mktemp("job-config"))) as c:
        yield c


def wait_job(client, job_id, timeout=300.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        job = client.get(f"/api/v1/jobs/{job_id}").json()["job"]
        if job["state"] in ("done", "interrupted", "busy"):
            return job
        time.sleep(0.5)
    pytest.fail(f"job {job_id} never reached a final state within {timeout:g} s")


def test_job_runs_and_the_new_file_searches_back(jobs_client, scratch):
    # A busy window first: the job POSTed here is live, and a second request is answered.
    first = jobs_client.post("/api/v1/index", json={})
    assert first.status_code == 202
    live = first.json()["job"]
    assert live["state"] == "queued"
    busy = jobs_client.post("/api/v1/index", json={})
    assert busy.status_code == 409                       # answered, never queued blind
    assert live["id"] in busy.json()["detail"]
    assert wait_job(jobs_client, live["id"])["state"] == "done"   # quiet corpus: counts, 0 new

    # Now the real stretch: a new file, indexed by a job, searchable after it.
    (scratch / "lighthouse.txt").write_text(NEW_SENTENCE, encoding="utf-8")
    started = jobs_client.post("/api/v1/index", json={})
    assert started.status_code == 202
    job = wait_job(jobs_client, started.json()["job"]["id"])
    assert job["state"] == "done"
    assert job["files_done"] >= 1                        # the engine's file events, counted
    assert job["errors"] == 0
    assert job["searchable"] >= 7 and job["pending"] == 0  # the `idle` totals, the engine's
    reply = jobs_client.post("/api/v1/search",
                             json={"q": "lighthouse stormy sea at dusk", "kinds": ["doc"],
                                   "limit": 5}).json()
    assert Path(reply["hits"][0]["path"]).name == "lighthouse.txt"  # reload→prewarm happened


def test_job_log_holds_the_engines_counts(jobs_client):
    jobs = jobs_client.get("/api/v1/jobs").json()["jobs"]
    done = [j for j in jobs if j["state"] == "done"]
    assert done, "no completed job in the log"
    last = done[-1]
    assert {"id", "state", "started", "ended", "seconds", "files", "searchable",
            "pending", "vectors", "files_done", "files_failed", "errors"} <= set(last)
    assert last["vectors"] >= 7                          # every file embedded, the engine's


def test_unknown_job_is_404(jobs_client):
    assert jobs_client.get("/api/v1/jobs/not-a-job").status_code == 404


def seed_job_log(tmp_path, lines, name="jobs.jsonl"):
    """A job log as a hard kill left it — the boot's replay input, written before spawn."""
    config_dir = tmp_path / "config"
    config_dir.mkdir(exist_ok=True)
    (config_dir / name).write_text("".join(json.dumps(l) + "\n" for l in lines), encoding="utf-8")
    return config_dir


def test_a_run_never_logged_finished_recovers_as_interrupted(scratch, scratch_index, tmp_path):
    """README §5 verbatim: *an interrupted job is recovered as interrupted, never as
    success*. The log line says `running` with no ending; the next boot rewrites it."""
    folder, _ = scratch_index
    config_dir = seed_job_log(tmp_path, [{"id": "abc123", "state": "running", "started": 0}])
    with live_server(_cfg(folder, (scratch,)), config_dir=str(config_dir)) as c:
        assert c.get("/api/v1/jobs/abc123").json()["job"]["state"] == "interrupted"
        lines = [json.loads(l) for l in config_dir.joinpath("jobs.jsonl").read_text().splitlines()]
        assert lines[-1]["id"] == "abc123" and lines[-1]["state"] == "interrupted"


def test_replay_ignores_lines_that_are_not_job_records(scratch, scratch_index, tmp_path):
    """A hand-edited/merged log must not brick the boot: a replay that indexed `record["id"]`
    on any parsed JSON raised on a bare `5`, the boot died, and the app refused to start.
    Torn tails and non-record lines are skipped; the one real `running` record still
    recovers as *interrupted* and the `done` record stays done."""
    folder, _ = scratch_index
    config_dir = tmp_path / "junk-cfg"
    config_dir.mkdir()
    (config_dir / "jobs.jsonl").write_text(
        "[1, 2, 3]\n5\nnull\n\"a string\"\n{\"state\":\"running\"}\n"   # valid JSON, not records
        '{"id":"abc123","state":"running","started":0}\n'
        '{"id":"real", "state":"done", "ended":1}\n'
        "truncat",                                                       # torn tail
        encoding="utf-8")
    with live_server(_cfg(folder, (scratch,)), config_dir=str(config_dir)) as c:
        assert c.get("/api/v1/jobs/abc123").json()["job"]["state"] == "interrupted"
        assert c.get("/api/v1/jobs/real").json()["job"]["state"] == "done"


def test_plain_sync_retires_walked_root_ghosts(jobs_client, scratch):
    """Found by suite 11: the engine's plain sync retires rows for files missing under
    the roots it walks — the walk *is* the scope, and a vanished file inside it is
    removed at sync. (Rows whose paths are outside the walked roots are a different
    animal: the next test pins them, with prune.)"""
    before = jobs_client.get("/api/v1/status").json()["searchable"]

    (scratch / "canary.txt").write_text(CANARY, encoding="utf-8")
    job = jobs_client.post("/api/v1/index", json={}).json()["job"]
    assert wait_job(jobs_client, job["id"])["state"] == "done"
    assert jobs_client.get("/api/v1/status").json()["searchable"] == before + 1
    hits = jobs_client.post("/api/v1/search", json={
        "q": "red canoe glass-still lake northern lights", "kinds": ["doc"],
        "limit": 3}).json()["hits"]
    assert any(h["path"].endswith("canary.txt") for h in hits)      # indexed, searchable

    (scratch / "canary.txt").unlink()                              # the file is gone
    job = jobs_client.post("/api/v1/index", json={}).json()["job"]  # plain sync…
    assert wait_job(jobs_client, job["id"])["state"] == "done"
    assert jobs_client.get("/api/v1/status").json()["searchable"] == before  # …row gone
    hits = jobs_client.post("/api/v1/search", json={
        "q": "red canoe glass-still lake northern lights", "kinds": ["doc"],
        "limit": 3}).json()["hits"]
    assert not any(h["path"].endswith("canary.txt") for h in hits) # row gone, hits gone


def test_out_of_scope_rows_survive_plain_sync_and_retire_under_prune(tmp_path):
    """The compose-proof ghost, at scratch scale: rows whose paths live OUTSIDE the
    server's roots (the old root scope after a remount) keep counting in
    `files`/`searchable`/`vectors` — plain syncs walk the current roots and never
    touch them; diluting z for live hits is what that dilution looked like in the
    proof. `prune: true` — the engine's `--prune` — retires them: "files no longer
    found under these roots" includes paths outside the roots. The persisted job
    record echoes the flag, and survives a restart (replay)."""
    s2 = tmp_path / "old-root"
    s2.mkdir()
    (s2 / "canary2.txt").write_text(CANARY, encoding="utf-8")
    idx = tmp_path / "ghost-index"
    idx.mkdir()
    nowater = {"DIGUP_SERVER_PREWARM": "0"}          # status ops don't need the model

    # A server whose roots INCLUDE the folder: the canary is indexed, searchable.
    with live_server(_cfg(idx, (s2,)), config_dir=str(tmp_path / "job-config-a"),
                     env_extra=nowater) as scope_a:
        job = scope_a.post("/api/v1/index", json={}).json()["job"]
        assert wait_job(scope_a, job["id"])["state"] == "done"
        assert scope_a.get("/api/v1/status").json()["searchable"] == 1

    # The server AFTER the remount: its roots no longer include that folder.
    empty = tmp_path / "new-root"
    empty.mkdir()
    with live_server(_cfg(idx, (empty,)), config_dir=str(tmp_path / "job-config-b"),
                     env_extra=nowater) as scope_b:
        status = scope_b.get("/api/v1/status").json()
        assert status["searchable"] == 1 and status["files"] == 1  # the ghost counts

        job = scope_b.post("/api/v1/index", json={}).json()["job"]   # plain sync…
        assert wait_job(scope_b, job["id"])["state"] == "done"
        assert scope_b.get("/api/v1/status").json()["searchable"] == 1  # …row SURVIVES

        started = scope_b.post("/api/v1/index", json={"prune": True})
        job_id = started.json()["job"]["id"]
        assert started.json()["job"].get("prune") is True             # echoed live
        assert wait_job(scope_b, job_id)["state"] == "done"
        assert scope_b.get("/api/v1/status").json()["searchable"] == 0  # retired
        assert scope_b.get(f"/api/v1/jobs/{job_id}").json()["job"].get("prune") is True


def test_the_job_timeout_is_an_operator_knob(scratch, tmp_path_factory):
    """3600 s is the runner's patience for corpora (the scratch walk finishes in minutes at
    worst). A real tree's first pass outlives an hour — measured 2026-11-04: a 131,972-file
    job recorded *interrupted with its partial counters* at 19,007 when the default expired
    (and the next job resumed — README §5). The operator owns the ceiling:
    `DIGUP_SERVER_JOB_TIMEOUT_S`, read at the boot, per `.env.example`.

    Observable shape: a server booted with a 1 s ceiling cannot outlive the model load that
    opens every walk (no event inside 1 s), so its job records *interrupted* quoting the
    timeout — and the runner stays ALIVE to answer the next job (the crash path of the same
    loop: an escaped timeout is a job ending, never a wedged writer). The same tree on a
    default boot finishes *done*, which is the "default stays the shipping one" half."""
    idx = tmp_path_factory.mktemp("knob-index")
    with live_server(_cfg(idx, (scratch,)),
                     config_dir=str(tmp_path_factory.mktemp("knob-config-tight")),
                     env_extra={"DIGUP_SERVER_JOB_TIMEOUT_S": "1"}) as tight:
        job_id = tight.post("/api/v1/index", json={}).json()["job"]["id"]
        deadline = time.monotonic() + 120.0
        while time.monotonic() < deadline:
            job = tight.get(f"/api/v1/jobs/{job_id}").json()["job"]
            if job["state"] in ("done", "interrupted", "busy"):
                break
            time.sleep(0.5)
        assert job["state"] == "interrupted", job
        # The binary's own words (protocol.rs `Timeout`): "index-service: no event
        # within 1 s" — the writer's event read hit the operator's 1 s ceiling.
        assert "no event" in job["why"] and "within 1 s" in job["why"], job["why"]
        assert "index-service" in job["why"], job["why"]
        assert job.get("files_done") is not None, "an interrupted record carries its counters"
        next_job = tight.post("/api/v1/index", json={})            # the loop survived
        assert next_job.status_code == 202, next_job.text

    idx2 = tmp_path_factory.mktemp("knob-index-default")
    with live_server(_cfg(idx2, (scratch,)),
                     config_dir=str(tmp_path_factory.mktemp("knob-config-default"))) as open_book:
        job_id = open_book.post("/api/v1/index", json={}).json()["job"]["id"]
        assert wait_job(open_book, job_id, timeout=900.0)["state"] == "done"


def test_the_knobs_are_read_from_the_environment(scratch, scratch_index):
    """`DIGUP_SERVER_BACKFILL=0` reaches the engine argv (the `--backfill` flag off),
    `DIGUP_SERVER_JOB_LOG_SIZE` and `DIGUP_SERVER_JOB_TIMEOUT_S` are read at the boot:
    the settings are env-driven, per `.env.example`; defaults stay the shipping ones
    (backfill on, 1 MiB, 3600 s). The argv half is the harness's own (it is what builds
    the engine command lines); the log-size and job-timeout halves are pinned by the two
    live tests in this module, which boot servers carrying those env values and read what
    the server does with them (rotation files, a job interrupted at the ceiling)."""
    folder, _ = scratch_index
    cfg = EngineConfig(index=str(folder), roots=(str(scratch),))
    assert "--backfill" not in cfg.index_argv()
    assert "--backfill" in cfg.index_argv(backfill=True)


def _log_tail_lines(config_dir: Path, job_id: str) -> list[dict]:
    out = []
    for name in sorted(p.name for p in config_dir.iterdir() if p.name.startswith("jobs.jsonl")):
        for line in (config_dir / name).read_text(encoding="utf-8").splitlines():
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue                                    # the torn tail is skipped
            if isinstance(record, dict) and record.get("id") == job_id:
                out.append(record)
    return out


def test_a_stop_mid_job_is_interrupted_and_the_next_job_resumes(tmp_path, scratch):
    """Stage 8, and gate 1 of the python-free plan in suite shape: SIGTERM sends the
    engine's `stop` — the writer closes its stream at the safe point **between files** —
    the job records *interrupted* with its partial counts, never success, the process ends
    on the signal inside the grace (the Python client reports the signal as `-15`, the
    shell reports the same death as 143), and SQLite stays whole: the next writer takes the
    lock, resumes the walk (files already embedded come back `unchanged`) and finishes it.

    Land the stop in the model-load phase — the one window every platform must cross
    (~800 MB of weights before the engine's first `file` event). Measured 2026-11-05 on a
    warm box the whole scratch embed (8 files) finished *inside* 6 s, so `partial <
    expected` is a timing fact of the cold runner, not a demand: `partial` may legitimately
    be 0 (a job stopped before the engine's first `file` event completed zero files — that
    is the truth, and the record says 0)."""
    idx = tmp_path / "resume-index"
    expected = sum(1 for f in scratch.rglob("*") if f.is_file())   # session scratch grows:
    app = LiveApp(_cfg(idx, (scratch,)), config_dir=str(tmp_path / "resume-cfg"))
    app.start()
    job_id = app.client.post("/api/v1/index", json={}).json()["job"]["id"]
    deadline = time.monotonic() + 120.0
    while time.monotonic() < deadline:                          # wait for the writer to run
        if app.client.get(f"/api/v1/jobs/{job_id}").json()["job"]["state"] == "running":
            break
        time.sleep(0.2)
    time.sleep(1.0)                    # under every measured embed phase, above no load floor
    began = time.monotonic()
    rc = app.stop()                    # SIGTERM: `docker stop`'s own verb
    took = time.monotonic() - began
    assert rc in (-15, 143), f"the stop did not end on the signal (rc={rc})"
    assert took < 90.0, f"the stop outlived the 90 s grace ({took:.1f} s)"
    assert "Application shutdown complete" in app.tail(200_000)

    app.restart()                      # the next boot: replay, then the resume
    job = app.client.get(f"/api/v1/jobs/{job_id}").json()["job"]
    assert job["state"] == "interrupted"
    assert "stopped" in job.get("why", "")            # truthful: never success
    partial = job.get("files_done")
    assert partial is not None, "an interrupted record always carries its counters"

    # One writer, answered busy: a writer stopped mid-model-load keeps its flock for a
    # moment while the engine closes its stream, and the server answers `busy` (the honest
    # 409) for a request in that window. Measured: under machine load that window is
    # seconds. The contract is "answered busy, then the next job runs the walk", so the
    # resume is asked until it is accepted — not assumed to land on the first try.
    deadline = time.monotonic() + 420.0
    done = None
    while time.monotonic() < deadline:
        started = app.client.post("/api/v1/index", json={})
        if started.status_code == 409:
            time.sleep(3.0)                                  # the writer is closing: the flock frees
            continue
        job2 = started.json()["job"]["id"]
        while time.monotonic() < deadline and app.client.get(
                f"/api/v1/jobs/{job2}").json()["job"]["state"] in ("queued", "running"):
            time.sleep(2.0)
        done = app.client.get(f"/api/v1/jobs/{job2}").json()["job"]
        if done["state"] != "busy":
            break
        time.sleep(3.0)
    assert done is not None and done["state"] == "done", done
    assert done["searchable"] == expected and done["pending"] == 0  # index == the walk
    assert done["vectors"] >= 10
    # The engine's own arithmetic, measured in Stage 8 (8 + 7 = 15): the interrupted job
    # embedded `partial` files, the resumed job embeds the REST, so the two records sum to
    # the walk. `partial` may legitimately be 0 (see the model-load note above).
    assert 0 <= partial < expected
    assert partial + done["files_done"] >= expected
    app.stop()


def test_a_hard_killed_job_recovers_with_the_totals_the_log_last_saw(tmp_path, scratch):
    """The durable half of the same rule, on the kill path: the engine's `idle` totals ride
    in the log, so a record recovered after a hard kill is *interrupted* **and** carries
    the last totals the engine published — not an empty shell. SIGKILL mid-embed is the
    container SIGKILL past the grace; the next boot's replay is what answers.

    The window is the engine's own: the kill must land after the first totals the engine
    publishes and before the walk's last file. The scratch clip (every frame a segment)
    keeps that window seconds wide on a warm box; a machine that beat the window on one
    index gets a second and third attempt on fresh indexes, and the attempt count is
    reported — the suite bars skips, and "the box was faster than the kill" is a fact to
    print, not a green by construction."""
    attempts = 0
    for n in range(3):
        attempts += 1
        idx = tmp_path / f"kill-index-{n}"
        config_dir = Path(tmp_path / f"kill-cfg-{n}")
        app = LiveApp(_cfg(idx, (scratch,)), config_dir=str(config_dir))
        app.start()
        job_id = app.client.post("/api/v1/index", json={}).json()["job"]["id"]
        deadline = time.monotonic() + 400.0
        window = False
        while time.monotonic() < deadline:
            job = app.client.get(f"/api/v1/jobs/{job_id}").json()["job"]
            if job["state"] in ("done", "interrupted", "busy"):
                break                                  # the box beat the window: retry
            if job.get("files_done"):                  # the engine published its first totals
                window = True
                break
            time.sleep(0.2)
        if not window:
            app.stop(sig=signal.SIGKILL)
            continue
        app.stop(sig=signal.SIGKILL)                    # the grace is over: hard kill
        last = _log_tail_lines(config_dir, job_id)[-1]
        assert last["state"] == "running", "the kill must leave the log's last line mid-run"
        app.restart()
        recovered = app.client.get(f"/api/v1/jobs/{job_id}").json()["job"]
        assert recovered["state"] == "interrupted", recovered    # never success (README §5)
        for key in ("files", "files_done", "vectors", "pending", "searchable"):
            assert recovered.get(key) == last.get(key), (key, recovered.get(key), last.get(key))
        app.stop()
        return
    pytest.fail(f"the embed finished before the kill window closed on all {attempts} attempts")


def test_the_job_log_rotates_and_history_survives_rotation(scratch, scratch_index, tmp_path):
    """Stage 8: the job log rotates at size (`DIGUP_SERVER_JOB_LOG_SIZE`); replay reads
    ALL rotations oldest-first, so the job history outlives any rotation. Live shape:
    six jobs with a 900-byte ceiling, then the next boot's `GET /api/v1/jobs` shows
    every one of them, `done`, none ghosted."""
    folder, _ = scratch_index
    config_dir = tmp_path / "rot-cfg"
    ids = []
    with live_server(_cfg(folder, (scratch,)), config_dir=str(config_dir),
                     env_extra={"DIGUP_SERVER_JOB_LOG_SIZE": "900"}) as c:
        for _ in range(6):
            job_id = c.post("/api/v1/index", json={}).json()["job"]["id"]
            ids.append(job_id)
            deadline = time.monotonic() + 300.0
            while time.monotonic() < deadline:
                state = c.get(f"/api/v1/jobs/{job_id}").json()["job"]["state"]
                if state in ("done", "interrupted", "busy"):
                    break
                time.sleep(0.5)
            assert state == "done", c.get(f"/api/v1/jobs/{job_id}").json()
    files = [f.name for f in config_dir.iterdir() if f.name.startswith("jobs.jsonl")]
    assert len(files) >= 2, files                                   # rotation happened
    with live_server(_cfg(folder, (scratch,)), config_dir=str(config_dir),
                     env_extra={"DIGUP_SERVER_JOB_LOG_SIZE": "900"}) as replay:
        history = {j["id"]: j["state"] for j in replay.get("/api/v1/jobs").json()["jobs"]}
        assert set(ids) <= set(history)                             # every job visible
        assert all(state == "done" for jid, state in history.items() if jid in ids)
