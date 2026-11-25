//! The published OpenAPI surface — `GET /api/v1/openapi.json` and the browsable
//! docs page `GET /api/v1/docs`.
//!
//! FastAPI served both for free from its own router; the axum port must carry them
//! by hand, because `docs/API.md` **promises** them (its opening lines name both
//! URLs, `security::OPEN_PATHS` keeps both unguarded by design, and the suite
//! pins them live: `tests/test_api.py` checks the table's shape,
//! `tests/test_api_docs.py` fails CI if a route lands in this table and not in
//! `API.md`, and `tests/test_security.py` pins that a token-guarded deployment
//! still answers both with no key).
//!
//! The table below is pinned to THIS server's route registrations
//! (`main.rs`'s `Router`): the spec describes the endpoints `API.md` documents
//! and the server relays; it invents nothing about the engine. Keep the two in
//! step — a router route missing here is an undocumented endpoint, and the docs
//! tripwire says so in CI.

use serde_json::{json, Value};

/// One `target` query parameter — the named-indexes scope (docs/API.md,
/// "named indexes"): guarded endpoints answer their own target by default;
/// an application's key is refused on any other.
fn target_param() -> Value {
    json!({
        "name": "target",
        "in": "query",
        "required": false,
        "description": "Named index to answer (defaults to the caller's own target)",
        "schema": { "type": "string" }
    })
}

fn get_op(summary: &str, id: &str) -> Value {
    json!({
        "tags": ["api"],
        "summary": summary,
        "operationId": id,
        "parameters": [target_param()],
        "responses": {
            "200": { "description": summary },
            "401": { "description": "bearer token required (www-authenticate: Bearer)" },
            "403": { "description": "the token's scope does not cover this" },
            "409": { "description": "a job/estimate is already running" },
            "502": { "description": "the engine CLI failed" },
            "503": { "description": "the engine's refusal, relayed verbatim" },
        },
    })
}

fn post_op(summary: &str, id: &str, body: Value) -> Value {
    json!({
        "tags": ["api"],
        "summary": summary,
        "operationId": id,
        "parameters": [target_param()],
        "requestBody": {
            "required": true,
            "content": { "application/json": { "schema": body } }
        },
        "responses": {
            "200": { "description": summary },
            "202": { "description": "the job is queued (index)" },
            "401": { "description": "bearer token required (www-authenticate: Bearer)" },
            "403": { "description": "the token's scope does not cover this" },
            "409": { "description": "a job/estimate is already running" },
            "422": { "description": "validation: a list of {type, loc, msg} objects" },
            "502": { "description": "the engine CLI failed" },
            "503": { "description": "the engine's refusal, relayed verbatim" },
        },
    })
}

/// Path → operations, pinned to `main.rs`'s router. Keep in step.
fn paths() -> Value {
    // The entries with query parameters are built outside the big table's
    // `json!` (their nesting pushes the macro past the default recursion limit).
    let faces_similar = json!({
        "tags": ["api"],
        "summary": "Faces similar to one face (the engine's brute-force cosine rank)",
        "operationId": "faces_similar",
        "parameters": [
            target_param(),
            { "name": "face", "in": "query", "required": true, "schema": { "type": "integer", "minimum": 1 } },
            { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 } }
        ],
        "responses": {
            "200": { "description": "Faces similar to one face (the engine's brute-force cosine rank)" },
            "401": { "description": "bearer token required (www-authenticate: Bearer)" },
            "403": { "description": "the token's scope does not cover this" },
            "422": { "description": "validation: a list of {type, loc, msg} objects" },
            "502": { "description": "the engine CLI failed" },
            "503": { "description": "the engine's refusal, relayed verbatim" }
        }
    });
    let faces_person_show = json!({
        "tags": ["api"],
        "summary": "One person's faces: engine rows with box/kps/score + preview media handles",
        "operationId": "faces_person_show",
        "parameters": [
            target_param(),
            { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 1, "maximum": 10000 } },
            { "name": "offset", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 0 } },
            { "name": "order", "in": "query", "required": false, "schema": { "type": "string", "enum": ["photo", "scan"] } }
        ],
        "responses": {
            "200": { "description": "One person's faces: engine rows with box/kps/score + preview media handles" },
            "401": { "description": "bearer token required (www-authenticate: Bearer)" },
            "403": { "description": "the token's scope does not cover this" },
            "422": { "description": "validation: a list of {type, loc, msg} objects" },
            "502": { "description": "the engine CLI failed" },
            "503": { "description": "the engine's refusal, relayed verbatim" }
        }
    });
    json!({
        "/api/v1/health": {
            "get": get_op("Engine identity + live index counts (503 while the searcher respawns; unguarded)", "health")
        },
        "/api/v1/status": {
            "get": get_op("Live status: roots, kinds table, vectors, encoder state", "status")
        },
        "/api/v1/search": {
            "post": post_op(
                "The engine's ranking, relayed verbatim: hits, strong, meaning, ms",
                "search",
                json!({
                    "type": "object",
                    "required": ["q"],
                    "properties": {
                        "q": { "type": "string", "maxLength": 512 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 40 },
                        "kinds": { "type": "array", "items": { "type": "string", "enum": ["doc", "image", "video", "code"] } },
                        "meaning": { "type": "boolean", "default": true },
                        "person": { "type": "integer", "minimum": 1, "description": "People filter (faces store, consent-gated): keep only hits whose photo carries this person's face; the engine's z-scores ride verbatim" },
                        "name_only": { "type": "boolean", "default": false, "description": "Names only (#66): the engine's names-only path (FTS5 name segments, no model); the daemon composes the typed name: spelling, the reply shape is the ordinary search answer" }
                    }
                })
            )
        },
        "/api/v1/embed_ahead": {
            "post": post_op(
                "Keyword-first typing preview (the UI's box)",
                "embed_ahead",
                json!({
                    "type": "object",
                    "required": ["q"],
                    "properties": {
                        "q": { "type": "string", "maxLength": 512 },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                    }
                })
            )
        },
        "/api/v1/estimate": {
            "post": post_op(
                "The engine's own forecast (file counts per root folder)",
                "estimate",
                json!({ "type": "object", "properties": {} })
            )
        },
        "/api/v1/index": {
            "post": post_op(
                "Queue one crawl (202 accepted, 409 busy — one writer per index)",
                "index",
                json!({
                    "type": "object",
                    "properties": {
                        "prune": { "type": "boolean", "default": false,
                                   "description": "also retire rows no longer found under the roots" }
                    }
                })
            )
        },
        "/api/v1/mcp": {
            "post": post_op(
                "Model Context Protocol peer (JSON-RPC 2.0, one message per POST): tools search/status/show \
                 for any valid key, + estimate/index for admin keys; every tool relays the engine's answer verbatim",
                "mcp",
                json!({ "type": "object", "properties": {} })
            )
        },
        "/api/v1/jobs": {
            "get": get_op("The target's job records, insertion order", "jobs")
        },
        "/api/v1/jobs/{job_id}": {
            "get": {
                "tags": ["api"],
                "summary": "One job record",
                "operationId": "job_show",
                "parameters": [
                    { "name": "job_id", "in": "path", "required": true,
                      "schema": { "type": "string" } }
                ],
                "responses": {
                    "200": { "description": "One job record" },
                    "404": { "description": "unknown job id" }
                }
            }
        },
        "/api/v1/media/{media_id}/preview": {
            "get": {
                "tags": ["api"],
                "summary": "Image/video/pdf preview (JPEG) for a server-issued handle",
                "operationId": "media_preview",
                "parameters": [
                    { "name": "media_id", "in": "path", "required": true,
                      "description": "A handle issued by an earlier search reply (never a path)",
                      "schema": { "type": "string" } },
                    { "name": "pixels", "in": "query", "required": false,
                      "schema": { "type": "integer", "minimum": 32, "maximum": 2048, "default": 512 } }
                ],
                "responses": {
                    "200": { "description": "preview image bytes" },
                    "404": { "description": "dead media handle" },
                    "503": { "description": "the engine's refusal, relayed verbatim" }
                }
            }
        },
        "/api/v1/media/{media_id}/text": {
            "get": {
                "tags": ["api"],
                "summary": "Text document body (docs only)",
                "operationId": "media_text",
                "parameters": [{ "name": "media_id", "in": "path", "required": true,
                                 "schema": { "type": "string" } }],
                "responses": {
                    "200": { "description": "the document's text" },
                    "404": { "description": "dead media handle" }
                }
            }
        },
        "/api/v1/media/{media_id}/file": {
            "get": {
                "tags": ["api"],
                "summary": "The original file (Range-aware; served spans cap at 8 MiB)",
                "operationId": "media_file",
                "parameters": [{ "name": "media_id", "in": "path", "required": true,
                                 "schema": { "type": "string" } }],
                "responses": {
                    "200": { "description": "the file" },
                    "206": { "description": "one requested range (Content-Range set)" },
                    "404": { "description": "dead media handle" },
                    "416": { "description": "range outside the file" }
                }
            }
        },
        "/api/v1/targets": {
            "get": get_op("The named-index registry with counts (admin sees all, an app its own)", "targets_list"),
            "post": post_op(
                "Register an application target at runtime (admin key) — minted private key + own index",
                "targets_register",
                json!({
                    "type": "object",
                    "required": ["name", "roots"],
                    "properties": {
                        "name": { "type": "string" },
                        "roots": { "type": "array", "items": { "type": "string" } },
                        "index": { "type": "string", "description": "default <config>/index-<name>" }
                    }
                })
            )
        },
        "/api/v1/tokens": {
            "get": get_op("The server-minted keys (shown key-free in a minted deployment)", "tokens_list"),
            "post": post_op(
                "Regenerate the minted keys (admin scope; 400 when env is the authority)",
                "tokens_rotate",
                json!({ "type": "object", "properties": {} })
            )
        },
        "/api/v1/faces/status": {
            "get": get_op("Faces state: consent, tier pins present, the engine's counts", "faces_status")
        },
        "/api/v1/faces/consent": {
            "get": get_op("The target's consent record (consented + the accepted tier)", "faces_consent_get"),
            "post": post_op(
                "The affirmative license acceptance (admin; writes the 0600 record)",
                "faces_consent_post",
                json!({
                    "type": "object",
                    "required": ["accept"],
                    "properties": {
                        "accept": { "type": "boolean", "enum": [true] },
                        "det_model": { "type": "string", "enum": ["scrfd_10g_bnkps.onnx"] },
                        "emb_model": { "type": "string", "enum": ["w600k_mbf.onnx", "w600k_r50.onnx", "adaface_ir_18.onnx", "adaface_ir_50.onnx", "adaface_ir_101.onnx"] }
                    }
                })
            ),
            "delete": get_op("Withdraw consent (admin; removes the record, the store stays)", "faces_consent_delete")
        },
        "/api/v1/faces/scan": {
            "post": post_op(
                "The faces scan on the target's own roots (admin; 409 when a scan runs; counts relayed)",
                "faces_scan",
                json!({
                    "type": "object",
                    "properties": { "limit": { "type": "integer", "minimum": 1 } }
                })
            )
        },
        "/api/v1/faces/persons": {
            "get": json!({
                "tags": ["api"],
                "summary": "The person cards: id, name, count, cover face (with its media handle)",
                "operationId": "faces_persons",
                "parameters": [
                    target_param(),
                    { "name": "limit", "in": "query", "required": false, "schema": { "type": "integer", "minimum": 1, "maximum": 1000000 } },
                    { "name": "named", "in": "query", "required": false, "schema": { "type": "boolean" } }
                ],
                "responses": {
                    "200": { "description": "The person cards: id, name, count, cover face (with its media handle)" },
                    "401": { "description": "bearer token required (www-authenticate: Bearer)" },
                    "403": { "description": "the token's scope does not cover this" },
                    "422": { "description": "validation: a list of {type, loc, msg} objects" },
                    "502": { "description": "the engine CLI failed" },
                    "503": { "description": "the engine's refusal, relayed verbatim" }
                }
            })
        },
        "/api/v1/faces/person/{id}": {
            "get": faces_person_show
        },
        "/api/v1/faces/name": {
            "post": post_op(
                "Name a person (admin)",
                "faces_name",
                json!({
                    "type": "object",
                    "required": ["person", "name"],
                    "properties": {
                        "person": { "type": "integer", "minimum": 1 },
                        "name": { "type": "string", "minLength": 1, "maxLength": 120 }
                    }
                })
            )
        },
        "/api/v1/faces/merge": {
            "post": post_op(
                "Merge two persons (admin)",
                "faces_merge",
                json!({
                    "type": "object",
                    "required": ["into", "from"],
                    "properties": {
                        "into": { "type": "integer", "minimum": 1 },
                        "from": { "type": "integer", "minimum": 1 }
                    }
                })
            )
        },
        "/api/v1/faces/delete": {
            "post": post_op(
                "Delete a person — its faces queue for re-clustering (admin)",
                "faces_delete",
                json!({
                    "type": "object",
                    "required": ["person"],
                    "properties": { "person": { "type": "integer", "minimum": 1 } }
                })
            )
        },
        "/api/v1/faces/confirm": {
            "post": post_op(
                "Confirm a queued face into a person (admin; the human answer to a collision)",
                "faces_confirm",
                json!({
                    "type": "object",
                    "required": ["face", "person"],
                    "properties": {
                        "face": { "type": "integer", "minimum": 1 },
                        "person": { "type": "integer", "minimum": 1 }
                    }
                })
            )
        },
        "/api/v1/faces/pending": {
            "get": get_op("The collision guard's review queue with the engine's top candidates", "faces_pending")
        },
        "/api/v1/faces/similar": {
            "get": faces_similar
        },
        "/api/v1/openapi.json": {
            "get": get_op("This document (unguarded by design)", "openapi")
        },
        "/api/v1/docs": {
            "get": get_op("The browsable docs page (unguarded by design)", "docs")
        }
    })
}

/// `GET /api/v1/openapi.json` — the machine-readable route table.
pub fn spec() -> Value {
    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "DigUp API",
            "version": "v1",
            "description": "A self-hosted DigUp server: a thin HTTP surface over the \
                engine's published interfaces. Every number answered is the engine's. \
                The human reference and compatibility promise is server/docs/API.md; \
                this table is the route surface that document names."
        },
        "servers": [{ "url": "/" }],
        "paths": paths(),
    })
}

/// `GET /api/v1/docs` — a self-contained browsable page: no scripts, no external
/// URLs (the UI's §10.4 rule, applied to the server's own page). The table is
/// this module's table; the prose is API.md's rules, quoted short.
pub const DOCS_HTML: &str = r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="referrer" content="none">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>DigUp API v1 — docs</title>
<style>
 body{font:15px/1.55 system-ui,sans-serif;margin:2rem auto;max-width:60rem;padding:0 1rem;color:#181818}
 h1{font-size:1.45rem} h2{font-size:1.1rem;margin-top:1.6rem}
 code{font-family:ui-monospace,monospace;font-size:.93em}
 a{color:#0a58ca}
 table{border-collapse:collapse;width:100%}
 th,td{border:1px solid #bbb;padding:.4rem .6rem;text-align:left;vertical-align:top}
 th{background:#f3f3f3} td.m{white-space:nowrap;font-weight:600}
 @media (prefers-color-scheme:dark){body{background:#151618;color:#d8d8d8}th{background:#24262a}th,td{border-color:#444}a{color:#7fb3ff}}
</style>
</head><body>
<h1>DigUp API <code>v1</code></h1>
<p>The self-hosted search server is a thin HTTP wrapper over the DigUp engine:
every answer is the engine's own — its ranking, its counts, its timings, its
refusals — relayed verbatim. The human reference and the compatibility promise
is <code>server/docs/API.md</code>; the machine-readable route table is
<a href="/api/v1/openapi.json"><code>/api/v1/openapi.json</code></a>. This page
is self-contained: no scripts, no external URLs.</p>

<h2>Endpoints</h2>
<table>
<tr><th>Method</th><th>Path</th><th>What it answers</th></tr>
<tr><td class="m">GET</td><td><code>/api/v1/health</code></td><td>Engine identity + live index counts; 503 while the searcher respawns (a healthy container counts 503 as alive). Unguarded — the healthcheck has no key.</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/status</code></td><td>Live status: the server's own roots, the kinds table, vectors, encoder state.</td></tr>
<tr><td class="m">POST</td><td><code>/api/v1/search</code></td><td><code>{"q", "limit" (1–100, default 40), "kinds", "meaning"}</code> — the engine's ranking: <code>hits, strong, meaning, ms</code>. An honest empty is an empty, not a fill.</td></tr>
<tr><td class="m">POST</td><td><code>/api/v1/embed_ahead</code></td><td>Keyword-first typing preview for the search box.</td></tr>
<tr><td class="m">POST</td><td><code>/api/v1/estimate</code></td><td>The engine's forecast (per-folder file counts). One full-tree crawl server-wide: a second request answers 409.</td></tr>
<tr><td class="m">POST</td><td><code>/api/v1/index</code></td><td>Queue one crawl (<code>{"prune"}</code> optional): 202 accepted, 409 when a writer is running. One writer per index; a job never auto-starts from a boot.</td></tr>
<tr><td class="m">POST</td><td><code>/api/v1/mcp</code></td><td>Model Context Protocol peer (JSON-RPC 2.0, one message per POST): tools <code>search</code>/<code>status</code>/<code>show</code> for any valid key, <code>estimate</code>/<code>index</code> for admin keys; every tool relays the engine's answer verbatim.</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/jobs</code></td><td>The target's job records, insertion order (<code>queued/running/done/interrupted/busy</code>).</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/jobs/{job_id}</code></td><td>One job record; 404 for an unknown id.</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/media/{media_id}/preview?pixels=</code></td><td>Image/video/pdf preview as JPEG. Handles are issued per reply and name server-side files only — never client paths.</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/media/{media_id}/text</code></td><td>Text document body (documents only).</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/media/{media_id}/file</code></td><td>The original file, Range-aware (single ranges; served spans cap at 8 MiB; 206 answers carry Content-Range).</td></tr>
<tr><td class="m">GET/POST</td><td><code>/api/v1/targets</code></td><td>List the named-index registry (with counts) / register an application at runtime (admin key: minted private key, own index folder).</td></tr>
<tr><td class="m">GET/POST</td><td><code>/api/v1/tokens</code></td><td>Show the server-minted keys / regenerate them (admin scope; 400 when the environment is the authority).</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/openapi.json</code></td><td>This route table (machine-readable).</td></tr>
<tr><td class="m">GET</td><td><code>/api/v1/docs</code></td><td>This page.</td></tr>
</table>

<h2>The shape a client must know</h2>
<ul>
<li><strong>One server process.</strong> Media handles live in its cache and the job
    runner is a thread inside it; replicas 404 handles issued by the other process.</li>
<li><strong>Answers are scoped to roots.</strong> The server answers only for files
    under its configured roots; rows outside the scope keep counting in the store
    (the cure is <code>POST /index {"prune":true}</code>).</li>
<li><strong>Errors are one <code>{"detail": …}</code> body</strong> — a string, safe to
    show the user, except validation (<code>422</code>), whose detail is a list of
    <code>{type, loc, msg}</code> objects. <code>401</code> carries
    <code>www-authenticate: Bearer</code>.</li>
<li><strong>Tokens are opt-in.</strong> No token env → the server is unguarded
    (localhost default). A named read token covers the search side; index/estimate
    need the admin token; an application's minted key answers its own named target
    and nothing else. <code>/health</code>, this page, and the OpenAPI table stay
    open on purpose.</li>
<li><strong>Indexing never starts by itself</strong> — the UI's Index now, or
    <code>POST /index</code>. A SIGTERM mid-job records the job <em>interrupted</em>
    with its partial counters, never success; the next job resumes the walk.</li>
</ul>

<p class="muted">Server code: <code>crates/server</code> (the <code>digup-server</code>
binary). Contract suite: <code>server/tests</code> (it drives this binary over HTTP).
API reference: <code>server/docs/API.md</code>.</p>
</body></html>
"#;
