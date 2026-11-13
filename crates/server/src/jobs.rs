//! Index jobs (`jobs.py`): the server's single writer, one job at a time,
//! honest states. The rules this file lives by (README §5):
//!
//! * **One indexing job at a time** — jobs serialize in a worker thread *and*
//!   the adapter's `flock` guards the index; a second request is answered
//!   "busy, job X is running", never queued blind. The active marker clears
//!   when the writer releases the lock, *before* the `done` record is
//!   published: a client that can read `done` can always start the next job.
//! * **An interrupted job is recovered as *interrupted*, never as success.**
//!   The job log is append-only JSON-lines beside the index
//!   (`<config>/jobs.jsonl`), rotated at size; history replays from all
//!   rotations, oldest first. A job that starts and never finishes (crash,
//!   kill, container stop) is marked `interrupted` at the next startup.
//! * **Container stop during a job: two honest paths, both measured (Stage 8):**
//!   inside the 90 s grace the lifespan finishes — job *interrupted* **with
//!   its partial counts**, `Application shutdown complete`, exit 143. Grace
//!   shorter than the embed left: SIGKILL ends the container as 137, and the
//!   log's `running` line with no ending replays as **interrupted** at the
//!   next boot — never success. SQLite stays whole either way.
//! * **Progress is counted, never invented** — the job's numbers are the
//!   engine's own events (`file` done/failed, `error`, the `idle` totals).
//! * After a **done** job the searcher is brought current: `reload()` →
//!   `prewarm()`, the sequence the contract suite proved.

use std::collections::{BTreeMap, VecDeque};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::config::EngineConfig;
use crate::media::random_hex12;
use crate::protocol::{JsonLinesProcess, ProtocolError};
use crate::service::SearchService;

/// `JOB_LOG` — the log name; app targets keep `jobs-<name>.jsonl`.
pub const JOB_LOG: &str = "jobs.jsonl";

/// States the server publishes. A job that never ran because the lock was
/// held is `busy` (answered, not silently dropped).
pub const STATE_QUEUED: &str = "queued";
pub const STATE_RUNNING: &str = "running";
pub const STATE_DONE: &str = "done";
pub const STATE_INTERRUPTED: &str = "interrupted";
pub const STATE_BUSY: &str = "busy";

/// `LOCK_NAME` (index.py): the writer lock rides beside the index.
pub const LOCK_NAME: &str = ".digup-server.writer.lock";

/// A job is queued or running. The API answers 409 with the live job's id.
#[derive(Clone, Debug)]
pub struct JobBusy(pub String);

impl std::fmt::Display for JobBusy {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "index job {} is already queued or running", self.0)
    }
}

/// `IndexBusy`: another writer holds this index. The server never queues
/// blind: a job says so.
#[derive(Clone, Debug)]
pub struct IndexBusy(pub String);

impl std::fmt::Display for IndexBusy {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{}", self.0)
    }
}

// ── the flock-guarded writer helper (engine/index.py) ───────────────────────

/// A supervised `digup index-service` — spawn, command lines, event stream,
/// single writer. The OS flock rides the process: opened `a+` beside the
/// index, held for exactly as long as the helper lives.
pub struct IndexService {
    config: EngineConfig,
    argv: Vec<String>,
    proc: Option<Arc<JsonLinesProcess>>,
    lock_file: Option<std::fs::File>,
}

impl IndexService {
    pub fn new(config: &EngineConfig, backfill: bool, prune: bool, linger_s: f64) -> Self {
        Self {
            config: config.clone(),
            argv: config.index_argv(backfill, prune, None, None, None, Some(linger_s)),
            proc: None,
            lock_file: None,
        }
    }

    /// Spawn under the writer lock. `index-service` has no "ready" event: it
    /// starts working from the first `sync`.
    pub fn start(&mut self) -> Result<(), IndexBusy> {
        if self.proc.is_some() {
            // Python's RuntimeError("already started") — the runner never does
            // this; the variant pins the shape.
            return Err(IndexBusy("index-service: already started".into()));
        }
        let _ = std::fs::create_dir_all(&self.config.index);
        let lock_path = self.config.index.join(LOCK_NAME);
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) => return Err(IndexBusy(format!("{}: {error}", lock_path.display()))),
        };
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            // The refusal sentence is exactly index.py's: fcntl's OSError rides as
            // the `from exc` cause and never reaches the text.
            return Err(IndexBusy(format!(
                "{}: another index-service is writing",
                self.config.index.display()
            )));
        }
        self.lock_file = Some(file);
        let proc = Arc::new(JsonLinesProcess::new(
            self.argv.clone(),
            "index-service",
            self.config.env(),
        ));
        // ready_event=None: the helper answers from the first `sync`. A
        // spawn failure releases the lock (finish() is idempotent — the
        // runner calls it on every ending).
        match proc.start(None, Duration::from_secs(60)) {
            Ok(_ready) => {
                self.proc = Some(proc);
                Ok(())
            }
            Err(error) => {
                self.finish();
                Err(IndexBusy(format!("index-service: {error}")))
            }
        }
    }

    /// EOF (the helper stops itself at a safe point) and release the lock.
    /// Idempotent: the runner calls it on every ending.
    pub fn finish(&mut self) {
        if let Some(proc) = self.proc.take() {
            proc.close(Duration::from_secs(5));
        }
        if let Some(file) = self.lock_file.take() {
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            drop(file);
        }
    }

    pub fn sync(&self) -> Result<(), ProtocolError> {
        self.command("sync")
    }

    pub fn stop(&self) -> Result<(), ProtocolError> {
        self.command("stop")
    }

    fn command(&self, line: &str) -> Result<(), ProtocolError> {
        match &self.proc {
            Some(proc) => proc.send_line(line),
            None => Err(ProtocolError::Internal("index-service: not started".into())),
        }
    }

    /// `events()`: event lines until the helper's own `{"event": "exit"}`
    /// (the stream's end, normal service); EOF without it is `EngineExited`,
    /// which the runner reads as a death.
    pub fn next_event(&self, timeout: Duration) -> Result<Option<Value>, ProtocolError> {
        match &self.proc {
            Some(proc) => match proc.next_event(timeout) {
                Ok(event) => {
                    if event.get("event").and_then(Value::as_str) == Some("exit") {
                        Ok(None) // the stream's end (protocol.events returns on exit)
                    } else {
                        Ok(Some(event))
                    }
                }
                Err(error) => Err(error),
            },
            None => Err(ProtocolError::Internal("index-service: not started".into())),
        }
    }
}

// ── the runner ──────────────────────────────────────────────────────────────

type AfterDone = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

/// One lock for the runner's bookkeeping — Python's `with self._lock` covers
/// check-and-set in one piece, so the same single lock carries the job
/// records, the active marker, and the queue. (Splitting them invites a
/// check-then-set race the Python shape structurally lacks.)
#[derive(Default)]
struct State {
    /// Live job records, in insertion order (the API publishes that order).
    order: Vec<String>,
    by_id: BTreeMap<String, Map<String, Value>>,
    /// queued-or-running job id — the "one at a time".
    active: Option<String>,
    queue: VecDeque<String>,
}

struct Inner {
    config: EngineConfig,
    log_path: PathBuf,
    after_done: Option<AfterDone>,
    backfill: bool,
    linger_s: f64,
    timeout_s: f64,
    rotate_bytes: u64,
    state: Mutex<State>,
    wake: (Mutex<bool>, Condvar),
    stopping: AtomicBool,
    /// The live writer, for `shutdown()`'s stop command.
    service: Mutex<Option<Arc<Mutex<IndexService>>>>,
}

/// `JobRunner`: serialized index jobs over `IndexService`; `jobs()`/`job()`
/// for the API, the log in JSONL with size rotation and boot replay.
pub struct JobRunner {
    inner: Arc<Inner>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl JobRunner {
    pub fn new(
        config: EngineConfig,
        config_dir: &Path,
        log_name: &str,
        after_done: Option<AfterDone>,
        backfill: bool,
        sync_timeout_s: f64,
        log_rotate_bytes: u64,
    ) -> Self {
        let inner = Arc::new(Inner {
            config,
            log_path: config_dir.join(log_name),
            after_done,
            backfill,
            linger_s: 8.0,
            timeout_s: sync_timeout_s,
            rotate_bytes: log_rotate_bytes,
            state: Mutex::new(State::default()),
            wake: (Mutex::new(false), Condvar::new()),
            stopping: AtomicBool::new(false),
            service: Mutex::new(None),
        });
        let _ = std::fs::create_dir_all(config_dir);
        inner.recover();
        let worker_inner = inner.clone();
        let worker = std::thread::spawn(move || run_jobs(worker_inner));
        Self {
            inner,
            worker: Mutex::new(Some(worker)),
        }
    }

    /// Queue one job (optionally with the engine's `--prune`). `JobBusy`
    /// (with the live id) when one is in flight.
    pub fn start(&self, prune: bool) -> Result<Map<String, Value>, JobBusy> {
        let inner = &self.inner;
        let id = random_hex12();
        let started = epoch_seconds();
        let mut job = Map::new();
        job.insert("id".into(), json!(id));
        job.insert("state".into(), json!(STATE_QUEUED));
        job.insert("started".into(), json!(started));
        if prune {
            job.insert("prune".into(), json!(true));
        }
        // The log record carries the job's prune flag; the in-memory state is
        // `queued` while the log line already says the job is running.
        let mut record = Map::new();
        record.insert("id".into(), json!(&id));
        record.insert("state".into(), json!(STATE_RUNNING));
        record.insert("started".into(), json!(started));
        if prune {
            record.insert("prune".into(), json!(true));
        }
        {
            let mut state = inner.state.lock().expect("the runner state");
            if state.active.is_some() {
                return Err(JobBusy(state.active.clone().expect("checked some")));
            }
            state.order.push(id.clone());
            state.by_id.insert(id.clone(), job.clone());
            state.active = Some(id.clone());
            state.queue.push_back(id.clone());
        }
        inner.append(&record);
        {
            let mut wake = inner.wake.0.lock().expect("the wake flag");
            *wake = true;
            inner.wake.1.notify_all();
        }
        Ok(job)
    }

    pub fn job(&self, job_id: &str) -> Option<Map<String, Value>> {
        self.inner
            .state
            .lock()
            .expect("the runner state")
            .by_id
            .get(job_id)
            .cloned()
    }

    pub fn jobs(&self) -> Vec<Map<String, Value>> {
        let state = self.inner.state.lock().expect("the runner state");
        state
            .order
            .iter()
            .filter_map(|id| state.by_id.get(id))
            .cloned()
            .collect()
    }

    /// Stop the worker. A running job is asked to stop **at the engine's safe
    /// point** (the `stop` command ends the walk between files) and is logged
    /// *interrupted*. Inside the stop grace this finishes the lifespan (143);
    /// a kill past the grace replays as *interrupted* at the next boot.
    pub fn shutdown(&self) {
        let inner = &self.inner;
        inner.stopping.store(true, Ordering::SeqCst);
        let service = inner.service.lock().expect("the service slot").clone();
        if let Some(service) = service {
            // Already gone is fine: the join below handles the worker.
            let _ = service.lock().expect("the writer").stop();
        }
        {
            let mut wake = inner.wake.0.lock().expect("the wake flag");
            *wake = true;
            inner.wake.1.notify_all();
        }
        // One file's embed at the safe point fits the 20 s join window. The
        // `mark` below takes the state lock for itself and releases it before
        // appending — Python's measured self-deadlock (holding a plain Lock
        // across the mark hung the lifespan on docker stop) is structurally
        // impossible here.
        let worker = self.worker.lock().expect("the worker slot").take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        let active = inner.state.lock().expect("the runner state").active.clone();
        if let Some(active) = active {
            inner.mark(
                &active,
                STATE_INTERRUPTED,
                Some("server stopped"),
                Map::new(),
            );
        }
    }
}

/// `_run_jobs`: the worker loop. Honors `stopping` on every wake — a
/// container stop is a 10 s grace, not 30.
fn run_jobs(inner: Arc<Inner>) {
    loop {
        if inner.stopping.load(Ordering::SeqCst) {
            break;
        }
        {
            let mut wake = inner.wake.0.lock().expect("the wake flag");
            while !*wake && !inner.stopping.load(Ordering::SeqCst) {
                wake = inner.wake.1.wait(wake).expect("the wake condvar");
            }
            *wake = false;
        }
        loop {
            let job_id = {
                let mut state = inner.state.lock().expect("the runner state");
                state.queue.pop_front()
            };
            let Some(job_id) = job_id else { break };
            // Any escape from run_one would strand `active` forever and wedge
            // all indexing behind a job that can never run — every later
            // request answering 409 until a restart. The runner marks instead.
            let outcome = inner.run_one(&job_id);
            *inner.service.lock().expect("the service slot") = None;
            {
                let mut state = inner.state.lock().expect("the runner state");
                state.active = None;
            }
            if let Err(error) = outcome {
                inner.mark(
                    &job_id,
                    STATE_INTERRUPTED,
                    Some(&format!("job runner: {error}")),
                    Map::new(),
                );
            }
        }
    }
}

impl Inner {
    /// `_run_one`: one job from pickup to its published ending.
    fn run_one(&self, job_id: &str) -> Result<(), String> {
        let (prune, started) = {
            let state = self.state.lock().expect("the runner state");
            let job = state.by_id.get(job_id).cloned().unwrap_or_default();
            (
                job.get("prune").and_then(Value::as_bool).unwrap_or(false),
                job.get("started").and_then(Value::as_f64),
            )
        };
        {
            let mut state = self.state.lock().expect("the runner state");
            if let Some(job) = state.by_id.get_mut(job_id) {
                // The worker has picked it up: GET /jobs says so.
                job.insert("state".into(), json!(STATE_RUNNING));
            }
        }
        // Counters exist from the first moment of the walk, at zero:
        // `shutdown()` can land during the model load, before the engine has
        // emitted a single `file` event, and the honest answer is "0 done",
        // never an absent counter.
        self.progress(
            job_id,
            false,
            integer_counts(&[("files_done", 0), ("files_failed", 0), ("errors", 0)]),
        );

        let mut service = IndexService::new(&self.config, self.backfill, prune, self.linger_s);
        let busy = service.start().err().map(|IndexBusy(why)| why);
        let service = Arc::new(Mutex::new(service));
        {
            let mut slot = self.service.lock().expect("the service slot");
            *slot = Some(service.clone());
        }
        if let Some(why) = busy {
            // Answered, not queued blind.
            {
                let mut state = self.state.lock().expect("the runner state");
                state.active = None;
            }
            *self.service.lock().expect("the service slot") = None;
            self.mark(job_id, STATE_BUSY, Some(&why), Map::new());
            service.lock().expect("the writer").finish();
            return Ok(());
        }

        // Progress is published AS IT ARRIVES, onto the live job record:
        // whichever path ends the job (the writer exiting, a timeout,
        // shutdown's "server stopped", an escaped exception) answers with the
        // counts the engine really emitted.
        let mut done = 0i64;
        let mut failed = 0i64;
        let mut errors = 0i64;
        let timeout = Duration::from_secs_f64(self.timeout_s);
        let mut end_reason: Option<String> = None;
        // The guard is scoped in a block, NOT a `match service.lock().sync()`
        // scrutinee: a scrutinee temporary lives to the END of the match, so the
        // `service.lock()` calls inside the event loop below re-enter the same
        // non-reentrant std Mutex and self-deadlock — measured 2026-11-13 as
        // every job hanging in `running` for its full timeout (the crawl had
        // completed, the record never moved). Python's `_mark` note at
        // jobs.py:137 documents the same trap from the other side.
        let sync_result = {
            let writer = service.lock().expect("the writer");
            writer.sync()
        }; // the guard ENDS here: the loop below takes the writer lock itself.
        match sync_result {
            Ok(()) => {
                while end_reason.is_none() {
                    match service.lock().expect("the writer").next_event(timeout) {
                        Ok(None) => break, // the helper's own exit event
                        Ok(Some(event)) => match event.get("event").and_then(Value::as_str) {
                            Some("file") => {
                                match event.get("state") {
                                    Some(Value::String(state)) if state == "done" => done += 1,
                                    Some(Value::String(_)) => failed += 1,
                                    _ => {}
                                }
                                self.progress(
                                    job_id,
                                    false,
                                    integer_counts(&[
                                        ("files_done", done),
                                        ("files_failed", failed),
                                    ]),
                                );
                            }
                            Some("error") => {
                                errors += 1;
                                self.progress(job_id, false, integer_counts(&[("errors", errors)]));
                            }
                            Some("idle") => {
                                // The engine's own batch totals ride into the
                                // log (a handful per job), so a record
                                // recovered after a hard kill carries the last
                                // totals the engine published.
                                let mut map = integer_counts(&[
                                    ("files_done", done),
                                    ("files_failed", failed),
                                    ("errors", errors),
                                ]);
                                for key in ["files", "searchable", "pending", "vectors"] {
                                    map.insert(
                                        key.into(),
                                        event.get(key).cloned().unwrap_or(Value::Null),
                                    );
                                }
                                self.progress(job_id, true, map);
                            }
                            _ => {}
                        },
                        Err(ProtocolError::Exited(exited)) => end_reason = Some(exited.to_string()),
                        Err(ProtocolError::Timeout(why)) => end_reason = Some(why),
                        Err(other) => return Err(other.to_string()),
                    }
                }
            }
            Err(ProtocolError::Exited(exited)) => end_reason = Some(exited.to_string()),
            Err(ProtocolError::Timeout(why)) => end_reason = Some(why),
            Err(other) => return Err(other.to_string()),
        }

        if let Some(why) = end_reason {
            // The writer is done: a new job may queue.
            {
                let mut state = self.state.lock().expect("the runner state");
                state.active = None;
            }
            *self.service.lock().expect("the service slot") = None;
            service.lock().expect("the writer").finish();
            let counts = self.live_counts(job_id);
            self.mark(job_id, STATE_INTERRUPTED, Some(&why), counts);
            return Ok(());
        }

        service.lock().expect("the writer").finish(); // idempotent: releases the lock
        *self.service.lock().expect("the service slot") = None;
        // The writer being finished IS the job being done: clear `active`
        // first, so a client that reads `done` can always start the next job
        // — never a done-then-409 window.
        {
            let mut state = self.state.lock().expect("the runner state");
            state.active = None;
        }

        let counts = self.live_counts(job_id);
        let ended = epoch_seconds();
        let started = started.unwrap_or(ended);
        // The done record's key order is `_mark`'s kwargs order: id, state, ended,
        // seconds, then the engine's counts (publish_order keeps this exact form).
        let mut extra = Map::new();
        extra.insert("ended".into(), json!(ended));
        extra.insert("seconds".into(), json!(round1(ended - started)));
        for (key, value) in counts {
            extra.insert(key, value);
        }
        if self.stopping.load(Ordering::SeqCst) {
            // Asked to stop mid-job (container stop): the engine closed the
            // stream at the safe point. Honest: *interrupted* with the
            // partial counts, never success, and no reload→prewarm — the
            // searcher is quitting right behind us.
            self.mark(
                job_id,
                STATE_INTERRUPTED,
                Some("server stopped during job"),
                extra,
            );
            return Ok(());
        }
        self.mark(job_id, STATE_DONE, None, extra.clone());
        if let Some(after_done) = &self.after_done {
            // The job is done and stays done — a refusal on the refresh
            // (ok:false, EngineExited, restart races, a prewarm past its 60 s
            // window) must not kill the runner thread. The next search gets
            // the searcher back through the adapter's own respawn.
            if let Err(exc) = after_done() {
                self.mark(
                    job_id,
                    STATE_DONE,
                    Some(&format!("searcher refresh deferred: {exc}")),
                    extra,
                );
            }
        }
        Ok(())
    }

    /// `_live_counts`: the engine's totals, read from the live job record —
    /// keys the engine never published read as None. One source, and a 1.8 M-
    /// file job carries no second copy of every event in a list.
    fn live_counts(&self, job_id: &str) -> Map<String, Value> {
        let job = {
            let state = self.state.lock().expect("the runner state");
            state.by_id.get(job_id).cloned().unwrap_or_default()
        };
        let mut map = Map::new();
        for key in [
            "files",
            "searchable",
            "pending",
            "vectors",
            "files_done",
            "files_failed",
            "errors",
        ] {
            map.insert(key.into(), job.get(key).cloned().unwrap_or(Value::Null));
        }
        map
    }

    /// `_progress`: merge the engine's counts into the live record while the
    /// job runs. In-memory by default — a per-file log line is not worth a
    /// 50 k-line log; `log=true` rides the engine's own `idle` totals.
    fn progress(&self, job_id: &str, log: bool, updates: Map<String, Value>) {
        {
            let mut state = self.state.lock().expect("the runner state");
            if let Some(job) = state.by_id.get_mut(job_id) {
                for (key, value) in &updates {
                    job.insert(key.clone(), value.clone());
                }
            }
        }
        if log {
            let mut record = Map::new();
            record.insert("id".into(), json!(job_id));
            record.insert("state".into(), json!(STATE_RUNNING));
            for (key, value) in &updates {
                record.insert(key.clone(), value.clone());
            }
            self.append(&record);
        }
    }

    /// `_mark`: publish the ending — in-memory and in the log. The persisted
    /// record echoes the request's prune flag.
    fn mark(&self, job_id: &str, state_name: &str, why: Option<&str>, extra: Map<String, Value>) {
        let prune = {
            let mut state = self.state.lock().expect("the runner state");
            let mut prune = false;
            if let Some(job) = state.by_id.get_mut(job_id) {
                for (key, value) in &extra {
                    job.insert(key.clone(), value.clone());
                }
                job.insert("state".into(), json!(state_name));
                if let Some(why) = why {
                    job.insert("why".into(), json!(why));
                }
                prune = job.get("prune").and_then(Value::as_bool).unwrap_or(false);
            }
            prune
        };
        let mut record = Map::new();
        record.insert("id".into(), json!(job_id));
        record.insert("state".into(), json!(state_name));
        for (key, value) in &extra {
            record.insert(key.clone(), value.clone());
        }
        if let Some(why) = why {
            record.insert("why".into(), json!(why));
        }
        if prune {
            record.insert("prune".into(), json!(true));
        }
        self.append(&record);
    }

    /// `_append`: rotate at size — the timestamp-suffixed rotation is ONE
    /// rename so replay order is a sort (the old chain of renames was
    /// O(rotations) and a kill mid-shift left it out of order); the live log
    /// (no suffix) is last by construction, the newest.
    fn append(&self, record: &Map<String, Value>) {
        let path = &self.log_path;
        if self.rotate_bytes > 0
            && std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > self.rotate_bytes
        {
            // ns-clock: two rotations in one second sort right.
            let stamp = now_ns().to_string();
            let mut rotated = format!("{}.{}", path.display(), stamp);
            while Path::new(&rotated).exists() {
                rotated = format!("{rotated}x");
            }
            let _ = std::fs::rename(path, &rotated);
        }
        let line = format!("{}\n", serde_json::to_string(record).expect("json"));
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = log.write_all(line.as_bytes());
        }
    }

    /// Every job log, oldest first: rotations sort by their ns-suffix, the
    /// live log (no suffix) is last.
    fn log_files(&self) -> Vec<PathBuf> {
        let dir = self.log_path.parent().unwrap_or(Path::new("."));
        let base = self
            .log_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let siblings: Vec<String> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut rotated: Vec<String> = siblings
            .into_iter()
            .filter(|name| name.starts_with(&format!("{base}.")) && *name != base)
            .collect();
        rotated.sort();
        let mut files: Vec<PathBuf> = rotated.into_iter().map(|name| dir.join(name)).collect();
        if self.log_path.exists() {
            files.push(self.log_path.clone());
        }
        files
    }

    /// `_recover`: replay the log (rotations included, oldest first) — a
    /// `running` job with no final line is *interrupted*. Never success.
    fn recover(&self) {
        let mut seen: Vec<(String, String)> = Vec::new();
        for path in self.log_files() {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in content.lines() {
                let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else {
                    continue; // a torn tail: half a line is not a record
                };
                let Some(Value::String(id)) = record.get("id") else {
                    continue; // valid JSON that is not a job record
                };
                let state = record
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                seen.push((id.clone(), state));
                let mut state_slot = self.state.lock().expect("the runner state");
                if !state_slot.order.contains(&id) {
                    state_slot.order.push(id.clone());
                }
                state_slot.by_id.insert(id.clone(), record);
            }
        }
        for (id, state) in seen {
            if state == STATE_RUNNING {
                self.mark(
                    &id,
                    STATE_INTERRUPTED,
                    Some("server restart: job never finished"),
                    Map::new(),
                );
            }
        }
    }
}

fn integer_counts(items: &[(&str, i64)]) -> Map<String, Value> {
    let mut map = Map::new();
    for (key, value) in items {
        map.insert((*key).into(), json!(value));
    }
    map
}

fn epoch_seconds() -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    (now * 1000.0).round() / 1000.0
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// `round(x, 1)` for the job's `seconds`.
fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// `make_runner`'s after_done: the searcher's reload → prewarm, the sequence
/// the contract suite proved (Stage 3, `test_reload_after_indexing`).
pub fn after_done_for(service: Arc<SearchService>) -> AfterDone {
    Box::new(move || {
        service.reload().map_err(|e| e.to_string())?;
        service.prewarm().map_err(|e| e.to_string())?;
        Ok(())
    })
}
