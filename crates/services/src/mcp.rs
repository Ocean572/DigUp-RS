//! The Model Context Protocol peer (MCP, protocol version 2025-06-18): JSON-RPC 2.0
//! messages, one JSON object per line (stdio) or per POST (the server's `/api/v1/mcp`),
//! answered by `step()` here and executed by the front-end that hosts it (`crates/cli`
//! runs `digup mcp` over stdio; `crates/server` answers POSTs inside the api_gate,
//! sharing its bearer gate, target scoping and `principal=` audit lines verbatim).
//!
//! The thin-client law governs this module like every other front-end (docs/implementation/
//! README §"the one fact that matters most"): a `tools/call` reply carries **the engine's
//! answer verbatim** in `structuredContent` — the same fields, numbers and order the desktop
//! UI reads — plus a short human `text` block for token-cheap skimming. Nothing here
//! re-ranks, re-formats scores, or invents a number: the tool set is the search daemon's
//! verbs (`search`, `text`, `status`), the CLI's `estimate --json`, and the server's job
//! queue (`index`) — each relayed as it answers.
//!
//! The gating mirrors the REST gate, quoted not paraphrased: the mutating tools
//! (`estimate`, `index`) exist only for the admin principal; a read-scope key calling them
//! lands on the REST gate's own words ("that is this server's read-scope token…"), and the
//! stdio front-end never advertises `index` at all (a local `digup mcp` shares the user's
//! own file permissions — no gate to speak of there, so it advertises `estimate` only with
//! `--allow-admin`).

use serde_json::{Map, Value};

/// The MCP version this peer speaks (spec-latest at the port date; the stdio and HTTP
/// front-ends both negotiate it at `initialize`).
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "digup";

/// JSON-RPC 2.0 codes (the spec's set; -32603 is the executor's umbrella).
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// The REST gate's own words for a read-scope key on a mutating verb (main.rs, verbatim
/// quote) — the MCP layer repeats them, it does not author new refusals.
pub const READ_SCOPE_WORDS: &str =
    "that is this server's read-scope token; indexing and estimating need the admin token";

/// The front-end's surfaces: the API (tokens, targets) vs stdio (the user's own box).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Api,
    Stdio,
}

pub struct CallOutcome {
    pub value: Value,
    pub is_error: bool,
}

/// `step()`: the request's reply when the peer answers it itself, or the tool call the
/// front-end must execute (verbs await: the server's handlers are async, the stdio
/// client blocks — `step()` stays sync for both).
pub enum Step {
    Reply(Value),
    ToolCall {
        id: Value,
        name: String,
        args: Map<String, Value>,
    },
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    serde_json::json!({"name": name, "description": description, "inputSchema": schema})
}

fn object(props: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let mut map = Map::new();
    for (key, schema) in props {
        map.insert(key.to_string(), schema);
    }
    serde_json::json!({
        "type": "object",
        "properties": map,
        "required": required,
        "additionalProperties": false,
    })
}

/// The advertised tool set. `search` relays the search verb's answer in full (its rows
/// carry path/kind/segment/z/loc/excerpt exactly as the UI reads them); `limit` caps the
/// reply's rows (the engine caps at 100); `show` is the daemon's `text` verb — the stored
/// segment text the hit rows name by `segment_id`; the mutating pair rides the API's
/// admin principal only (stdio: `estimate` behind `--allow-admin`, `index`
/// stdio-invisible); `people` is the faces surface (API-only, consent-gated — the
/// REST gate answers its walls on the tool reply, see the push below).
pub fn tool_defs(surface: Surface, allow_admin: bool) -> Vec<Value> {
    let mut defs = vec![
        tool(
            "search",
            "Semantic + keyword search over the DigUp index. The answer rows are the \
             engine's own: path, kind, segment, score, z, loc, excerpt and timings relay \
             verbatim — no server-side re-ranking or invented numbers. The optional \
             `person` filter drops whole rows on the faces store's own membership fact \
             (which photos carry that person's face); the surviving rows' numbers ride \
             unchanged.",
            object(
                vec![
                    ("q", serde_json::json!({"type": "string", "description": "the words to search for"})),
                    ("limit", serde_json::json!({"type": "integer", "minimum": 1, "maximum": 100, "default": 10, "description": "max answer rows (the engine caps at 100)"})),
                    ("code", serde_json::json!({"type": "boolean", "default": false, "description": "search the code index instead"})),
                    ("kinds", serde_json::json!({"type": "array", "items": {"type": "string", "enum": ["screenshot", "image", "pdf", "doc", "audio", "video"]}, "description": "kind filter; omit = all kinds"})),
                    ("name_only", serde_json::json!({"type": "boolean", "default": false, "description": "Names only (#66): match file names, the engine's names-only path, no model; the query rides verbatim"})),
                ],
                &["q"],
            ),
        ),
        tool(
            "status",
            "The index's live counts, verbatim: files, searchable, pending, unfinished, vectors, encoder state.",
            object(vec![], &[]),
        ),
        tool(
            "show",
            "Read one segment's stored full text by its segment_id (from a search row). Relayed verbatim; null when the segment has no stored text.",
            object(
                vec![
                    ("segment_id", serde_json::json!({"type": "integer", "description": "the hit row's segment_id"})),
                    ("code", serde_json::json!({"type": "boolean", "default": false, "description": "the segment lives in the code index"})),
                ],
                &["segment_id"],
            ),
        ),
    ];
    if surface == Surface::Api {
        // Round six (2026-11-23, the user's design): the People filter rides the
        // API's search — `person` keeps only the rows whose photo carries that
        // person's face (the face store's own membership facts, relayed; the
        // engine's z-scores ride verbatim, untouched). stdio has no consent seam
        // (the same rule as the `people` tool below), so the param is invisible
        // on the stdio surface.
        if let Some(search) = defs
            .iter_mut()
            .find(|def| def.get("name").and_then(Value::as_str) == Some("search"))
        {
            if let Some(props) = search
                .get_mut("inputSchema")
                .and_then(|schema| schema.get_mut("properties"))
                .and_then(Value::as_object_mut)
            {
                props.insert(
                    "person".to_string(),
                    serde_json::json!({"type": "integer", "minimum": 1, "description": "People filter: keep only rows whose photo carries this person's face (faces-store membership, relayed verbatim; omit = no filter). 400-shape text when the deployment is not consented"}),
                );
            }
        }
        // The People tab (faces, FACES.md Phase 3). API surface only: the faces
        // verbs ride the server's own config (the consent record lives in the
        // server's config folder, the scan serializes on the server's faces writer
        // lock) — stdio has no consent seam, so the tool is invisible there. The
        // reads ride the ordinary bearer gate like the REST GETs; `scan` rides the
        // admin wall, and the REST gate itself answers a read key (403-shape text
        // quoted on the tool reply — the peer core authors no new refusals).
        defs.push(tool(
            "people",
            "The People tab (face recognition, consent-gated): status/persons/pending \
             read the surface's state, person reads one person's face rows, scan starts \
             a face scan (synchronous on the engine's clock — seconds per photo; \
             serialized behind the faces writer lock, busy answers 409-shape text). \
             Every action answers 400-shape text until the deployment records the \
             user's face-recognition acceptance; the counts and rows relay verbatim.",
            object(
                vec![
                    ("action", serde_json::json!({"type": "string", "enum": ["status", "persons", "pending", "person", "scan"], "default": "status", "description": "what to ask the faces surface"})),
                    ("person", serde_json::json!({"type": "integer", "description": "the person id (action=person)"})),
                ],
                &[],
            ),
        ));
    }
    if allow_admin {
        defs.push(tool(
            "estimate",
            "A crawl forecast for the index's roots: per-folder file counts, bytes, and the engine's own cost seconds. Read-scope keys never see this tool.",
            object(vec![], &[]),
        ));
    }
    if surface == Surface::Api && allow_admin {
        defs.push(tool(
            "index",
            "Queue an index job (the server's one-writer gate answers 409-shape text when a job is already running). The job runs on in the server's runner; this answers its queue record.",
            object(vec![], &[]),
        ));
    }
    defs
}

fn known(surface: Surface, allow_admin: bool, name: &str) -> bool {
    tool_defs(surface, allow_admin)
        .iter()
        .any(|def| def.get("name").and_then(Value::as_str) == Some(name))
}

fn envelope(id: Value, body: Map<String, Value>) -> Value {
    let mut reply = Map::new();
    reply.insert("jsonrpc".into(), Value::String("2.0".to_string()));
    reply.insert("id".into(), id);
    for (key, value) in body {
        reply.insert(key, value);
    }
    Value::Object(reply)
}

fn result(id: Value, value: Value) -> Value {
    let mut body = Map::new();
    body.insert("result".into(), value);
    envelope(id, body)
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    let mut inner = Map::new();
    inner.insert("code".into(), serde_json::json!(code));
    inner.insert("message".into(), Value::String(message.to_string()));
    let mut body = Map::new();
    body.insert("error".into(), Value::Object(inner));
    envelope(id, body)
}

/// A `tools/call` answer shaped to the spec: a skimming `text` block, the engine's
/// answer verbatim in `structuredContent`, and the `isError` fact when the engine
/// refused (its words, quoted — 503/403 shapes included).
pub fn tool_result(name: &str, id: Value, outcome: CallOutcome) -> Value {
    let mut body = Map::new();
    let mut call = Map::new();
    call.insert(
        "content".into(),
        Value::Array(vec![serde_json::json!({
            "type": "text",
            "text": summary(name, &outcome),
        })]),
    );
    call.insert("structuredContent".into(), outcome.value);
    call.insert("isError".into(), serde_json::json!(outcome.is_error));
    body.insert("result".into(), Value::Object(call));
    envelope(id, body)
}

fn summary(name: &str, outcome: &CallOutcome) -> String {
    let value = &outcome.value;
    if outcome.is_error {
        return match value.get("detail").and_then(Value::as_str) {
            Some(text) => text.to_string(),
            None => value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the engine refused")
                .to_string(),
        };
    }
    match name {
        "search" => {
            let empty: Vec<Value> = Vec::new();
            let rows = value
                .get("hits")
                .and_then(Value::as_array)
                .unwrap_or(&empty);
            let strong = rows
                .iter()
                .filter(|row| {
                    row.get("z")
                        .and_then(Value::as_f64)
                        .is_some_and(|z| z >= 2.0)
                })
                .count();
            let mut text = format!("{} hits, {} strong\n", rows.len(), strong);
            for row in rows.iter().take(10) {
                let path = row.get("path").and_then(Value::as_str).unwrap_or("?");
                let kind = row.get("kind").and_then(Value::as_str).unwrap_or("?");
                match row.get("z").and_then(Value::as_f64) {
                    Some(z) => text.push_str(&format!("{kind} {path} z {z:.2}\n")),
                    None => text.push_str(&format!("{kind} {path}\n")),
                }
            }
            text
        }
        "status" => format!(
            "files {} searchable {} pending {} vectors {}",
            value.get("files").and_then(Value::as_i64).unwrap_or(0),
            value.get("searchable").and_then(Value::as_i64).unwrap_or(0),
            value.get("pending").and_then(Value::as_i64).unwrap_or(0),
            value.get("vectors").and_then(Value::as_i64).unwrap_or(0),
        ),
        "show" => match value.get("text") {
            Some(Value::String(text)) => format!("{} chars", text.chars().count()),
            _ => "no stored text".to_string(),
        },
        "estimate" => {
            let folders = value.get("folders").and_then(Value::as_array);
            format!("{} folders", folders.map(Vec::len).unwrap_or(0))
        }
        "index" => {
            // The REST 202 body nests the record: {"ok": true, "job": {…}} — relay
            // the job id and state quoted from inside it, as the relay law asks.
            let job = value.get("job");
            format!(
                "job {} ({})",
                job.as_ref()
                    .and_then(|job| job.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                job.as_ref()
                    .and_then(|job| job.get("state"))
                    .and_then(Value::as_str)
                    .unwrap_or("queued"),
            )
        }
        _ => "ok".to_string(),
    }
}

/// Answer one message. `id` absent = a notification (no response, spec rule) — `None`.
pub fn step(req: &Value, surface: Surface, allow_admin: bool, server_version: &str) -> Step {
    let id = match req.get("id") {
        Some(id) => id.clone(),
        None => return Step::Reply(Value::Null), // notifications/initialized and friends
    };
    match req.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let asked = req
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION);
            let version = if asked == PROTOCOL_VERSION {
                asked
            } else {
                PROTOCOL_VERSION
            };
            Step::Reply(result(
                id,
                serde_json::json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": SERVER_NAME, "version": server_version},
                    "instructions": "Every tool relays the DigUp engine's answer verbatim — \
                                     same rows, scores and timings the desktop UI reads. A \
                                     refusal ('ok:false', a z-gate empty answer) is the \
                                     engine declining to guess; pass it on, do not invent hits.",
                }),
            ))
        }
        Some("ping") => Step::Reply(result(id, serde_json::json!({}))),
        Some("tools/list") => Step::Reply(result(
            id,
            serde_json::json!({"tools": tool_defs(surface, allow_admin)}),
        )),
        Some("tools/call") => {
            let name = match req.pointer("/params/name").and_then(Value::as_str) {
                Some(name) => name.to_string(),
                None => {
                    return Step::Reply(error(id, INVALID_PARAMS, "tools/call needs a name"));
                }
            };
            let args = req
                .pointer("/params/arguments")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if !known(surface, allow_admin, &name) {
                // The mutating tools are invisible to read-scope keys: on the API the
                // answer is the REST gate's own words, quoted (the tool set above is
                // the gate). On stdio there is no gate to speak of — the tool is
                // simply not part of that surface.
                if surface == Surface::Api && matches!(name.as_str(), "estimate" | "index") {
                    return Step::Reply(tool_result(
                        &name,
                        id,
                        CallOutcome {
                            value: serde_json::json!({"detail": READ_SCOPE_WORDS, "status": 403}),
                            is_error: true,
                        },
                    ));
                }
                return Step::Reply(error(id, INVALID_PARAMS, &format!("no such tool: {name}")));
            }
            Step::ToolCall { id, name, args }
        }
        Some(other) => Step::Reply(error(
            id,
            METHOD_NOT_FOUND,
            &format!("method not known to this server: {other}"),
        )),
        None => Step::Reply(error(id, INVALID_REQUEST, "no method")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Option<Value>, id: Value) -> Value {
        let mut req = serde_json::json!({"jsonrpc": "2.0", "method": method, "id": id});
        if let Some(params) = params {
            req.as_object_mut()
                .expect("object")
                .insert("params".into(), params);
        }
        req
    }

    #[test]
    fn initialize_negotiates_the_protocol_version_and_names_the_server() {
        let step = step(
            &request(
                "initialize",
                Some(
                    serde_json::json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
                ),
                serde_json::json!(1),
            ),
            Surface::Api,
            false,
            "0.8.0",
        );
        let Step::Reply(reply) = step else {
            panic!("initialize answers by itself");
        };
        let result = &reply["result"]; // the JSON-RPC envelope nests the payload
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "digup");
        assert_eq!(result["serverInfo"]["version"], "0.8.0");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn a_newer_client_version_still_gets_the_version_this_peer_speaks() {
        let step = step(
            &request(
                "initialize",
                Some(serde_json::json!({"protocolVersion": "2099-01-01"})),
                serde_json::json!(1),
            ),
            Surface::Api,
            false,
            "0.8.0",
        );
        let Step::Reply(reply) = step else {
            panic!("initialize answers by itself");
        };
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn notifications_answer_nothing() {
        let step = step(
            &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            Surface::Api,
            false,
            "0.8.0",
        );
        let Step::Reply(reply) = step else {
            panic!("a notification is answered by itself");
        };
        assert!(reply.is_null());
    }

    #[test]
    fn the_read_key_sees_the_read_tools_the_admin_sees_the_mutating_pair() {
        let read = tool_defs(Surface::Api, false);
        let read: Vec<&str> = read
            .iter()
            .map(|def| def["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(read, vec!["search", "status", "show", "people"]);
        let admin = tool_defs(Surface::Api, true);
        let admin: Vec<&str> = admin
            .iter()
            .map(|def| def["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(
            admin,
            vec!["search", "status", "show", "people", "estimate", "index"]
        );
        // The stdio surface has no job runner to reach: index is invisible there,
        // estimate rides --allow-admin. Faces is server config (the consent record
        // lives in the server's config folder) — people is API-surface-only.
        let stdio_defs = tool_defs(Surface::Stdio, false);
        let stdio: Vec<&str> = stdio_defs
            .iter()
            .map(|def| def["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(stdio, vec!["search", "status", "show"]);
        let stdio_admin_defs = tool_defs(Surface::Stdio, true);
        let stdio_admin: Vec<&str> = stdio_admin_defs
            .iter()
            .map(|def| def["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(stdio_admin, vec!["search", "status", "show", "estimate"]);
    }

    #[test]
    fn a_read_key_calling_a_mutating_tool_lands_on_the_rest_gate_s_words_quoted() {
        let step = step(
            &request(
                "tools/call",
                Some(serde_json::json!({"name": "index", "arguments": {}})),
                serde_json::json!(7),
            ),
            Surface::Api,
            false,
            "0.8.0",
        );
        let Step::Reply(reply) = step else {
            panic!("the gate answers without executing");
        };
        assert_eq!(reply["result"]["isError"], serde_json::json!(true));
        assert_eq!(reply["result"]["content"][0]["text"], READ_SCOPE_WORDS);
        assert_eq!(reply["result"]["structuredContent"]["status"], 403);
    }

    #[test]
    fn the_stdio_surface_has_no_gate_so_its_missing_mutating_tools_are_plain_unknowns() {
        // No bearer gate, no read-scope words there: stdio's tool set simply ends
        // at the daemon's verbs (the CLI suite pins the same fact end to end).
        let Step::Reply(reply) = step(
            &request(
                "tools/call",
                Some(serde_json::json!({"name": "index", "arguments": {}})),
                serde_json::json!(14),
            ),
            Surface::Stdio,
            false,
            "0.8.0",
        ) else {
            panic!("answered by itself");
        };
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
        assert!(reply["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("index"));
    }

    #[test]
    fn a_known_tool_hands_the_call_to_the_host() {
        let step = step(
            &request(
                "tools/call",
                Some(
                    serde_json::json!({"name": "search", "arguments": {"q": "lemon cake", "limit": 5}}),
                ),
                serde_json::json!(8),
            ),
            Surface::Stdio,
            false,
            "0.8.0",
        );
        let Step::ToolCall { id, name, args } = step else {
            panic!("the host runs known tools");
        };
        assert_eq!(id, serde_json::json!(8));
        assert_eq!(name, "search");
        assert_eq!(args.get("q").expect("q"), "lemon cake");
        assert_eq!(args.get("limit").expect("limit"), 5);
    }

    #[test]
    fn an_unknown_tool_is_invalid_params_an_unknown_method_is_method_not_found() {
        let Step::Reply(reply) = step(
            &request(
                "tools/call",
                Some(serde_json::json!({"name": "reboot"})),
                serde_json::json!(9),
            ),
            Surface::Api,
            false,
            "0.8.0",
        ) else {
            panic!("answered by itself");
        };
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);
        let Step::Reply(reply) = step(
            &request(
                "resources/read",
                Some(serde_json::json!({})),
                serde_json::json!(10),
            ),
            Surface::Api,
            false,
            "0.8.0",
        ) else {
            panic!("answered by itself");
        };
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn a_search_answer_carries_the_engine_rows_verbatim_plus_a_skimming_text() {
        let rows = serde_json::json!({
            "ok": true,
            "hits": [{"path": "/media/x/recipe.txt", "kind": "doc", "segment_id": 4, "z": 8.081, "excerpt": "Lemon cake"}],
            "ms": 421,
            "phase": "meaning",
        });
        let reply = tool_result(
            "search",
            serde_json::json!(11),
            CallOutcome {
                value: rows.clone(),
                is_error: false,
            },
        );
        // structuredContent is the engine's answer, byte-for-byte: same keys, same
        // numbers — the relay rule pinned where it lives.
        assert_eq!(reply["result"]["structuredContent"], rows);
        assert_eq!(reply["result"]["isError"], serde_json::json!(false));
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .expect("text")
            .to_string();
        assert!(text.starts_with("1 hits, 1 strong"));
        assert!(text.contains("doc /media/x/recipe.txt z 8.08"));
    }

    #[test]
    fn a_refusal_lands_as_is_error_with_the_engine_s_own_words() {
        let reply = tool_result(
            "search",
            serde_json::json!(12),
            CallOutcome {
                value: serde_json::json!({"ok": false, "error": "index is open elsewhere"}),
                is_error: true,
            },
        );
        assert_eq!(reply["result"]["isError"], serde_json::json!(true));
        assert_eq!(
            reply["result"]["content"][0]["text"],
            "index is open elsewhere"
        );
    }

    #[test]
    fn ping_answers_an_empty_result_and_the_envelope_carries_the_request_id() {
        let Step::Reply(reply) = step(
            &request("ping", None, serde_json::json!("abc")),
            Surface::Api,
            false,
            "0.8.0",
        ) else {
            panic!("ping answers by itself");
        };
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], "abc");
        assert_eq!(reply["result"], serde_json::json!({}));
    }
}
