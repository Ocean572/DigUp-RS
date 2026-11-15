//! The HTTP /api/v1 wrapper — Rust port (`server/digup_server/api.py` as axum).
//!
//! Stage 4 (b–e): the full endpoint surface — health, status, search,
//! embed_ahead, media preview/text/file, estimate, index + jobs, targets,
//! tokens — with the middleware stack the FastAPI wrapper runs
//! (PrincipalLog → Cors → HostOriginGuard → BearerAuth → routes), the
//! server-issued media handles (`HitCache`), the flock-guarded job runner,
//! the minted-key/token surface, and the UI mount. Each piece mirrors its
//! Python counterpart's measured shape; the contract suite (4f) runs against
//! this process.
//!
//! The invariants this process must keep (README §5/§6/§7, AGENTS.md):
//! exactly ONE server process — the media-id LRU and the job runners live in
//! it; helpers spawn as direct children (argv arrays, never shell wrappers —
//! ParentWatch kills children whose parent is PID 1); the engine's numbers
//! relay verbatim, never re-ranked, never invented; media endpoints accept
//! ONLY server-issued ids, never client paths; `/health` answers 503 while
//! the searcher is mid-respawn, and the container's healthcheck counts 503
//! as alive on purpose; one writer per index (the flock beside it), one
//! estimate server-wide.
//!
//! The log lines follow uvicorn's sequence (`INFO: …`, stdout) because the
//! deploy's stop/health gates read them: the graceful `docker stop` is
//! measured as exit **143** plus `Application shutdown complete` plus the
//! running job recorded *interrupted*. uvicorn ends the process BY the
//! signal (re-raise after the graceful part) — so the shell reports 143/130
//! and a Python subprocess reports -15.

// The stage-4 surface implements the whole protocol ahead of its gates
// (render slots, the raw passthrough, the index argv builders, send_line):
// their contract tests land with the 4f suite.
#![allow(dead_code)]

mod cli;
mod config;
mod jobs;
mod media;
mod protocol;
mod rlock;
mod security;
mod service;
mod slots;
mod targets;
mod validate;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Extension, Request, State};
use axum::http::header::{
    HeaderName, HeaderValue, ACCESS_CONTROL_ALLOW_ORIGIN, CACHE_CONTROL, CONTENT_TYPE, VARY,
    WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use cli::CliError;
use config::{format_gf64, EngineConfig, ServerConfig};
use jobs::{JobBusy, JobRunner};
use media::{HitCache, HitEntry, INLINE_KINDS};
use security::{Auth, AuthState};
use service::{CallError, SearchService};
use targets::{Target, MAIN_TARGET};

/// `StatusCache` (api.py:234): `digup status` per call is real SQLite counting
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
            if now
                .checked_duration_since(*stamp)
                .is_none_or(|age| age <= self.ttl)
            {
                return Ok(data.clone());
            }
        }
        let data = cli::status(cfg, Duration::from_secs(60))?;
        map.insert(key, (Instant::now(), data.clone()));
        Ok(data.clone())
    }
}

/// `Scope` (api.py:355): one target's supervised machinery — its engine
/// config, its search-service child, its job runner (own flock beside its
/// own index, own jobs log), sharing the deployment's single status cache.
pub struct Scope {
    cfg: EngineConfig,
    service: Arc<SearchService>,
    jobs: Arc<JobRunner>,
}

/// The app state (api.py's closure): the deployment config, the main
/// target's searcher and job runner (the lifespan owns them), the registry
/// of named indexes, the media-handle LRU, the live Auth (rotation swaps
/// the value in place), lazily built scopes, and the one-estimate gate.
pub struct App {
    cfg: ServerConfig,
    main: Arc<SearchService>,
    main_jobs: Arc<JobRunner>,
    status: StatusCache,
    registry: RwLock<targets::Registry>,
    hit_cache: HitCache,
    auth: Arc<AuthState>,
    scopes: Mutex<BTreeMap<String, Arc<Scope>>>,
    scope_build: Mutex<()>,
    estimate_busy: AtomicBool,
    allowed_hosts: Option<Vec<String>>,
    allowed_origins: Option<Vec<String>>,
    /// The UI mount (Stage 6): `DIGUP_SERVER_UI`, else the payload-adjacent
    /// bundle (`<exe dir>/ui/dist`, install-relative like Python's
    /// `Path(__file__).parent.parent/ui/dist`) when it carries an
    /// `index.html`. A DEFAULT with no bundle is the silent skip Python
    /// answers (no mount, API only); an explicit value with no index.html
    /// is a loud startup error, not a silent 404 page.
    ui: Option<PathBuf>,
}

/// The default UI bundle, install-relative the way Python spells it —
/// `Path(__file__).resolve().parent.parent / "ui" / "dist"` (api.py:936)
/// resolves the bundle beside the installed package. The running binary's
/// own folder is the Rust payload's install location: the Dockerfile's
/// runtime stage copies the built dist into the payload FOLDER, beside
/// `digup-server` (`/opt/digup/ui/dist`), so the default finds the bundle
/// the payload ships. The audit-base baked `env!("CARGO_MANIFEST_DIR")`
/// into the binary — the BUILD tree's path: copied to any other machine
/// the default named a tree that is not there (the silent-skip shape,
/// a bundle the payload ships that never mounts), and on the build
/// machine it mounted the dev tree's dist — a host artifact the payload
/// does not ship and docker ignores (the Dockerfile ships only the
/// frontend-builder's dist).
fn default_ui_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ui").join("dist")))
}

/// `create_app`'s UI mount (api.py:935-941) with the reference's three answers:
/// an EXPLICIT `DIGUP_SERVER_UI` with an `index.html` mounts it; an explicit
/// value with none refuses startup with the reference's FileNotFoundError text
/// (relayed byte for byte); NO knob answers the default, and a default with no
/// bundle is the SILENT skip (no mount, API only — Python's `if/elif` with no
/// else: the mount line is simply skipped).
fn ui_mount(cfg_ui: Option<&PathBuf>) -> Result<Option<PathBuf>, String> {
    match cfg_ui {
        Some(dir) => {
            if dir.join("index.html").is_file() {
                Ok(Some(dir.clone()))
            } else {
                Err(format!(
                    "DIGUP_SERVER_UI={dir} has no index.html — serve the built UI: cd server/ui && npm run build",
                    dir = dir.display()
                ))
            }
        }
        None => Ok(default_ui_dir().filter(|dir| dir.join("index.html").is_file())),
    }
}

type Shared = Arc<App>;

impl App {
    /// The lifespan's startup (api.py:484): the main searcher spawns as a
    /// direct child of this process, the registry and minted keys are built
    /// (startup dies loudly on a malformed target or keys file — a config
    /// error is an operator fact, not a suggestion), the prewarm runs
    /// best-effort, and the main job runner (log replay included) starts.
    fn new(cfg: ServerConfig) -> Result<Shared, String> {
        let engine = cfg.engine();
        let registry =
            targets::build_registry(&cfg.roots, &cfg.index.to_string_lossy(), &cfg.config)?;
        let app_target_names: Vec<String> = registry
            .iter()
            .filter(|target| !target.main)
            .map(|target| target.name.clone())
            .collect();
        let auth = security::mint_tokens(&cfg.config, &app_target_names, cfg.mint_keys)?;
        let auth = Arc::new(RwLock::new(auth));
        // The UI mount, checked at config time like Python's create_app.
        let ui = ui_mount(cfg.ui.as_ref())?;
        let main = Arc::new(SearchService::new(engine.clone()));
        main.start().map_err(|error| format!("{error}"))?;
        if cfg.prewarm {
            // api.py:492-496 catches (EngineExited, EngineRefused, TimeoutError)
            // ONLY: the searcher respawns on demand and startup must not die on
            // those. A CLIError - the engine binary failing to spawn (search.py:
            // 150-152) - is NOT caught: the lifespan raises and uvicorn exits.
            // The audit-base swallowed every CallError, booting a server whose
            // engine binary has vanished.
            if let Err(error) = main.prewarm() {
                match &error {
                    service::CallError::Exited(_)
                    | service::CallError::Refused(_)
                    | service::CallError::Timeout(_) => eprintln!("prewarm: {error}"),
                    _ => return Err(format!("prewarm: {error}")),
                }
            }
        }
        // The durable job-log folder: JobRunner.__init__ does os.makedirs inside
        // the lifespan (jobs.py:86), so the reference DIES here (measured:
        // FileNotFoundError + "Application startup failed. Exiting.") and the log
        // replay, the interrupted-on-stop contract and the job history live on it.
        // The audit-base swallowed the failure: the stack booted healthy and lost
        // its durable facts in silence.
        std::fs::create_dir_all(&cfg.config)
            .map_err(|error| format!("{}: {error}", cfg.config.display()))?;
        let jobs = Arc::new(JobRunner::new(
            engine.clone(),
            &cfg.config,
            jobs::JOB_LOG,
            Some(jobs::after_done_for(main.clone())),
            cfg.backfill,
            cfg.job_timeout_s,
            cfg.job_log_size,
        ));
        let app = Arc::new(App {
            status: StatusCache::new(Duration::from_secs_f64(cfg.status_ttl_s)),
            hit_cache: HitCache::new(1024),
            scope_build: Mutex::new(()),
            estimate_busy: AtomicBool::new(false),
            allowed_hosts: security::allowed_hosts_from_env(),
            allowed_origins: security::allowed_origins_from_env(),
            registry: RwLock::new(registry),
            scopes: Mutex::new(BTreeMap::new()),
            ui,
            main,
            main_jobs: jobs,
            auth,
            cfg,
        });
        // `scopes[MAIN_TARGET]` lands with the lifespan (api.py:504).
        app.scopes.lock().expect("the scopes").insert(
            MAIN_TARGET.to_string(),
            Arc::new(Scope {
                cfg: engine,
                service: app.main.clone(),
                jobs: app.main_jobs.clone(),
            }),
        );
        Ok(app)
    }

    /// `scope_for` (api.py:382): the main scope always exists; app targets
    /// spawn on first use (searcher prewarmed best-effort — a preview call
    /// must not die here), their own runner, the shared status cache. One
    /// spawn per target, ever (the `scope_build` lock rides the check).
    fn scope_for(&self, name: &str) -> Result<Arc<Scope>, ApiError> {
        if let Some(scope) = self.scopes.lock().expect("the scopes").get(name) {
            return Ok(scope.clone());
        }
        let target = self
            .registry
            .read()
            .expect("the registry")
            .get(name)
            .cloned()
            .ok_or_else(|| {
                ApiError::Http(
                    StatusCode::NOT_FOUND,
                    format!(
                        "no such index target {} — see /api/v1/targets",
                        targets::py_repr(name)
                    ),
                )
            })?;
        let _guard = self.scope_build.lock().expect("the scope builder");
        if let Some(scope) = self.scopes.lock().expect("the scopes").get(name) {
            return Ok(scope.clone());
        }
        let cfg = EngineConfig {
            index: PathBuf::from(&target.index),
            roots: target.roots.clone(),
            binary: self.cfg.binary.clone(),
            excluded: self.cfg.excluded.clone(),
            excluded_extensions: self.cfg.excluded_extensions.clone(),
            models_dir: self.cfg.models_dir.clone(),
            encoder_idle: self.cfg.encoder_idle,
        };
        // os.makedirs(target.index, exist_ok=True) in scope_for: a failure raises
        // through the lifespan (same class as the job-log folder above).
        std::fs::create_dir_all(&cfg.index).map_err(|error| {
            // os.makedirs(target.index, exist_ok=True) in scope_for: a failure raises
            // through the lifespan (same class as the job-log folder above) - and
            // scope_for is called from request paths too, where an uncaught OSError
            // is Starlette's plain 500.
            eprintln!("target {name}: {}: {error}", cfg.index.display());
            ApiError::Internal
        })?;
        let service = Arc::new(SearchService::new(cfg.clone()));
        service.start()?;
        if let Err(error) = service.prewarm() {
            match &error {
                service::CallError::Exited(_)
                | service::CallError::Refused(_)
                | service::CallError::Timeout(_) => eprintln!("prewarm({name}): {error}"),
                // api.py:413-416 catches the same three: a CLIError from a target's
                // searcher spawn is an uncaught exception - 500 on a request path.
                _ => {
                    eprintln!("prewarm({name}): {error}");
                    return Err(ApiError::Internal);
                }
            }
        }
        let runner = Arc::new(JobRunner::new(
            cfg.clone(),
            &self.cfg.config,
            &format!("jobs-{name}.jsonl"),
            Some(jobs::after_done_for(service.clone())),
            self.cfg.backfill,
            self.cfg.job_timeout_s,
            self.cfg.job_log_size,
        ));
        let scope = Arc::new(Scope {
            cfg,
            service,
            jobs: runner,
        });
        self.scopes
            .lock()
            .expect("the scopes")
            .insert(name.to_string(), scope.clone());
        Ok(scope)
    }

    /// `target_counts` (api.py:463): counts for the `/targets` listing, all
    /// from the engine and WITHOUT spawning anything for a GET: a target
    /// whose searcher is already running answers with its live status (the
    /// searcher is the target's own engine, the authority on a store it has
    /// open); an unspawned target reads the read-only CLI status; mid-reload
    /// refusals fall back to it. An index folder with no store file has
    /// nothing to report (null) — creating a store is never a GET's business.
    fn target_counts(&self, target: &Target) -> Result<(Value, Value), ApiError> {
        let store = Path::new(&target.index).join("index.sqlite");
        if !Path::new(&target.index).is_dir() || !store.exists() {
            return Ok((Value::Null, Value::Null));
        }
        let scope = self
            .scopes
            .lock()
            .expect("the scopes")
            .get(&target.name)
            .cloned();
        if let Some(scope) = scope {
            match scope.service.status() {
                Ok(live) => {
                    let live = strip(live);
                    return Ok((
                        live.get("files").cloned().unwrap_or(Value::Null),
                        live.get("searchable").cloned().unwrap_or(Value::Null),
                    ));
                }
                // mid reload→prewarm: the CLI's words, below.
                Err(CallError::Exited(_))
                | Err(CallError::Refused(_))
                | Err(CallError::Timeout(_)) => {}
                Err(other) => return Err(other.into()),
            }
        }
        let cfg = EngineConfig {
            index: PathBuf::from(&target.index),
            roots: target.roots.clone(),
            binary: self.cfg.binary.clone(),
            excluded: Vec::new(),
            excluded_extensions: Vec::new(),
            models_dir: self.cfg.models_dir.clone(),
            encoder_idle: config::DEFAULT_ENCODER_IDLE,
        };
        match self.status.get(&cfg) {
            Ok(facts) => {
                let files = facts
                    .get("kinds")
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|row| row.get("files").and_then(Value::as_i64))
                            .sum::<i64>()
                    })
                    .unwrap_or(0);
                Ok((
                    json!(files),
                    facts.get("vectors").cloned().unwrap_or(Value::Null),
                ))
            }
            // EngineExited/EngineRefused/TimeoutError/FileNotFoundError/OSError
            // (Python's tuple): nothing to report, honestly.
            Err(_) => Ok((Value::Null, Value::Null)),
        }
    }

    /// The lifespan shutdown (api.py:507): the main runner first (a running
    /// job is recorded *interrupted* — the truthful state), then the main
    /// searcher, then every app scope's runner and searcher.
    fn shutdown(&self) {
        self.main_jobs.shutdown();
        self.main.quit();
        let scopes: Vec<(String, Arc<Scope>)> = self
            .scopes
            .lock()
            .expect("the scopes")
            .iter()
            .map(|(name, scope)| (name.clone(), scope.clone()))
            .collect();
        for (name, scope) in scopes {
            if name != MAIN_TARGET {
                scope.jobs.shutdown();
                scope.service.quit();
            }
        }
    }
}

/// `target_for_path` (api.py:421): which target owns a media path — the
/// longest matching root wins; the registry is read live (registrations
/// land at runtime).
fn target_for_path(app: &App, path: &str) -> String {
    let registry = app.registry.read().expect("the registry");
    targets::target_for_path(&registry, path)
}

/// The request's target (`digup.target`, default the main media index —
/// the bearer gate binds it, see `security.BearerAuth`).
fn request_target(ctx: &Ctx) -> String {
    ctx.target.clone()
}
// ── request extensions (Python's `scope["digup.principal"]` / `["digup.target"]`) ──

/// The principal the bearer gate accepted (`read`, a map name, `admin`,
/// `admin (read scope)`, `app:<target>`, `none`), the target the request
/// addresses, and the CORS mark (`digup.cors_allowed`). Defaults: the gate
/// leaves these unset when auth is off — endpoints read them with the
/// Python defaults (`""` / MAIN_TARGET).
#[derive(Clone, Default)]
pub struct Ctx {
    principal: String,
    target: String,
    cors_allowed: bool,
}

// ── the error map (api.py's exception handlers + HTTPException detail) ──────────

/// Engine supervision facts become HTTP facts, quoted, not dressed up:
/// `EngineExited`/`EngineRefused` → 503, `TimeoutError` → 504, `CLIError` →
/// 502, `JobBusy` → 409. A FastAPI `HTTPException` carries its `detail`
/// string (the `Http` variant — endpoints raise these directly); an
/// unmapped exception is Starlette's plain-text "Internal Server Error"
/// with 500 (measured: text/plain, NOT the JSON `detail` shape).
#[derive(Debug)]
pub enum ApiError {
    Engine(String),
    Refused(String),
    Timeout(String),
    Cli(String),
    Busy(String),
    Http(StatusCode, String),
    Internal,
}

impl ApiError {
    fn http(status: StatusCode, detail: impl Into<String>) -> ApiError {
        ApiError::Http(status, detail.into())
    }
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

impl From<JobBusy> for ApiError {
    fn from(busy: JobBusy) -> Self {
        ApiError::Busy(busy.to_string())
    }
}

impl From<protocol::ProtocolError> for ApiError {
    fn from(error: protocol::ProtocolError) -> Self {
        CallError::from(error).into()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if matches!(self, ApiError::Internal) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                "Internal Server Error",
            )
                .into_response();
        }
        let (code, detail) = match self {
            ApiError::Engine(what) => (StatusCode::SERVICE_UNAVAILABLE, what),
            ApiError::Refused(what) => (StatusCode::SERVICE_UNAVAILABLE, what),
            ApiError::Timeout(what) => (StatusCode::GATEWAY_TIMEOUT, what),
            ApiError::Cli(what) => (StatusCode::BAD_GATEWAY, what),
            ApiError::Busy(what) => (StatusCode::CONFLICT, what),
            ApiError::Http(code, detail) => (code, detail),
            ApiError::Internal => unreachable!("handled above"),
        };
        (code, Json(json!({"detail": detail}))).into_response()
    }
}

/// `_strip` (api.py:148): drop the protocol `id`; keep every field the
/// engine produced, in its own words.
fn strip(reply: Value) -> Value {
    match reply {
        Value::Object(mut row) => {
            row.remove("id");
            Value::Object(row)
        }
        other => other,
    }
}

/// FastAPI's 422 shape: `{"detail": [ {type, loc, msg, input, ctx?}, … ] }`.
fn detail_response(errors: Vec<Value>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"detail": errors})),
    )
        .into_response()
}

/// `_refuse` (security.py:133): one JSON body with a `detail` string — the
/// shape `docs/API.md` documents for middleware refusals; a 401 carries
/// `www-authenticate: Bearer`.
fn refuse(status: StatusCode, detail: String) -> Response {
    let mut parts = (status, Json(json!({"detail": detail}))).into_response();
    // FastAPI's JSONResponse answers content-type `application/json`
    // (no charset) — axum's Json says the same.
    parts
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if status == StatusCode::UNAUTHORIZED {
        parts
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    parts
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

// ── file serving: Starlette's FileResponse shape, measured (probe-fastapi.log) ──
//
// etag: quoted MD5 hex over `f"{mtime}-{size}"` — the mtime is Python's
// float repr (`1731029259.0`, NOT `1731029259`: Rust's `{}` drops the
// `.0`, so `py_float_repr` restores it); last-modified: RFC 1123 GMT.
// Conditional GET (`if-none-match` exact-equal, `if-modified-since`
// `last_modified <= imds`) → 304. Ranges: single range → 206 chunked
// (no content-length) with content-range + accept-ranges; unsatisfiable →
// 416 `bytes */{size}`. `head` is a HEAD request (headers, empty body).

fn py_float_repr(value: f64) -> String {
    let text = format!("{value}");
    if !text.contains('.') && !text.contains('e') && !text.contains("inf") && !text.contains("NaN")
    {
        // Python prints integral floats with the `.0` suffix.
        format!("{text}.0")
    } else {
        text
    }
}

fn etag_of(mtime: f64, size: u64) -> String {
    let digest = md5::compute(format!(
        "{repr}-{size}",
        repr = py_float_repr(mtime),
        size = size
    ));
    format!("\"{:x}\"", digest)
}

/// `urllib`-decoded path for static lookup (`%20` rides as a space).
fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&path[at + 1..at + 3], 16) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Starlette StaticFiles' content-type: `mimetypes.guess_type` with
/// `utf-8` appended for every `text/*` (an unknown suffix guesses
/// text/plain — the `.map` sourcemaps ride as text/plain too).
fn static_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/vnd.microsoft.icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "map" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        _ => "text/plain; charset=utf-8",
    }
}

/// `Response` from a status, a header list, and a body — the tuple
/// `IntoResponse` impls cap at fixed-size arrays, and the file answers
/// build header lists.
fn status_with_headers(
    status: StatusCode,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Body,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    for (name, value) in headers {
        response.headers_mut().insert(name, value);
    }
    response
}

fn header(name: &str, value: &str) -> (HeaderName, HeaderValue) {
    (
        HeaderName::from_lowercase(name.to_lowercase().as_bytes()).expect("header name"),
        HeaderValue::try_from(value).expect("header value"),
    )
}

/// Starlette's `FileResponse._parse_range_header` + `_parse_ranges` (the
/// shipped version's full grammar — measured shape-for-shape 2026-11-13):
/// a header the app's capped `parse_range` did not answer goes through this,
/// and its refusals are plain-text 400s, not a silent full file. More than
/// one merged range is Starlette's multipart answer; API.md documents
/// "single ranges only", so the port serves the whole file there (the
/// accepted deviation, recorded in the port notes).
enum StarFull {
    Full,
    BadRequest(&'static str),
    Unsatisfiable,
    /// (start, end) INCLUSIVE — Starlette's tuples are end-exclusive.
    Range(i64, i64),
}

fn starlette_full(header: Option<&str>, size: i64) -> StarFull {
    let Some(raw) = header else {
        return StarFull::Full;
    };
    let Some((units, spec)) = raw.split_once('=') else {
        return StarFull::BadRequest("Malformed range header.");
    };
    if units.trim().to_lowercase() != "bytes" {
        return StarFull::BadRequest("Only support bytes range");
    }
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() > 100 {
        return StarFull::Full; // Starlette's max_ranges: `[]` → the simple answer
    }
    // Python's ints are unbounded (measured: `bytes=0-18446744073709551616`
    // answers 206 bytes 0-36/37), so the numbers ride i128 and clamp — a
    // 20-digit end is not a parse failure, it is the end of the file.
    let size = size as i128;
    let mut ranges: Vec<(i128, i128)> = Vec::new(); // (start, end EXCLUSIVE)
    for part in parts {
        let part = part.trim();
        if part.is_empty() || part == "-" {
            continue; // an empty part asks for nothing
        }
        let Some((start_s, end_s)) = part.split_once('-') else {
            continue; // no dash: not a range
        };
        let (start_s, end_s) = (start_s.trim(), end_s.trim());
        let start = if !start_s.is_empty() {
            match start_s.parse::<i128>() {
                Ok(value) => value,
                Err(_) => continue,
            }
        } else {
            match end_s.parse::<i128>() {
                Ok(length) => (size - length).max(0),
                Err(_) => continue,
            }
        };
        let end = if !start_s.is_empty() && !end_s.is_empty() {
            match end_s.parse::<i128>() {
                Ok(value) if value < size => value + 1,
                Ok(_) => size,
                Err(_) => continue,
            }
        } else {
            size
        };
        ranges.push((start, end));
    }
    if ranges.is_empty() {
        return StarFull::BadRequest("Range header: range must be requested");
    }
    if ranges
        .iter()
        .any(|(start, _)| !(0 <= *start && *start < size))
    {
        return StarFull::Unsatisfiable;
    }
    if ranges.iter().any(|(start, end)| start >= end) {
        return StarFull::BadRequest("Range header: start must be less than end");
    }
    if ranges.len() > 1 {
        ranges.sort();
        let mut merged: Vec<(i128, i128)> = vec![ranges[0]];
        for (start, end) in ranges[1..].iter() {
            let last = merged.last_mut().expect("non-empty");
            if *start <= last.1 {
                last.1 = last.1.max(*end);
            } else {
                merged.push((*start, *end));
            }
        }
        ranges = merged;
    }
    if ranges.len() == 1 {
        let (start, end) = ranges[0];
        // Both fit: the checks above put the range inside the file (0 <= start
        // < size, end <= size, size is an i64 file length).
        let start = i64::try_from(start).expect("inside the file");
        let end = i64::try_from(end - 1).expect("inside the file");
        return StarFull::Range(start, end);
    }
    StarFull::Full // Starlette's multipart: API.md's "single ranges only"
}

/// The shared file answer: `headers` are the endpoint's own
/// (content-disposition/nosniff for `/file`, cache-control for the UI),
/// Starlette contributes etag/last-modified/content-length, conditionals
/// answer 304, ranges answer 206 (chunked) / 416. `cap_span` is api.py's
/// 8 MiB server cap (the media `/file` endpoint); the mount/preview use
/// Starlette's uncapped parse.
/// The shape a file answer rides (one struct keeps `file_response` inside
/// clippy's argument budget): `head` — headers-only answer; `cap_span` —
/// the media endpoint's own capped `parse_range` arm (8 MiB); `live_size`
/// — the LIVE HANDLE's fstat the app-span layer prints (api.py:739; None:
/// FileResponse stats fresh); `mount` — the StaticFiles flag where the
/// conditional 304 lives (starlette 1.10: FileResponse has no conditional
/// check at all).
#[derive(Clone, Copy)]
struct FileOptions {
    head: bool,
    cap_span: bool,
    live_size: Option<i64>,
    mount: bool,
}

fn file_response(
    path: &Path,
    media_type: &str,
    headers: Vec<(HeaderName, HeaderValue)>,
    request_headers: &HeaderMap,
    opts: FileOptions,
) -> Response {
    let FileOptions {
        head,
        cap_span,
        live_size,
        mount,
    } = opts;
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                "Not Found",
            )
                .into_response()
        }
    };
    let size = meta.len() as i64;
    // The app-span layer's size rides the live handle's fstat (api.py:739:
    // `os.fstat(handle.fileno()).st_size` feeds `parse_range`, and the 206
    // Content-Range and the 416 `bytes */{size}` print THAT number). The
    // starlette layer (400/416/206/200 of FileResponse) rides FileResponse's
    // own `os.stat` — the port's `fs::metadata`. A replacement between the
    // handle's check and the stream is the reference's own window: the
    // headers ride the handle, the bytes ride the fresh open (api.py:760's
    // `ranged()` opens the path anew). The audit-base ran the stat's number
    // through both layers: measured seam probe (20-byte file, live handle
    // 15): `bytes=0-9` answers `bytes 0-9/15` in the reference,
    // `bytes 0-9/20` at the audit-base; `bytes=10-20` answers 416
    // `bytes */15` in the reference, 206 `bytes 10-19/20` at the audit-base.
    let app_size = live_size.unwrap_or(size);
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let etag = etag_of(mtime, meta.len());
    let last_modified =
        httpdate::fmt_http_date(std::time::UNIX_EPOCH + Duration::from_secs(mtime as u64));
    let mut extra = headers.clone();
    // Starlette's FileResponse carries `accept-ranges: bytes` on every answer.
    extra.push(header("accept-ranges", "bytes"));
    extra.push(header("etag", &etag));
    extra.push(header("last-modified", &last_modified));

    let range_header = request_headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    // Conditional GET: the 304 lives in StaticFiles, NOT in FileResponse
    // (starlette 1.10, measured in the pinned site-packages): responses.py
    // has no conditional check at all — `if-none-match`/`if-modified-since`
    // are consulted only by `staticfiles.py:214-228` (`is_not_modified`),
    // and its 304 keeps ONLY the NOT_MODIFIED_HEADERS (cache-control,
    // content-location, date, etag, expires, vary — 304 drops last-modified
    // and content-length; the audit-base echoed the FULL header set). The
    // media `/file` and `/preview` endpoints answer FileResponse/StreamingResponse:
    // `If-None-Match` on them is a NORMAL 200/206 (the audit-base 304'd —
    // a video element's `Range`+`If-None-Match` revalidation is exactly this
    // request, and Python serves the span). StaticFiles' comparison is
    // WEAK: `*` matches; every comma-part of the list, stripped and with
    // the `W/` prefix removed, is compared to the response's etag.
    let not_modified = |extra: &[(HeaderName, HeaderValue)]| -> Response {
        const KEEP: [&str; 6] = [
            "cache-control",
            "content-location",
            "date",
            "etag",
            "expires",
            "vary",
        ];
        let kept: Vec<(HeaderName, HeaderValue)> = extra
            .iter()
            .filter(|(name, _)| KEEP.contains(&name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        status_with_headers(StatusCode::NOT_MODIFIED, kept, Body::empty())
    };
    if mount {
        // `is_not_modified` (staticfiles.py:211-228) DECIDES on the
        // PRESENCE of `if-none-match`: a header that does not match
        // returns False from the etag branch — `if-modified-since` is
        // never consulted behind a non-matching etag list (the audit-base
        // ran the two checks in sequence and 304'd on the imds leg
        // behind an unmatched inm). The comparison is WEAK: `*`
        // matches; every comma-part, stripped, with the `W/` prefix
        // removed (`str.removeprefix`, head-only, once), is compared to
        // the response's own etag.
        if let Some(inm) = request_headers.get("if-none-match") {
            let hits = match inm.to_str() {
                Ok(value) => {
                    let value = value.trim();
                    value == "*"
                        || value.split(',').any(|tag| {
                            let tag = tag.trim();
                            let tag = tag.strip_prefix("W/").unwrap_or(tag);
                            tag == etag
                        })
                }
                // A non-ASCII list is a string starlette compares too;
                // it can never equal the ASCII etag: presence decides,
                // answer is "modified".
                Err(_) => false,
            };
            if hits {
                return not_modified(&extra);
            }
        } else if let Some(imds) = request_headers
            .get("if-modified-since")
            .and_then(|v| v.to_str().ok())
            .and_then(|value| httpdate::parse_http_date(value).ok())
        {
            let file_time = std::time::UNIX_EPOCH + Duration::from_secs(mtime.floor() as u64);
            if file_time <= imds {
                return not_modified(&extra);
            }
        }
    }
    // The two layers, measured header-for-header on 2026-11-13:
    //  * api.py's capped `parse_range` answers its own 206 (StreamingResponse:
    //    chunked, NO content-length, NO etag/last-modified — the endpoint's
    //    headers only) and its 416 (content-range + accept-ranges + length 0);
    //  * what it leaves (Span::None) lands on Starlette's FileResponse, whose
    //    single range answers 206 WITH content-length and the stat headers,
    //    whose refusals are plain-text 400s, and whose 416 is the
    //    PlainTextResponse shape (content-range + length 0, no accept-ranges).
    let app_span = if cap_span {
        media::parse_range(range_header.as_deref(), app_size)
    } else {
        media::Span::None
    };

    if let media::Span::Range(start, end) = app_span {
        // 206 chunked (no content-length) — a 20 GB clip streams.
        use std::io::Seek;
        let Ok(mut file) = std::fs::File::open(path) else {
            return ApiError::Internal.into_response();
        };
        if file.seek(std::io::SeekFrom::Start(start as u64)).is_err() {
            return ApiError::Internal.into_response();
        }
        // Chunked at 1 MiB pieces: a media element's seek answer must not carry a
        // content-length (Starlette's StreamingResponse), and the span STREAMS piece by
        // piece — `ranged()`'s generator (`media::span_stream`, api.py:761-768), one read
        // per piece. The audit-base buffered the capped 8 MiB span in a `vec![0u8; span]`
        // and served it from memory, reading it with ONE `read`: a short read (davfs2,
        // FUSE, network mounts) truncated the body below its own Content-Range.
        let body = Body::from_stream(media::span_stream(file, (end - start + 1) as u64));
        // The app's own header list (no stat headers: `extra` rode the File
        // path only); accept-ranges rides with the endpoint's headers.
        // The endpoint's header dict carries accept-ranges once (api.py:747-751;
        // `dict` assignment cannot duplicate a key); HeaderMap needs a replace,
        // not an append — the audit-base appended a second `accept-ranges`.
        let mut headers = headers;
        // The endpoint's header list ALREADY carries accept-ranges (it
        // rides the Python dict at api.py:747). The audit-base appended a
        // second tuple; `status_with_headers` inserts each pair (dict
        // semantics), so the second was a REPLACE — no wire change, shape
        // hygiene only. This arm adds only content-range and the media
        // type: the push order lands on Python's dict order (disposition,
        // ranges, nosniff, range) plus the media type appended.
        headers.push(header(
            "content-range",
            &format!("bytes {start}-{end}/{size}", end = end, size = app_size),
        ));
        headers.push(header("content-type", media_type));
        return status_with_headers(StatusCode::PARTIAL_CONTENT, headers, body);
    }

    match if matches!(app_span, media::Span::Unsatisfiable) {
        StarFull::Unsatisfiable
    } else {
        starlette_full(range_header.as_deref(), size)
    } {
        StarFull::BadRequest(why) => (
            StatusCode::BAD_REQUEST,
            [
                header("content-type", "text/plain; charset=utf-8"),
                header("content-length", &why.len().to_string()),
            ],
            Body::from(why.to_string()),
        )
            .into_response(),
        StarFull::Unsatisfiable if cap_span => (
            // api.py's own 416: content-range + accept-ranges, empty body.
            // The number is the live handle's fstat (the `size` of
            // `f"bytes */{size}"` at api.py:754 — parse_range refused on
            // the SAME handle-fed number, the pair must agree).
            StatusCode::RANGE_NOT_SATISFIABLE,
            [
                header("content-range", &format!("bytes */{app_size}")),
                header("accept-ranges", "bytes"),
                header("content-length", "0"),
            ],
            Body::empty(),
        )
            .into_response(),
        StarFull::Unsatisfiable => (
            // Starlette's PlainTextResponse(416): the range, length 0, plain.
            StatusCode::RANGE_NOT_SATISFIABLE,
            [
                header("content-range", &format!("bytes */{size}")),
                header("content-length", "0"),
                header("content-type", "text/plain; charset=utf-8"),
            ],
            Body::empty(),
        )
            .into_response(),
        StarFull::Range(start, end) => {
            // FileResponse's single range: the span, WITH content-length and
            // the stat headers (measured: the media endpoint's FileResponse
            // path and the static mount answer the same shape). The span STREAMS
            // (`media::span_stream`): the uncapped path takes `bytes=0-<size-1>` of a
            // 20 GB clip — the audit-base's `vec![0u8; span]` + `read_exact` made ONE
            // request allocate and fill the whole file in RAM (a memory amplification
            // the wrapper never has: FileResponse seeks and iterates). With
            // content-length riding, the piece sizes are wire-invisible; the span is
            // read piece by piece, the answer bytes identical.
            use std::io::Seek;
            let Ok(mut file) = std::fs::File::open(path) else {
                return ApiError::Internal.into_response();
            };
            if file.seek(std::io::SeekFrom::Start(start as u64)).is_err() {
                return ApiError::Internal.into_response();
            }
            let span = (end - start + 1) as usize;
            extra.push(header("content-length", &span.to_string()));
            extra.push(header("content-type", media_type));
            extra.push(header(
                "content-range",
                &format!("bytes {start}-{end}/{size}", end = end),
            ));
            let body = if head {
                Body::empty()
            } else {
                Body::from_stream(media::span_stream(file, span as u64))
            };
            status_with_headers(StatusCode::PARTIAL_CONTENT, extra, body)
        }
        StarFull::Full => {
            // Shadow: the built header list (endpoint headers + etag +
            // last-modified) carries the 200 answer, content-length rides.
            let mut extra = extra;
            extra.push(header("content-length", &meta.len().to_string()));
            extra.push(header("content-type", media_type));
            let body = if head {
                Body::empty()
            } else {
                match std::fs::File::open(path) {
                    Ok(file) => Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
                        tokio::fs::File::from_std(file),
                        1 << 16,
                    )),
                    Err(_) => {
                        return (
                            StatusCode::NOT_FOUND,
                            [(CONTENT_TYPE, "text/plain; charset=utf-8")],
                            "Not Found",
                        )
                            .into_response()
                    }
                }
            };
            status_with_headers(StatusCode::OK, extra, body)
        }
    }
}
// ── the middleware stack (api.py:537-540, one layer per function) ──────────
//
// FastAPI wraps middleware OUTERSMOST-last-added, giving the stack
// PrincipalLog → Cors → HostOriginGuard → BearerAuth → routes. The same
// sequence runs here in one `from_fn_with_state` layer: the audit line
// sees every guarded request and its accepted principal, CORS answers the
// widget preflights and marks allowlisted origins for the guard, the
// guard refuses a wrong Host before authentication, and the bearer gate
// compares tokens. All knobs are inert when unset (README §7, stage-12).

fn state_changing(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn hhmmss() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// The audit line (`security.PrincipalLog`): one line per guarded API
/// request, naming the principal the bearer gate accepted and — when the
/// gate resolved one — the target it addressed.
fn audit_line(method: &str, path: &str, status: u16, principal: &str, target: Option<&str>) {
    println!(
        "{ts} {method} {path} -> {status} principal={principal}{target}",
        ts = hhmmss(),
        target = target.map(|t| format!(" target={t}")).unwrap_or_default(),
    );
}

/// `query_target(scope)`: the target a request explicitly addresses
/// (`?target=name`); None when it does not (the FIRST occurrence wins,
/// like Python's `parse_qs(...)[0]`).
fn query_target(uri: &Uri) -> Option<String> {
    query_param(uri, "target")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The guard's view of the request path: uvicorn decodes `scope["path"]`
/// (percent-decoding, h11_impl's "PERFORM PERCENT DECODING"), the middlewares `rstrip
///("/")` it (security.py:474, :563). Decode, then strip — the wrapper's order. The
/// guard, the OPEN_PATHS/ADMIN_ROUTES tests, and the audit line all see THIS string.
fn guard_path_of(raw_path: &str) -> String {
    percent_decode(raw_path).trim_end_matches('/').to_string()
}

async fn api_gate(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let method_str = method.as_str().to_string();
    let uri = req.uri().clone();
    let raw_path = uri.path().to_string();
    // Python's path for the guard, the OPEN_PATHS/ADMIN_ROUTES tests, and the audit
    // line is uvicorn's PERCENT-DECODED request target, then `rstrip("/")`
    // (security.py:474 BearerAuth, :563 PrincipalLog; uvicorn decodes scope["path"]).
    // axum's `uri.path()` is the RAW target: decode, then strip — the wrapper's order.
    // A client that encodes its way in slipped past the raw-form checks the audit-base
    // ran: `/%61pi/v1/search` (decoded /api/v1/search) is guarded + audited by Python
    // and was neither here (unguarded: a token-guarded search answered key-free), and
    // `POST /api/v1/index%2F` (decoded /api/v1/index/) is an ADMIN route for Python
    // (admin demanded) while the raw form matched no ADMIN_ROUTES entry — a reader
    // token reached the index-queue endpoint. Routing stays on the raw target (axum's
    // matchit): the deviation the mount's note records (Python's router answers the
    // decoded path, incl. its 307 slash-redirect; here an encoded path answers 404).
    let guard_path = guard_path_of(&raw_path);
    let guarded =
        guard_path.starts_with("/api/v1") && !security::OPEN_PATHS.contains(&guard_path.as_str());
    let headers = req.headers().clone();

    let origin_raw = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Cors normalizes: strip().lower().rstrip("/") (exact origins).
    let origin_norm = origin_raw
        .trim()
        .to_lowercase()
        .trim_end_matches('/')
        .to_string();
    let cors_origin = app.allowed_origins.as_ref().and_then(|allowed| {
        if !origin_norm.is_empty() && allowed.iter().any(|o| o == &origin_norm) {
            Some(origin_norm.clone())
        } else {
            None
        }
    });
    // `scope["digup.cors_allowed"]` — the guard's cross-origin exception.
    let cors_allowed = cors_origin.is_some();

    let mut ctx = Ctx {
        principal: String::new(),
        target: MAIN_TARGET.to_string(),
        cors_allowed,
    };
    let mut target_set = false;

    let finish = |response: Response, ctx: &Ctx, target_set: bool| -> Response {
        let mut response = response;
        if guarded {
            audit_line(
                &method_str,
                &guard_path,
                response.status().as_u16(),
                if ctx.principal.is_empty() {
                    "none"
                } else {
                    &ctx.principal
                },
                if target_set {
                    Some(ctx.target.as_str())
                } else {
                    None
                },
            );
        }
        if let Some(origin) = &cors_origin {
            // `_decorate`: ACAO + Vary on every response from an allowlisted origin.
            if let Ok(value) = HeaderValue::try_from(origin) {
                response
                    .headers_mut()
                    .insert(ACCESS_CONTROL_ALLOW_ORIGIN, value);
            }
            response
                .headers_mut()
                .insert(VARY, HeaderValue::from_static("Origin"));
        }
        response
    };
    // Cors: a preflight from an allowlisted origin answers 204 and never
    // reaches the inner app; the audit line (outermost) sees the 204.
    if let Some(origin) = cors_origin.clone() {
        if method == Method::OPTIONS {
            let response = (
                StatusCode::NO_CONTENT,
                [
                    (
                        ACCESS_CONTROL_ALLOW_ORIGIN,
                        HeaderValue::try_from(origin).expect("origin header"),
                    ),
                    header("access-control-allow-methods", "POST, GET, OPTIONS"),
                    header(
                        "access-control-allow-headers",
                        "authorization, content-type",
                    ),
                    header("access-control-max-age", "600"),
                    (VARY, HeaderValue::from_static("Origin")),
                    (axum::http::header::CONTENT_LENGTH, HeaderValue::from(0u64)),
                ],
                Body::empty(),
            )
                .into_response();
            return finish(response, &ctx, target_set);
        }
    }

    // HostOriginGuard: installed always, engaged when the operator names
    // the hosts this deployment serves (every HTTP request, UI included).
    if let Some(allowed) = &app.allowed_hosts {
        let host_raw = headers
            .get(axum::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_string());
        let host_disp = host_raw.clone().unwrap_or_else(|| "none".to_string());
        let host = host_raw
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !allowed.iter().any(|h| h == &host) {
            return finish(
                refuse(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "the Host header ({host_disp}) is not one of this server's allowed hosts"
                    ),
                ),
                &ctx,
                target_set,
            );
        }
        if !origin_raw.is_empty()
            && state_changing(&method)
            && !cors_allowed
            && security::origin_host(&origin_raw) != host
        {
            return finish(
                refuse(
                    StatusCode::FORBIDDEN,
                    format!(
                        "the Origin ({origin_raw}) is not this server's own origin; \
                     state-changing requests must come from the same host"
                    ),
                ),
                &ctx,
                target_set,
            );
        }
    }

    // BearerAuth: the token scopes (security.py:443), target binding
    // included. `admin` answers every target; `read`/plain named tokens
    // answer the main media index; an `app:<name>` token answers its own
    // target and nothing else; mutating routes (index/estimate) accept an
    // app token only for its own target.
    if guarded {
        let auth = app.auth.read().expect("the auth state").clone();
        if auth.enabled() {
            let admin_route = security::ADMIN_ROUTES
                .iter()
                .any(|(m, p)| *m == method_str && *p == guard_path);
            let open_reads = auth.read.is_none()
                && auth.readers.is_empty()
                && !(admin_route && auth.admin.is_some());
            if !open_reads {
                let supplied = security::bearer_of(&headers);
                let requested = query_target(&uri);
                match supplied {
                    None => {
                        // Minted-key deployment, no key presented: the main
                        // index answers its reads to the operator's own
                        // browser. Index/estimate and application targets
                        // keep needing keys.
                        if !admin_route
                            && !security::env_secrets(&auth)
                            && matches!(requested.as_deref(), None | Some("media"))
                        {
                            ctx.principal = "none".to_string();
                            ctx.target = MAIN_TARGET.to_string();
                            target_set = true;
                        } else {
                            return finish(
                                refuse(
                                    StatusCode::UNAUTHORIZED,
                                    "this server asks for a bearer token".to_string(),
                                ),
                                &ctx,
                                target_set,
                            );
                        }
                    }
                    Some(token) => {
                        let names = auth.token_names(&token);
                        if names.is_empty() {
                            return finish(
                                refuse(
                                    StatusCode::UNAUTHORIZED,
                                    "the bearer token is not this server's token".to_string(),
                                ),
                                &ctx,
                                target_set,
                            );
                        }
                        let app_target = names
                            .iter()
                            .find(|name| name.starts_with("app:"))
                            .map(|name| name[4..].to_string());
                        // An app-bound token addresses ITS OWN target by
                        // default; an explicit `?target=` it does not match
                        // is refused below.
                        let target = match (&app_target, &requested) {
                            (Some(own), None) => own.clone(),
                            (_, Some(wanted)) => wanted.clone(),
                            _ => MAIN_TARGET.to_string(),
                        };
                        let is_admin = names.iter().any(|name| name == "admin");
                        if target == MAIN_TARGET {
                            if app_target.is_some() {
                                return finish(refuse(
                                    StatusCode::FORBIDDEN,
                                    "that token answers the application's own index; the media \
                                     index needs this server's read or admin token"
                                        .to_string(),
                                ), &ctx, target_set);
                            }
                        } else if !(is_admin || app_target.as_deref() == Some(target.as_str())) {
                            return finish(
                                refuse(
                                    StatusCode::FORBIDDEN,
                                    format!(
                                        "target {} is answered by its own application token \
                                     (or the admin token) — not by this one",
                                        targets::py_repr(&target)
                                    ),
                                ),
                                &ctx,
                                target_set,
                            );
                        }
                        if admin_route && !is_admin {
                            // Mutating routes: admin always; an app token for
                            // its OWN target queues its own crawl.
                            if app_target.as_deref() == Some(target.as_str()) {
                                ctx.principal = format!("app:{}", app_target.expect("checked"));
                                ctx.target = target.clone();
                                target_set = true;
                            } else {
                                return finish(
                                    refuse(
                                        StatusCode::FORBIDDEN,
                                        "that is this server's read-scope token; indexing and \
                                     estimating need the admin token"
                                            .to_string(),
                                    ),
                                    &ctx,
                                    target_set,
                                );
                            }
                        } else {
                            ctx.target = target.clone();
                            target_set = true;
                            if admin_route {
                                ctx.principal = "admin".to_string();
                            } else if is_admin {
                                ctx.principal = "admin (read scope)".to_string();
                            } else {
                                ctx.principal = names[0].clone();
                            }
                        }
                    }
                }
            }
        }
    }

    // FastAPI's `@app.get` routes answer GET only, so a HEAD on an api path
    // falls past the partial match into the `/` mount's static lookup — and the
    // standing DEVIACTION for non-canonical targets lives here: Starlette matches the
    // DECODED path and 307-redirects a trailing slash, axum's matchit matches the RAW
    // target, so `/api/v1/health/` and `/%61pi/v1/search` land on the mount's 404
    // (the guard/audit in `api_gate` see the decoded form, as Python's do; the
    // contract suite pins the canonical forms, and nothing on the wire sends encoded
    // targets). A HEAD on an api path can only miss there (measured 2026-11-13:
    // HEAD `/api/v1/status` → 404,
    // HEAD `/api/v1/media/<id>/file` → 404, HEAD `/api/v1/health` → 404).
    // axum's GET filter answers HEAD itself, so the mount's answer is built
    // here — AFTER the bearer gate, so the audit line names the principal the
    // gate accepted, exactly like PrincipalLog.
    if method == Method::HEAD && guard_path.starts_with("/api/v1") {
        let mut req = req;
        req.extensions_mut().insert(ctx.clone());
        let response = json_response(StatusCode::NOT_FOUND, json!({"detail": "Not Found"}));
        return finish(response, &ctx, target_set);
    }

    let mut req = req;
    req.extensions_mut().insert(ctx.clone());
    let response = next.run(req).await;
    finish(response, &ctx, target_set)
}
// ── endpoints ────────────────────────────────────────────────────────────────

/// The body of a model endpoint: FastAPI's 422 shapes for a missing body and
/// a JSON decode error (`json_invalid`'s loc carries the byte offset
/// pydantic computed; the suite pins `detail` items, not offsets).
fn parse_body(body: &[u8]) -> Result<Value, Box<Response>> {
    if body.is_empty() {
        let missing = detail_response(vec![json!({
            "type": "missing",
            "loc": ["body"],
            "msg": "Field required"
        })]);
        return Err(Box::new(missing));
    }
    serde_json::from_slice::<Value>(body).map_err(|_| {
        let invalid = detail_response(vec![json!({
            "type": "json_invalid",
            "loc": ["body", 0],
            "msg": "JSON decode error"
        })]);
        Box::new(invalid)
    })
}

/// Run one request's engine work on the blocking pool (FastAPI runs its
/// sync endpoints in a threadpool; `scope_for` may spawn a child and wait
/// for its `ready` handshake, engine calls ride the service's RLock).
async fn in_scope<T>(
    app: Shared,
    target: String,
    f: impl FnOnce(&Arc<Scope>) -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let scope = app.scope_for(&target)?;
        f(&scope)
    })
    .await
    .map_err(|_| ApiError::Internal)?
}

/// `GET /api/v1/health` — engine identity (the cached CLI's words) plus the
/// searcher's live counts. The container's healthcheck reads the MAIN
/// target only; 503 with the engine's own sentence means the searcher is
/// mid-respawn: the container is healthy, the healthcheck counts 503 alive.
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

/// `GET /api/v1/status?target=name` — counts: the searcher's live + the
/// CLI's per-kind table, plus the target's name and the deployment's own
/// roots published for the UI's info panel (env-configured or
/// targets-registered, never client-supplied: showing them is not path
/// acceptance, §7).
async fn status_endpoint(State(app): State<Shared>, Extension(ctx): Extension<Ctx>) -> Response {
    let target = request_target(&ctx);
    let result = in_scope(app.clone(), target.clone(), move |scope| {
        let live = strip(scope.service.status()?);
        let cli = app.status.get(&scope.cfg)?;
        let mut row = match live {
            // Python splats the live status (`{**live, ...}` api.py:592): a
            // non-mapping is a TypeError -> 500. The audit-base answered a 200
            // built on an empty base row.
            Value::Object(row) => row,
            _ => return Err(ApiError::Internal),
        };
        row.insert("target".into(), json!(target));
        for field in [
            "kinds",
            "fingerprints",
            "build_tag",
            "vectors_version",
            "index_size",
        ] {
            // Python SUBSCRIPTS all five (`cli["kinds"]`, api.py:594-601): an
            // engine reply that omits one is a KeyError -> 500, measured on the
            // pinned fastapi/starlette. Relay the subscript, invent no null.
            row.insert(
                field.into(),
                cli.get(field).cloned().ok_or(ApiError::Internal)?,
            );
        }
        row.insert(
            "roots".into(),
            Value::Array(scope.cfg.roots.iter().map(|root| json!(root)).collect()),
        );
        Ok(Value::Object(row))
    })
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The cache entry for one hit, spelled as Python's subscripts (api.py:619-623):
/// `hit["path"]`, `hit["kind"]`, `hit["segment_id"]` are KEYED reads - a missing
/// key is a KeyError, a typed field of the wrong type is the model's Validation-
/// Error, and both are Starlette's plain 500 (measured on the pinned fastapi/
/// starlette, 2026-11-15). The audit-base defaulted them to ""/0 - a 200 whose
/// media_id then resolves to a WRONG row. `loc` is the one `.get()` (Python
/// `hit.get("loc")`: absent answers None).
fn hit_fields(hit: &Value, code: bool) -> Result<HitEntry, ApiError> {
    let row = hit.as_object().ok_or(ApiError::Internal)?;
    Ok(HitEntry {
        path: row
            .get("path")
            .and_then(Value::as_str)
            .ok_or(ApiError::Internal)?
            .to_string(),
        kind: row
            .get("kind")
            .and_then(Value::as_str)
            .ok_or(ApiError::Internal)?
            .to_string(),
        segment_id: row
            .get("segment_id")
            .and_then(Value::as_i64)
            .ok_or(ApiError::Internal)?,
        loc: row.get("loc").and_then(Value::as_f64),
        code,
    })
}

/// `POST /api/v1/search` — ranking passes through untouched. Each hit
/// carries `media_id` — the server's handle for `/media/{id}/…` (HitCache:
/// ids from the index, never paths from the client); every engine field
/// rides verbatim, `SearchReply`'s declared fields included (a missing
/// hits/strong/meaning/ms is the model's ValidationError in Python: 500;
/// `warning` is null when the engine had none).
async fn search(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    body: Bytes,
) -> Response {
    let value = match parse_body(&body) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let req = match validate::search(&value) {
        Ok(req) => req,
        Err(errors) => return detail_response(errors),
    };
    let target = request_target(&ctx);
    let result = in_scope(app.clone(), target, move |scope| {
        let reply = scope.service.search(
            &req.q,
            req.kinds,
            req.limit,
            req.code,
            req.meaning,
            req.prefix_last,
        )?;
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
        let hits: Vec<Value> = match row.get("hits").and_then(Value::as_array) {
            // `for hit in stripped["hits"]` (api.py:619): a hits value that is
            // not a list is the model's ValidationError on the way in - 500.
            Some(rows) => rows
                .iter()
                .map(|hit| {
                    let fields = hit_fields(hit, req.code)?;
                    // api.py:281 mints the media id with `uuid.uuid4()` inside
                    // the search loop: an os.urandom failure is an uncaught
                    // OSError — the plain 500 (the id mint is NOT a default
                    // path, F-11).
                    let media_id = app.hit_cache.put(fields).map_err(|_| ApiError::Internal)?;
                    // Python splats the hit itself ({**hit, "media_id": ...}):
                    // every engine field rides, nothing is defaulted.
                    let mut row = hit.as_object().ok_or(ApiError::Internal)?.clone();
                    row.insert("media_id".into(), json!(media_id));
                    Ok(Value::Object(row))
                })
                .collect::<Result<Vec<Value>, ApiError>>()?,
            None => return Err(ApiError::Internal),
        };
        row.insert("hits".into(), Value::Array(hits));
        Ok(Value::Object(row))
    })
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /api/v1/embed_ahead` — the typist's warm-up: the engine embeds and
/// caches the text so far, so the pause's vector is ready for the next
/// search. Plain passthrough (`_strip`), like the Python endpoint.
async fn embed_ahead(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    body: Bytes,
) -> Response {
    let value = match parse_body(&body) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let req = match validate::embed_ahead(&value) {
        Ok(req) => req,
        Err(errors) => return detail_response(errors),
    };
    let target = request_target(&ctx);
    let result = in_scope(app, target, move |scope| {
        let reply = scope.service.embed_ahead(&req.q, req.code)?;
        Ok(strip(reply))
    })
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /api/v1/estimate` — the engine's own forecast, from its own JSON
/// (`estimate --json`, one JSON line per root folder). ParentWatch rule: a
/// direct child of this process. One full-tree crawl at a time (the UI
/// retry click and a second tab must not stack crawls). The ceiling is the
/// operator's: `DIGUP_SERVER_ESTIMATE_TIMEOUT_S` (an expired walk answers
/// 502 quoting the engine's own timeout sentence; the engine keeps
/// walking).
async fn estimate(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    body: Bytes,
) -> Response {
    if let Err(errors) = validate::estimate_request(&body) {
        return detail_response(errors);
    }
    let target = request_target(&ctx);
    // `sc = scope_for(request_target(request))` FIRST (api.py:1516), the busy gate
    // after it — and BOTH on the blocking pool: FastAPI runs its sync endpoints in a
    // threadpool, `scope_for` on a cold target spawns the target's searcher and waits
    // for the `ready` handshake (20–30 s of engine warm, AGENTS.md), which must never
    // run on a tokio worker (the audit-base ran it there, stalling every request parked
    // on that worker). `in_scope` is the port's established shape (the media endpoints
    // ride it). The gate opens on every ending, inside the same task.
    let timeout = match crate::config::knob_float("DIGUP_SERVER_ESTIMATE_TIMEOUT_S", 120.0) {
        Ok(seconds) => Duration::from_secs_f64(seconds),
        // api.py:642 reads this knob PER REQUEST: a typo is an uncaught
        // ValueError in the handler - Starlette's plain 500, not a silent
        // default and not a boot death.
        Err(_) => return ApiError::Internal.into_response(),
    };
    let outcome = in_scope(app.clone(), target, move |scope| {
        if app
            .estimate_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(ApiError::http(
                StatusCode::CONFLICT,
                "an estimate is already running — the engine walks every folder once at a time",
            ));
        }
        let result = cli::estimate_lines(&scope.cfg, timeout)
            .map(|folders| json!({"folders": folders}))
            .map_err(ApiError::from);
        // `finally: estimate_busy.release()` — the gate opens on every ending.
        app.estimate_busy.store(false, Ordering::SeqCst);
        result
    })
    .await;
    match outcome {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /api/v1/index` (202) — an indexing job arrives here, runs
/// serialized under the adapter's flock, and a second request answers 409
/// with the live job's id. `prune: true` also retires index rows for files
/// no longer found under the roots (the engine's own `--prune`). Indexing
/// never starts by itself (README §5).
async fn index(State(app): State<Shared>, Extension(ctx): Extension<Ctx>, body: Bytes) -> Response {
    let prune = match validate::index_request(&body) {
        Ok(prune) => prune,
        Err(errors) => return detail_response(errors),
    };
    let target = request_target(&ctx);
    // `sc = scope_for(...)` then `sc.jobs.start(prune=prune)` (api.py) — on the
    // blocking pool (`in_scope`): a cold target's first index request pays the
    // searcher spawn, and `jobs.start` takes the flock + starts the runner's queue
    // (the flock wait is blocking IO, the same class as the searcher spawn).
    let outcome = in_scope(app, target, move |scope| {
        scope
            .jobs
            .start(prune)
            .map(|job| json!({"ok": true, "job": job}))
            .map_err(|error| match error {
                jobs::StartError::Busy(busy) => ApiError::from(busy),
                // The job id mint failed (uuid4 has no fallback, jobs.py:99):
                // an uncaught OSError on POST /index — the plain 500 (F-11).
                jobs::StartError::Random(why) => {
                    eprintln!("job id: {why}");
                    ApiError::Internal
                }
            })
    })
    .await;
    match outcome {
        Ok(value) => (StatusCode::ACCEPTED, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `GET /api/v1/jobs` — the target's job records, insertion order (the
/// in-memory state of `jobs.jsonl`'s story, replayed at boot).
async fn jobs_endpoint(State(app): State<Shared>, Extension(ctx): Extension<Ctx>) -> Response {
    let target = request_target(&ctx);
    // `scope_for` first (api.py:1575), on the blocking pool: the lazy supervision
    // spawns this target's searcher + runner on the first look (a runtime-registered
    // target sees its first GET /jobs here).
    let outcome = in_scope(app, target, move |scope| {
        Ok(json!({"jobs": scope.jobs.jobs()}))
    })
    .await;
    match outcome {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `GET /api/v1/jobs/{job_id}` — one job record (states
/// `queued/running/done/interrupted/busy`; a done job's counts are the
/// engine's events).
async fn job_show(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Response {
    let target = request_target(&ctx);
    // `scope_for(...).jobs.job(job_id)` (api.py), on the blocking pool.
    let outcome = in_scope(app, target, move |scope| {
        scope
            .jobs
            .job(&job_id)
            .map(|found| json!({"job": found}))
            .ok_or_else(|| ApiError::http(StatusCode::NOT_FOUND, format!("no job {job_id}")))
    })
    .await;
    match outcome {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `_media_scope_allows` (api.py:438): media handles resolve to the target
/// that owns the path. A principal with NO key touches the main index only
/// — the minted-shape opening is for the main index's reads; an
/// application's own index keeps needing its own key (or the admin's).
fn media_scope_allows(app: &App, ctx: &Ctx, path: &str) -> Result<String, ApiError> {
    let name = target_for_path(app, path);
    if (ctx.principal.is_empty() || ctx.principal == "none") && name != MAIN_TARGET {
        return Err(ApiError::http(
            StatusCode::FORBIDDEN,
            "that file belongs to an application's own index — its key answers it",
        ));
    }
    Ok(name)
}

fn no_media(media_id: &str) -> ApiError {
    ApiError::http(StatusCode::NOT_FOUND, format!("no media handle {media_id}"))
}

/// `GET /api/v1/media/{media_id}/preview?pixels=N` — the JPEGs the
/// engine's `render` wrote under `<index>/previews`, served only when the
/// reply's path resolves inside that folder. Previews are the pixels'
/// source: the type is `image/jpeg`, plain, from the engine's file.
async fn preview(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    headers: HeaderMap,
    method: Method,
    uri: Uri,
    axum::extract::Path(media_id): axum::extract::Path<String>,
) -> Response {
    // `pixels: int = Query(256, ge=32, le=2048)`.
    let pixels_raw = query_param(&uri, "pixels").unwrap_or_else(|| "256".to_string());
    let pixels = match validate::check_pixels(&pixels_raw) {
        Ok(pixels) => pixels,
        Err(errors) => return detail_response(errors),
    };
    let entry = match app.hit_cache.get(&media_id) {
        Some(entry) => entry,
        None => return no_media(&media_id).into_response(),
    };
    let owner = match media_scope_allows(&app, &ctx, &entry.path) {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    let result = in_scope(app.clone(), owner, move |scope| {
        let reply = scope
            .service
            .render(&entry.path, &entry.kind, entry.loc, pixels)?;
        let image = reply
            .get("image")
            .and_then(Value::as_str)
            .map(targets::resolve);
        let Some(image) = image else {
            // `reply["image"]` missing is Python's KeyError: unmapped → 500.
            return Err(ApiError::Internal);
        };
        let previews = targets::resolve(&format!("{}/previews", scope.cfg.index.to_string_lossy()));
        if !image.starts_with(&format!("{previews}/")) {
            return Err(ApiError::http(
                StatusCode::INTERNAL_SERVER_ERROR,
                "render answered outside previews",
            ));
        }
        Ok(image)
    })
    .await;
    let image = match result {
        Ok(image) => PathBuf::from(image),
        Err(error) => return error.into_response(),
    };
    file_response(
        &image,
        "image/jpeg",
        Vec::new(),
        &headers,
        FileOptions {
            head: method == Method::HEAD,
            cap_span: false,
            live_size: None, // FileResponse stats fresh; the preview has no handle
            mount: false,    // no conditional 304 on a FileResponse answer
        },
    )
}

/// `GET /api/v1/media/{media_id}/text` — the segment's text, the engine's
/// `text` op, `_strip`ed. Code hits answer their own segment (`code` rode
/// the search request that minted the handle).
async fn media_text(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    axum::extract::Path(media_id): axum::extract::Path<String>,
) -> Response {
    let entry = match app.hit_cache.get(&media_id) {
        Some(entry) => entry,
        None => return no_media(&media_id).into_response(),
    };
    let owner = match media_scope_allows(&app, &ctx, &entry.path) {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    let result = in_scope(app, owner, move |scope| {
        let reply = scope.service.text(entry.segment_id, entry.code)?;
        Ok(strip(reply))
    })
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `GET /api/v1/media/{media_id}/file` — the file itself, the desktop's
/// "Open" (Stage 10). Only a server-issued handle, and the path it names
/// must resolve inside the server's *configured* roots (the engine
/// produced it; the roots are the deployment's own, never a client's). The
/// type is read from the file's magic bytes; media kinds arrive inline so
/// the browser opens them, everything else as an attachment, code/doc as
/// text/plain — script is never marked script here. Ranges are honored
/// (206 + Content-Range, span-capped at 8 MiB): that is what lets
/// `<video …#t=loc>` seek, and "save as" download.
async fn media_file(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    headers: HeaderMap,
    method: Method,
    axum::extract::Path(media_id): axum::extract::Path<String>,
) -> Response {
    let entry = match app.hit_cache.get(&media_id) {
        Some(entry) => entry,
        None => return no_media(&media_id).into_response(),
    };
    let owner = match media_scope_allows(&app, &ctx, &entry.path) {
        Ok(owner) => owner,
        Err(error) => return error.into_response(),
    };
    // The target whose roots own it.
    let roots: Vec<String> = {
        let registry = app.registry.read().expect("the registry");
        registry
            .get(&owner)
            .map(|target| target.roots.clone())
            .unwrap_or_default()
    };
    let resolved = targets::resolve(&entry.path);
    let inside = roots
        .iter()
        .map(|root| targets::resolve(root))
        .any(|root| resolved == root || resolved.starts_with(&format!("{root}/")));
    if !inside {
        return ApiError::http(
            StatusCode::NOT_FOUND,
            "file is outside the server's media roots",
        )
        .into_response();
    }
    // The type, the size, the span — from one open (fstat of the live
    // handle: a replacement mid-request cannot make the headers lie about
    // the bytes). FileNotFoundError/IsADirectoryError/PermissionError all
    // become the same honest 404.
    let live_size = match std::fs::File::open(&resolved) {
        Ok(handle) => match handle.metadata() {
            Ok(meta) if meta.is_file() => meta.len() as i64,
            _ => {
                return ApiError::http(
                    StatusCode::NOT_FOUND,
                    "the file is gone (it left the folders)",
                )
                .into_response()
            }
        },
        Err(_) => {
            return ApiError::http(
                StatusCode::NOT_FOUND,
                "the file is gone (it left the folders)",
            )
            .into_response()
        }
    };
    // `size = os.fstat(handle.fileno()).st_size` — the SAME handle's number
    // parse_range, the 206 Content-Range and the 416 ride (api.py:739), read
    // WHILE the handle is open and dropped at the arm's end (Python's `with`
    // closes at the block end). It feeds file_response as `live_size`; the
    // starlette layer (a span the endpoint hands on) re-stats the path
    // itself, exactly as FileResponse re-stats in Python.
    let mut media_type = media::sniff_type(Path::new(&resolved));
    if media_type == "application/octet-stream" && (entry.kind == "code" || entry.kind == "doc") {
        media_type = "text/plain; charset=utf-8".to_string();
    }
    // RFC 5987: names with any characters, escaped.
    let name = media::quote_filename(
        Path::new(&resolved)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .as_str(),
    );
    let disposition = if INLINE_KINDS.contains(&entry.kind.as_str()) {
        "inline"
    } else {
        "attachment"
    };
    let owned_headers = vec![
        header(
            "content-disposition",
            &format!("{disposition}; filename*=UTF-8''{name}"),
        ),
        header("accept-ranges", "bytes"),
        // Belt for the text/plain path: the magic-bytes typing already
        // refuses to call script script; nosniff says the browser must not
        // guess.
        header("x-content-type-options", "nosniff"),
    ];
    file_response(
        Path::new(&resolved),
        &media_type,
        owned_headers,
        &headers,
        FileOptions {
            head: method == Method::HEAD,
            cap_span: true,
            live_size: Some(live_size),
            mount: false, // FileResponse/StreamingResponse: NO conditional 304
        },
    )
}

/// The first occurrence of a query param (FastAPI's `Query` on
/// `?pixels=1&pixels=2` keeps the first). The value decodes through
/// `query_decode` — the query layer's rule, shared with `query_target`
/// (one parser for both readers: pixels, target; FastAPI's `Query` and
/// api.py's `parse_qs` run the SAME `parse_qsl` machinery).
fn query_param(uri: &Uri, key: &str) -> Option<String> {
    let query = uri.query()?;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix(&format!("{key}=")) {
            return Some(query_decode(value));
        }
    }
    None
}

/// `urllib.parse.unquote_plus`, measured on CPython 3.12 (parse_qsl decodes
/// query VALUES through it): a literal `+` is a SPACE, then `%XX` runs —
/// `%2B` decodes to a literal `+` the space rule never sees. The PATH
/// decoder (`percent_decode`, uvicorn's h11_impl "PERFORM PERCENT DECODING")
/// keeps `+` standing: the two layers decode differently, and the request
/// target rides the path rule.
fn query_decode(value: &str) -> String {
    percent_decode(&value.replace('+', " "))
}
/// `_admin_scope` (api.py:854): the gate records an admin token on a read
/// route as `admin (read scope)` — this IS the admin scope's surface, so
/// both forms are accepted. In an admin-token-only deployment the gate
/// deliberately leaves reads OPEN, so the principal is `none` even when the
/// admin key is presented — compare the bearer itself: admin-scope surfaces
/// answer the admin key's owner.
fn admin_scope(app: &App, ctx: &Ctx, bearer: Option<&str>) -> Result<(), ApiError> {
    if ctx.principal == "admin" || ctx.principal == "admin (read scope)" {
        return Ok(());
    }
    let auth = app.auth.read().expect("the auth state");
    if let (Some(admin), Some(supplied)) = (auth.admin.as_deref(), bearer) {
        if !admin.is_empty() && security::tokens_match(admin, supplied) {
            return Ok(());
        }
    }
    Err(ApiError::http(
        StatusCode::FORBIDDEN,
        "the server's keys and application registrations answer the admin token only",
    ))
}

/// `_tokens_payload`: the keys the server MINTED are shown; environment-
/// configured secrets are reported as configured, never echoed (the same
/// rule the Nextcloud bridge lives by).
fn tokens_payload(auth: &Auth) -> Value {
    let apps: Vec<Value> = auth
        .readers
        .iter()
        .filter(|(name, _)| name.starts_with("app:"))
        .map(|(name, token)| {
            let generated = auth.minted.iter().any(|minted| minted == name);
            json!({
                "name": name[4..].to_string(),
                "token": if generated { Value::String(token.clone()) } else { Value::Null },
                "generated": generated,
            })
        })
        .collect();
    let mut payload = Map::new();
    payload.insert(
        "admin".into(),
        if auth.minted.iter().any(|minted| minted == "admin") {
            json!(auth.admin)
        } else {
            Value::Null
        },
    );
    payload.insert("admin_configured".into(), json!(auth.admin.is_some()));
    payload.insert("read_configured".into(), json!(auth.read.is_some()));
    payload.insert("apps".into(), Value::Array(apps));
    Value::Object(payload)
}

/// `GET /api/v1/targets` — the named indexes this server supervises. An
/// application token sees its own target only (the binding the bearer gate
/// enforces); read/admin tokens see the full registry, in the registry's
/// insertion order (main first). Counts come from the engine, without
/// spawning anything for a GET.
async fn targets_endpoint(State(app): State<Shared>, Extension(ctx): Extension<Ctx>) -> Response {
    let app_target = ctx
        .principal
        .strip_prefix("app:")
        .map(|name| name.to_string());
    let task = {
        let app = app.clone();
        tokio::task::spawn_blocking(move || {
            let registry = app.registry.read().expect("the registry");
            let mut rows = Vec::new();
            for target in registry.iter() {
                if let Some(own) = &app_target {
                    if target.name != *own {
                        continue;
                    }
                }
                let (files, searchable) = app.target_counts(target)?;
                rows.push(json!({
                    "name": target.name,
                    "roots": target.roots,
                    "index": target.index,
                    "main": target.main,
                    "files": files,
                    "searchable": searchable,
                }));
            }
            Ok::<_, ApiError>(rows)
        })
    };
    match task.await {
        Ok(Ok(rows)) => Json(json!({"targets": rows})).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => ApiError::Internal.into_response(),
    }
}

/// `POST /api/v1/targets` — an application registers at a RUNNING server
/// (stage 14): the standalone mints its API key at boot; the application
/// brings that key here with its name + folders, and the server mints its
/// PRIVATE key, registers its own index folder (default `<config>/index-
/// <name>`), and persists the registration (targets.json — a restart keeps
/// it). Validation is the startup validator: a runtime registration cannot
/// be sloppier than an environment one.
async fn register_target(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(error) = admin_scope(&app, &ctx, security::bearer_of(&headers).as_deref()) {
        return error.into_response();
    }
    let value = match parse_body(&body) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let req = match validate::target_register(&value) {
        Ok(req) => req,
        Err(errors) => return detail_response(errors),
    };
    let index = req.index.clone().unwrap_or_else(|| {
        app.cfg
            .config
            .join(format!("index-{}", req.name.trim().to_lowercase()))
            .to_string_lossy()
            .into_owned()
    });
    let target = match targets::validated(
        &req.name,
        &req.roots.join(":"),
        &index,
        Some(&app.cfg.index.to_string_lossy()),
    ) {
        Ok(target) => target,
        Err(why) => return ApiError::http(StatusCode::BAD_REQUEST, why).into_response(),
    };
    if app
        .registry
        .read()
        .expect("the registry")
        .get(&target.name)
        .is_some()
    {
        return ApiError::http(
            StatusCode::CONFLICT,
            format!(
                "target {} is already registered",
                targets::py_repr(&target.name)
            ),
        )
        .into_response();
    }
    app.registry
        .write()
        .expect("the registry")
        .insert(target.clone());

    // Mint the application's PRIVATE key (shown once, here). Env-bound
    // targets keep their env token; minted keys persist 0600.
    let key = format!("app:{}", target.name);
    let token = {
        let mut auth = app.auth.write().expect("the auth state");
        let existing = auth
            .readers
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, token)| token.clone());
        let token = match existing {
            Some(token) if auth.minted.iter().any(|minted| minted == &key) => token,
            _ => {
                // api.py: the mint is `secrets.token_urlsafe(24)` with no
                // fallback — an os.urandom failure on this request path is
                // an uncaught OSError, the plain 500 (F-11).
                let fresh = match security::urlsafe_token() {
                    Ok(fresh) => fresh,
                    Err(_) => return ApiError::Internal.into_response(),
                };
                if let Some(entry) = auth.readers.iter_mut().find(|(name, _)| *name == key) {
                    entry.1 = fresh.clone();
                } else {
                    auth.readers.push((key.clone(), fresh.clone()));
                }
                if !auth.minted.iter().any(|minted| minted == &key) {
                    auth.minted.push(key.clone());
                }
                fresh
            }
        };
        // The keys file ride-along: minting persists, a corrupt file is a
        // config fact (an unfixable write is the unmapped 500 it is in
        // Python).
        if security::write_tokens_file(&app.cfg.config.join(security::TOKENS_FILE), &auth).is_err()
        {
            return ApiError::Internal.into_response();
        }
        token
    };

    // Persist the registration (main is not a registration; a restart
    // rebuilds it from env).
    let persist: Map<String, Value> = {
        let registry = app.registry.read().expect("the registry");
        registry
            .iter()
            .filter(|target| !target.main)
            .map(|target| {
                (
                    target.name.clone(),
                    json!({"roots": target.roots, "index": target.index}),
                )
            })
            .collect()
    };
    let path = app.cfg.config.join("targets.json");
    // `json.dump(handle, indent=2)` — pretty, no trailing newline.
    let pretty = serde_json::to_string_pretty(&Value::Object(persist)).expect("json");
    if std::fs::write(&path, pretty).is_err() {
        return ApiError::Internal.into_response();
    }

    Json(json!({
        "name": target.name,
        "roots": target.roots,
        "index": target.index,
        "token": token,
        "generated": true,
    }))
    .into_response()
}

/// `GET /api/v1/tokens` — the keys the server minted itself, admin scope
/// only, EXCEPT the minted-shape opening (2026-11-08 brief round 6): a
/// server that MINTED its keys shows them to its own gear — the UI reads
/// this with no key in the browser, exactly as the main page searches with
/// no key. A deployment that TYPED its secrets never displays them
/// (`env_secrets`); an application's key never lists the server's keys.
async fn tokens_endpoint(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    headers: HeaderMap,
) -> Response {
    let principal = ctx.principal.clone();
    let auth = app.auth.read().expect("the auth state").clone();
    if (principal.is_empty() || principal == "none") && !security::env_secrets(&auth) {
        return Json(tokens_payload(&auth)).into_response();
    }
    if let Err(error) = admin_scope(&app, &ctx, security::bearer_of(&headers).as_deref()) {
        return error.into_response();
    }
    Json(tokens_payload(&auth)).into_response()
}

/// `POST /api/v1/tokens` — REGENERATE the minted keys (UI: Regenerate,
/// behind a confirm). Every minted value is freshly minted and persisted
/// 0600, the old values die immediately, and the bearer gate reads the
/// SAME live object — one swap lands everywhere. A deployment whose keys
/// all come from env answers 400: nothing to rotate.
async fn rotate_tokens(
    State(app): State<Shared>,
    Extension(ctx): Extension<Ctx>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = admin_scope(&app, &ctx, security::bearer_of(&headers).as_deref()) {
        return error.into_response();
    }
    let auth = app.auth.read().expect("the auth state").clone();
    let rotated = match security::rotate_minted(&auth, &app.cfg.config) {
        Ok(Some(rotated)) => rotated,
        Ok(None) => {
            return ApiError::http(
                StatusCode::BAD_REQUEST,
                "nothing to rotate — this server's keys are set in its environment, not minted",
            )
            .into_response()
        }
        Err(_) => return ApiError::Internal.into_response(),
    };
    *app.auth.write().expect("the auth state") = rotated.clone();
    let mut payload = match tokens_payload(&rotated) {
        Value::Object(payload) => payload,
        _ => Map::new(),
    };
    payload.insert("rotated".into(), json!(true));
    Json(Value::Object(payload)).into_response()
}

// ── the UI mount (Stage 6: same origin, so CORS is not needed at all) ────────
//
// FastAPI serves the BUILT frontend at `/` with a catch-all mount, and the
// measured edge shapes (probe-fastapi.log) fall out of that mount: GET/HEAD
// unmatched → JSON 404 `Not Found` (NO trailing-slash redirect — the
// `/api/v1/health/` probe answers plain 404); every non-GET/HEAD method
// partial-matches → JSON 405 `Method Not Allowed` + the sorted `allow`
// header. FastAPI's `@app.get` routes do NOT answer HEAD (measured: HEAD
// `/api/v1/status` → 404), so the Rust routes register GET only and HEAD
// lands on the mount. The bearer gate and the audit line already ran
// before any of this (api.py:537-540's stack).

/// Starlette's `StaticFiles.get_response` refusal for any method it does not
/// serve (measured, UI deployed — the mount FULL-matches everything, so this
/// is the ONLY 405 shape the deployment answers): `allow: GET, HEAD` with the
/// FastAPI JSON detail. OPTIONS lands here too (StaticFiles serves GET/HEAD
/// only).
fn static_405() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [header("allow", "GET, HEAD")],
        Json(json!({"detail": "Method Not Allowed"})),
    )
        .into_response()
}

/// The router's fallback — Starlette's `/` mount: GET/HEAD is the static
/// lookup (a miss is the router's JSON 404, "Not Found"); every other method
/// is StaticFiles' constant 405. It rides as every route's method fallback
/// too, because a method mismatch falls past the partial match to the mount
/// (measured 2026-11-13: PUT `/api/v1/search` answers `allow: GET, HEAD`, NOT
/// `POST` — the mount's full match beats the route's partial one).
async fn fallback(State(app): State<Shared>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let head = method == Method::HEAD;
    if method == Method::GET || head {
        if let Some(root) = &app.ui {
            if let Some(response) = static_file(root, &path, req.headers(), head) {
                return response;
            }
        }
        return json_response(StatusCode::NOT_FOUND, json!({"detail": "Not Found"}));
    }
    static_405()
}

/// `UIFiles(directory=ui, html=True)` + the `no-cache` decoration:
// cache-control: no-cache on EVERY file answer (etag in, 304 out — the
// measured Chrome lesson: without the header, reloads served a stylesheet
// the server no longer had; revalidation on every load is what makes an
// upgrade land).
fn static_file(root: &Path, url_path: &str, headers: &HeaderMap, head: bool) -> Option<Response> {
    let decoded = percent_decode(url_path);
    let mut target = root.to_path_buf();
    for part in decoded.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains('\0') {
            return None; // no traversal: the mount serves its own folder, ever
        }
        target.push(part);
    }
    let root_real = std::fs::canonicalize(root).ok()?;
    let real = std::fs::canonicalize(&target).ok()?;
    if !real.starts_with(&root_real) {
        return None;
    }
    let mut file = real;
    if file.is_dir() {
        // html=True: a directory answers its index.html.
        file.push("index.html");
    }
    if !file.is_file() {
        return None;
    }
    let media_type = static_type(&file);
    Some(file_response(
        &file,
        media_type,
        vec![(CACHE_CONTROL, HeaderValue::from_static("no-cache"))],
        headers,
        FileOptions {
            head,
            cap_span: false,
            live_size: None, // StaticFiles stats its own
            mount: true,     // the mount is where the conditional 304 lives
        },
    ))
}
// ── startup / shutdown (uvicorn's exact sequence) ────────────────────────────

/// SIGTERM/SIGINT: uvicorn's sequence — drain connections, run the lifespan's
/// shutdown (the runners record *interrupted*, `quit()` ends the helper
/// tree), and the process ends BY THE SIGNAL: uvicorn re-raises it after
/// the graceful part, so the shell/docker report 143 on `docker stop` (the
/// measured graceful outcome with the compose 90 s grace) and 130 on Ctrl-C.
async fn shutdown_signal(signal: Arc<AtomicI32>) {
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
    signal.store(sig, Ordering::SeqCst);
    println!("INFO: Shutting down");
}

fn get_only<H, T>(handler: H) -> MethodRouter<Shared>
where
    H: axum::handler::Handler<T, Shared>,
    T: 'static,
{
    // FastAPI's `@app.get` routes answer GET only — HEAD lands on the mount
    // (measured: HEAD `/api/v1/status` → 404), so `get()`'s automatic HEAD
    // is not what the contract asks for.
    axum::routing::MethodRouter::new()
        .on(MethodFilter::GET, handler)
        .fallback(fallback)
}

fn post_only<H, T>(handler: H) -> MethodRouter<Shared>
where
    H: axum::handler::Handler<T, Shared>,
    T: 'static,
{
    axum::routing::MethodRouter::new()
        .on(MethodFilter::POST, handler)
        .fallback(fallback)
}

#[tokio::main]
async fn main() -> ExitCode {
    let cfg = match ServerConfig::from_env() {
        Ok(cfg) => cfg,
        // config_from_env refuses the same facts (the empty-roots RuntimeError,
        // the DIGUP_SERVER_INDEX KeyError, the knob ValueErrors): the boot dies,
        // exit 1 - the uvicorn lifespan shape the port already uses at App::new.
        Err(why) => {
            eprintln!("Application startup failed. Exiting: {why}");
            return ExitCode::FAILURE;
        }
    };
    println!("INFO: Started server process [{}]", std::process::id());
    println!("INFO: Waiting for application startup.");
    // The lifespan + the config-time build (registry, minted keys, the UI
    // mount check): startup dies loudly on a malformed target, a corrupt
    // keys file, or a UI folder without index.html — a config error is an
    // operator fact, not a suggestion (uvicorn exits on a lifespan raise).
    let app: Shared = match App::new(cfg) {
        Ok(app) => app,
        Err(why) => {
            eprintln!("Application startup failed. Exiting: {why}");
            return ExitCode::FAILURE;
        }
    };
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
    // GET/HEAD/POST per path exactly as FastAPI declares them; every
    // method-mismatch lands on `api_method_fallback` (the measured 404-vs-
    // 405 shapes), and unmatched paths on `fallback` (the UI mount). The
    // gate layer wraps EVERYTHING — refusals, 404s and 405s are audited
    // and decorated like Python's stack does them.
    let router = Router::new()
        .route("/api/v1/health", get_only(health))
        .route("/api/v1/status", get_only(status_endpoint))
        .route("/api/v1/search", post_only(search))
        .route("/api/v1/embed_ahead", post_only(embed_ahead))
        .route("/api/v1/estimate", post_only(estimate))
        .route("/api/v1/index", post_only(index))
        .route("/api/v1/jobs", get_only(jobs_endpoint))
        .route("/api/v1/jobs/{job_id}", get_only(job_show))
        .route("/api/v1/media/{media_id}/preview", get_only(preview))
        .route("/api/v1/media/{media_id}/text", get_only(media_text))
        .route("/api/v1/media/{media_id}/file", get_only(media_file))
        .route(
            "/api/v1/targets",
            get_only(targets_endpoint).post(register_target),
        )
        .route(
            "/api/v1/tokens",
            get_only(tokens_endpoint).post(rotate_tokens),
        )
        .fallback(fallback)
        .layer(axum::middleware::from_fn_with_state(app.clone(), api_gate))
        .with_state(app.clone());
    let signal = Arc::new(AtomicI32::new(0));
    let served = {
        let signal = signal.clone();
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal(signal))
            .await
    };
    // The lifespan shutdown: runners first (a running job records
    // *interrupted* with its partial counts — the honest state the stop
    // gate reads), then the searcher trees, main target then app scopes.
    println!("INFO: Waiting for application shutdown.");
    app.shutdown();
    println!("INFO: Application shutdown complete.");
    println!("INFO: Finished server process [{}]", std::process::id());
    if let Err(error) = served {
        eprintln!("server: {error}");
        return ExitCode::FAILURE;
    }
    // The measured graceful stop: `docker stop` with the compose 90 s grace
    // leaves 143 (128+SIGTERM) plus the audit trail. uvicorn re-raises the
    // signal after its graceful part — the process ends BY it, which is
    // what makes 143/130 the exit statuses shells report.
    let sig = signal.load(Ordering::SeqCst);
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

#[cfg(test)]
mod starlette_range_tests {
    use super::{starlette_full, StarFull};

    fn shape(header: &str, size: i64) -> String {
        match starlette_full(Some(header), size) {
            StarFull::Full => "full".into(),
            StarFull::BadRequest(why) => format!("400 {why}"),
            StarFull::Unsatisfiable => "416".into(),
            StarFull::Range(start, end) => format!("206 {start}-{end}"),
        }
    }

    #[test]
    fn the_matrix_measured_against_the_shipped_stack() {
        // uvicorn+httptools+Starlette, measured 2026-11-13 on a 37-byte file;
        // the answers are the media `/file` endpoint's FileResponse path and
        // the static mount's — same grammar, same refusals.
        let cases = [
            ("bytes=0-99", "206 0-36"),
            ("bytes= 0-10", "206 0-10"),
            ("bytes=1-2,3", "206 1-2"),
            ("bytes=abc", "400 Range header: range must be requested"),
            ("bytes=", "400 Range header: range must be requested"),
            ("bytes=-", "400 Range header: range must be requested"),
            (
                "bytes=0-10;append=1",
                "400 Range header: range must be requested",
            ),
            ("items=0-10", "400 Only support bytes range"),
            ("rangeish", "400 Malformed range header."),
            ("", "400 Malformed range header."),
            ("bytes=5-2", "400 Range header: start must be less than end"),
            ("bytes=-0", "416"),
            ("bytes=100000000-", "416"),
            ("bytes=-5", "206 32-36"),
            ("bytes=0-0", "206 0-0"),
            ("bytes=10-", "206 10-36"),
            ("bytes=000-009", "206 0-9"),
            ("bytes=0-18446744073709551616", "206 0-36"),
            // API.md documents "single ranges only": the merged pair is
            // Starlette's multipart answer, the port serves the whole file.
            ("bytes=0-10,20-30", "full"),
            ("bytes=0-10,10-20", "206 0-20"),
        ];
        for (header, want) in cases {
            assert_eq!(shape(header, 37), want, "range {header:?}");
        }
        assert_eq!(shape("bytes=0-9", 2223), "206 0-9");
    }
}

#[cfg(test)]
mod tests {
    use super::guard_path_of;

    /// The wrapper decodes before it strips (uvicorn's `scope["path"]`, then
    /// `rstrip("/")`): an encoded prefix is `/api/v1` for the guard — GUARDED and
    /// audited, and an encoded trailing slash strips after decoding, so
    /// `/api/v1/health%2F` is an OPEN path and `POST /api/v1/index%2F` is an ADMIN
    /// route. The audit-base ran both tests on the raw form: `/api/v1` slipped past
    /// the raw `starts_with` (a guarded search answered key-free, unaudited), and
    /// `%2F` never stripped (an admin route matched no ADMIN_ROUTES entry — a
    /// reader token reached the index-queue endpoint).
    #[test]
    fn the_guard_sees_the_decoded_stripped_path() {
        assert_eq!(guard_path_of("/api/v1/search"), "/api/v1/search");
        assert_eq!(guard_path_of("/api/v1/health///"), "/api/v1/health");
        assert_eq!(guard_path_of("/%61pi/v1/search"), "/api/v1/search");
        assert_eq!(guard_path_of("/api/v1/index%2F"), "/api/v1/index");
        assert_eq!(guard_path_of("/api/v1/health%2F"), "/api/v1/health");
    }

    #[test]
    fn hit_subscripts_answer_500_where_python_raises() {
        // F-09: the cache entry rides Python's keyed reads (api.py:619-623).
        use super::{hit_fields, ApiError};
        // A hit row missing path/kind/segment_id, carrying them as the wrong
        // type, or not an object at all is a 500 there - the audit-base
        // answered 200 with ""/0 defaults, caching a media_id that resolves
        // to a WRONG row (segment 0).
        use serde_json::json;
        let good = json!({"path": "/a/b.jpg", "kind": "photo", "segment_id": 3, "loc": 1.5});
        let entry = hit_fields(&good, false).unwrap();
        assert_eq!(entry.path, "/a/b.jpg");
        assert_eq!(entry.kind, "photo");
        assert_eq!(entry.segment_id, 3);
        assert_eq!(entry.loc, Some(1.5));
        // loc is the .get(): absent is fine, None.
        let no_loc = json!({"path": "/a", "kind": "photo", "segment_id": 0});
        assert_eq!(hit_fields(&no_loc, true).unwrap().loc, None);
        // keyed reads refused -> Internal (the 500):
        let missing = json!({"kind": "photo", "segment_id": 3});
        assert!(matches!(
            hit_fields(&missing, false),
            Err(ApiError::Internal)
        ));
        let wrong_type = json!({"path": 5, "kind": "photo", "segment_id": 3});
        assert!(matches!(
            hit_fields(&wrong_type, false),
            Err(ApiError::Internal)
        ));
        let non_object = json!(5);
        assert!(matches!(
            hit_fields(&non_object, false),
            Err(ApiError::Internal)
        ));
        // segment_id as a float (2.0) is refused here and a ValidationError in
        // Python (the engine emits integers; the parity is the refusal class).
        let floaty = json!({"path": "/a", "kind": "photo", "segment_id": 2.0});
        assert!(matches!(
            hit_fields(&floaty, false),
            Err(ApiError::Internal)
        ));
    }

    #[test]
    fn the_query_parser_is_one_shape_and_decodes_plus_as_space() {
        // Redundancy fix (the two hand-rolled parsers) + the parity fact it
        // exposed: parse_qsl decodes query VALUES with unquote_plus
        // (measured CPython 3.12): a literal `+` is a space, `%2B` is a
        // literal `+`. The audit-base ran `percent_decode` (the PATH rule)
        // on query values: `?pixels=1+ ` answers int("1  ") = 1 (200) in
        // Python where the audit-base 422s, and `?target=web+shop` names
        // "web shop" in Python, "web+shop" in the audit-base.
        use super::{query_decode, query_param, query_target};
        use axum::http::Uri;
        let parse =
            |qs: &str| query_target(&format!("/api/v1/status?{qs}").parse::<Uri>().unwrap());
        // unquote_plus, measured: plus first, then %XX.
        assert_eq!(query_decode("web+shop"), "web shop");
        assert_eq!(query_decode("a%2Bb"), "a+b"); // %2B rides a literal +
        assert_eq!(query_decode("1+ "), "1  ");
        assert_eq!(
            query_decode("a+b%20c"),
            "a b c" // plus AND %20 — the space rule does not eat %20
        );
        // The first occurrence wins (parse_qs(...)[0] / FastAPI's Query).
        let uri = "/api/v1/search?q=x&target=first&target=second"
            .parse::<Uri>()
            .unwrap();
        assert_eq!(query_target(&uri).as_deref(), Some("first"));
        // trim + non-empty (api.py: `.strip()`, `if value`):
        assert_eq!(parse("target=+").as_deref(), None); // " " trims empty
        assert_eq!(parse("target=%20%20").as_deref(), None);
        assert_eq!(parse("target=a+b").as_deref(), Some("a b"));
        assert_eq!(parse("target=%20web%20").as_deref(), Some("web"));
        assert_eq!(parse("x=1").as_deref(), None); // no target at all
                                                   // pixels flow: the audit-base 422s on "1+ " (parse fails on "1+"),
                                                   // the fixed decode lands on trim -> 1, like Python's int("1  ").
                                                   // `?pixels=1+ ` — the same 422 on both sides (1 < 32), the DETAIL
                                                   // differs: Python decodes to "1  ", parses int("1  ") = 1, and the
                                                   // range check fires (greater_than_equal); the audit-base's
                                                   // percent_decode left "1+ ", and the int parse fires (int_parsing).
        let uri = "/api/v1/media/x/file?pixels=1+%20".parse::<Uri>().unwrap();
        assert_eq!(query_param(&uri, "pixels").as_deref(), Some("1  "));
        let fixed = super::validate::check_pixels("1  ").expect_err("1 < 32");
        assert_eq!(fixed[0]["type"], serde_json::json!("greater_than_equal")); // Python's 422
                                                                               // The audit-base shape inline (the fails-before): percent_decode
                                                                               // leaves the plus, check_pixels refuses "1+ " — the SAME status,
                                                                               // a DIFFERENT detail type.
        assert_eq!(super::percent_decode("1+ "), "1+ ");
        let base = super::validate::check_pixels("1+ ").expect_err("no int parse");
        assert_eq!(base[0]["type"], serde_json::json!("int_parsing"));
        // In range with the space rule: pixels=+256 -> " 256" -> 256 (200).
        let uri = "/api/v1/media/x/file?pixels=+256".parse::<Uri>().unwrap();
        assert_eq!(query_param(&uri, "pixels").as_deref(), Some(" 256"));
        assert_eq!(
            super::validate::check_pixels(" 256").expect("int( 256) = 256"),
            256
        );
        // Paths keep the path rule: uvicorn decodes %XX, leaves `+` standing.
        assert_eq!(super::guard_path_of("/a+b/c"), "/a+b/c");
    }
    #[test]
    fn the_ui_default_rides_the_running_binary_not_the_build_tree() {
        use super::{default_ui_dir, ui_mount};

        // F-12. Python's default is install-relative (api.py:936,
        // `Path(__file__).parent.parent / "ui" / "dist"`): the bundle
        // beside the installed code. The port spells it beside the running
        // binary; the payload stage copies the dist there (`ui/dist` under
        // the folder holding `digup-server`).
        let exe = std::env::current_exe().expect("the running binary");
        let want = exe.parent().expect("exe folder").join("ui").join("dist");
        assert_eq!(default_ui_dir().as_deref(), Some(want.as_path()));
        // The three answers (api.py:937-941): explicit + index.html mounts;
        // explicit without it refuses with the reference's text; the
        // default-with-no-bundle is the silent skip (None, no refusal).
        let dir = temp_ui_dir("ui-mount");
        assert_eq!(
            ui_mount(Some(&dir)).unwrap().as_deref(),
            Some(dir.as_path())
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(ui_mount(Some(&dir)).is_err());
        let refusal = ui_mount(Some(&dir)).unwrap_err();
        assert_eq!(
            refusal,
            format!(
                "DIGUP_SERVER_UI={} has no index.html — serve the built UI: cd server/ui && npm run build",
                dir.display()
            )
        );
        // No knob: the default (target/debug/deps/ui/dist — never built in
        // a test run) is absent, and the answer is the silent None.
        assert_eq!(ui_mount(None).unwrap(), None);
        // The audit-base, recorded inline (the fails-before): the default was
        // `env!("CARGO_MANIFEST_DIR")/../../server/ui/dist` — the BUILD tree's
        // absolute path compiled into the binary. A payload copied to any
        // other machine carried a default naming a tree that is not there
        // (silent-skip: the payload's own shipped bundle never mounted), and
        // on the build machine it found the dev tree's dist — the host's
        // git-ignored, docker-ignored build artifact, never the payload's.
        let baked = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../server/ui/dist");
        assert!(
            baked.to_string_lossy().contains("crates/server"),
            "the audit-base default names the build tree: {baked:?}"
        );
    }

    fn temp_ui_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("digup-f12-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html></html>").unwrap();
        dir
    }

    #[test]
    fn status_identity_fields_are_keyed_reads() {
        // The five fields the endpoint splats from the CLI status (api.py:594-
        // 601 `cli["kinds"]` ...): subscript-parity, the same relay.
        use super::ApiError;
        use serde_json::{json, Map};
        let mut row = Map::new();
        let cli = json!({"files": 4, "kinds": [], "fingerprints": "x", "build_tag": "t",
                         "vectors_version": 6, "index_size": 100});
        for field in [
            "kinds",
            "fingerprints",
            "build_tag",
            "vectors_version",
            "index_size",
        ] {
            row.insert(
                field.into(),
                cli.get(field).cloned().ok_or(ApiError::Internal).unwrap(),
            );
        }
        assert_eq!(row.get("index_size"), Some(&json!(100)));
        // The audit-base shape (unwrap_or(Value::Null)) would answer Null here;
        // Python raises KeyError -> 500:
        let relay = truncated_get_kinds();
        assert!(matches!(relay, Err(ApiError::Internal))); // the relay refuses, like the reference
    }

    fn truncated_get_kinds() -> Result<serde_json::Value, super::ApiError> {
        use super::ApiError;
        use serde_json::json;
        let truncated = json!({"files": 4});
        truncated.get("kinds").cloned().ok_or(ApiError::Internal)
    }

    /// The app-span layer (api.py's capped parse_range, 206 StreamingResponse,
    /// 416) rides the LIVE HANDLE's fstat (api.py:739); the starlette layer
    /// (FileResponse) stats the path itself. The audit-base ran one number
    /// (its own stat) through both.
    #[test]
    fn the_app_span_answers_print_the_live_handle_size() {
        use super::media::{parse_range, Span};
        use super::{file_response, header, FileOptions};
        use axum::http::HeaderMap;
        let opts = |head: bool, cap_span: bool, live_size: Option<i64>, mount: bool| FileOptions {
            head,
            cap_span,
            live_size,
            mount,
        };
        let dir = std::env::temp_dir().join("digup-f15-live-size");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.bin");
        std::fs::write(&path, vec![b'a'; 20]).unwrap(); // 20 bytes on disk
        let owned = || {
            vec![
                header("content-disposition", "inline; filename*=UTF-8''clip.bin"),
                header("accept-ranges", "bytes"),
                header("x-content-type-options", "nosniff"),
            ]
        };
        let range = |spec: &str| {
            let mut hm = HeaderMap::new();
            let (name, value) = header("range", spec);
            hm.insert(name, value);
            hm
        };
        // Live handle fstat 15 (Python's replacement window: the headers
        // ride the handle, the bytes ride the fresh open at api.py:760).
        let resp = file_response(
            &path,
            "application/octet-stream",
            owned(),
            &range("bytes=0-9"),
            opts(false, true, Some(15), false),
        );
        assert_eq!(resp.status(), 206);
        assert_eq!(resp.headers().get("content-range").unwrap(), "bytes 0-9/15");
        assert_eq!(
            resp.headers().get_all("accept-ranges").into_iter().count(),
            1
        );
        assert!(resp.headers().get("content-length").is_none()); // chunked
                                                                 // bytes=10-20 clamps on the HANDLE's 15 -> 10..=14 (the audit-base
                                                                 // clamped on the disk's 20: `bytes 10-19/20`).
        let resp = file_response(
            &path,
            "application/octet-stream",
            owned(),
            &range("bytes=10-20"),
            opts(false, true, Some(15), false),
        );
        assert_eq!(resp.status(), 206);
        assert_eq!(
            resp.headers().get("content-range").unwrap(),
            "bytes 10-14/15"
        );
        // bytes=15-18: start >= 15 unsatisfiable on the handle number — 416
        // `bytes */15` (the pair must agree: parse_range refused on the SAME
        // number the 416 prints). The fails-before, pinned inline: the
        // audit-base clamped on 20 and answered 206.
        assert!(matches!(
            parse_range(Some("bytes=15-18"), 20),
            Span::Range(15, 18)
        ));
        let resp = file_response(
            &path,
            "application/octet-stream",
            owned(),
            &range("bytes=15-18"),
            opts(false, true, Some(15), false),
        );
        assert_eq!(resp.status(), 416);
        assert_eq!(resp.headers().get("content-range").unwrap(), "bytes */15");
        assert_eq!(resp.headers().get("accept-ranges").unwrap(), "bytes");
        assert_eq!(resp.headers().get("content-length").unwrap(), "0");
        // No range: FileResponse stats FRESH — the 200 answers the disk's
        // length (content-length 20), NOT the handle's 15; FileResponse
        // re-stats in Python too (starlette's own `os.stat(path)`).
        let resp = file_response(
            &path,
            "application/octet-stream",
            owned(),
            &HeaderMap::new(),
            opts(false, true, Some(15), false),
        );
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers().get("content-length").unwrap(), "20");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The 304 lives in StaticFiles, NOT in FileResponse (starlette 1.10,
    /// read in the pinned site-packages): responses.py has no conditional
    /// check at all, `is_not_modified` is staticfiles-only, and its 304
    /// keeps ONLY NOT_MODIFIED_HEADERS. The audit-base 304'd every
    /// `file_response` call — media `/file` and `/preview` included — with
    /// the full header set.
    #[test]
    fn the_conditional_304_lives_only_in_the_mount_and_keeps_only_not_modified_headers() {
        use super::{etag_of, file_response, header, FileOptions};
        use axum::http::HeaderMap;
        let opts = |head: bool, cap_span: bool, live_size: Option<i64>, mount: bool| FileOptions {
            head,
            cap_span,
            live_size,
            mount,
        };
        let dir = std::env::temp_dir().join("digup-f15-304");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.css");
        std::fs::write(&path, b"body { margin: 0 }").unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let etag = etag_of(mtime, meta.len());
        let mount = || vec![header("cache-control", "no-cache")];
        let inm = |value: &str| {
            let mut hm = HeaderMap::new();
            let (name, value) = header("if-none-match", value);
            hm.insert(name, value);
            hm
        };
        // The mount: `is_not_modified` fires at response CONSTRUCTION
        // (staticfiles.py:188-191), before FileResponse's range branch —
        // a Range + If-None-Match revalidation 304s on the mount.
        let resp = file_response(
            &path,
            "text/css; charset=utf-8",
            mount(),
            &inm(&etag),
            opts(false, false, None, true),
        );
        assert_eq!(resp.status(), 304);
        let headers = resp.headers();
        // NOT_MODIFIED_HEADERS (staticfiles.py:24-31): of the mount answer's
        // cache-control/accept-ranges/etag/last-modified, ONLY
        // cache-control + etag survive. The audit-base echoed ALL FOUR.
        assert_eq!(headers.len(), 2);
        assert_eq!(headers.get("cache-control").unwrap(), "no-cache");
        assert_eq!(headers.get("etag").unwrap(), etag.as_str());
        assert!(headers.get("last-modified").is_none());
        assert!(headers.get("accept-ranges").is_none());
        // The WEAK comparison (staticfiles.py:215-219): `*`, the `W/`
        // prefix (removeprefix, head-only), comma-lists stripped.
        for probe in [
            "*",
            &format!("W/{etag}"),
            &format!("foo, {etag}"),
            &format!("{etag},foo"),
        ] {
            let resp = file_response(
                &path,
                "text/css; charset=utf-8",
                mount(),
                &inm(probe),
                opts(false, false, None, true),
            );
            assert_eq!(resp.status(), 304, "the probe {probe} must 304");
        }
        assert_eq!(
            file_response(
                &path,
                "text/css; charset=utf-8",
                mount(),
                &inm("foo"),
                opts(false, false, None, true),
            )
            .status(),
            200
        );
        // PRESENCE DECIDES: is_not_modified RETURNS from the etag branch —
        // a non-matching inm behind a fresh if-modified-since answers
        // 200 (the audit-base ran the legs in sequence and 304'd).
        let future = httpdate::fmt_http_date(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.floor() as u64 + 86_400),
        );
        let mut hm = inm("foo");
        let (name, value) = header("if-modified-since", &future);
        hm.insert(name, value);
        assert_eq!(
            file_response(
                &path,
                "text/css; charset=utf-8",
                mount(),
                &hm,
                opts(false, false, None, true),
            )
            .status(),
            200
        );
        // imds alone: at/after the (whole-second) last-modified -> 304;
        // older -> 200.
        let mut hm = HeaderMap::new();
        let (name, value) = header("if-modified-since", &future);
        hm.insert(name, value);
        assert_eq!(
            file_response(
                &path,
                "text/css; charset=utf-8",
                mount(),
                &hm,
                opts(false, false, None, true),
            )
            .status(),
            304
        );
        let past = httpdate::fmt_http_date(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.floor() as u64 - 10),
        );
        let mut hm = HeaderMap::new();
        let (name, value) = header("if-modified-since", &past);
        hm.insert(name, value);
        assert_eq!(
            file_response(
                &path,
                "text/css; charset=utf-8",
                mount(),
                &hm,
                opts(false, false, None, true),
            )
            .status(),
            200
        );
        // The media `/file` and `/preview` shapes: FileResponse /
        // StreamingResponse — NO conditional check. If-None-Match rides as
        // a normal answer: no range -> 200 with the etag header present
        // (the audit-base 304'd); a Range + If-None-Match revalidation —
        // exactly what a video element sends — serves the SPAN (206),
        // the audit-base answered a bodyless 304.
        let media = || {
            vec![
                header("content-disposition", "inline; filename*=UTF-8''clip.bin"),
                header("accept-ranges", "bytes"),
            ]
        };
        let resp = file_response(
            &path,
            "application/octet-stream",
            media(),
            &inm(&etag),
            opts(false, true, None, false),
        );
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers().get("etag").unwrap(), etag.as_str());
        let mut hm = inm(&etag);
        let (name, value) = header("range", "bytes=0-9");
        hm.insert(name, value);
        let resp = file_response(
            &path,
            "application/octet-stream",
            media(),
            &hm,
            opts(false, true, Some(17), false),
        );
        assert_eq!(resp.status(), 206);
        assert_eq!(resp.headers().get("content-range").unwrap(), "bytes 0-9/17");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
