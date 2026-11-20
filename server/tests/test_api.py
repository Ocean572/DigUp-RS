"""The API contract (Stage 4): `/api/v1` over the real adapter, the real engine, no mocks.

Runs in the `server-test` container: TestClient drives the FastAPI app in-process; the
app's lifespan spawns `search-service` as a **direct child of pytest** — a live parent,
which is the ParentWatch rule (reconnaissance §15 item 8) the API must keep in production.

Every assertion here is about *pass-through*: the API invents nothing and hides nothing —
the engine's ranking, fields, timings, empties and errors reach the client as the engine
made them, minus the protocol's own `id` bookkeeping.
"""

from __future__ import annotations

import struct
import threading
import time
from pathlib import Path

import pytest

# The live shape (plan P3): the shipping `digup-server` binary over HTTP — the suite is a
# thin client of the product, the same relationship every UI has to it. The server's
# helpers are its own children (ParentWatch's chain), and the fixture's teardown is
# SIGTERM: the measured stop contract (interrupted + 143 inside the grace).
from conftest import _cfg
from support import LiveApp, live_server

# The three wakeboard media — the corpus's planted answer to "wakeboarding". Their ORDER
# is not portable: the embedding matmul moves with CPU features. Measured 2026-11-04 on
# x86_64 (the suite container, this box): logan 1.571 > vienna 1.531 > aarhus-04 0.441;
# the arm64-Mac measurements the strict pin carried before ranked vienna first — the top
# two are 0.040 z apart, inside cross-platform noise. What IS portable (and is exactly
# the server's contract) is WHICH three files answer the query, and that they clear the
# engine's gate: the server relays the engine's per-platform order verbatim — that
# pass-through is pinned by the hit-field and reply-shape tests, not by hard-coding one
# machine's ranking here.
WAKEBOARD_TOP3 = {"wakeboarding-vienna.webm", "wakeboard-logan.jpg", "wakeboarding-aarhus-04.jpg"}


@pytest.fixture(scope="module")
def client(corpus, index, tmp_path_factory):
    with live_server(_cfg(index, (corpus,)),
                     config_dir=str(tmp_path_factory.mktemp("api-config"))) as c:
        yield c


@pytest.fixture(scope="module")
def big_photo(tmp_path_factory):
    """One indexed photo BIGGER than the 8 MiB range cap, in its own tree and index.

    Written in Python as a 4096×4096 24-bit BMP (50 MB, decodable by the engine's stb
    seam), named `big png.png` so the query's words are the name's own FTS tokens
    (`big`, `png` — the name segment splits on spaces and slashes). A fixture that
    *renders* with ffmpeg needs lavfi: the payload's ffmpeg is decode-only (measured:
    the mandelbrot source dies with exit 234 on it), and the suite must not grow a
    dependency on an apt ffmpeg inside the harness image for one fixture. A BMP is
    dependency-free, past the cap by construction, and keyword-matchable by name."""
    root = tmp_path_factory.mktemp("big-photo-tree")
    path = root / "big png.png"
    width = height = 4096
    row = b"\x13\x37\x59" * width                      # 24-bit BGR; 12288 B ≡ 0 (mod 4)
    pixels = row * height
    header = struct.pack("<2sIHHI", b"BM", 54 + len(pixels), 0, 0, 54)
    header += struct.pack("<IiiHHIIiiII", 40, width, height, 1, 24, 0,
                          len(pixels), 0, 0, 0, 0)
    path.write_bytes(header + pixels)
    assert path.stat().st_size > 8 * 1024 * 1024       # the fixture's promise, by size
    folder = tmp_path_factory.mktemp("big-photo-index")
    app = LiveApp(_cfg(folder, (root,)), config_dir=str(tmp_path_factory.mktemp("big-photo-config")))
    app.start()
    try:
        job = app.client.post("/api/v1/index", json={})
        assert job.status_code == 202, job.text
        deadline = time.time() + 600
        while time.time() < deadline:
            st = app.client.get("/api/v1/status").json()
            if st.get("pending") == 0 and st.get("searchable", 0) >= 1:
                break
            time.sleep(1)
        reply = app.client.post("/api/v1/search", json={"q": "big png", "limit": 10}).json()
        ids = [h["media_id"] for h in reply["hits"] if h.get("kind") == "image"]
        assert ids, "no image handle issued for the big photo"
        yield app, ids[0]
    finally:
        app.stop()


@pytest.fixture(scope="module")
def deep_tree(tmp_path_factory):
    """A tree whose engine WALK takes seconds: 300 nested folders × 100 files. The
    busy-estimate rule (one crawl server-wide) and the estimate-timeout knob are only
    observable on a walk that outlives a burst of requests: 12 threads fire inside
    milliseconds, the walk takes ~1 s, so the overlap — the 409 — is guaranteed, and a
    0.01 s ceiling cannot fit a walk of 30k files (the 502 half)."""
    root = tmp_path_factory.mktemp("deep-tree")
    for d in range(300):
        sub = root / f"d{d:03d}"
        sub.mkdir()
        for f in range(100):
            (sub / f"f{f:03d}.txt").write_text("", encoding="utf-8")
    return root


def paths(hits):
    return [Path(h["path"]).name for h in hits]


def test_health_publishes_the_engines_identity(client):
    body = client.get("/api/v1/health").json()
    assert body["ok"] is True
    eng = body["engine"]
    assert "embeddinggemma-2@" in eng["fingerprints"]
    assert eng["build_tag"].startswith("b")           # llama.cpp b11461
    assert eng["vectors_version"] == 1
    live = body["live"]
    assert live["files"] >= 15 and live["searchable"] == live["files"]
    assert live["pending"] == 0
    assert live["encoder"] in {"stopped", "starting", "ready", "unavailable"}


def test_status_adds_the_cli_kind_table(client):
    body = client.get("/api/v1/status").json()
    kinds = {k["kind"]: k["files"] for k in body["kinds"]}
    assert kinds.get("image") == 12 and kinds.get("video") == 3
    assert body["build_tag"].startswith("b")


def test_search_answers_with_the_engines_ranking(client):
    reply = client.post("/api/v1/search", json={"q": "wakeboarding", "limit": 10}).json()
    assert set(paths(reply["hits"])[:3]) == WAKEBOARD_TOP3  # the set is portable; the order is the engine's (see WAKEBOARD_TOP3)
    assert reply["meaning"] is True and reply["strong"] >= 3
    assert {"embed", "search"} <= set(reply["ms"])    # the engine's own milliseconds
    assert "id" not in reply                          # protocol bookkeeping stays inside
    top = reply["hits"][0]
    assert {"path", "kind", "segment", "segment_id", "score", "modified", "size", "info"} \
        <= set(top)                                    # hitFields(), verbatim


def test_the_honest_empty_survives_the_http_leg(client):
    reply = client.post("/api/v1/search",
                        json={"q": "a person riding a wakeboard behind a boat"}).json()
    assert reply["hits"] == []                         # the refusal is published, not filled
    assert reply["meaning"] is True


def test_keyword_only_search_when_meaning_is_off(client):
    reply = client.post("/api/v1/search",
                        json={"q": "wakeboarding", "meaning": False}).json()
    assert reply["meaning"] is False
    assert reply["hits"]                               # keyword-only still answers


def test_kind_filter_is_the_engines(client):
    reply = client.post("/api/v1/search",
                        json={"q": "wakeboarding", "kinds": ["video"]}).json()
    assert reply["hits"] and all(h["kind"] == "video" for h in reply["hits"])


def test_the_default_limit_is_the_engines_default(client):
    """The API's default `limit` equals the engine's own (40, `SearchService.swift:130`): a
    client that names no limit gets the engine's answer, not a shorter one — the server
    invents no cap of its own. Pinned twice: the typed default, and the reply on the same
    query against an explicit 40 (the UI sends 40 explicitly, so nothing client-side moves)."""
    # (the typed default is the binary's own constant — crates/server pins 40 in
    # `cargo test`; what this suite can see is that an unnamed limit and an
    # explicit 40 answer identically, which is the "server invents no cap" fact.)
    quiet = client.post("/api/v1/search", json={"q": "wakeboarding"}).json()
    explicit = client.post("/api/v1/search", json={"q": "wakeboarding", "limit": 40}).json()
    assert paths(quiet["hits"]) == paths(explicit["hits"])
    assert quiet["strong"] == explicit["strong"]


def test_error_bodies_are_one_detail_string(client):
    """API.md's error table is a promise: every documented error is one JSON object whose
    `detail` is safe to show a user verbatim. Pinned on the cheap ones — an unknown job id,
    a dead media handle on all three media endpoints, and the validation case whose `detail`
    is a **list of objects** (the UI renders each item; a page that prints the raw array is
    the bug this pins against)."""
    unknown = client.get("/api/v1/jobs/deadbeefdeadbe")
    assert unknown.status_code == 404 and isinstance(unknown.json()["detail"], str)
    for suffix in ("preview", "text", "file"):
        dead = client.get(f"/api/v1/media/deadbeefdeadbe/{suffix}")
        assert dead.status_code == 404 and isinstance(dead.json()["detail"], str)
    invalid = client.post("/api/v1/search", json={"q": "x", "kinds": ["not-a-kind"]})
    assert invalid.status_code == 422
    assert invalid.headers["content-type"].startswith("application/json")
    detail = invalid.json()["detail"]
    assert isinstance(detail, list) and detail
    assert all({"type", "loc", "msg"} <= set(item) for item in detail)


def test_media_ids_are_per_reply_across_the_http_surface(client):
    """`docs/API.md`'s rule, pinned over HTTP: the same file in two replies carries two
    handles, so a client may not cache a `media_id` across searches (and the engine's own
    `path` + `kind` + `segment_id` + `loc` are what it should key on instead)."""
    def top(c):
        hit = c.post("/api/v1/search", json={"q": "wakeboarding", "limit": 1}).json()["hits"][0]
        return Path(hit["path"]).name, hit["media_id"]
    (name_a, id_a) = top(client)
    (name_b, id_b) = top(client)
    assert name_a == name_b and id_a != id_b


def test_the_api_validates_what_the_engine_would_silently_drop(client):
    bad = client.post("/api/v1/search", json={"q": "x", "kinds": ["not-a-kind"]})
    assert bad.status_code == 422                      # engine drops; API says so
    assert client.post("/api/v1/search", json={"q": "x" * 513}).status_code == 422
    assert client.post("/api/v1/search", json={"q": "x", "limit": 0}).status_code == 422
    assert client.post("/api/v1/search", json={}).status_code == 422


def test_estimate_is_the_engines_forecast_as_json(client, corpus):
    reply = client.post("/api/v1/estimate", json={}).json()
    folder = reply["folders"][0]
    assert folder["readable"] is True
    assert folder["folder"] == str(corpus)
    assert folder["files"]["image"] == 12              # the engine's own counts


def test_openapi_is_the_versioned_public_surface(client):
    spec = client.get("/api/v1/openapi.json").json()
    assert all(p.startswith("/api/v1/") for p in spec["paths"])
    assert {"health", "status", "search", "estimate"} <= \
        {p.rsplit("/", 1)[1] for p in spec["paths"]}


# The refusal path (`EngineRefused` → 503 with the engine's sentence verbatim) moved to
# `cargo test -p digup-server` with the wrapper: the adapter raises it (pinned below, the
# harness layer survives the flip), and the HTTP mapping is the binary's own code path.
def test_a_refusal_reply_is_raised_in_the_adapter(index, corpus):
    """The same fact one layer down: `SearchService` raises `EngineRefused` on an
    `ok:false` reply (the engine's refusal, `SearchService.swift:106,115`) — stub proc,
    no engine, the reply is the engine's refusal shape verbatim."""
    from conftest import _cfg
    from engine.protocol import EngineRefused
    from engine.search import SearchService

    svc = SearchService(_cfg(index, (corpus,)))

    class StubProc:
        alive = True
        def request(self, fields, *, timeout=None):
            return {"id": 1, "ok": False, "error": "index is reloading"}

    svc._proc = StubProc()
    with pytest.raises(EngineRefused) as raised:
        svc.search("anything")
    assert "index is reloading" in str(raised.value)

def test_status_shows_the_servers_own_roots(client, corpus):
    """Stage 10: the UI shows *where this server looks*. These are the deployment's own
    configured paths (env) — publishing them is presentation, not path acceptance (§7)."""
    assert client.get("/api/v1/status").json()["roots"] == [str(corpus)]


def test_startup_prewarms_the_searcher(client):
    """Stage 7: the lifespan prewarms at startup (the engine's `prewarm` op), so a
    freshly booted server answers the first search with meaning + real z-scores —
    no cold-encoder keyword fallback in front of a first-time user."""
    status = client.get("/api/v1/status").json()
    assert status["vectors"] > 0                 # vectors opened at startup, not lazily
    reply = client.post("/api/v1/search", json={"q": "wakeboarding", "limit": 3}).json()
    assert reply["meaning"] is True and reply["strong"] >= 3


# ── review-round units (this file's fixtures stay real: app + engine, no mocks) ────────


def test_open_and_huge_ranges_are_capped_at_the_served_span(big_photo, tmp_path_factory):
    """`bytes=0-` is what every browser media element sends first. Uncapped, it read the
    whole file into RAM (a 20 GB clip asked for a 20 GB buffer). Open ranges, huge
    explicit ranges and huge suffix ranges all cap at the served span (8 MiB,
    `media.rs:MAX_RANGE_SPAN`). Observable over HTTP: a media file BIGGER than the cap,
    asked for ranges that outrange it — every answer is 206 and the served span is
    exactly the cap, never the whole file; a normal short span stays untouched."""
    app, media_id = big_photo
    whole = app.client.get(f"/api/v1/media/{media_id}/file")
    size = len(whole.content)
    assert size > 8 * 1024 * 1024, size                      # the fixture's promise
    huge = app.client.get(f"/api/v1/media/{media_id}/file",
                          headers={"range": "bytes=1000-9000000000"})
    assert huge.status_code == 206
    unit, rng_total = huge.headers["content-range"].split(" ")   # "bytes" + "1000-…/size"
    rng, total = rng_total.split("/")
    assert unit == "bytes" and int(total) == size                 # the cap names the real file
    start, end = (int(v) for v in rng.split("-"))
    assert start == 1000 and end - start + 1 == 8 * 1024 * 1024
    assert len(huge.content) == 8 * 1024 * 1024
    open_range = app.client.get(f"/api/v1/media/{media_id}/file", headers={"range": "bytes=0-"})
    assert open_range.status_code == 206
    assert len(open_range.content) == 8 * 1024 * 1024
    suffix = app.client.get(f"/api/v1/media/{media_id}/file", headers={"range": "bytes=-40"})
    assert suffix.status_code == 206 and len(suffix.content) == 40
    short = app.client.get(f"/api/v1/media/{media_id}/file", headers={"range": "bytes=0-99"})
    assert short.status_code == 206 and len(short.content) == 100      # normal spans untouched
def test_a_slow_estimate_answers_busy_never_parallel(deep_tree, tmp_path_factory):
    """`estimate` walks the whole tree as a sync endpoint with a server-wide one-crawl
    rule: a second UI tab, a user retry, a retry during a crawl must never stack
    full-tree crawls. Observable: fire a crowd of estimates at a tree whose crawl takes
    seconds — the first answers the engine's forecast, every request that overlapped it
    answers 409 quoting the rule, and a later one runs clean."""
    with live_server(_cfg(tmp_path_factory.mktemp("busy-config-index"), (deep_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("busy-config"))) as c:
        results = []
        threads = [threading.Thread(target=lambda b=results: b.append(
            c.post("/api/v1/estimate", json={}))) for _ in range(12)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(60)
        codes = sorted(r.status_code for r in results)
        assert 200 in codes and codes.count(200) < len(codes), codes   # the crawl, and the refusals
        for r in results:
            if r.status_code == 409:
                assert "already running" in r.json()["detail"]
        later = c.post("/api/v1/estimate", json={})
        assert later.status_code == 200                                 # the lock released
def test_the_estimate_timeout_is_an_operator_knob(deep_tree, tmp_path_factory):
    """The 120 s default fits corpus-sized trees; a real media tree on a spinning disk
    needs minutes (measured 2026-11-04: 187k files on a 2 TB HDD crossed the default →
    502). Observable: the knob is read at boot (the env the container ships), so a
    server booted with a tiny ceiling answers a slow walk as a 502 quoting the timeout,
    and the default boot answers the same tree clean."""
    with live_server(_cfg(tmp_path_factory.mktemp("knob-index"), (deep_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("knob-config-tight")),
                     env_extra={"DIGUP_SERVER_ESTIMATE_TIMEOUT_S": "0.01"}) as tight:
        r = tight.post("/api/v1/estimate", json={})
        assert r.status_code == 502, r.text
        assert "timed out" in r.json()["detail"]
    with live_server(_cfg(tmp_path_factory.mktemp("knob-index2"), (deep_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("knob-config-default"))) as open_book:
        assert open_book.post("/api/v1/estimate", json={}).status_code == 200
# The `subprocess.TimeoutExpired` → quoted 502 mapping is now observable through the
# estimate-knob test above (a real crawl crossing a real ceiling): the 502 quote IS the
# mapping, with no monkeypatch in sight.
