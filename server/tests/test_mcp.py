"""The Model Context Protocol peer (POST /api/v1/mcp), as the SHIPPED server answers it.

The contract this pins is the thin-client law one layer up from REST: a `tools/call`
answer carries the engine's reply VERBATIM in `structuredContent` (the same fields the
desktop UI reads, the transport `id` stripped exactly like REST strips it) plus a short
skimming text — the server authors no numbers here. The gating mirrors the REST gate
field for field: bearer 401s, the read/admin split, the 403 refusal quoted in the
gate's own words, the target scoping, and the deployment's method shapes (the mount's
404/405 ride the route untouched).

Transport: streamable-HTTP stateless — one JSON-RPC 2.0 message per POST, one answer
per POST, `mcp-session-id` echoed; a notification answers 202 with an empty body (the
spec's rule). The stdio front-end (`digup mcp`) is pinned by the CLI suite.
"""

from __future__ import annotations

import pytest

from conftest import _cfg
from support import live_server

READ = "s3cret"
ADMIN = "adm1n"

# The REST gate's refusal, quoted (crates/services/src/mcp.rs READ_SCOPE_WORDS):
# the MCP layer repeats the gate's words, it does not author new refusals.
SCOPE_WORDS = ("that is this server's read-scope token; indexing and estimating "
               "need the admin token")


@pytest.fixture(scope="module")
def mcp_open(corpus, index, tmp_path_factory):
    """No tokens named: the gate is inert, so key-free reads — and the admin tools
    riding the same openness — are what an inert deployment must answer.
    (FACE_MIN_FACES=1: the people-scan law below pins the singleton shape —
    the core law is the engine's default and pinned by the daemon gates; the
    suite relay runs the law-off deployment, testing the knob chain live.)"""
    with live_server(_cfg(index, (corpus,)),
                     config_dir=str(tmp_path_factory.mktemp("mcp-open-config")),
                     env_extra={"DIGUP_SERVER_FACE_MIN_FACES": "1"}) as c:
        yield c


@pytest.fixture(scope="module")
def mcp_scoped(corpus, index, tmp_path_factory):
    with live_server(_cfg(index, (corpus,)),
                     config_dir=str(tmp_path_factory.mktemp("mcp-scoped-config")),
                     env_extra={"DIGUP_SERVER_TOKEN": READ,
                                "DIGUP_SERVER_ADMIN_TOKEN": ADMIN,
                                "DIGUP_SERVER_FACE_MIN_FACES": "1"}) as c:
        yield c


def rpc(client, token, method, params=None, msg_id=None, raw=None):
    headers = {} if token is None else {"Authorization": f"Bearer {token}"}
    if raw is not None:
        return client.post("/api/v1/mcp", content=raw, headers=headers)
    body = {"jsonrpc": "2.0", "method": method}
    if msg_id is not None:
        body["id"] = msg_id
    if params is not None:
        body["params"] = params
    return client.post("/api/v1/mcp", json=body, headers=headers)


def call(client, token, tool, arguments=None, msg_id=1):
    return rpc(client, token, "tools/call",
               {"name": tool, "arguments": arguments or {}}, msg_id).json()["result"]


def tool_names(client, token):
    body = rpc(client, token, "tools/list", msg_id=2).json()
    return [defn["name"] for defn in body["result"]["tools"]]


# ── the handshake, the gate, and the transport shapes ──────────────────────────

def test_initialize_negotiates_the_version_and_echoes_the_session_header(mcp_open):
    first = rpc(mcp_open, None, "initialize",
                {"protocolVersion": "2025-06-18", "capabilities": {},
                 "clientInfo": {"name": "suite", "version": "1"}}, msg_id=1)
    assert first.status_code == 200, first.text
    result = first.json()["result"]
    assert result["protocolVersion"] == "2025-06-18"
    assert result["serverInfo"]["name"] == "digup"
    assert isinstance(result["serverInfo"]["version"], str) and result["serverInfo"]["version"]
    assert isinstance(result["capabilities"]["tools"], dict)
    assert result["instructions"]                      # the relay law, stated to clients

    session = first.headers["mcp-session-id"]
    assert session
    ping = rpc(mcp_open, None, "ping", msg_id=2)
    assert ping.status_code == 200
    assert ping.headers["mcp-session-id"] == session   # stable for the process
    assert ping.json()["result"] == {}


def test_the_bearer_gate_guards_the_endpoint(mcp_scoped):
    naked = rpc(mcp_scoped, None, "initialize", {}, msg_id=1)
    assert naked.status_code == 401
    assert naked.headers["www-authenticate"] == "Bearer"
    assert isinstance(naked.json()["detail"], str)
    assert rpc(mcp_scoped, "wrong", "ping", msg_id=1).status_code == 401
    assert rpc(mcp_scoped, READ, "ping", msg_id=1).status_code == 200


def test_a_notification_answers_202_with_an_empty_body(mcp_scoped):
    accepted = rpc(mcp_scoped, READ, "notifications/initialized")   # no id: a notification
    assert accepted.status_code == 202, accepted.text
    assert accepted.content == b""


def test_malformed_bodies_answer_400_with_the_jsonrpc_error(mcp_scoped):
    broken = rpc(mcp_scoped, READ, None, raw=b"{not json")
    assert broken.status_code == 400
    assert broken.json()["error"]["code"] == -32700
    batch = rpc(mcp_scoped, READ, None,
                raw=b'[{"jsonrpc":"2.0","id":1,"method":"ping"}]')
    assert batch.status_code == 400
    assert batch.json()["error"]["code"] == -32600      # one message per POST, spec rule


def test_the_route_rides_the_deployment_method_shapes(mcp_scoped):
    """The 404/405 machinery is the router's, not the endpoint's: GET is the UI mount's
    miss (JSON 404, the measured shape), PUT is the mount's constant 405 `allow: GET, HEAD`."""
    get = mcp_scoped.get("/api/v1/mcp", headers={"Authorization": f"Bearer {READ}"})
    assert get.status_code == 404
    assert get.json()["detail"] == "Not Found"
    put = mcp_scoped.put("/api/v1/mcp", json={}, headers={"Authorization": f"Bearer {READ}"})
    assert put.status_code == 405
    assert put.headers["allow"] == "GET, HEAD"
    assert put.json()["detail"] == "Method Not Allowed"


# ── the tool set mirrors the REST gate ─────────────────────────────────────────

def test_the_read_key_sees_the_read_tools_the_admin_sees_the_mutating_pair(mcp_scoped):
    # `people` is a read tool for the manifest (the faces GETs ride the ordinary
    # bearer gate, so a read key may call it — the consent wall then answers 400
    # through the relay, pinned below); `scan` inside it rides the REST admin gate.
    assert tool_names(mcp_scoped, READ) == ["search", "status", "show", "people"]
    assert tool_names(mcp_scoped, ADMIN) == ["search", "status", "show", "people", "estimate", "index"]


def test_the_inert_gate_advertises_the_full_tool_set_key_free(mcp_open):
    # No tokens named: the REST gate is inert — key-free estimate/index answer 202/200
    # (test_security pins that). The MCP tools ride the SAME openness: an inert box is
    # the user's own localhost, and hiding tools the same key-free HTTP answers plainly
    # would invent a gate the deployment does not have.
    assert tool_names(mcp_open, None) == ["search", "status", "show", "people", "estimate", "index"]


def test_a_read_key_calling_the_mutating_tools_lands_on_the_rest_gate_words(mcp_scoped):
    for tool in ("estimate", "index"):
        result = call(mcp_scoped, READ, tool)
        assert result["isError"] is True
        assert result["content"][0]["text"] == SCOPE_WORDS, tool
        assert result["structuredContent"]["detail"] == SCOPE_WORDS, tool
        assert result["structuredContent"]["status"] == 403, tool


def test_an_admin_key_can_estimate_and_index(mcp_scoped):
    forecast = call(mcp_scoped, ADMIN, "estimate")
    assert forecast["isError"] is False, forecast
    folders = forecast["structuredContent"]["folders"]
    assert isinstance(folders, list) and folders
    assert all("folder" in row for row in folders)      # the engine's per-folder rows

    queued = call(mcp_scoped, ADMIN, "index")
    assert queued["isError"] is False, queued
    job = queued["structuredContent"]["job"]      # the REST 202 body: {"ok", "job": {…}}
    assert isinstance(job["id"], str)
    assert job["state"] in ("queued", "running")
    jobs = mcp_scoped.get("/api/v1/jobs", headers={"Authorization": f"Bearer {READ}"})
    assert jobs.status_code == 200
    assert any(record["id"] == job["id"] for record in jobs.json()["jobs"])


# ── the relay law: the engine's answer, verbatim ───────────────────────────────

def test_a_search_tool_call_relays_the_rest_answer_field_for_field(mcp_scoped):
    direct = mcp_scoped.post("/api/v1/search", json={"q": "wakeboarding", "limit": 5},
                             headers={"Authorization": f"Bearer {READ}"})
    assert direct.status_code == 200, direct.text
    result = call(mcp_scoped, READ, "search", {"q": "wakeboarding", "limit": 5})
    assert result["isError"] is False
    relayed = dict(result["structuredContent"])
    rest = dict(direct.json())
    assert "id" not in relayed and "id" not in rest      # both strip the transport id
    relayed.pop("ms", None)
    rest.pop("ms", None)                                 # timings move per call; facts don't
    relayed.pop("warning", None)
    rest.pop("warning", None)   # an encoder warning belongs to the call that paid the load
    # Preview handles are issued per reply (the media contract: a fresh id per answer),
    # so they cannot compare equal across two calls — strip them row by row, on both sides.
    for row in relayed.get("hits", []) + rest.get("hits", []):
        row.pop("media_id", None)
    assert relayed == rest, "the tool must answer exactly what the REST endpoint answers"
    assert relayed["hits"], "the corpus's planted wakeboard answers must come through"
    summary = result["content"][0]["text"]
    assert summary.startswith(f"{len(rest['hits'])} hits,")


def test_the_status_tool_counts_match_the_rest_status(mcp_scoped):
    rest = mcp_scoped.get("/api/v1/status", headers={"Authorization": f"Bearer {READ}"}).json()
    result = call(mcp_scoped, READ, "status")
    assert result["isError"] is False
    counts = result["structuredContent"]
    for key in ("files", "searchable", "pending", "unfinished", "vectors"):
        assert counts[key] == rest[key], key


def test_the_people_tool_relays_the_consent_wall_exactly_as_the_rest_gate(mcp_scoped):
    # The faces wall on the tool reply: the same handler the browser calls, so the
    # 400 sentence is the gate's own words, verbatim (the peer core authors none).
    result = call(mcp_scoped, READ, "people", {"action": "persons"})
    assert result["isError"] is True
    direct = mcp_scoped.get("/api/v1/faces/persons", headers={"Authorization": f"Bearer {READ}"})
    assert direct.status_code == 400, direct.text
    # The relay is the REST body, whole: the consent wall's 400 carries `detail`
    # (the bearer gate's 403 body carries `status` — the estimate/index pin above
    # proves that shape; the faces wall's body does not invent one).
    assert result["structuredContent"] == direct.json(), (result, direct.json())
    # A read key asking the scan lands on the bearer admin wall — measured
    # 2026-11-22, this box: the MIDDLEWARE fires first on REST (403 with the
    # SCOPE body, {"detail": …} as refuse() shapes it), before the handler's
    # consent guard. A tool call skips the middleware, so the front-end quotes
    # the SAME sentence for the SAME principal (the thin-client law: the tool
    # answers exactly what REST answers).
    refused = call(mcp_scoped, READ, "people", {"action": "scan"})
    assert refused["isError"] is True
    rest_scan = mcp_scoped.post("/api/v1/faces/scan", json={},
                                headers={"Authorization": f"Bearer {READ}"})
    assert rest_scan.status_code == 403, rest_scan.text
    assert refused["structuredContent"] == rest_scan.json(), (refused, rest_scan.json())
    # An unknown action lands on the JSON-RPC invalid-params refusal — the same
    # shape as `show` without a segment_id: execute()'s Err goes out as an
    # envelope error (there is no tools/call result to read), not an isError
    # tool result.
    bad = rpc(mcp_scoped, READ, "tools/call",
              {"name": "people", "arguments": {"action": "reboot"}}, msg_id=41).json()
    assert bad["error"]["code"] == -32602, bad
    assert "no such action" in bad["error"]["message"], bad["error"]
    # action=person without a person id is refused as bad params.
    missing = rpc(mcp_scoped, READ, "tools/call",
                  {"name": "people", "arguments": {"action": "person"}}, msg_id=42).json()
    assert missing["error"]["code"] == -32602, missing


def test_the_people_tool_scans_the_corpus_and_relays_the_person_rows(mcp_open):
    # The inert box is the user's own localhost: the key-free consent act, the key-free
    # scan (synchronous on the engine's clock — the 12 corpus photos, seconds per
    # photo), and the rows relay field-for-field from the same REST answers.
    status = call(mcp_open, None, "people")
    assert status["isError"] is False and status["structuredContent"]["consented"] is False
    assert mcp_open.post("/api/v1/faces/consent", json={"accept": True}).json()["consented"] is True

    body = {"jsonrpc": "2.0", "id": 50, "method": "tools/call",
            "params": {"name": "people", "arguments": {"action": "scan"}}}
    scanned = mcp_open.post("/api/v1/mcp", json=body, timeout=900.0).json()["result"]
    assert scanned["isError"] is False, scanned
    scan = scanned["structuredContent"]["scan"]
    # The law, not one run's numbers: the 12 corpus photos are walked whole —
    # first run decodes them (files 12, fresh 0), a suite rerun on the same
    # faces.sqlite skips every one as recorded (files 0, fresh 12; schema 2's
    # piggyback freshness). And the scan's own arithmetic: every face assigned.
    assert scan["files"] + scan["fresh"] == 12, scan
    assert scan["assigned"] == scan["faces"], scan
    assert scan["faces"] >= 0, scan

    counts = mcp_open.get("/api/v1/faces/status").json()["counts"]
    rest = mcp_open.get("/api/v1/faces/persons").json()
    relayed = call(mcp_open, None, "people", {"action": "persons"})
    assert relayed["isError"] is False
    # Preview handles are minted per answer (the media contract: a fresh id per
    # reply), so they cannot compare equal across two calls — strip them row by
    # row, on both sides, exactly as the search relay test does (every cover row
    # carries one on both sides — asserted on the way through the strip).
    for row in relayed["structuredContent"]["persons"] + rest["persons"]:
        assert "media_id" in row["cover_face"], row
        row["cover_face"].pop("media_id")
    assert relayed["structuredContent"] == rest, "the tool answers what REST answers"
    persons = rest["persons"]
    # Person law (the suite's law for centroid geometry, FACES.md): every stored
    # face lands in a person, nothing queued, 1 <= persons <= faces.
    assert counts["assigned"] == counts["faces"], counts
    assert counts["pending"] == 0, counts
    assert 1 <= len(persons) <= counts["faces"], (persons, counts)
    assert sum(p["count"] for p in persons) == counts["assigned"]   # every face landed
    assert all(p["cover_face"]["width"] > 0 for p in persons)  # the crop map rides the relay
    one = call(mcp_open, None, "people", {"action": "person", "person": persons[0]["id"]})
    assert one["isError"] is False
    assert [f["id"] for f in one["structuredContent"]["faces"]] == \
           [f["id"] for f in mcp_open.get(f"/api/v1/faces/person/{persons[0]['id']}").json()["faces"]]


def test_the_show_tool_reads_a_segments_stored_text(mcp_scoped):
    hits = mcp_scoped.post("/api/v1/search", json={"q": "wakeboarding", "limit": 5},
                           headers={"Authorization": f"Bearer {READ}"}).json()["hits"]
    assert hits
    result = call(mcp_scoped, READ, "show", {"segment_id": hits[0]["segment_id"]})
    assert result["isError"] is False, result
    assert isinstance(result["structuredContent"]["text"], str)
    assert "id" not in result["structuredContent"]       # the daemon's transport id, stripped


def test_an_unknown_tool_and_method_answer_jsonrpc_errors(mcp_scoped):
    unknown = rpc(mcp_scoped, READ, "tools/call", {"name": "reboot"}, msg_id=9).json()
    assert unknown["error"]["code"] == -32602
    method = rpc(mcp_scoped, READ, "resources/read", {}, msg_id=10).json()
    assert method["error"]["code"] == -32601
