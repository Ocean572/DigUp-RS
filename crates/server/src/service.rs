//! `digup search-service`: the hot searcher, the server's primary window on the
//! engine (`engine/search.py`). Replies pass through verbatim: no re-ranking, no
//! invented similarity numbers, and "no good matches" stays an empty `hits` — the
//! engine refusing to guess is a fact to publish, not to paper over.
//!
//! The service idles itself out (model after `--encoder-idle`, process after twice
//! that); a call treats that as a restart, not an error — spawn again, request
//! once. Requests are serialized here (the engine is thread-safe per line, but one
//! in-flight query keeps the reply routing trivial and the encoder warm for the
//! typist: `embed_ahead` on each keystroke). `render` is the engine's own
//! concurrent work on two slots, answering out of order and routed by `id`, so it
//! holds the serialize lock only for the spawn check.
//!
//! The error kinds mirror the Python layer's raises so the API maps them the same
//! way: `EngineExited`/`EngineRefused` → 503, the request `TimeoutError` → 504,
//! `CLIError` → 502; the plain `RuntimeError`s (the ready-handshake mismatch,
//! "not started") are unmapped — 500.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::config::EngineConfig;
use crate::protocol::{
    EngineExited, EngineRefused, JsonLinesProcess, ProtocolError, DEFAULT_REQUEST_TIMEOUT,
    READY_TIMEOUT,
};
use crate::rlock::RLock;
use crate::slots::Slots;

pub struct SearchService {
    config: EngineConfig,
    timeout: Duration,
    /// Python's `threading.RLock`: `_call` → `_spawn_locked` re-enters the same
    /// lock (the spawn check rides the serialize lock).
    lock: Arc<RLock>,
    /// The process, guarded by `lock` — touched only while its guard is held, so
    /// this mutex never blocks.
    proc: std::sync::Mutex<Option<Arc<JsonLinesProcess>>>,
    render_slots: Slots,
}

/// `_checked`: a reply with `ok:false` is the engine refusing (its words, not
/// ours) — raised, never returned, so no endpoint reads `hits`/`files` out of an
/// answer that has none. `ok:false` ALONE is enough: a refusal shaped without
/// `error` would otherwise reach the API's `stripped["hits"]`. Python's `is
/// False`: a non-bool `ok` (a number) is NOT a refusal, and `as_bool` is the
/// exact match of that.
fn checked(reply: Value, fields: &Map<String, Value>) -> Result<Value, EngineRefused> {
    if reply.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(EngineRefused {
            op: fields
                .get("op")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            error: match reply.get("error") {
                Some(Value::String(text)) => text.clone(),
                Some(value) => value.to_string(),
                None => "no reason given".to_string(),
            },
        });
    }
    Ok(reply)
}

fn op_fields(name: &str) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("op".into(), Value::String(name.to_string()));
    fields
}

impl SearchService {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            timeout: DEFAULT_REQUEST_TIMEOUT,
            lock: RLock::new(),
            proc: std::sync::Mutex::new(None),
            render_slots: Slots::new(2),
        }
    }

    /// Spawn and hand-shake: the engine's `{"event": "ready", "index": …}`.
    pub fn start(&self) -> Result<Value, ProtocolError> {
        let _guard = self.lock.lock();
        self.spawn()
    }

    /// The process slot, read/owned through its mutex — `&self` methods need a
    /// guard, not `get_mut` (the RLock above is what serializes the callers).
    fn proc_of(&self) -> Option<Arc<JsonLinesProcess>> {
        self.proc.lock().expect("the process slot").clone()
    }

    fn set_proc(&self, proc: Option<Arc<JsonLinesProcess>>) {
        *self.proc.lock().expect("the process slot") = proc;
    }

    /// `quit()`: ask the engine to quit (`{"op":"quit"} → {"ok":true}`), then join
    /// the process. The request's own death/timeout is swallowed (Python catches
    /// EngineExited/TimeoutError here).
    pub fn quit(&self) {
        let _guard = self.lock.lock();
        let proc = self.proc_of();
        if let Some(proc) = proc {
            let _ = proc.request(&op_fields("quit"), Duration::from_secs(10));
            proc.close(Duration::from_secs(5));
        }
        self.set_proc(None);
    }

    pub fn running(&self) -> bool {
        self.proc_of().map(|proc| proc.alive()).unwrap_or(false)
    }

    // ── the protocol, one method per op ──────────────────────────────────────────────

    pub fn search(
        &self,
        q: &str,
        kinds: Option<Vec<String>>,
        limit: i64,
        code: bool,
        meaning: bool,
        prefix_last: bool,
    ) -> Result<Value, CallError> {
        let mut fields = op_fields("search");
        fields.insert("q".into(), Value::String(q.to_string()));
        fields.insert("code".into(), Value::Bool(code));
        fields.insert("limit".into(), Value::from(limit));
        fields.insert("meaning".into(), Value::Bool(meaning));
        fields.insert("prefix_last".into(), Value::Bool(prefix_last));
        // `if kinds:` — an empty list means "no kinds filter" in Python; None rides
        // identically, so an empty Vec also drops the field.
        if let Some(kinds) = kinds.filter(|kinds| !kinds.is_empty()) {
            fields.insert(
                "kinds".into(),
                Value::Array(kinds.into_iter().map(Value::String).collect()),
            );
        }
        self.call(&fields, None, true)
    }

    pub fn embed_ahead(&self, q: &str, code: bool) -> Result<Value, CallError> {
        let mut fields = op_fields("embed_ahead");
        fields.insert("q".into(), Value::String(q.to_string()));
        fields.insert("code".into(), Value::Bool(code));
        self.call(&fields, None, true)
    }

    pub fn text(&self, segment: i64, code: bool) -> Result<Value, CallError> {
        let mut fields = op_fields("text");
        fields.insert("segment".into(), Value::from(segment));
        fields.insert("code".into(), Value::Bool(code));
        self.call(&fields, None, true)
    }

    pub fn render(
        &self,
        path: &str,
        kind: &str,
        loc: Option<f64>,
        pixels: i64,
    ) -> Result<Value, CallError> {
        let mut fields = op_fields("render");
        fields.insert("path".into(), Value::String(path.to_string()));
        fields.insert("kind".into(), Value::String(kind.to_string()));
        fields.insert("pixels".into(), Value::from(pixels));
        if let Some(loc) = loc {
            fields.insert("loc".into(), Value::from(loc));
        }
        // `with self._render_slots:` — the engine's own render concurrency, two at a
        // time, answering out of order by `id`. The serialize lock rides the spawn
        // check only: holding it for the engine's 120 s of ffmpeg work would undo
        // the design ("typing never waits on one").
        self.render_slots.acquire();
        let result = self.render_call(&fields);
        self.render_slots.release();
        result
    }

    /// `_render`: lock for the spawn check, request outside it, restart once on a
    /// death (the second failure's kind decides: exited → 503, timeout → 504).
    fn render_call(&self, fields: &Map<String, Value>) -> Result<Value, CallError> {
        const RENDER_TIMEOUT: Duration = Duration::from_secs(120);
        let proc = {
            let _guard = self.lock.lock();
            if self.proc_of().is_none() {
                self.spawn()?;
            }
            self.proc_of().expect("spawn set the process")
        };
        match proc.request(fields, RENDER_TIMEOUT) {
            Ok(reply) => Ok(checked(reply, fields)?),
            Err(ProtocolError::Exited(_)) => {
                let fresh = {
                    let _guard = self.lock.lock();
                    self.spawn()?;
                    self.proc_of().expect("spawn set the process")
                };
                Ok(checked(fresh.request(fields, RENDER_TIMEOUT)?, fields)?)
            }
            Err(other) => Err(other.into()),
        }
    }

    /// After indexing ran: reopen the index. The engine answers `{ok, vectors, code_vectors}`.
    pub fn reload(&self) -> Result<Value, CallError> {
        self.call(&op_fields("reload"), None, true)
    }

    pub fn status(&self) -> Result<Value, CallError> {
        self.call(&op_fields("status"), None, true)
    }

    pub fn summaries(&self, code: bool) -> Result<Value, CallError> {
        let mut fields = op_fields("summaries");
        fields.insert("code".into(), Value::Bool(code));
        self.call(&fields, None, true)
    }

    pub fn prewarm(&self) -> Result<Value, CallError> {
        self.call(&op_fields("prewarm"), Some(Duration::from_secs(60)), true)
    }

    /// An op this wrapper has no method for — passthrough, refusal replies
    /// included: `ok:false` reaches the caller verbatim here (a Stage-3 contract
    /// test pins exactly that). Typed ops raise `EngineRefused` instead.
    pub fn raw(&self, fields: &Map<String, Value>) -> Result<Value, CallError> {
        self.call(fields, None, false)
    }

    // ── the supervision ────────────────────────────────────────────────────────────────

    /// `_call`: one query in flight (the RLock serializes the whole call), spawn
    /// when no process is set, restart ONCE when the engine idled out. A
    /// `TimeoutError` is not a death: it propagates untouched (504).
    fn call(
        &self,
        fields: &Map<String, Value>,
        timeout: Option<Duration>,
        check: bool,
    ) -> Result<Value, CallError> {
        let timeout = timeout.unwrap_or(self.timeout);
        let _guard = self.lock.lock();
        if self.proc_of().is_none() {
            self.spawn()?;
        }
        let proc = self.proc_of().expect("spawn set the process");
        let reply = match proc.request(fields, timeout) {
            Ok(reply) => reply,
            Err(ProtocolError::Exited(_)) => {
                // The engine idled out (or was killed): normal. Start it again, once.
                self.spawn()?;
                let fresh = self.proc_of().expect("spawn set the process");
                fresh.request(fields, timeout)?
            }
            Err(other) => return Err(other.into()),
        };
        if check {
            Ok(checked(reply, fields)?)
        } else {
            Ok(reply)
        }
    }

    /// `_spawn_locked`.
    fn spawn(&self) -> Result<Value, ProtocolError> {
        let proc = Arc::new(JsonLinesProcess::new(
            self.config.search_argv(),
            "search-service",
            self.config.env(),
        ));
        let ready = match proc.start(Some("ready"), READY_TIMEOUT) {
            Ok(ready) => ready,
            Err(error) => {
                // A handshake failure must not leak the child: its pipes and reader
                // threads would outlive every reference to it until its own idle-exit.
                proc.close(Duration::from_secs(5));
                return Err(error);
            }
        };
        let old = self.proc_of();
        if let Some(old) = old.as_ref() {
            if !old.alive() {
                old.close(Duration::from_secs(5)); // join the reaped straggler
            }
        }
        self.set_proc(Some(proc));
        Ok(ready)
    }
}

/// The engine's refusals and deaths, as the API maps them: `EngineExited` and
/// `EngineRefused` answer 503, the request `TimeoutError` 504, `CLIError` 502;
/// the protocol's `RuntimeError`s are unmapped — 500.
#[derive(Debug)]
pub enum CallError {
    Exited(EngineExited),
    Refused(EngineRefused),
    Timeout(String),
    Cli(String),
    Internal(String),
}

impl From<ProtocolError> for CallError {
    fn from(error: ProtocolError) -> Self {
        match error {
            ProtocolError::Exited(exited) => CallError::Exited(exited),
            ProtocolError::Timeout(what) => CallError::Timeout(what),
            ProtocolError::Cli(what) => CallError::Cli(what),
            ProtocolError::Internal(what) => CallError::Internal(what),
        }
    }
}

impl From<EngineRefused> for CallError {
    fn from(refused: EngineRefused) -> Self {
        CallError::Refused(refused)
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Exited(exited) => write!(out, "{exited}"),
            CallError::Refused(refused) => write!(out, "{refused}"),
            CallError::Timeout(what) | CallError::Cli(what) | CallError::Internal(what) => {
                write!(out, "{what}")
            }
        }
    }
}
