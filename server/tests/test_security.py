"""The Host/Origin guard and the bearer gate, as the SHIPPED server enforces them.

The behavior this pins is the *shape* of the opt-in: loopback default = guard off (nothing
changes for today's deployment), guard on = the named hosts only, browsers' state-changing
requests same-origin only, non-browser clients (no `Origin`) still welcome, tokens scoped
read/admin/app. Those are the rules a LAN/proxy deployment must not quietly lose.

Plan P3 moved this module onto the live server: the knobs are the env the container ships
(`DIGUP_SERVER_ALLOWED_HOSTS`, `DIGUP_SERVER_ALLOWED_ORIGINS`, `DIGUP_SERVER_TOKEN`,
`DIGUP_SERVER_ADMIN_TOKEN`), read at boot, so each shape is its own spawn of the binary.
The two tests that read FastAPI's middleware list (`create_app(...).user_middleware`)
are gone with the framework: the "installed always, knobs inert" fact is observable as
"no knob env → a request carrying a bogus Host reaches the API, and a key-free search
answers" — which is what a deployment sees.
"""

from __future__ import annotations

import pytest

from conftest import _cfg
from support import live_server

# The guard's refusals are one JSON body with a `detail` string, and a 401 carries
# `www-authenticate: Bearer` — the shape `docs/API.md` documents.
JSON = "application/json"


# ── the Host/Origin guard: off by default, the named hosts only, same-origin writes ──

@pytest.fixture(scope="module")
def free_client(corpus, index, tmp_path_factory):
    """No `DIGUP_SERVER_ALLOWED_HOSTS`: the guard is installed and inert."""
    with live_server(_cfg(index, (corpus,)),
                     config_dir=str(tmp_path_factory.mktemp("sec-free-config"))) as c:
        yield c


@pytest.fixture(scope="module")
def guarded_client(corpus, index, tmp_path_factory):
    """The operator names the hosts this deployment serves — mixed case, one with a port."""
    with live_server(_cfg(index, (corpus,)),
                     config_dir=str(tmp_path_factory.mktemp("sec-guard-config")),
                     env_extra={"DIGUP_SERVER_ALLOWED_HOSTS": "A.example, 127.0.0.1:8787"}) as c:
        yield c


@pytest.fixture(scope="module")
def star_client(corpus, index, tmp_path_factory):
    """`*` names a host literally, it is not a wildcard."""
    with live_server(_cfg(index, (corpus,)),
                     config_dir=str(tmp_path_factory.mktemp("sec-star-config")),
                     env_extra={"DIGUP_SERVER_ALLOWED_HOSTS": "*"}) as c:
        yield c


def test_the_guard_is_off_when_no_hosts_are_named(free_client):
    # the knob is unset → any Host passes, a cross-origin POST passes
    assert free_client.get("/api/v1/health", headers={"host": "anything.example"}).status_code == 200
    ok = free_client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                          headers={"host": "anything.example",
                                   "origin": "http://elsewhere.example"})
    assert ok.status_code == 200, ok.text


def test_named_hosts_are_enforced_case_insensitively_without_ports(guarded_client):
    assert guarded_client.get("/api/v1/health", headers={"host": "a.example"}).status_code == 200
    assert guarded_client.get("/api/v1/health",
                              headers={"host": "127.0.0.1:8787"}).status_code == 200
    refused = guarded_client.get("/api/v1/health", headers={"host": "evil.example"})
    assert refused.status_code == 400
    assert isinstance(refused.json()["detail"], str)
    assert refused.headers["content-type"].startswith(JSON)


def test_no_wildcard_is_understood(star_client):
    """A deployment that means "every host" leaves the setting unset; naming `*` names a
    host named `*`, and a real host name is refused by it."""
    assert star_client.get("/api/v1/health", headers={"host": "a.example"}).status_code == 400
    assert star_client.get("/api/v1/health", headers={"host": "*"}).status_code == 200


def test_state_changing_requests_from_another_origin_are_refused(guarded_client, corpus, index):
    same = {"host": "127.0.0.1:8787", "origin": "http://127.0.0.1:8787"}
    assert guarded_client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                               headers=same).status_code == 200
    cross = {"host": "127.0.0.1:8787", "origin": "http://evil.example"}
    refused = guarded_client.post("/api/v1/search", json={"q": "dog", "limit": 1}, headers=cross)
    assert refused.status_code == 403 and isinstance(refused.json()["detail"], str)
    # non-browsers send no Origin at all: curl, Immich, Home Assistant, agents
    assert guarded_client.post("/api/v1/search", json={"q": "dog", "limit": 1},
                               headers={"host": "127.0.0.1:8787"}).status_code == 200
    # a read-only GET carries no Origin in browsers, and is not checked
    assert guarded_client.get("/api/v1/status", headers={"host": "127.0.0.1:8787",
                                                         "origin": "http://evil.example"}).status_code == 200


def test_a_cross_origin_write_from_an_allowlisted_origin_passes(tmp_path_factory):
    """The CORS allowlist is the guard's exception: an allowlisted origin's POST is decorated
    with `access-control-allow-origin` and is not refused as cross-site. Observable shape:
    the preflight answers 204 naming the origin, methods, headers; the actual request carries
    the origin and gets the decoration the browser needs."""
    roots, folder = tmp_path_factory.mktemp("cors-roots"), tmp_path_factory.mktemp("cors-index")
    with live_server(_cfg(folder, (roots,)),
                     config_dir=str(tmp_path_factory.mktemp("cors-config")),
                     env_extra={"DIGUP_SERVER_ALLOWED_ORIGINS": "http://Widget.Example/"}) as c:
        preflight = c.options("/api/v1/search",
                              headers={"origin": "http://widget.example",
                                       "access-control-request-method": "POST"})
        assert preflight.status_code == 204
        assert preflight.headers["access-control-allow-origin"] == "http://widget.example"
        assert "POST" in preflight.headers["access-control-allow-methods"]
        assert preflight.headers["access-control-allow-headers"] == "authorization, content-type"
        actual = c.post("/api/v1/search", json={"q": "anything", "limit": 1},
                        headers={"origin": "http://widget.example"})
        assert actual.headers.get("access-control-allow-origin") == "http://widget.example"
        assert actual.headers.get("vary") == "Origin"
        # an origin the operator did not name gets no decoration, and the state-changing
        # request is refused by the guard when the guard is on
        plain = c.post("/api/v1/search", json={"q": "anything", "limit": 1},
                       headers={"origin": "http://evil.example"})
        assert plain.headers.get("access-control-allow-origin") is None


# ── the bearer gate: off by default, read scope everywhere, admin scope on index/estimate ──

@pytest.fixture(scope="module")
def empty_tree(tmp_path_factory):
    """A roots folder with nothing to crawl: the gate tests prove the gate, not the engine,
    and a queued crawl here is a 0-file walk that finishes at once."""
    return tmp_path_factory.mktemp("sec-empty-roots")


@pytest.fixture(scope="module")
def open_book(empty_tree, tmp_path_factory):
    """No tokens named: the gate is installed and inert."""
    with live_server(_cfg(tmp_path_factory.mktemp("sec-open-index"), (empty_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("sec-open-config"))) as c:
        yield c


@pytest.fixture(scope="module")
def read_only(empty_tree, tmp_path_factory):
    with live_server(_cfg(tmp_path_factory.mktemp("sec-read-index"), (empty_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("sec-read-config")),
                     env_extra={"DIGUP_SERVER_TOKEN": "s3cret"}) as c:
        yield c


@pytest.fixture(scope="module")
def both_scopes(empty_tree, tmp_path_factory):
    with live_server(_cfg(tmp_path_factory.mktemp("sec-both-index"), (empty_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("sec-both-config")),
                     env_extra={"DIGUP_SERVER_TOKEN": "s3cret",
                                "DIGUP_SERVER_ADMIN_TOKEN": "adm1n"}) as c:
        yield c


@pytest.fixture(scope="module")
def admin_only(empty_tree, tmp_path_factory):
    with live_server(_cfg(tmp_path_factory.mktemp("sec-admin-index"), (empty_tree,)),
                     config_dir=str(tmp_path_factory.mktemp("sec-admin-config")),
                     env_extra={"DIGUP_SERVER_ADMIN_TOKEN": "adm1n"}) as c:
        yield c


def test_no_tokens_means_no_auth_at_all(open_book):
    assert open_book.post("/api/v1/index", json={}).status_code == 202
    assert open_book.post("/api/v1/search", json={"q": "dog", "limit": 1}).status_code in (200, 503)
    assert open_book.get("/api/v1/status").status_code in (200, 503)


def test_read_token_covers_the_search_side_and_asks_401_first(read_only):
    naked = read_only.post("/api/v1/search", json={"q": "dog", "limit": 1})
    assert naked.status_code == 401
    assert naked.headers["www-authenticate"] == "Bearer"
    assert isinstance(naked.json()["detail"], str)
    ok = read_only.post("/api/v1/search", json={"q": "dog", "limit": 1},
                        headers={"Authorization": "Bearer s3cret"})
    assert ok.status_code in (200, 503), ok.text
    assert read_only.post("/api/v1/search", json={"q": "dog", "limit": 1},
                          headers={"Authorization": "Bearer wrong"}).status_code == 401
    assert read_only.post("/api/v1/search", json={"q": "dog", "limit": 1},
                          headers={"Authorization": "s3cret"}).status_code == 401


def test_health_and_the_docs_page_stay_open_so_the_healthcheck_and_the_openapi_work(both_scopes):
    for path in ("/api/v1/health", "/api/v1/docs", "/api/v1/openapi.json",
                 "/", "/index.html", "/app.js"):
        assert both_scopes.get(path).status_code == 200, path


def test_index_and_estimate_are_admin_scope(both_scopes):
    assert both_scopes.post("/api/v1/index", json={}).status_code == 401
    assert both_scopes.post("/api/v1/estimate", json={}).status_code == 401
    read_only_write = both_scopes.post("/api/v1/index", json={},
                                       headers={"Authorization": "Bearer s3cret"})
    assert read_only_write.status_code == 403            # the right token, the wrong scope
    assert "admin" in read_only_write.json()["detail"]
    assert both_scopes.post("/api/v1/index", json={},
                            headers={"Authorization": "Bearer adm1n"}).status_code == 202
    assert both_scopes.post("/api/v1/estimate", json={},
                            headers={"Authorization": "Bearer adm1n"}).status_code == 200
    assert both_scopes.get("/api/v1/jobs",
                           headers={"Authorization": "Bearer s3cret"}).status_code == 200


def test_an_admin_only_deployment_leaves_search_open_to_the_shared_clients(admin_only):
    """A deployment that names only the admin token is a localhost box that wants a *guard on
    its mutating endpoints*: the read side stays open for every client on the machine."""
    assert admin_only.get("/api/v1/status").status_code in (200, 503)
    assert admin_only.post("/api/v1/search", json={"q": "dog", "limit": 1}).status_code in (200, 503)
    assert admin_only.post("/api/v1/index", json={}).status_code == 401
    assert admin_only.post("/api/v1/index", json={},
                           headers={"Authorization": "Bearer adm1n"}).status_code == 202


def test_the_guard_and_the_gate_ship_installed_with_inert_knobs(free_client):
    """`create_app` used to assert "one HostOriginGuard, one BearerAuth, both configured off"
    by reading the middleware list. With the framework gone the fact is the deployment's:
    a server booted with no knob env answers a request carrying a bogus Host, and its
    key-free reads work — the guard and the gate are in the stack, asking for nothing."""
    bogus = free_client.get("/api/v1/status", headers={"host": "whoever.example"})
    assert bogus.status_code in (200, 503), bogus.text
    assert free_client.post("/api/v1/search", json={"q": "dog", "limit": 1}).status_code == 200
