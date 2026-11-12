//! The HTTP /api/v1 wrapper — Rust port (`server/digup_server/api.py` as axum).
//!
//! Stage 4a: the skeleton — env config, the JSON-lines helper client
//! (`protocol.rs`), the one-shot CLI (`cli.rs`), and the four endpoints that
//! answer the searcher and the CLI's words: `GET /health`, `GET /status`,
//! `POST /search`, `POST /embed_ahead`. Tokens/security (4c), jobs + the writer
//! flock (4d), named targets (4e), and media handles (4b) land later, each with
//! its gate.
//!
//! The invariants this process must keep (`docs/implementation/README.md`,
//! AGENTS.md): exactly ONE server process — the media-id LRU and the job runner
//! live in it; helpers spawn as direct children (argv arrays, never shell
//! wrappers — ParentWatch kills children whose parent is PID 1); the engine's
//! numbers relay verbatim, never re-ranked, never invented; `/health` answers 503
//! while the searcher is mid-respawn, and the container's healthcheck counts 503
//! as alive on purpose.
//!
//! The log lines follow uvicorn's sequence (`INFO: …`, stdout) because the
//! deploy's stop/health gates read them: the graceful `docker stop` is measured
//! as exit **143** plus `Application shutdown complete` plus the running job
//! recorded *interrupted*.

// The 4a skeleton already implements the whole protocol surface (render slots,
// text/summaries/reload/raw ops, the index/estimate argv builders, send_line):
// the endpoints that use them land in stages 4b–4f, and their gates pin them.
#![allow(dead_code)]

mod cli;
mod config;
mod protocol;
mod rlock;
mod service;
mod slots;
mod validate;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use cli::CliError;
use config::{format_gf64, EngineConfig, ServerConfig};
use service::{CallError, SearchService};

/// `StatusCache` (api.py:235): `digup status` per call is real SQLite counting
/// work; the UI polls `/status` every 15 s and the healthcheck hits `/health`
/// every 30 s — the identity fields move slowly, so the polls stop costing a
/// subprocess each. Cached per index config (one target's counts must never be
/// served for another), TTL from `DIGUP_SERVER_STATUS_TTL_S`.
pub struct StatusCache {
    ttl: Duration,
    inner: Mutex<HashMap<String, (Instant, Value)>>,
}

impl StatusCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, cfg: &EngineConfig) -> Result<Value, CliError> {
        // Python's key is `str(config)` (the dataclass repr); any key with the
        // same granularity works: one entry per distinct engine+index+roots scope.
        let key = format!(
            "EngineConfig(index='{}', roots=({}), binary='{}', excluded=({}), excluded_ext=({}), models_dir=({}), encoder_idle={})",
            cfg.index.to_string_lossy(),
            cfg.roots.join(","),
            cfg.binary,
            cfg.excluded.join(","),
            cfg.excluded_extensions.join(","),
            cfg.models_dir
                .as_ref()
                .map(|d| format!("'{d}'", d = d.to_string_lossy()))
                .unwrap_or("None".into()),
            format_gf64(cfg.encoder_idle)
        );
        let mut map = self.inner.lock().expect("the status cache");
        let now = Instant::now();
        if let Some((stamp, data)) = map.get(&key) {
            // `now - stamp > ttl` refreshes; equality serves the cache (Python's `>`).
            if now.checked_duration_since(*stamp).map_or(true, |age| age <= self.ttl) {
                return Ok(data.clone());
            }
        }
        let data = cli::status(cfg, Duration::from_secs(60))?;
        map.insert(key, (Instant::now(), data.clone()));
        Ok(data)
    }
}

/// `MAIN_TARGET` (targets.py): the main media index is named `media` (reserved
/// — an app target may never take it), and `/status` publishes it.
pub const MAIN_TARGET: &str = "media";

/// The app state: the server config, the main target's searcher, the shared
/// status cache. Named targets (4e) grow per-scope maps here.
pub struct App {
    cfg: ServerConfig,
    main: SearchService,
    status: StatusCache,
}

impl App {
    fn new(cfg: ServerConfig) -> Self {
        let engine = cfg.engine();
        Self {
            main: SearchService::new(engine),
            status: StatusCache::new(Duration::from_secs_f64(cfg.status_ttl_s)),
            cfg,
        }
    }
}

type Shared = Arc<App>;

/// The API's error map (api.py:543-563): `EngineExited`/`EngineRefused` → 503,
/// `TimeoutError` → 504, `CLIError` → 502, `JobBusy` → 409 (4d); a FastAPI
/// `HTTPException` carries its `detail` string, and an unmapped exception is
/// `{"detail": "Internal Server Error"}` with 500.
pub enum ApiError {
    Engine(String),
    Refused(String),
    Timeout(String),
    Cli(String),
    Internal,
}

impl From<CallError> for ApiError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::Exited(exited) => ApiError::Engine(exited.to_string()),
            CallError::Refused(refused) => ApiError::Refused(refused.to_string()),
            CallError::Timeout(what) => ApiError::Timeout(what),
            CallError::Cli(what) => ApiError::Cli(what),
            CallError::Internal(_) => ApiError::Internal,
        }
    }
}

impl From<CliError> for ApiError {
    fn from(error: CliError) -> Self {
        ApiError::Cli(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // FastAPI's 500 for an unmapped exception is Starlette's plain-text
        // "Internal Server Error" (text/plain), NOT the JSON `detail` shape the
        // mapped HTTPException handlers produce.
        if matches!(self, ApiError::Internal) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                "Internal Server Error",
            )
                .into_response();
        }
        let (code, detail) = match self {
            ApiError::Engine(what) => (StatusCode::SERVICE_UNAVAILABLE, what),
            ApiError::Refused(what) => (StatusCode::SERVICE_UNAVAILABLE, what),
            ApiError::Timeout(what) => (StatusCode::GATEWAY_TIMEOUT, what),
            ApiError::Cli(what) => (StatusCode::BAD_GATEWAY, what),
            ApiError::Internal => unreachable!("handled above"),
        };
        (code, Json(json!({"detail": detail}))).into_response()
    }
}

/// `_strip` (api.py:148): drop the protocol `id`; keep every field the engine
/// produced, in its own words.
fn strip(reply: Value) -> Value {
    match reply {
        Value::Object(mut row) => {
            row.remove("id");
            Value::Object(row)
        }
        other => other,
    }
}

fn detail_response(errors: Vec<Value>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"detail": errors})),
    )
        .into_response()
}

/// `POST /api/v1/search` — ranking passes through untouched. Hits get the
/// server-issued `media_id` in stage 4b (the HitCache); for now the hits are the
/// engine's rows verbatim.
async fn search(State(app): State<Shared>, body: Bytes) -> Response {
    let value = if body.is_empty() {
        // FastAPI on a bodyless POST to a model endpoint: a missing `body`.
        return detail_response(vec![json!({
            "type": "missing", "loc": ["body"], "msg": "Field required"
        })]);
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) => value,
            // `json_invalid`'s loc carries the byte offset pydantic computed; the
            // shape is the point (the suite pins `detail` items, not offsets).
            Err(_) => {
                return detail_response(vec![json!({
                    "type": "json_invalid", "loc": ["body", 0], "msg": "JSON decode error"
                })])
            }
        }
    };
    let req = match validate::search(&value) {
        Ok(req) => req,
        Err(errors) => return detail_response(errors),
    };
    let task = {
        let app = app.clone();
        tokio::task::spawn_blocking(move || {
            let reply = app
                .main
                .search(
                    &req.q,
                    req.kinds,
                    req.limit,
                    req.code,
                    req.meaning,
                    req.prefix_last,
                )?;
            // `SearchReply` is a pydantic model: the declared fields are part of
            // the published shape — hits/strong/meaning/ms (a missing one is the
            // model's ValidationError in Python: 500) and `warning` (null when
            // the engine had none).
            let mut row = match strip(reply) {
                Value::Object(row) => row,
                _ => return Err(ApiError::Internal),
            };
            for field in ["hits", "strong", "meaning", "ms"] {
                if !row.contains_key(field) {
                    return Err(ApiError::Internal);
                }
            }
            row.entry("warning".to_string()).or_insert(Value::Null);
            Ok::<_, ApiError>(Value::Object(row))
        })
    };
    match task.await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
}

/// `POST /api/v1/embed_ahead` — the typist's warm-up: the engine embeds and
/// caches the text so far, so the pause's vector is ready for the next search.
/// Plain passthrough (`_strip`), like the Python endpoint.
async fn embed_ahead(State(app): State<Shared>, body: Bytes) -> Response {
    let value = if body.is_empty() {
        return detail_response(vec![json!({
            "type": "missing", "loc": ["body"], "msg": "Field required"
        })]);
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(value) => value,
            Err(_) => {
                return detail_response(vec![json!({
                    "type": "json_invalid", "loc": ["body", 0], "msg": "JSON decode error"
                })])
            }
        }
    };
    let req = match validate::embed_ahead(&value) {
        Ok(req) => req,
        Err(errors) => return detail_response(errors),
    };
    let task = {
        let app = app.clone();
        tokio::task::spawn_blocking(move || {
            let reply = app.main.embed_ahead(&req.q, req.code)?;
            Ok::<_, ApiError>(strip(reply))
        })
    };
    match task.await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
}

/// `GET /api/v1/health` — engine identity (the cached CLI's words) plus the
/// searcher's live counts. 503 with the engine's own sentence means the searcher
/// is mid-respawn: the container is healthy; the healthcheck counts 503 as alive.
async fn health(State(app): State<Shared>) -> Response {
    let task = {
        let app = app.clone();
        tokio::task::spawn_blocking(move || {
            let live = strip(app.main.status()?);
            let identity = app.status.get(&app.cfg.engine())?;
            Ok::<_, ApiError>(json!({
                "ok": true,
                "engine": {
                    "fingerprints": identity["fingerprints"],
                    "build_tag": identity["build_tag"],
                    "vectors_version": identity["vectors_version"],
                    "index": identity["index"],
                    "index_size": identity["index_size"],
                },
                "live": live,
            }))
        })
    };
    match task.await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
}

/// `GET /api/v1/status` — counts: the searcher's live + the CLI's per-kind
/// table, plus the deployment's own roots published for the UI's info panel
/// (never client-supplied: showing them is not path acceptance).
async fn status_endpoint(State(app): State<Shared>) -> Response {
    let task = {
        let app = app.clone();
        tokio::task::spawn_blocking(move || {
            let live = strip(app.main.status()?);
            let cli = app.status.get(&app.cfg.engine())?;
            let mut row = match live {
                Value::Object(row) => row,
                _ => Map::new(),
            };
            row.insert("target".into(), json!(MAIN_TARGET));
            for field in [
                "kinds",
                "fingerprints",
                "build_tag",
                "vectors_version",
                "index_size",
            ] {
                row.insert(field.into(), cli.get(field).cloned().unwrap_or(Value::Null));
            }
            row.insert(
                "roots".into(),
                Value::Array(app.cfg.roots.iter().map(|root| json!(root)).collect()),
            );
            Ok::<_, ApiError>(Value::Object(row))
        })
    };
    match task.await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
}

/// SIGTERM/SIGINT: uvicorn's sequence — drain connections, run the lifespan's
/// shutdown (`quit()` ends the helper tree), and the process ends BY THE
/// SIGNAL: uvicorn re-raises it after the graceful part, so the shell/docker
/// report 143 on `docker stop` (the measured graceful outcome with the compose
/// 90 s grace) and 130 on Ctrl-C.
async fn shutdown_signal(signal: Arc<std::sync::atomic::AtomicI32>) {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("SIGINT handler");
    let sig = tokio::select! {
        _ = terminate.recv() => libc::SIGTERM,
        _ = interrupt.recv() => libc::SIGINT,
    };
    // axum's `with_graceful_shutdown` needs `Future<Output = ()>`: the signal
    // number travels through the shared cell to main, which re-raises it there.
    signal.store(sig, std::sync::atomic::Ordering::SeqCst);
    println!("INFO: Shutting down");
}

#[tokio::main]
async fn main() -> ExitCode {
    let cfg = ServerConfig::from_env();
    let app: Shared = Arc::new(App::new(cfg));
    println!("INFO: Started server process [{}]", std::process::id());
    println!("INFO: Waiting for application startup.");
    // The lifespan's startup prewarm: the engine's `prewarm` op opens the vectors
    // and warms the searcher; startup must not die when it fails (the searcher
    // respawns on demand).
    if app.cfg.prewarm {
        if let Err(error) = app.main.prewarm() {
            eprintln!("prewarm: {error}");
        }
    }
    println!("INFO: Application startup complete.");
    let addr = format!("{}:{}", app.cfg.host, app.cfg.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("cannot bind {addr}: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("INFO: Uvicorn running on http://{addr} (Press CTRL+C to quit)");
    let router = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/status", get(status_endpoint))
        .route("/api/v1/search", post(search))
        .route("/api/v1/embed_ahead", post(embed_ahead))
        .with_state(app.clone());
    let signal = Arc::new(std::sync::atomic::AtomicI32::new(0));
    let served = {
        let signal = signal.clone();
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal(signal))
            .await
    };
    // The lifespan shutdown: `service.quit()` ends the helper tree (ParentWatch
    // takes any stragglers with it). Named targets join here when 4e lands.
    println!("INFO: Waiting for application shutdown.");
    app.main.quit();
    println!("INFO: Application shutdown complete.");
    println!("INFO: Finished server process [{}]", std::process::id());
    if let Err(error) = served {
        eprintln!("server: {error}");
        return ExitCode::FAILURE;
    }
    // The measured graceful stop: `docker stop` with the compose 90 s grace
    // leaves 143 (128+SIGTERM) and the job recorded *interrupted*. uvicorn
    // re-raises the signal after its graceful part — the process ends BY it,
    // which is what makes 143/130 the exit statuses shells report.
    let sig = signal.load(std::sync::atomic::Ordering::SeqCst);
    if sig != 0 {
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
        // raise() not landing would be a bug; the fallback keeps the status honest.
        std::process::exit(128 + sig);
    }
    ExitCode::SUCCESS
}
