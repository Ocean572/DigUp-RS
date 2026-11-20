"""Stage 12 integration groundwork (docs/implementation/stage-12-integrations.md): the
named-token map, the CORS allowlist, the principal log, the embed widget, and the
Nextcloud compose shape. Plan P3 moved the middleware tests onto the live server — the
same knobs the container ships (`DIGUP_SERVER_READ_TOKENS`, `DIGUP_SERVER_ALLOWED_ORIGINS`,
`DIGUP_SERVER_ALLOWED_HOSTS`, `DIGUP_SERVER_TOKEN`, `DIGUP_SERVER_ADMIN_TOKEN`), each shape
its own spawn of the binary, refusals read as HTTP answers, and the principal log read from
the server's own log file (the audit line IS the binary's stdout line). The rules they pin:

* `DIGUP_SERVER_READ_TOKENS=name:token,…` — every entry guards the read scope, each is
  attributable by name, one entry's revocation touches no other, a typo entry is skipped
  loudly (the warning names it, on the server's log);
* the CORS allowlist is exact origins only, inert when unset, and it is the boundary
  that lets an allowlisted browser widget POST (the guard's cross-origin exception is
  set ONLY by a CORS-passed preflight, stage-12 §11.4);
* every guarded request lands in the log with its accepted principal;
* the embed widget follows the same UI rules as the full UI (no external URLs, the
  built Lit+Vite shape served same-origin, server-issued media handles, the token
  never in a `<img>`-visible URL).
"""

from __future__ import annotations

import re
import time

import pytest

from conftest import _cfg
from support import LiveApp, live_server


@pytest.fixture(scope="module")
def empty_tree(tmp_path_factory):
    """Roots with nothing to crawl: these tests prove the gate, not the engine."""
    return tmp_path_factory.mktemp("integ-empty-roots")


def started(tmp_path_factory, cfg, config_name, env_extra=None):
    """A live server handed out as the APP (not the client): the tests below read both
    its HTTP answers and its log lines (the principal audit)."""
    app = LiveApp(cfg, config_dir=str(tmp_path_factory.mktemp(config_name)),
                  env_extra=env_extra)
    app.start()
    try:
        yield app
    finally:
        app.stop()


def audit_lines(app: LiveApp) -> list[str]:
    return [line for line in app.tail(200_000).splitlines() if "principal=" in line]


def writer_is_free(app: LiveApp, token: str) -> None:
    """Run one crawl to its end, asking through the honest 409.

    The module-scoped fixtures above already queued crawls, and a writer keeps its
    flock for the closing moment after its last job (`AGENTS.md`: a second `POST
    /index` answers **409** — busy means a job is running). The audit pin below must
    measure its lines from a request the writer ACCEPTS, so the writer is run to its
    end first — and its requests, all of them, land before `base` is read."""
    headers = {"Authorization": f"Bearer {token}"}
    deadline = time.monotonic() + 240.0
    while time.monotonic() < deadline:
        r = app.client.post("/api/v1/index", json={}, headers=headers)
        if r.status_code == 409:
            time.sleep(1.0)
            continue
        assert r.status_code == 202, (r.status_code, r.text)
        job_id = r.json()["job"]["id"]
        while time.monotonic() < deadline:
            state = app.client.get(f"/api/v1/jobs/{job_id}", headers=headers).json()["job"]["state"]
            if state in ("done", "interrupted", "busy"):
                assert state == "done", state
                return
            time.sleep(0.5)
        break
    pytest.fail("the writer never came free")


# ── the named-token map: parsing, scopes, revocation-isolation ───────────────────────────

@pytest.fixture(scope="module")
def map_app(empty_tree, tmp_path_factory):
    """Names mixed-case with a duplicate, and one entry that is not a name:token pair."""
    yield from started(
        tmp_path_factory, _cfg(tmp_path_factory.mktemp("integ-map-index"), (empty_tree,)),
        "integ-map-config",
        {"DIGUP_SERVER_READ_TOKENS": "nc-a:tokA, nc-b:tokB,NC-A:dup,broken-no-colon",
         "DIGUP_SERVER_ADMIN_TOKEN": "adm"})


def test_the_map_is_the_live_scope_map_it_parsed(map_app):
    """Names lowercase, FIRST entry wins on a duplicate, a typo entry is skipped (it is
    nobody's token): all three are the answers the gate gives, on the wire."""
    def search(token):
        return map_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                                   headers={"Authorization": f"Bearer {token}"})
    for token in ("tokA", "tokB"):
        assert search(token).status_code in (200, 503), token
    assert search("dup").status_code == 401            # the lowercase dupe lost to the first
    assert search("no-colon").status_code == 401       # the typo entry minted no token
    assert search("adm").status_code in (200, 503)     # admin covers the read scope too


def test_a_dropped_map_entry_names_itself_in_the_server_log(map_app):
    """`DIGUP_SERVER_READ_TOKENS` parses at boot; an entry with no colon is a typo, and a
    silently-dropped token is an outage nobody asked for — the server says which entry."""
    assert "broken-no-colon" in map_app.tail(200_000)


def test_map_tokens_guard_reads_and_admin_stays_admins_alone(map_app):
    for token in ("tokA", "tokB"):
        assert map_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                                   headers={"Authorization": f"Bearer {token}"}).status_code \
            in (200, 503)
        refused = map_app.client.post("/api/v1/index", json={},
                                      headers={"Authorization": f"Bearer {token}"})
        assert refused.status_code == 403 and "admin" in refused.json()["detail"]
    assert map_app.client.post("/api/v1/index", json={},
                               headers={"Authorization": "Bearer adm"}).status_code == 202
    assert map_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                               headers={"Authorization": "Bearer nope"}).status_code == 401
    assert map_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1}).status_code == 401


def test_each_request_is_attributed_to_the_principal_that_asked(map_app):
    def post(path, token, json_body=None):
        return map_app.client.post(path, json=json_body if json_body is not None else {"q": "dog"},
                                   headers={"Authorization": f"Bearer {token}"})
    # the map fixture is module-scoped: earlier tests already wrote audit lines, so the
    # pin reads the lines THIS test's requests added, in order. The writer is run to its
    # end first (its answer must be the accepted 202, not a closing-moment 409), and
    # those requests are the lines BEFORE `base`.
    writer_is_free(map_app, "adm")
    base = len(audit_lines(map_app))
    assert post("/api/v1/search", "tokA").status_code in (200, 503)
    assert post("/api/v1/search", "tokB").status_code in (200, 503)
    assert post("/api/v1/index", "adm", {}).status_code == 202
    lines = audit_lines(map_app)[base:]
    assert "principal=nc-a" in lines[0] and "principal=nc-b" in lines[1]
    assert "principal=admin" in lines[2]
    # health/docs stay out of the audit line: they are the healthcheck and the docs page
    map_app.client.get("/api/v1/health")
    assert not any("/api/v1/health" in line for line in audit_lines(map_app))


@pytest.fixture(scope="module")
def plain_read_app(empty_tree, tmp_path_factory):
    """All three token knobs at once: `auth_from_env` assembles read + admin + the map, and
    an admin token on the read side says so in the audit line."""
    yield from started(
        tmp_path_factory, _cfg(tmp_path_factory.mktemp("integ-plain-index"), (empty_tree,)),
        "integ-plain-config",
        {"DIGUP_SERVER_TOKEN": "plain", "DIGUP_SERVER_ADMIN_TOKEN": "adm",
         "DIGUP_SERVER_READ_TOKENS": "nc-a:tokA,nc-b:tokB"})


def test_read_scope_admin_scope_and_the_map_coexist_in_one_boot(plain_read_app):
    c = plain_read_app.client
    assert c.post("/api/v1/search", json={"q": "dog", "limit": 1},
                  headers={"Authorization": "Bearer plain"}).status_code in (200, 503)
    assert c.post("/api/v1/search", json={"q": "dog", "limit": 1},
                  headers={"Authorization": "Bearer tokB"}).status_code in (200, 503)
    assert c.post("/api/v1/search", json={"q": "dog", "limit": 1},
                  headers={"Authorization": "Bearer adm"}).status_code in (200, 503)
    assert c.post("/api/v1/search", json={"q": "dog", "limit": 1},
                  headers={"Authorization": "Bearer nope"}).status_code == 401
    lines = audit_lines(plain_read_app)
    assert any("principal=read" in line for line in lines)
    assert any("principal=nc-b" in line for line in lines)
    assert any("principal=admin (read scope)" in line for line in lines)


@pytest.fixture(scope="module")
def open_app(empty_tree, tmp_path_factory):
    yield from started(tmp_path_factory,
                       _cfg(tmp_path_factory.mktemp("integ-open-index"), (empty_tree,)),
                       "integ-open-config")


def test_an_unguarded_deployment_still_logs_its_principals_as_none(open_app):
    assert open_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1}).status_code \
        in (200, 503)
    assert any("principal=none" in line and "/api/v1/search" in line
               for line in audit_lines(open_app))


# ── the CORS allowlist: exact origins, inert by default, the widget's boundary ───────────

def test_the_cors_allowlist_is_inert_by_default(tmp_path_factory):
    """No `DIGUP_SERVER_ALLOWED_ORIGINS`: the middleware passes requests through and
    decorates NOTHING — no CORS headers, period (server-to-server stays normal)."""
    roots, folder = tmp_path_factory.mktemp("cors-off-roots"), tmp_path_factory.mktemp("cors-off-index")
    with live_server(_cfg(folder, (roots,)),
                     config_dir=str(tmp_path_factory.mktemp("cors-off-config"))) as c:
        preflight = c.options("/api/v1/search", headers={"origin": "http://a.example",
                                                         "access-control-request-method": "POST"})
        assert "access-control-allow-origin" not in preflight.headers
        plain = c.get("/api/v1/status", headers={"origin": "http://a.example"})
        assert "access-control-allow-origin" not in plain.headers
        # a request with no Origin at all is untouched too
        assert "access-control-allow-origin" not in c.get("/api/v1/status").headers


@pytest.fixture(scope="module")
def widget_app(empty_tree, tmp_path_factory):
    """The composed stack as a deployment names it: the hosts it serves, the one widget
    origin it embeds, and the read token for the widget."""
    yield from started(
        tmp_path_factory, _cfg(tmp_path_factory.mktemp("integ-widget-index"), (empty_tree,)),
        "integ-widget-config",
        {"DIGUP_SERVER_ALLOWED_HOSTS": "nc.example, 127.0.0.1",
         "DIGUP_SERVER_ALLOWED_ORIGINS": "http://nc.example",
         "DIGUP_SERVER_TOKEN": "tokA"})


def test_the_allowlisted_widget_posts_cross_origin_with_the_bearer_token(widget_app):
    widget = {"host": "nc.example:8787", "origin": "http://nc.example",
              "Authorization": "Bearer tokA"}
    ok = widget_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1}, headers=widget)
    assert ok.status_code in (200, 503), ok.text
    assert ok.headers["access-control-allow-origin"] == "http://nc.example"
    assert any("principal=read" in line for line in audit_lines(widget_app))
    # the preflight lands before the POST and passes the guard (OPTIONS is not state-changing)
    pre = widget_app.client.options("/api/v1/search", headers={"host": "nc.example:8787",
                                                                "origin": "http://nc.example"})
    assert pre.status_code == 204


def test_an_unallowlisted_browser_still_hits_the_same_origin_rule(widget_app):
    """The guard's cross-origin POST refusal survives CORS: a page the operator never
    allowlisted gets no exception, token or no token."""
    refused = widget_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                                     headers={"host": "nc.example:8787",
                                              "origin": "http://evil.example",
                                              "Authorization": "Bearer tokA"})
    assert refused.status_code == 403
    # server-to-server (no Origin at all) remains the normal, unblessed path
    assert widget_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                                  headers={"host": "nc.example:8787",
                                           "Authorization": "Bearer tokA"}).status_code \
        in (200, 503)
    # the host allowlist outranks everything: a stale Host is refused before tokens
    assert widget_app.client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                                  headers={"host": "evil.example",
                                           "origin": "http://nc.example",
                                           "Authorization": "Bearer tokA"}).status_code == 400


# The FastAPI-era test that read `create_app(...).user_middleware` (four middlewares,
# PrincipalLog outermost, knobs from env) retired with the framework: the composed stack
# is one `api_gate` layer in the binary (`crates/server/src/main.rs`), and the suite
# pins what the stack DOES — the widget tests above are its end-to-end proof.


# ── the embed widget: the same UI rules as the full UI, over the same static mount ──────

@pytest.fixture(scope="module")
def embed_client(empty_tree, tmp_path_factory):
    """The widget needs only the static mount — no engine, no data (the mount is the
    binary's own: `DIGUP_SERVER_UI`, else the payload-adjacent `ui/dist`)."""
    with live_server(_cfg(tmp_path_factory.mktemp("integ-embed-index"), (empty_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("integ-embed-config"))) as c:
        yield c


def test_the_embed_page_is_served_and_dependency_free(embed_client):
    page = embed_client.get("/embed.html")
    assert page.status_code == 200
    html = page.text
    assert "DigUp" in html and "embed.js" in html and "embed.css" in html
    assert not re.search(r'(src|href)="(https?:)?//', html), "the widget must load no external URLs"
    # The built shape (2026-11-08 migration): ES-module entry, and the shared measured
    # palette rides with the widget's own CSS graph (Vite extracts the tokens into a
    # hashed stylesheet the html links — the widget page stays self-contained).
    assert re.search(r'type="module"', html)
    assert re.search(r'href="\./(assets/[^"]+\.css)"', html)
    assert 'name="referrer" content="none"' in html         # no referrer leaves the widget
    for asset in ("/embed.css", "/embed.js"):
        assert embed_client.get(asset).status_code == 200
    # the shared static mount decorates every embed byte the same way (no stale UI)
    assets = ["/embed.html", "/embed.css", "/embed.js"]
    assets += ["/" + p for p in re.findall(r'href="\./((?:chunks|assets)/[^"]+)"', html)]
    for asset in assets:
        reply = embed_client.get(asset)
        assert reply.status_code == 200, asset
        assert reply.headers["cache-control"] == "no-cache", asset
        assert "etag" in reply.headers, asset


def test_the_embed_widget_uses_the_published_api_and_hides_the_token(embed_client):
    # The widget's bundle: the embed entry plus the chunks its html links (the api
    # service is a shared module — the pins below name the rules, and the rules are
    # tested as behavior in src/embed.test.js / src/services/api.test.js).
    html = embed_client.get("/embed.html").text
    js = embed_client.get("/embed.js").text + "\n" + "\n".join(
        embed_client.get("/" + p).text
        for p in re.findall(r'href="\./((?:chunks|assets)/[^"]+\.js)"', html))
    assert "fetch(" in js and "/api/v1/search" in js and "/api/v1/embed_ahead" in js
    # embed.html DECLARES the widget; the entry wires that element (the double-mount
    # bug — an unwired twin above the working widget — is pinned out, 2026-11-08).
    assert html.count("<digup-embed></digup-embed>") == 1
    assert 'document.querySelector("digup-embed")' in js
    assert "media_id" in js and "/preview" in js and "/file" in js   # server-issued handles
    assert "sessionStorage" in js and "history.replaceState" in js   # the token leaves the URL
    assert "URL.createObjectURL" in js and "IntersectionObserver" in js
    assert "postMessage" in js and "digup-host" in js                # the host drives the theme
    assert "embed_ahead" in js and "meaning" in js                   # keyword-first typing
    assert 'This DigUp server asks for its read token' in js          # 401 asks once


# ── the token map over the REAL API + engine: what the Nextcloud app's calls do ──────────

def test_estimate_argv_carries_the_excludes_the_job_will_honour():
    """Stage-12 §11.3, measured on the Nextcloud pilot: the forecast must describe the
    tree the indexer WOULD walk. An estimate that counted the folders the job excludes
    promised work nobody will do (the pilot counted 120 with 36 real files)."""
    from engine import EngineConfig
    cfg = EngineConfig(index="/idx", roots=("/media/photos",), excluded=("/media/photos/appdata_x",),
                       excluded_extensions=("heic",))
    argv = cfg.estimate_argv()
    assert argv[:3] == ["digup", "estimate", "/media/photos"]
    assert argv.count("--exclude") == 1 and "--exclude" in argv
    assert argv[argv.index("--exclude") + 1] == "/media/photos/appdata_x"
    assert argv[argv.index("--exclude-ext") + 1] == "heic"
    assert argv[-1] == "--json"
    plain = EngineConfig(index="/idx", roots=("/r",)).estimate_argv()
    assert "--exclude" not in plain and "--exclude-ext" not in plain   # knobs inert when unset
    # Measured 2026-11-08 on the pilot: the job path was the gap — index-service ran with
    # no excludes and walked the tree's appdata (1.47M preview files) while estimate
    # (carrying them) reported 131,972. The job must carry the SAME list.
    job = cfg.index_argv()
    assert job[:3] == ["digup", "index-service", "/media/photos"]
    assert job.count("--exclude") == 1 and job.count("--exclude-ext") == 1
    assert job[job.index("--exclude") + 1] == "/media/photos/appdata_x"
    assert job[job.index("--exclude-ext") + 1] == "heic"
    plain_job = EngineConfig(index="/idx", roots=("/r",)).index_argv()
    assert "--exclude" not in plain_job and "--exclude-ext" not in plain_job


def test_the_map_protects_the_real_api_end_to_end(corpus, index, tmp_path_factory):
    with live_server(_cfg(index, (corpus,)),
                     config_dir=str(tmp_path_factory.mktemp("integ-e2e-config")),
                     env_extra={"DIGUP_SERVER_READ_TOKENS": "nc-a:tokA,nc-b:tokB",
                                "DIGUP_SERVER_ADMIN_TOKEN": "adm"}) as c:
        assert c.get("/api/v1/health").status_code == 200          # health: open, always
        assert c.post("/api/v1/search", json={"q": "wakeboarding"}).status_code == 401
        for token in ("tokA", "tokB"):
            reply = c.post("/api/v1/search", json={"q": "wakeboarding", "limit": 3},
                           headers={"Authorization": f"Bearer {token}"})
            assert reply.status_code == 200 and reply.json()["hits"]
            assert c.post("/api/v1/index", json={},
                          headers={"Authorization": f"Bearer {token}"}).status_code == 403
            assert c.get("/api/v1/media/deadbeefcafe/preview",
                         headers={"Authorization": f"Bearer {token}"}).status_code == 404
        # POST /index with the admin token starts a real writer job on the session index —
        # that path is pinned end-to-end in test_jobs.py over a scratch index, not here.
        assert c.post("/api/v1/estimate", json={},
                      headers={"Authorization": "Bearer adm"}).status_code == 200
        assert c.post("/api/v1/estimate", json={}).status_code == 401
        assert c.get("/api/v1/jobs", headers={"Authorization": "Bearer tokA"}).status_code == 200
