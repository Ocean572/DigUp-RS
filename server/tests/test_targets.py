"""Stage 13 — named indexes and minted application keys (`targets.rs`, `mint_tokens`,
target-scoped bearer gate, the API's per-target supervision).

The promise under test is the user's flow, end to end: the compose server runs
standalone over its media index; an application (Nextcloud's bridge) gets its own minted
key, pastes it into its settings, and every call that carries it — status, search, index,
jobs, media — answers the application's OWN index. The media index is untouched by that
application's crawl, one writer per index keeps both crawls independent, and no token
ever answers a target it was not minted for.

Plan P3 put every one of these on the live server: the registry knobs are the env the
container ships (`DIGUP_SERVER_APP_TARGETS`, `DIGUP_SERVER_MINT_KEYS`, `DIGUP_SERVER_CONFIG`
— the config folder is the harness's own tmp dir), and a registry the validator rejects
kills the boot, so the malformed shapes are pinned as a server that refuses to come up,
quoting its rule in its log. Minted keys are read where they live — `<config>/tokens.json`,
0600 — and restart-stability is `app.restart()`, the container's own restart.
"""
from __future__ import annotations

import json
import os
import stat
import time
from pathlib import Path

import pytest

from conftest import _cfg
from support import DeadServerError, LiveApp, live_server

CANARY = "A red canoe rests on a glass-still lake under northern lights.\n"


def read_tokens(config_dir):
    """`<config>/tokens.json` as the server wrote it (0600, restart-stable)."""
    return json.loads(Path(config_dir, "tokens.json").read_text(encoding="utf-8"))


def keys_of(config_dir):
    """(minted admin key or None, minted reader keys). An env-typed admin is never
    stored in the file — the deployments that use it carry their own admin header."""
    keys = read_tokens(config_dir)
    return keys.get("admin"), keys.get("readers", {})


def audit_lines(app: LiveApp) -> list[str]:
    return [line for line in app.tail(200_000).splitlines() if "principal=" in line]


# ── registry: parsing, validation, merging (the env shape the container ships) ──────────

def test_the_registry_parses_the_env_shape_exactly_as_documented(tmp_path):
    """Two targets, one with two roots: the rows the running server answers ARE the parse —
    roots in order, the last folder the index, per-target supervision independent."""
    roots = tmp_path / "nc-a"
    roots2 = tmp_path / "nc-b"
    dc = tmp_path / "dc"
    for d in (roots, roots2, dc, tmp_path / "media"):
        d.mkdir()
    env = {
        "DIGUP_SERVER_APP_TARGETS":
            f"nc={roots}:{roots2}:{tmp_path / 'idx/nc'};dc={dc}:{tmp_path / 'idx/dc'}",
        "DIGUP_SERVER_MINT_KEYS": "1",
    }
    with live_server(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                     config_dir=str(tmp_path / "config"), env_extra=env) as c:
        adm, _ = keys_of(tmp_path / "config")
        rows = {t["name"]: t for t in c.get("/api/v1/targets",
                                            headers={"Authorization": f"Bearer {adm}"}
                                            ).json()["targets"]}
        assert rows["nc"]["roots"] == [str(roots), str(roots2)]
        assert rows["nc"]["index"] == str(tmp_path / "idx/nc")
        assert rows["dc"]["roots"] == [str(dc)] and rows["dc"]["index"] == str(tmp_path / "idx/dc")


def test_the_registry_refuses_every_shape_that_could_silently_break_a_target(tmp_path):
    """Every malformed shape kills the boot, quoting its rule: a target that parses to
    something silently wrong (a missing index, a relative path, a reserved name, a typo in a
    later entry) is a crawl that never lands where the operator thinks it does."""
    bad = ["x=/media/a",                        # no index folder
           "media=/media/a:/idx",               # reserved name
           "nc=relative/path:/idx/nc",          # root not absolute
           "nc=/media/a:relative-index",        # index not absolute
           "nc=/media/a:",                      # empty index
           "=/media/a:/idx",                    # empty name
           "nc=/media/a:/idx/nc;;dc=/media/b"]  # a later entry is broken
    for shape in bad:
        app = LiveApp(_cfg(tmp_path / "main-index", (tmp_path,)),
                      config_dir=str(tmp_path / f"bad-{abs(hash(shape))}"),
                      env_extra={"DIGUP_SERVER_APP_TARGETS": shape})
        with pytest.raises(DeadServerError):
            app.start()
        assert "target" in app.tail(4000).lower(), (shape, app.tail(1200))


def test_registry_merges_the_file_and_the_env_with_env_winning(tmp_path):
    """`<config>/targets.json` is where a runtime registration persists; the environment is
    the operator's declaration. The registry the running server answers is the merge, env
    winning on a name both name — and a file-only target joins it."""
    (tmp_path / "config").mkdir()
    (tmp_path / "config" / "targets.json").write_text(json.dumps({
        "dc": {"roots": [str(tmp_path / "media/from-file")], "index": str(tmp_path / "idx/file-dc")},
        "fl": {"roots": [str(tmp_path / "media/fl")], "index": str(tmp_path / "idx/fl")},
    }), encoding="utf-8")
    (tmp_path / "media").mkdir()
    env = {"DIGUP_SERVER_APP_TARGETS": f"dc={tmp_path / 'media/env'}:{tmp_path / 'idx/env-dc'}",
           "DIGUP_SERVER_MINT_KEYS": "1"}
    with live_server(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                     config_dir=str(tmp_path / "config"), env_extra=env) as c:
        adm, _ = keys_of(tmp_path / "config")
        A = {"Authorization": f"Bearer {adm}"}
        rows = {t["name"]: t for t in c.get("/api/v1/targets", headers=A).json()["targets"]}
        main = [row for row in rows.values() if row["main"]]
        assert main and main[0]["roots"] == [str(tmp_path / "media")]
        assert rows["dc"]["roots"] == [str(tmp_path / "media/env")]
        assert rows["dc"]["index"] == str(tmp_path / "idx/env-dc")
        assert rows["fl"]["roots"] == [str(tmp_path / "media/fl")]   # file-only target joined


def test_a_target_may_not_be_the_main_index_itself(tmp_path):
    (tmp_path / "config").mkdir()
    (tmp_path / "config" / "targets.json").write_text(
        json.dumps({"x": {"roots": [str(tmp_path / "media/a")],
                          "index": str(tmp_path / "main-index")}}), encoding="utf-8")
    (tmp_path / "media").mkdir()
    app = LiveApp(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                  config_dir=str(tmp_path / "config"))
    with pytest.raises(DeadServerError):
        app.start()
    assert "separate indexes" in app.tail(4000)


# ── minted keys: inert alone, stable once, env-authoritative, 0600 ─────────────────────

def test_minting_is_inert_without_targets_a_standalone_server_stays_exactly_it_was(tmp_path):
    (tmp_path / "media").mkdir()
    config = tmp_path / "config"
    with live_server(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                     config_dir=str(config)) as c:
        assert not (config / "tokens.json").exists()                 # nothing written, nothing minted
        assert c.get("/api/v1/status").status_code in (200, 503)     # nothing guarded
        assert c.post("/api/v1/index", json={}).status_code in (202, 503)


def test_minted_keys_fire_for_targets_are_restart_stable_and_live_0600(tmp_path):
    (tmp_path / "media").mkdir()
    config = tmp_path / "config"
    env = {"DIGUP_SERVER_APP_TARGETS": f"nc={tmp_path / 'media'}:{tmp_path / 'app-index'}"}
    app = LiveApp(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                  config_dir=str(config), env_extra=env)
    app.start()
    try:
        adm, readers = keys_of(config)
        assert adm and readers["app:nc"]
        mode = stat.S_IMODE(os.stat(config / "tokens.json").st_mode)
        assert mode == 0o600
        app.stop()
        # A restart re-reads the file: same keys, and still keys the server MINTED
        # (file-backed keys are server-generated keys: displayable on every boot, not only
        # the boot that generated them — the UI shows what the operator never typed).
        app.start()
        again = read_tokens(config)
        assert again["admin"] == adm and again["readers"]["app:nc"] == readers["app:nc"]
        tk = app.client.get("/api/v1/tokens", headers={"Authorization": f"Bearer {adm}"}).json()
        assert tk["admin"] == adm and tk["apps"][0]["token"] == readers["app:nc"]
        assert tk["apps"][0]["generated"] is True
    finally:
        app.stop()


def test_environment_tokens_are_authority_mint_only_fills_the_gaps(tmp_path):
    (tmp_path / "media").mkdir()
    config = tmp_path / "config"
    env = {"DIGUP_SERVER_APP_TARGETS":
           f"nc={tmp_path / 'media'}:{tmp_path / 'app-index'};dc={tmp_path / 'media'}:{tmp_path / 'app-index-dc'}",
           "DIGUP_SERVER_ADMIN_TOKEN": "adm",
           "DIGUP_SERVER_READ_TOKENS": f"app:nc:toknc"}
    with live_server(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                     config_dir=str(config), env_extra=env) as c:
        stored = read_tokens(config)
        assert "admin" not in stored                                  # env secrets never stored
        assert stored.get("readers", {}).get("app:nc") is None         # ditto
        app_dc = [a for a in c.get("/api/v1/tokens",
                                   headers={"Authorization": "Bearer adm"}).json()["apps"]
                  if a["name"] == "dc"]
        assert app_dc and app_dc[0]["generated"] is True               # gap filled for the rest
        assert app_dc[0]["token"] == stored["readers"]["app:dc"]
        nc = [a for a in c.get("/api/v1/tokens",
                               headers={"Authorization": "Bearer adm"}).json()["apps"]
              if a["name"] == "nc"]
        assert nc and nc[0]["token"] is None and nc[0]["generated"] is False   # never echoed
        # The env token keeps its authority: it answers its own target.
        assert c.get("/api/v1/status",
                     headers={"Authorization": "Bearer toknc"}).json()["target"] == "nc"


def test_rotation_offers_nothing_where_nothing_is_minted(tmp_path):
    """`rotate_minted` returns None when nothing is minted. Two live shapes of that None:
    a read-token deployment has no admin scope to ask with (403 naming the admin token),
    and the env-keyed deployment's own rotate answers 400 quoting the environment as the
    authority — `test_an_env_keyed_deployment_rotates_nothing` pins the second."""
    (tmp_path / "media").mkdir()
    config = tmp_path / "config"
    with live_server(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                     config_dir=str(config),
                     env_extra={"DIGUP_SERVER_TOKEN": "plain"}) as c:
        refused = c.post("/api/v1/tokens", headers={"Authorization": "Bearer plain"})
        assert refused.status_code == 403 and "admin" in refused.json()["detail"]
        assert not (config / "tokens.json").exists()


def test_read_token_names_may_carry_colons_the_last_one_is_the_separator(tmp_path):
    """`app:nc:tokB` — a target-bound name, separated at the LAST colon. The live proof is
    the scope the name binds: the token answers target `nc` and nothing else."""
    (tmp_path / "media").mkdir()
    with live_server(_cfg(tmp_path / "main-index", (tmp_path / "media",)),
                     config_dir=str(tmp_path / "config"),
                     env_extra={"DIGUP_SERVER_APP_TARGETS":
                                f"nc={tmp_path / 'media'}:{tmp_path / 'app-index'}",
                                "DIGUP_SERVER_READ_TOKENS": "nc-a:tokA,app:nc:tokB"}) as c:
        N = {"Authorization": "Bearer tokB"}
        assert c.get("/api/v1/status", headers=N).json()["target"] == "nc"
        assert c.get("/api/v1/status", params={"target": "media"}, headers=N).status_code == 403
        assert c.post("/api/v1/search", json={"q": "x", "limit": 1},
                      headers=N).status_code in (200, 403, 503)


# ── the bearer gate binds tokens to targets (live answers, no engine crawl) ─────────────

@pytest.fixture(scope="module")
def bound_client(tmp_path_factory):
    """The scope matrix's deployment: an admin key, a plain read key, one application whose
    key the server minted itself (env left that gap open). The header sets are prebuilt."""
    roots = tmp_path_factory.mktemp("bound-media")
    config = tmp_path_factory.mktemp("bound-config")
    app = LiveApp(_cfg(tmp_path_factory.mktemp("bound-main-index"), (roots,)),
                  config_dir=str(config),
                  env_extra={"DIGUP_SERVER_ADMIN_TOKEN": "adm",
                             "DIGUP_SERVER_TOKEN": "plain",
                             "DIGUP_SERVER_APP_TARGETS":
                                 f"nc={roots}:{tmp_path_factory.mktemp('bound-app-index')}"})
    app.start()
    app.adm = {"Authorization": "Bearer adm"}
    app.plain = {"Authorization": "Bearer plain"}
    app.nc = {"Authorization": f"Bearer {keys_of(config)[1]['app:nc']}"}
    try:
        yield app
    finally:
        app.stop()


def _index(c, headers, params=None):
    """Queue one crawl on THAT index and run it to its end. A writer that just finished
    keeps its flock for the closing moment (the engine's `stop` → helper exit), so the
    queue is asked until it is accepted — the honest 409 is answered, never assumed,
    exactly the rule `test_two_targets_crawl_independently…` pins between targets."""
    deadline = time.monotonic() + 120.0
    while time.monotonic() < deadline:
        r = c.post("/api/v1/index", json={}, params=params, headers=headers)
        if r.status_code == 409:
            time.sleep(1.0)
            continue
        assert r.status_code == 202, (r.status_code, r.text)
        assert wait_final(c, r.json()["job"]["id"], headers, params) == "done"
        return
    pytest.fail("the index queue never accepted a crawl")


def test_app_tokens_answer_their_own_target_and_nothing_else(bound_client):
    c = bound_client.client
    nc, adm, plain = bound_client.nc, bound_client.adm, bound_client.plain
    # A token that is nobody's is refused before any target logic runs.
    assert c.get("/api/v1/status",
                 headers={"Authorization": "Bearer not-this-servers-token"}).status_code == 401
    # The binding IS the token's default: plain paths answer its own index (the bridge
    # needs no target parameter), and any explicit target it does not match is refused.
    r = c.get("/api/v1/status", headers=nc)
    assert r.status_code == 200 and r.json()["target"] == "nc"
    assert c.get("/api/v1/status", params={"target": "nc"}, headers=nc).status_code == 200
    assert c.get("/api/v1/status", params={"target": "media"}, headers=nc).status_code == 403
    assert c.get("/api/v1/status", params={"target": "other"}, headers=nc).status_code == 403
    # Mutating routes: an app token for its own target queues its own crawl…
    _index(c, nc)                                   # own target by default
    assert c.post("/api/v1/index", json={}, params={"target": "media"},
                  headers=nc).status_code == 403    # …and never the media crawl
    _index(c, adm)                                  # media: admin's own
    _index(c, adm, {"target": "nc"})                # admin answers every target
    assert c.get("/api/v1/status", params={"target": "nc"}, headers=adm).status_code == 200
    # A plain read token answers the main index and never an application's.
    assert c.get("/api/v1/status", params={"target": "nc"},
                 headers=plain).status_code == 403
    assert c.get("/api/v1/status", headers=plain).status_code == 200


def test_the_principal_log_names_the_application_that_asked(bound_client):
    """The audit line names WHO asked and WHICH index it addressed (named indexes, stage 13)."""
    base = len(audit_lines(bound_client))
    bound_client.client.get("/api/v1/status", params={"target": "nc"}, headers=bound_client.nc)
    lines = audit_lines(bound_client)[base:]
    assert any("principal=app:nc" in line for line in lines)
    assert any("target=nc" in line for line in lines)


# ── end to end: minted key → target machinery, over the real engine ────────────────────

def test_an_application_key_answers_only_its_own_index_end_to_end(scratch, tmp_path):
    main_index = tmp_path / "main-index"
    app_index = tmp_path / "app-index"
    media = tmp_path / "media"
    media.mkdir()
    config = tmp_path / "server-config"
    env = {"DIGUP_SERVER_APP_TARGETS": f"nc={scratch}:{app_index}"}
    app = LiveApp(_cfg(main_index, (media,)), config_dir=str(config), env_extra=env)
    app.start()
    try:
        c = app.client
        adm, readers = keys_of(config)
        nctok, adm = readers["app:nc"], adm
        nc = {"Authorization": f"Bearer {nctok}"}
        admin = {"Authorization": f"Bearer {adm}"}

        # The target's status answers the target's roots, the media index stays the app's.
        st = c.get("/api/v1/status", headers=nc).json()
        assert st["roots"] == [str(scratch)] and st["target"] == "nc"
        assert c.get("/api/v1/status", headers=admin).json()["roots"] == [str(media)]

        # The application queues its OWN crawl — and never the media crawl.
        assert c.post("/api/v1/index", json={}, headers=nc).status_code == 202
        assert c.post("/api/v1/index", json={}, params={"target": "media"},
                      headers=nc).status_code == 403
        job_id = c.get("/api/v1/jobs", headers=nc).json()["jobs"][-1]["id"]
        assert wait_final(c, job_id, nc) == "done"
        st = c.get("/api/v1/status", headers=nc).json()
        assert st["files"] > 0 and st["searchable"] > 0
        assert c.get("/api/v1/status", headers=admin).json()["files"] == 0   # media untouched

        # Its own search answers from its own index; the media index has none of its files.
        hits = c.post("/api/v1/search", json={"q": "campfire stories", "limit": 5},
                      headers=nc).json()["hits"]
        assert hits and all(h["path"].startswith(str(scratch)) for h in hits)
        assert c.post("/api/v1/search", json={"q": "campfire stories"},
                      headers=admin).json()["hits"] == []

        # Media handles resolve their target: the app's handle previews through its
        # engine. The preview contract is the engine's: it renders image/pdf/video
        # segments, and a text doc answers the honest "unreadable: can't decode
        # image" refusal forever — so preview the IMAGE hit (and poll: right after a
        # job the searcher is mid reload→prewarm, and refusals are states, pinned
        # 503s in test_api, that clients retry).
        media_id = next(h["media_id"] for h in hits if h["kind"] == "image")
        assert wait_status(c, f"/api/v1/media/{media_id}/preview", nc, 200) == 200
        assert wait_status(c, f"/api/v1/media/{media_id}/preview", admin, 200) == 200

        # Jobs are per target: the app sees its own history; its log is its own file.
        assert all(j["state"] in ("done", "interrupted")
                   for j in c.get("/api/v1/jobs", headers=nc).json()["jobs"])
        assert c.get("/api/v1/jobs?target=media", headers=nc).status_code == 403
        assert (config / "jobs-nc.jsonl").exists()

        # Targets listing: admin sees both with counts; the app sees only its own.
        # The counts come from the CLI's own table (kinds total + vectors) — no engine
        # spawn for a listing.
        rows = {t["name"]: t for t in c.get("/api/v1/targets", headers=admin).json()["targets"]}
        assert set(rows) == {"media", "nc"}
        assert rows["nc"]["files"] > 0 and rows["nc"]["searchable"] > 0, \
            (rows["nc"]["files"], rows["nc"]["searchable"])
        assert rows["media"]["files"] == 0, rows["media"]["files"]
        assert {t["name"] for t in c.get("/api/v1/targets", headers=nc).json()["targets"]} == {"nc"}

        # Tokens: minted values shown to admin only, never to an app token.
        t = c.get("/api/v1/tokens", headers=admin).json()
        assert t["admin"] == adm and t["apps"][0]["token"] == nctok
        assert c.get("/api/v1/tokens", headers=nc).status_code == 403
    finally:
        app.stop()


def test_two_targets_crawl_independently_one_writer_per_index(scratch, tmp_path):
    """The busy guarantee is per index: a running job on target A never blocks target B
    (the flock lives beside each index folder) and never blocks the media crawl."""
    app_a, app_b = tmp_path / "idx-a", tmp_path / "idx-b"
    roots_a = tmp_path / "media-a"
    roots_a.mkdir()
    config = tmp_path / "config"
    env = {"DIGUP_SERVER_APP_TARGETS": f"a={scratch}:{app_a};b={roots_a}:{app_b}"}
    with live_server(_cfg(tmp_path / "main-index", (roots_a,)),
                     config_dir=str(config), env_extra=env) as c:
        adm, readers = keys_of(config)
        adm = {"Authorization": f"Bearer {adm}"}
        tok_a = {"Authorization": f"Bearer {readers['app:a']}"}
        tok_b = {"Authorization": f"Bearer {readers['app:b']}"}

        job = c.post("/api/v1/index", json={}, headers=tok_a).json()["job"]
        assert job["state"] in ("queued", "running")
        # Same target, second request: the honest 409 (one writer per index).
        busy = c.post("/api/v1/index", json={}, headers=tok_a)
        assert busy.status_code == 409 and job["id"] in busy.json()["detail"]
        # A different target answers 202 right away — independent writers, independent logs.
        assert c.post("/api/v1/index", json={}, headers=tok_b).status_code == 202
        # The media crawl is independent of both.
        assert c.post("/api/v1/index", json={}, headers=adm).status_code == 202
        for tok in (tok_a, tok_b):
            deadline = time.monotonic() + 900
            while time.monotonic() < deadline:
                jobs = c.get("/api/v1/jobs", headers=tok).json()["jobs"]
                if jobs and jobs[-1]["state"] in ("done", "interrupted"):
                    break
                time.sleep(1)
            assert jobs[-1]["state"] == "done"
        for name in ("a", "b"):
            assert (config / f"jobs-{name}.jsonl").exists()
        assert (config / "jobs.jsonl").exists()


def wait_final(client, job_id, headers, params=None, timeout=900.0) -> str:
    """Poll the job record to its final state. `params` MUST carry the `target` the job was
    queued on: job ids live in the TARGET's runner, and a `GET /jobs/{id}` with no `target`
    answers the caller's own — an admin watching an application's crawl must name it."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        state = client.get(f"/api/v1/jobs/{job_id}", params=params,
                           headers=headers).json()["job"]["state"]
        if state in ("done", "interrupted", "busy"):
            return state
        time.sleep(0.5)
    pytest.fail(f"job {job_id} never reached a final state within {timeout:g} s")


def wait_status(client, url, headers, want, timeout=120.0) -> int:
    """Poll a GET until it answers `want` — a searcher mid reload→prewarm answers the
    engine's 503 refusal, which the API surfaces verbatim (test_api pins it); clients
    retry, and so does this contract test. The failure names the LAST answer's body:
    the engine's own words are the diagnosis."""
    deadline = time.monotonic() + timeout
    code, body = None, ""
    while time.monotonic() < deadline:
        r = client.get(url, headers=headers)
        code, body = r.status_code, r.text[:200]
        if code == want:
            return code
        time.sleep(1.0)
    pytest.fail(f"{url} never answered {want} within {timeout:g} s "
                f"(last {code}: {body})")


# ── POST /api/v1/tokens — the UI's Regenerate (admin scope, minted keys only) ─────────

def test_the_admin_can_regenerate_the_minted_keys_and_the_old_values_die(tmp_path):
    config = tmp_path / "server-config"
    media = tmp_path / "media"
    media.mkdir()
    env = {"DIGUP_SERVER_APP_TARGETS": f"nc={media}:{tmp_path / 'app-index'}"}
    with live_server(_cfg(tmp_path / "main-index", (media,)),
                     config_dir=str(config), env_extra=env) as c:
        adm, readers = keys_of(config)
        adm, old_nc = adm, readers["app:nc"]
        A = {"Authorization": f"Bearer {adm}"}
        assert c.get("/api/v1/tokens", headers=A).json()["admin"] == adm
        r = c.post("/api/v1/tokens", headers=A)
        assert r.status_code == 200 and r.json()["rotated"] is True
        new_admin, new_nc = r.json()["admin"], r.json()["apps"][0]["token"]
        assert new_admin != adm and new_nc != old_nc
        # The rotation takes effect immediately — the old values die, the new ones answer:
        assert c.get("/api/v1/status", headers={"Authorization": f"Bearer {old_nc}"}
                     ).status_code == 401
        assert c.get("/api/v1/status", headers=A).status_code == 401
        st = c.get("/api/v1/status", headers={"Authorization": f"Bearer {new_nc}"})
        assert st.status_code == 200 and st.json()["target"] == "nc"
        assert c.post("/api/v1/tokens",
                      headers={"Authorization": f"Bearer {new_nc}"}).status_code == 403
        fresh = read_tokens(config)
        assert fresh["admin"] == new_admin and fresh["readers"]["app:nc"] == new_nc
        assert stat.S_IMODE(os.stat(config / "tokens.json").st_mode) == 0o600


def test_an_env_keyed_deployment_rotates_nothing(tmp_path):
    config = tmp_path / "server-config"
    media = tmp_path / "media"
    media.mkdir()
    with live_server(_cfg(tmp_path / "main-index", (media,)),
                     config_dir=str(config),
                     env_extra={"DIGUP_SERVER_ADMIN_TOKEN": "adm",
                                "DIGUP_SERVER_TOKEN": None,
                                "DIGUP_SERVER_APP_TARGETS": None,
                                "DIGUP_SERVER_MINT_KEYS": None}) as c:
        A = {"Authorization": "Bearer adm"}
        t = c.get("/api/v1/tokens", headers=A).json()
        assert t["admin"] is None and t["admin_configured"] is True    # never echoed
        # The typed-secrets shape stays guarded: a keyless call reaches the endpoint
        # (the gate's open door is the main index's READS; the keys surface is not a
        # read) and the endpoint refuses it — 403, keys shown to the admin token only.
        # Round 6 draws the open-DISPLAY line at SERVER-MINTED keys only.
        assert c.get("/api/v1/tokens").status_code == 403
        r = c.post("/api/v1/tokens", headers=A)
        assert r.status_code == 400 and "environment" in r.json()["detail"]
        assert not (config / "tokens.json").exists()


# ── the minted shape answers the MAIN index's reads openly (2026-11-08 user brief) ────

def test_a_minted_key_deployment_answers_main_reads_openly(tmp_path):
    """The user's model: the standalone box is searchable with no key; the minted keys
    are what the APPLICATIONS get. Keys typed in the environment (the guarded shape)
    keep guarding every call — test_security pins that shape unchanged."""
    config = tmp_path / "server-config"
    media = tmp_path / "media"
    media.mkdir()
    env = {"DIGUP_SERVER_APP_TARGETS": f"nc={media}:{tmp_path / 'app-index'}"}
    with live_server(_cfg(tmp_path / "main-index", (media,)),
                     config_dir=str(config), env_extra=env) as c:
        keys = read_tokens(config)
        nc = {"Authorization": f"Bearer {keys['readers']['app:nc']}"}
        # No key: the main index answers reads (status, search, targets).
        st = c.get("/api/v1/status").json()
        assert st["target"] == "media" and st["files"] == 0
        assert c.post("/api/v1/search", json={"q": "wakeboarding", "limit": 5}
                      ).status_code == 200
        names = {t["name"] for t in c.get("/api/v1/targets").json()["targets"]}
        assert "media" in names                                       # the gear's rows
        # What the keys are FOR stays guarded: the index operations, the keys surface,
        # and the application's OWN index (the open door is the main index's reads).
        assert c.post("/api/v1/index", json={}).status_code == 401
        assert c.post("/api/v1/estimate", json={}).status_code == 401
        assert c.get("/api/v1/status", params={"target": "nc"}).status_code == 401
        # Round 6 (the ticket model): a minted deployment shows its keys to the gear
        # WITHOUT a key in the browser — the same open shape as the main page's reads;
        # an application's own key never lists the server's keys.
        tk = c.get("/api/v1/tokens").json()
        assert tk["admin"] == keys["admin"] and any(a["name"] == "nc" for a in tk["apps"])
        assert c.get("/api/v1/tokens", headers=nc).status_code == 403
        # The app key keeps answering its own target and nothing else.
        assert c.get("/api/v1/status", headers=nc).json()["target"] == "nc"
        assert c.get("/api/v1/status", params={"target": "media"},
                     headers=nc).status_code == 403


def test_an_open_principal_never_previews_an_applications_files(tmp_path, scratch):
    """The minted shape opens the MAIN index's reads — media handles included, so the
    media-id path must stay scoped: a key-less request for a handle whose file lives in
    an application's roots is refused; the application's own key previews it."""
    config = tmp_path / "server-config"
    media = tmp_path / "media"
    media.mkdir()
    env = {"DIGUP_SERVER_APP_TARGETS": f"nc={scratch}:{tmp_path / 'app-index'}"}
    with live_server(_cfg(tmp_path / "main-index", (media,)),
                     config_dir=str(config), env_extra=env) as c:
        keys = read_tokens(config)
        nc = {"Authorization": f"Bearer {keys['readers']['app:nc']}"}
        assert c.post("/api/v1/index", json={}, headers=nc).status_code == 202
        job_id = c.get("/api/v1/jobs", headers=nc).json()["jobs"][-1]["id"]
        assert wait_final(c, job_id, nc) == "done"
        hits = c.post("/api/v1/search", json={"q": "campfire stories", "limit": 5},
                      headers=nc).json()["hits"]
        media_id = next(h["media_id"] for h in hits if h["kind"] == "image")
        assert wait_status(c, f"/api/v1/media/{media_id}/preview", nc, 200) == 200
        assert c.get(f"/api/v1/media/{media_id}/preview").status_code == 403
        assert c.get(f"/api/v1/media/{media_id}/file").status_code == 403
        assert c.get(f"/api/v1/media/{media_id}/text").status_code == 403


# ── stage 14: the standalone mints its key; applications register at runtime ──────────

def test_the_standalone_mints_its_api_key_when_asked(tmp_path):
    """DIGUP_SERVER_MINT_KEYS=1: the key exists at boot with no application registered —
    the gear's Copy row. Main reads stay open; the key guards index/estimate/keys."""
    config = tmp_path / "server-config"
    media = tmp_path / "media"
    media.mkdir()
    with live_server(_cfg(tmp_path / "main-index", (media,)),
                     config_dir=str(config),
                     env_extra={"DIGUP_SERVER_MINT_KEYS": "1",
                                "DIGUP_SERVER_APP_TARGETS": None,
                                "DIGUP_SERVER_TOKEN": None,
                                "DIGUP_SERVER_ADMIN_TOKEN": None}) as c:
        path = config / "tokens.json"
        adm = read_tokens(config)["admin"]
        A = {"Authorization": f"Bearer {adm}"}
        assert adm and stat.S_IMODE(os.stat(path).st_mode) == 0o600
        assert c.get("/api/v1/status").status_code == 200          # main reads: open
        assert c.post("/api/v1/index", json={}).status_code == 401  # mutations: keyed
        assert c.post("/api/v1/estimate", json={}).status_code == 401
        # Round 6: the minted key is DISPLAYED with no key in the browser — the gear's
        # Copy row is the ticket; the admin call shows the same value.
        tk = c.get("/api/v1/tokens").json()
        assert tk["admin"] == adm and tk["apps"] == []
        t = c.get("/api/v1/tokens", headers=A).json()
        assert t["admin"] == adm and t["apps"] == []
        assert c.post("/api/v1/estimate", headers=A, json={}).status_code == 200
        # An application's key never lists the server's keys.
        reg = c.post("/api/v1/targets", headers=A,
                     json={"name": "demo", "roots": [str(media)]})
        assert reg.status_code == 200
        assert c.get("/api/v1/tokens",
                     headers={"Authorization": f"Bearer {reg.json()['token']}"}).status_code == 403


def test_an_application_registers_at_a_running_server_and_gets_its_own_key(tmp_path, scratch):
    """The user's flow as one call: standalone key → POST /targets(name, roots) →
    private minted key + own index folder (default <config>/index-<name>) → its crawl
    runs in the background; the main page never sees it; the registration survives
    restart (targets.json + tokens.json)."""
    config = tmp_path / "server-config"
    media = tmp_path / "media"
    media.mkdir()
    app = LiveApp(_cfg(tmp_path / "main-index", (media,)), config_dir=str(config),
                  env_extra={"DIGUP_SERVER_MINT_KEYS": "1"})
    app.start()
    try:
        c = app.client
        adm = read_tokens(config)["admin"]
        A = {"Authorization": f"Bearer {adm}"}
        r = c.post("/api/v1/targets", headers=A, json={"name": "nc", "roots": [str(scratch)]})
        assert r.status_code == 200, r.text
        reg = r.json()
        assert reg["generated"] is True
        assert reg["index"] == str(config / "index-nc")
        N = {"Authorization": f"Bearer {reg['token']}"}
        assert c.get("/api/v1/status", headers=N).json()["target"] == "nc"
        assert c.get("/api/v1/status", params={"target": "media"},
                     headers=N).status_code == 403
        # its OWN crawl, in the background — and never anyone else's
        assert c.post("/api/v1/index", json={}, headers=N).status_code == 202
        job_id = c.get("/api/v1/jobs", headers=N).json()["jobs"][-1]["id"]
        assert wait_final(c, job_id, N) == "done"
        assert c.get("/api/v1/status", headers=N).json()["files"] > 0
        assert c.get("/api/v1/status").json()["files"] == 0            # main untouched
        # persisted: a restart re-reads both files (the keys and the registration)
        tj = json.loads((config / "targets.json").read_text(encoding="utf-8"))
        assert tj["nc"]["roots"] == [str(scratch)]
        tk = read_tokens(config)
        assert tk["readers"]["app:nc"] == reg["token"]
        app.stop()
        app.start()                                     # the container's own restart
        c = app.client
        assert c.get("/api/v1/status", headers=N).json()["target"] == "nc"
        # the startup validator runs at runtime: every refusal quotes its rule
        bad = [({"name": "media", "roots": [str(scratch)]}, 400),
               ({"name": "nc", "roots": [str(scratch)]}, 409),
               ({"name": "Bad Name", "roots": [str(scratch)]}, 400),
               ({"name": "x", "roots": ["relative/root"]}, 400),
               ({"name": "y", "roots": [str(scratch)],
                 "index": str(tmp_path / "main-index")}, 400)]
        for body, want in bad:
            assert c.post("/api/v1/targets", headers=A, json=body).status_code == want
        # key-less and application-keyed requests cannot register anything
        assert c.post("/api/v1/targets",
                      json={"name": "z", "roots": [str(scratch)]}).status_code == 403
        assert c.post("/api/v1/targets", headers=N,
                      json={"name": "z", "roots": [str(scratch)]}).status_code == 403
        # the open principal never previews the application's files
        hits = c.post("/api/v1/search", headers=N, json={"q": "campfire stories",
                      "limit": 5}).json()["hits"]
        media_id = next(h["media_id"] for h in hits if h["kind"] == "image")
        assert wait_status(c, f"/api/v1/media/{media_id}/preview", N, 200) == 200
        assert c.get(f"/api/v1/media/{media_id}/preview").status_code == 403
    finally:
        app.stop()
