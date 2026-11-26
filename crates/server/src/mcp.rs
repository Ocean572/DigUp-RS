//! `POST /api/v1/mcp` — the Model Context Protocol front-end of the API (the peer
//! core is `digup-services::mcp`). JSON-RPC 2.0 per POST, streamable-HTTP transport:
//! one message in, one JSON answer out, with an `mcp-session-id` header echoed (the
//! spec's stateless mode: no session table to keep, no SSE to stream — clients that
//! demand the SSE flavour reach this server through `mcp-remote`, the community
//! proxy, which translates; the stdio front-end (`digup mcp`) covers the clients
//! that speak stdio natively).
//!
//! The endpoint rides the `api_gate` like every guarded route: bearer gate, target
//! scoping (`?target=`, app keys answering their own index, 403s quoted from the
//! gate), CORS decoration, and the `principal= … target= …` audit line — the tool
//! calls are audited REST calls too, because the tools ARE the REST endpoints,
//! invoked in-process: `search`/`status`/`estimate`/`index` call the same handlers
//! the browser calls, so their answers relay verbatim by construction, and every
//! gate (422 body shapes, 409 busy, 503 engine words) lands on the tool reply
//! unchanged. `show` is the search daemon's `text` verb through the target's own
//! service window (the blocking pool rule, in_scope — never on a tokio worker).
//!
//! Tool visibility mirrors the REST gate: a read-scope key sees search/status/show
//! (+ people — the faces reads ride the ordinary bearer gate); the mutating pair
//! (estimate/index) exists only for the admin principal, and
//! calling them with a read key answers the gate's own words (the peer core quotes
//! them, this layer authors no new refusals).

use std::sync::LazyLock;

use axum::body::{to_bytes, Bytes};
use axum::extract::{Extension, State};
use axum::http::header::HeaderValue;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use digup_services::mcp::{self, CallOutcome, Step, Surface};

use crate::{in_scope, request_target, ApiError, Ctx, Shared};

/// Per-process, spec-unique, not a secret: the session header is a routing fact
/// (the hash seeds carry the OS randomness). The spec's stateless mode answers every
/// request on its own; a client that tracks the header gets a stable value back.
static SESSION_ID: LazyLock<String> = LazyLock::new(|| {
    use std::hash::BuildHasher;
    format!(
        "{:016x}",
        std::collections::hash_map::RandomState::new().hash_one(std::process::id())
    )
});

fn api_error_text(error: &ApiError) -> String {
    match error {
        ApiError::Engine(text)
        | ApiError::Refused(text)
        | ApiError::Timeout(text)
        | ApiError::Cli(text)
        | ApiError::Busy(text)
        | ApiError::Http(_, text) => text.clone(),
        ApiError::Internal => "internal error".to_string(),
    }
}

pub async fn mcp_post(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    body: Bytes,
) -> Response {
    let req: Value = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(e) => {
            return answer(
                mcp::error(Value::Null, mcp::PARSE_ERROR, &e.to_string()),
                StatusCode::BAD_REQUEST,
            )
        }
    };
    if !req.is_object() {
        return answer(
            mcp::error(
                Value::Null,
                mcp::INVALID_REQUEST,
                "the POST body is one JSON-RPC object (this server answers one message per POST)",
            ),
            StatusCode::BAD_REQUEST,
        );
    }
    // The gate's principal facts (api_gate): the admin bearer answers the mutating
    // pair; on a read route the admin key reads as `admin (read scope)` — tools/list
    // advertises the full set there, matching what this key's REST calls may do.
    // An app principal rides too: the gate enforces its own-target rule in the
    // handlers themselves. And `""` is the inert gate (no tokens named): there the
    // key-free REST admin routes are open, so the tools ride the same openness.
    let allow_admin = ctx.principal == "admin"
        || ctx.principal == "admin (read scope)"
        || ctx.principal.is_empty()
        || ctx.principal.starts_with("app:");
    match mcp::step(&req, Surface::Api, allow_admin, env!("CARGO_PKG_VERSION")) {
        Step::Reply(value) => {
            if value.is_null() {
                // A notification (spec rule: answered by nothing): streamable-HTTP
                // says 202 Accepted with an empty body.
                return answer(Value::Null, StatusCode::ACCEPTED);
            }
            answer(value, StatusCode::OK)
        }
        Step::ToolCall { id, name, args } => {
            let reply = match execute(app, &ctx, &name, &args).await {
                Ok(outcome) => mcp::tool_result(&name, id, outcome),
                Err((code, message)) => mcp::error(id, code, &message),
            };
            answer(reply, StatusCode::OK)
        }
    }
}

fn answer(value: Value, status: StatusCode) -> Response {
    let mut response = if value.is_null() {
        // The notification's 202: an empty body. `[]` headers are ambiguous in
        // axum 0.8 (E0282/E0283); `Body::empty()` is the unambiguous shape.
        (status, axum::body::Body::empty()).into_response()
    } else {
        (status, Json(value)).into_response()
    };
    if let Ok(header) = HeaderValue::from_str(&SESSION_ID) {
        response.headers_mut().insert("mcp-session-id", header);
    }
    response
}

async fn execute(
    app: Shared,
    ctx: &Ctx,
    name: &str,
    args: &Map<String, Value>,
) -> Result<CallOutcome, (i64, String)> {
    match name {
        "search" => {
            let Some(query) = args.get("q").and_then(Value::as_str) else {
                return Err((mcp::INVALID_PARAMS, "search needs q".to_string()));
            };
            let mut body = json!({
                "q": query,
                "limit": args.get("limit").and_then(Value::as_i64).unwrap_or(10).clamp(1, 100),
            });
            if let Some(code) = args.get("code").and_then(Value::as_bool) {
                body["code"] = json!(code);
            }
            if let Some(kinds) = args.get("kinds").and_then(Value::as_array) {
                body["kinds"] = json!(kinds);
            }
            // Names only (#66): relayed like the code/kinds fields — absent is
            // the plain search.
            if let Some(name_only) = args.get("name_only").and_then(Value::as_bool) {
                body["name_only"] = json!(name_only);
            }
            // Scope + extension (#206/#132/#188): relayed verbatim, same law.
            if let Some(under) = args.get("under").and_then(Value::as_array) {
                body["under"] = json!(under);
            }
            if let Some(ext) = args.get("ext").and_then(Value::as_array) {
                body["ext"] = json!(ext);
            }
            // The People filter (round six, 2026-11-23): relayed to the REST route
            // verbatim — absent means the pure, unfiltered search.
            if let Some(person) = args.get("person").and_then(Value::as_i64) {
                body["person"] = json!(person);
            }
            let response = crate::search(
                State(app),
                Extension(ctx.clone()),
                Bytes::from(serde_json::to_vec(&body).expect("built json")),
            )
            .await;
            Ok(relay(response).await)
        }
        "find_like" => {
            // #162/#121: the tool IS the REST route (the `people` law) — the
            // clicked hit rides as its minted handle, the route resolves it
            // through the HitCache, answers the media routes' 404 for a dead
            // one, and relays the engine's rows verbatim. Target scoping
            // (media_scope_allows + in_scope) lives in the handler.
            let Some(media_id) = args.get("media_id").and_then(Value::as_str) else {
                return Err((mcp::INVALID_PARAMS, "find_like needs media_id".to_string()));
            };
            let mut body = json!({
                "media_id": media_id,
                "limit": args.get("limit").and_then(Value::as_i64).unwrap_or(10).clamp(1, 100),
            });
            if let Some(kinds) = args.get("kinds").and_then(Value::as_array) {
                body["kinds"] = json!(kinds);
            }
            if let Some(under) = args.get("under").and_then(Value::as_array) {
                body["under"] = json!(under);
            }
            if let Some(ext) = args.get("ext").and_then(Value::as_array) {
                body["ext"] = json!(ext);
            }
            let response = crate::find_like(
                State(app),
                Extension(ctx.clone()),
                Bytes::from(serde_json::to_vec(&body).expect("built json")),
            )
            .await;
            Ok(relay(response).await)
        }
        "status" => {
            Ok(relay(crate::status_endpoint(State(app), Extension(ctx.clone())).await).await)
        }
        "estimate" => Ok(relay(
            crate::estimate(
                State(app),
                Extension(ctx.clone()),
                Bytes::from(b"{}".to_vec()),
            )
            .await,
        )
        .await),
        "index" => Ok(relay(
            crate::index(
                State(app),
                Extension(ctx.clone()),
                Bytes::from(b"{}".to_vec()),
            )
            .await,
        )
        .await),
        "people" => {
            // The faces surface (FACES.md Phase 3): the tool IS the REST routes —
            // every wall (consent 400, busy 409, admin 403 on scan) answers through
            // the same handlers the browser calls, and relay() carries their words.
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("status");
            let response = match action {
                "status" => crate::faces_status(State(app.clone()), Extension(ctx.clone())).await,
                "persons" => {
                    // The paintable window + the names list ride the route's own query
                    // law: `limit`/`named` args become the route's `?limit=`/`?named=`
                    // (the route answers its own 422 for a bad one — the tool relays,
                    // never edits).
                    let mut query = Vec::new();
                    if let Some(n) = args.get("limit").and_then(Value::as_i64) {
                        query.push(format!("limit={n}"));
                    }
                    if args.get("named").and_then(Value::as_bool) == Some(true) {
                        query.push("named=1".to_string());
                    }
                    let uri = if query.is_empty() {
                        Uri::from_static("/api/v1/faces/persons")
                    } else {
                        format!("/api/v1/faces/persons?{}", query.join("&"))
                            .parse()
                            .unwrap_or_else(|_| Uri::from_static("/api/v1/faces/persons?limit=x"))
                    };
                    crate::faces_persons(State(app.clone()), Extension(ctx.clone()), uri).await
                }
                "pending" => crate::faces_pending(State(app.clone()), Extension(ctx.clone())).await,
                "person" => {
                    let Some(person) = args.get("person").and_then(Value::as_i64) else {
                        return Err((
                            mcp::INVALID_PARAMS,
                            "action=person needs person".to_string(),
                        ));
                    };
                    // The painted page: `limit`/`offset`/`order` args become the route's
                    // query (same relay law as the window above).
                    let mut query = Vec::new();
                    if let Some(n) = args.get("limit").and_then(Value::as_i64) {
                        query.push(format!("limit={n}"));
                    }
                    if let Some(n) = args.get("offset").and_then(Value::as_i64) {
                        query.push(format!("offset={n}"));
                    }
                    if let Some(o) = args.get("order").and_then(Value::as_str) {
                        query.push(format!("order={o}"));
                    }
                    let uri = if query.is_empty() {
                        Uri::from_static("/api/v1/faces/person")
                    } else {
                        format!("/api/v1/faces/person?{}", query.join("&"))
                            .parse()
                            .unwrap_or_else(|_| Uri::from_static("/api/v1/faces/person?limit=x"))
                    };
                    crate::faces_person_show(
                        State(app.clone()),
                        Extension(ctx.clone()),
                        axum::extract::Path(person.to_string()),
                        uri,
                    )
                    .await
                }
                "scan" => {
                    // The scan rides the REST admin wall (FACES.md: scans are
                    // ADMIN_ROUTES). A browser call passes through the bearer
                    // middleware, which enforces it; tool calls skip the middleware
                    // (the gate admitted the principal for /mcp), so the front-end
                    // enforces the SAME principal rule — quoting the gate's sentence
                    // in the relay's exact body shape, refusing only what the
                    // middleware refuses (a read key; app tokens stay target-scoped
                    // like the middleware leaves them, inert/admin pass).
                    if ctx.principal == "read" {
                        return Ok(CallOutcome {
                            is_error: true,
                            value: json!({"detail": mcp::READ_SCOPE_WORDS}),
                        });
                    }
                    crate::faces_scan(
                        State(app.clone()),
                        Extension(ctx.clone()),
                        Bytes::from(b"{}".to_vec()),
                    )
                    .await
                }
                other => {
                    return Err((
                        mcp::INVALID_PARAMS,
                        format!("people: no such action: {other}"),
                    ));
                }
            };
            Ok(relay(response).await)
        }
        "show" => {
            let Some(segment) = args.get("segment_id").and_then(Value::as_i64) else {
                return Err((mcp::INVALID_PARAMS, "show needs segment_id".to_string()));
            };
            let code = args.get("code").and_then(Value::as_bool).unwrap_or(false);
            let target = request_target(ctx);
            // The daemon's verb through the target's window — the searcher spawn is
            // blocking work, never on a tokio worker (the in_scope rule the media
            // endpoints ride). The verb's answer relays whole: {text: …} or the
            // engine's refusal words.
            let reply = in_scope(app, target, move |scope: &std::sync::Arc<crate::Scope>| {
                scope.service.text(segment, code).map_err(ApiError::from)
            })
            .await
            .map_err(|e| (mcp::INTERNAL_ERROR, api_error_text(&e)))?;
            let mut reply = reply;
            if let Some(row) = reply.as_object_mut() {
                row.remove("id");
            }
            Ok(CallOutcome {
                is_error: false,
                value: reply,
            })
        }
        other => Err((mcp::INVALID_PARAMS, format!("no such tool: {other}"))),
    }
}

/// A REST answer, relayed: status line >= 400 becomes the tool's `isError`, the
/// body — JSON (detail shapes included) — relays whole minus the transport echo.
/// Plain-text refusals (500s) get a `detail` mirror so the summary has the words.
async fn relay(response: Response) -> CallOutcome {
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
    let mut value = serde_json::from_slice::<Value>(&bytes)
        .unwrap_or_else(|_| json!({"detail": String::from_utf8_lossy(&bytes).to_string()}));
    if let Some(row) = value.as_object_mut() {
        row.remove("id");
    }
    CallOutcome {
        is_error: parts.status.as_u16() >= 400,
        value,
    }
}
