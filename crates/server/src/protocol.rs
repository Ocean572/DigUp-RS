//! One `digup` helper process, read line by line, requests answered by `id`
//! (`engine/protocol.py`): the engine's protocol verbatim — one JSON object per
//! line on stdin, one per line on stdout carrying the request's `id`; lines
//! without an `id` are events; plain-text lines ("sync", "stop") drive
//! `index-service`, and closing stdin is EOF = stop there.
//!
//! The engine's own exits are normal, not failures: `search-service` releases the
//! model after `--encoder-idle` and exits itself after twice that (a shell starts
//! it again); `index-service` exits after `--linger` seconds of quiet, and on EOF.
//! `ParentWatch.start()` kills the helper the moment *this* process dies, so
//! helpers spawn directly — argv arrays, never a shell wrapper.
//!
//! A reader thread routes every line: requests never block on each other's
//! stderr, and a process that died hands `EngineExited` to whoever was waiting —
//! with the stderr tail, because the engine reports encoder failures there.
//!
//! The errors mirror `protocol.py`'s raises, because the API maps them by kind:
//! `EngineExited`/`EngineRefused` → 503, `TimeoutError` → 504, `CLIError` → 502,
//! `JobBusy` → 409, and a plain `RuntimeError` is UNMAPPED (FastAPI answers 500).

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::config::format_gf64;

/// A query on a cold CPU encodes in a few seconds; 120 s covers a loaded model
/// starting up.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// `digup --help` answers in ms; "ready" rides out with the index opening.
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The helper process ended. Normal for services that idle out.
#[derive(Clone, Debug)]
pub struct EngineExited {
    pub name: String,
    pub code: Option<i32>,
    pub stderr_tail: Vec<String>,
}

impl EngineExited {
    fn new(name: &str, code: Option<i32>, stderr_tail: &[String]) -> Self {
        Self {
            name: name.to_string(),
            code,
            stderr_tail: stderr_tail.to_vec(),
        }
    }
}

impl std::fmt::Display for EngineExited {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Python's `"; ".join(tail[-3:])`, and "no stderr" when there is none.
        let tail = if self.stderr_tail.is_empty() {
            "no stderr".to_string()
        } else {
            let start = self.stderr_tail.len().saturating_sub(3);
            self.stderr_tail[start..].join("; ")
        };
        write!(
            out,
            "{} process ended (exit {}): {}",
            self.name,
            self.code.map(|c| c.to_string()).unwrap_or("None".into()),
            tail
        )
    }
}

/// The helper ANSWERED, with its own refusal: `{"ok": false, "error": …}`
/// (`SearchService.swift:106,115`) — the engine's words ride along quoted; the
/// API answers 503.
#[derive(Clone, Debug)]
pub struct EngineRefused {
    pub op: String,
    pub error: String,
}

impl std::fmt::Display for EngineRefused {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Python's f-string with `{op!r}`: repr of a str is its double-quoted form,
        // which is exactly what a Rust `String`'s `{:?}` prints.
        write!(out, "engine refused {}: {}", crate::targets::py_repr(&self.op), self.error)
    }
}

/// Every raise site of `protocol.py`, as the API sees it:
/// `EngineExited` (503), the request/event `TimeoutError` (504), `CLIError`
/// (502 — the 502-shaped answer for a missing binary, search.py:150), and the
/// plain `RuntimeError`s ("already started", "not started", the ready-handshake
/// mismatch) which FastAPI leaves UNMAPPED: 500.
#[derive(Debug)]
pub enum ProtocolError {
    Exited(EngineExited),
    Timeout(String),
    Cli(String),
    Internal(String),
}

impl From<EngineExited> for ProtocolError {
    fn from(exited: EngineExited) -> Self {
        ProtocolError::Exited(exited)
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Exited(exited) => write!(out, "{exited}"),
            ProtocolError::Timeout(what) => write!(out, "{what}"),
            ProtocolError::Cli(what) => write!(out, "{what}"),
            ProtocolError::Internal(what) => write!(out, "{what}"),
        }
    }
}

#[derive(Default)]
struct Inner {
    /// Replies routed by the JSON `id` (its text form: numbers and strings ride
    /// the same map, as Python's dict keys them).
    replies: HashMap<String, Value>,
    /// The engine's own lines, in order. Bounded: the searcher never drains them.
    events: VecDeque<Value>,
    exited: Option<EngineExited>,
}

/// A spawned `digup` helper: `request()` for answers, `next_event()` for the
/// engine's own lines. All state is one mutex + condvar, like Python's `_cv`.
pub struct JsonLinesProcess {
    argv: Vec<String>,
    name: String,
    env: Vec<(String, OsString)>,
    inner: Arc<(Mutex<Inner>, Condvar)>,
    stdin: Mutex<Option<ChildStdin>>,
    /// One writer at a time: lines must not interleave. The child and the stderr
    /// tail are shared with the reader threads (Python keeps them on `self`).
    child: Arc<Mutex<Option<Child>>>,
    started: Mutex<bool>,
    ids: Mutex<i64>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

fn notify(inner: &(Mutex<Inner>, Condvar)) {
    let _guard = inner.0.lock().expect("the protocol lock");
    inner.1.notify_all();
}

impl JsonLinesProcess {
    pub fn new(
        argv: Vec<String>,
        name: &str,
        env: Vec<(String, OsString)>,
    ) -> Self {
        Self {
            argv,
            name: name.to_string(),
            env,
            inner: Arc::new((Mutex::new(Inner::default()), Condvar::new())),
            stdin: Mutex::new(None),
            child: Arc::new(Mutex::new(None)),
            started: Mutex::new(false),
            ids: Mutex::new(0),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Spawn and (when told) wait for the engine's opening `{"event": "ready", …}`.
    /// No shell wrapper: ParentWatch kills children whose parent dies.
    pub fn start(
        &self,
        ready_event: Option<&str>,
        ready_timeout: Duration,
    ) -> Result<Value, ProtocolError> {
        {
            let mut started = self.started.lock().expect("the started flag");
            if *started {
                // Python's RuntimeError — unmapped, so 500 (this wrapper never calls
                // start twice on one process; the variant pins the shape).
                return Err(ProtocolError::Internal(format!(
                    "{}: already started",
                    self.name
                )));
            }
            *started = true;
        }
        let mut command = Command::new(&self.argv[0]);
        command.args(&self.argv[1..]);
        command.env_clear();
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let spawn = command.spawn();
        // `search.py:150`'s mapping: the OSError becomes CLIError, the 502 shape.
        let mut child = match spawn {
            Ok(child) => child,
            Err(error) => {
                return Err(ProtocolError::Cli(format!(
                    "cannot start the engine helper: {error}"
                )))
            }
        };
        let stdin = child.stdin.take().expect("the child's stdin");
        let stdout = child.stdout.take().expect("the child's stdout");
        let stderr = child.stderr.take().expect("the child's stderr");
        *self.stdin.lock().expect("the stdin lock") = Some(stdin);
        *self.child.lock().expect("the child lock") = Some(child);

        std::thread::spawn({
            let inner = self.inner.clone();
            let name = self.name.clone();
            let child_slot = self.child.clone();
            let stderr_slot = self.stderr.clone();
            move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else {
                        break
                    };
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(obj) = serde_json::from_str::<Value>(line) else {
                        continue; // the engine's non-JSON chatter (llama logs) never answers a request
                    };
                    if !obj.is_object() {
                        continue;
                    }
                    {
                        let mut guard = inner.0.lock().expect("the protocol lock");
                        match obj.get("id") {
                            Some(id) if !id.is_null() => {
                                guard.replies.insert(id.to_string(), obj);
                            }
                            _ => {
                                if guard.events.len() >= 256 {
                                    guard.events.pop_front();
                                }
                                guard.events.push_back(obj);
                            }
                        }
                    }
                    inner.1.notify_all();
                }
                // EOF: remember the end so every waiter wakes with EngineExited.
                let code = {
                    let mut slot = child_slot.lock().expect("the child lock");
                    match slot.as_mut() {
                        Some(child) => child.wait().ok().map(|status| status.code()),
                        None => None,
                    }
                    .flatten()
                };
                let tail: Vec<String> =
                    stderr_slot.lock().expect("the stderr lock").iter().cloned().collect();
                {
                    let mut guard = inner.0.lock().expect("the protocol lock");
                    if guard.exited.is_none() {
                        guard.exited = Some(EngineExited::new(&name, code, &tail));
                    }
                }
                inner.1.notify_all();
            }
        });
        std::thread::spawn({
            let stderr_slot = self.stderr.clone();
            move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    let mut tail = stderr_slot.lock().expect("the stderr lock");
                    if tail.len() >= 50 {
                        tail.pop_front();
                    }
                    tail.push_back(line.trim_end().to_string());
                }
            }
        });

        match ready_event {
            Some(want) => match self.next_event(ready_timeout) {
                Ok(event) => {
                    if event.get("event").and_then(Value::as_str) != Some(want) {
                        // Python's `f"{name}: expected the {ready_event!r} event, got
                        // {event}"` — a RuntimeError, unmapped (500). The child must
                        // not leak (search.py:153 — its pipes outlive every reference
                        // until its own idle-exit).
                        self.close(Duration::from_secs(5));
                        return Err(ProtocolError::Internal(format!(
                            "{}: expected the {:?} event, got {}",
                            self.name, want, event
                        )));
                    }
                    Ok(event)
                }
                Err(error) => {
                    self.close(Duration::from_secs(5));
                    Err(error)
                }
            },
            None => Ok(Value::Null),
        }
    }

    pub fn alive(&self) -> bool {
        let mut slot = self.child.lock().expect("the child lock");
        match slot.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lock().expect("the stderr lock").iter().cloned().collect()
    }

    /// Close stdin (EOF: services stop themselves), then join, then terminate,
    /// then kill if it must (`close(graceful_s)`: wait, terminate + wait, kill).
    pub fn close(&self, graceful: Duration) {
        {
            let mut slot = self.stdin.lock().expect("the stdin lock");
            if let Some(stdin) = slot.take() {
                drop(stdin); // dropping the pipe closes it: the daemon reads EOF
            }
        }
        for phase in 0..2 {
            let deadline = Instant::now() + graceful;
            loop {
                let done = {
                    let mut slot = self.child.lock().expect("the child lock");
                    match slot.as_mut() {
                        Some(child) => child.try_wait().ok().flatten().is_some(),
                        None => true,
                    }
                };
                if done || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let mut slot = self.child.lock().expect("the child lock");
            if let Some(child) = slot.as_mut() {
                if matches!(child.try_wait(), Ok(None)) {
                    if phase == 0 {
                        // Python's `proc.terminate()` — SIGTERM, the engine's polite stop.
                        #[cfg(unix)]
                        {
                            let pid = child.id() as i32;
                            unsafe { libc::kill(pid, libc::SIGTERM) };
                        }
                    } else {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            }
        }
    }

    fn write_line(&self, bytes: &[u8]) -> Result<(), ProtocolError> {
        let mut slot = self.stdin.lock().expect("the stdin lock");
        let stdin = slot.as_mut().ok_or_else(|| {
            // Python's RuntimeError(f"{name}: not started") — unmapped, 500.
            ProtocolError::Internal(format!("{}: not started", self.name))
        })?;
        match stdin.write_all(bytes).and_then(|_| stdin.flush()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {
                drop(slot);
                Err(self.death().into())
            }
            Err(error) => Err(ProtocolError::Exited(EngineExited::new(
                &self.name,
                None,
                &[format!("stdin: {error}")],
            ))),
        }
    }

    /// One request, one reply with the same `id`. Raises `EngineExited` (503) or
    /// the TimeoutError text the API maps to 504.
    pub fn request(
        &self,
        fields: &Map<String, Value>,
        timeout: Duration,
    ) -> Result<Value, ProtocolError> {
        let mut row = fields.clone();
        let rid = {
            let mut id = self.ids.lock().expect("the id counter");
            *id += 1;
            *id
        };
        row.insert("id".into(), Value::from(rid));
        // `json.dumps(..., separators=(",", ":"))`: the compact form, no spaces.
        let mut line = serde_json::to_string(&Value::Object(row)).map_err(|e| {
            ProtocolError::Internal(format!("request: {e}"))
        })?;
        line.push('\n');
        self.write_line(line.as_bytes())?;

        let deadline = Instant::now() + timeout;
        // ONE guard across the check and the wait (`with self._cond:` in Python):
        // unlocking between them lets a push + notify land in the window, and the
        // waiter sleeps out its full timeout — a LOST WAKEUP, measured 2026-11-13
        // as index jobs hanging in `running` for the whole job timeout (the engine
        // had answered, the events were queued, the runner never woke).
        let mut guard = self.inner.0.lock().expect("the protocol lock");
        loop {
            if let Some(reply) = guard.replies.remove(&rid.to_string()) {
                return Ok(reply);
            }
            if let Some(exited) = guard.exited.clone() {
                return Err(exited.into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(guard);
                // A late reply must not pile up.
                self.inner
                    .0
                    .lock()
                    .expect("the protocol lock")
                    .replies
                    .remove(&rid.to_string());
                // Python: `f"{name}: no reply to {fields.get('op', fields)} within {timeout:g} s"`.
                let op = fields
                    .get("op")
                    .map(|value| {
                        value
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| value.to_string())
                    })
                    .unwrap_or_else(|| Value::Object(fields.clone()).to_string());
                return Err(ProtocolError::Timeout(format!(
                    "{}: no reply to {} within {} s",
                    self.name,
                    op,
                    format_gf64(timeout.as_secs_f64())
                )));
            }
            let (next, outcome) = self
                .inner
                .1
                .wait_timeout(guard, remaining)
                .expect("the protocol condvar");
            guard = next;
            if outcome.timed_out() {
                drop(guard);
                self.inner
                    .0
                    .lock()
                    .expect("the protocol lock")
                    .replies
                    .remove(&rid.to_string());
                let op = fields
                    .get("op")
                    .map(|value| {
                        value
                            .as_str()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| value.to_string())
                    })
                    .unwrap_or_else(|| Value::Object(fields.clone()).to_string());
                return Err(ProtocolError::Timeout(format!(
                    "{}: no reply to {} within {} s",
                    self.name,
                    op,
                    format_gf64(timeout.as_secs_f64())
                )));
            }
        }
    }

    /// A plain-text command (`index-service`: "sync", "stop", "backfill on", …).
    pub fn send_line(&self, line: &str) -> Result<(), ProtocolError> {
        self.write_line(format!("{line}\n").as_bytes())
    }

    /// The next event line, in order, or EngineExited when the process has ended.
    pub fn next_event(&self, timeout: Duration) -> Result<Value, ProtocolError> {
        let deadline = Instant::now() + timeout;
        // One guard across check + wait — see `request` for the measured race.
        let mut guard = self.inner.0.lock().expect("the protocol lock");
        loop {
            if let Some(event) = guard.events.pop_front() {
                return Ok(event);
            }
            if let Some(exited) = guard.exited.clone() {
                return Err(exited.into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // Python: `f"{name}: no event within {timeout:g} s"` → 504.
                return Err(ProtocolError::Timeout(format!(
                    "{}: no event within {} s",
                    self.name,
                    format_gf64(timeout.as_secs_f64())
                )));
            }
            let (next, outcome) = self.inner.1.wait_timeout(guard, remaining)
                .expect("the protocol condvar");
            guard = next;
            if outcome.timed_out() {
                return Err(ProtocolError::Timeout(format!(
                    "{}: no event within {} s",
                    self.name,
                    format_gf64(timeout.as_secs_f64())
                )));
            }
        }
    }

    /// The engine's exit as we noticed it: the real exit event if one arrived, EOF
    /// if not (`_death`).
    fn death(&self) -> EngineExited {
        let code = {
            let mut slot = self.child.lock().expect("the child lock");
            slot.as_mut()
                .and_then(|child| child.wait().ok())
                .and_then(|status| status.code())
        };
        let tail = self.stderr_tail();
        let mut guard = self.inner.0.lock().expect("the protocol lock");
        if guard.exited.is_none() {
            guard.exited = Some(EngineExited::new(&self.name, code, &tail));
        }
        let exited = guard.exited.clone().expect("just set");
        drop(guard);
        notify(&self.inner);
        exited
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod protocol_tests {
    use super::{JsonLinesProcess, ProtocolError};
    use std::sync::Arc;
    use std::time::Duration;

    /// The reader → queue → waiter handoff on a REAL child (`cat` echoes what we
    /// write). The 2026-11-13 job hang hinged on exactly this path: the runner
    /// parked in the condvar, the engine's lines were already queued, and the
    /// notify landed in the window left by unlocking BETWEEN the queue check and
    /// the wait — a lost wakeup, and the job stayed `running` for its whole
    /// timeout with a completed crawl in hand. A parked waiter must wake.
    #[test]
    fn a_parked_waiter_wakes_for_a_line_written_after_it_parks() {
        let proc = Arc::new(JsonLinesProcess::new(
            vec!["cat".to_string()],
            "echo",
            Vec::new(),
        ));
        proc.start(None, Duration::from_secs(5)).expect("cat spawns");
        let feeder = {
            let writer = proc.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                let _ = writer.send_line("{\"event\":\"fed\"}");
            })
        };
        let event = proc
            .next_event(Duration::from_secs(5))
            .expect("the fed line arrives");
        assert_eq!(event.get("event").and_then(|v| v.as_str()), Some("fed"));
        feeder.join().expect("feeder");
        proc.close(Duration::from_secs(2));
    }

    /// EOF wakes every waiter with EngineExited (`exited` set, notify fired).
    #[test]
    fn the_exit_of_a_child_wakes_the_waiter() {
        let proc = JsonLinesProcess::new(
            vec!["/bin/sh".to_string(), "-c".to_string(), "exit 3".to_string()],
            "quick",
            Vec::new(),
        );
        proc.start(None, Duration::from_secs(5)).expect("sh spawns");
        match proc.next_event(Duration::from_secs(5)) {
            Err(ProtocolError::Exited(exited)) => {
                assert!(format!("{exited}").contains("quick"), "names the helper: {exited}");
            }
            other => panic!("expected EngineExited, got {other:?}"),
        }
        proc.close(Duration::from_secs(2));
    }
}
