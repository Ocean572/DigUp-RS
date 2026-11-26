//! `digup index-service` (`IndexService`, IndexService.swift): the app's
//! indexing helper, one process per stretch of work.
//!
//! Protocol: one command per line on stdin, one JSON event per line on stdout.
//!   stdin:  sync | sync ["dir",…] | sync code | stop | backfill on | backfill off      (EOF = stop)
//!           The scoped `sync` (issue #7a) reconciles only the listed folders; a plain
//!           `sync` is the whole set of folders, byte-for-byte as ever, and outranks a
//!           pending scope.
//!   stdout: the events of `IndexService.swift` — missing, pruned, rescan,
//!           stopped, scanning, sync, file, rest, summary, waiting, idle,
//!           model, reindex, error (fatal), exit; events about the code index
//!           say "code": true.
//!
//! The loop: sync, index what the sync just queued (new files go first), then
//! the backfill if allowed; then the code index the same way; then the rest of
//! long files. Between batches it yields to stop and to "backfill off" at
//! once, and to a new sync once indexing has had about as long as the last
//! sync took (`yield_at`). When it's done it keeps a warm model for `linger`
//! seconds, then exits. Before the model has downloaded it only syncs, says
//! it's "waiting", and exits.
//!
//! The command thread speaks to the loop through atomics (the flags) and a
//! condvar (the wake) — Swift's NSLock over four Bools and a `yieldAt` date,
//! made concrete without handing the whole state to the stdin thread: the
//! loop owns the stores, the command thread only flips switches.
//!
//! Issue #155: the model takes its shape from the queue before any file is
//! read (`SharedEmbedder::note`); the code index is text alone. A queue read
//! that fails is answered conservatively: full, as it always was.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use digup_core::model::{FileKind, StoppedFile};
use digup_core::options::IndexOptions;
use digup_core::{IndexStore, CODE_FILE, MAIN_FILE};
use digup_runtime::indexer::{Event, Indexer, Step};
use digup_runtime::models::LlamaModels;
use digup_runtime::worker::Embedder;
use digup_runtime::{model_info, LlamaEmbedder};

use crate::emit::emit;
use crate::sharedembedder::SharedEmbedder;

/// The daemon's start shape (`indexService(_:)`'s Arguments). `options`/
/// `code_options` are `args.options` with the code index's overrides applied
/// by the CLI; `memory_limit`/`time_limit` configure the `Watchdog` (the CLI
/// starts it beside this).
pub struct IndexServiceConfig {
    pub index: PathBuf,
    pub roots: Vec<PathBuf>,
    pub code_roots: Vec<PathBuf>,
    pub options: IndexOptions,
    pub code_options: IndexOptions,
    pub prune: bool,
    pub backfill: bool,
    pub new_files_limit: i64,
    pub linger: f64,
}

/// One index: its store (inside its indexer), its indexer, its folders —
/// `IndexService.Part`.
struct Part {
    indexer: Indexer,
    roots: Vec<PathBuf>,
    keep_only: Option<Vec<PathBuf>>,
}

/// Everything the loop owns. Shared like the engine's class: one lock
/// (`Shared`'s `unsafe impl Send/Sync` names the lock as the safety, Swift's
/// `@unchecked Sendable`).
struct Serve {
    main: Part,
    code: Option<Part>,
    folder: PathBuf,
    model: Arc<SharedEmbedder>,
    /// The last report of folders that aren't there or can't be read, by
    /// index (said again only when it changes).
    unavailable: HashMap<bool, (Vec<String>, Vec<String>)>,
    /// A pass's progress by index: files done and the total so far, across
    /// the runs a pass takes. Cleared when the index goes idle.
    pass: HashMap<bool, (i64, i64)>,
}

/// The four command flags + the yield deadline, as atomics (see the header).
/// `yield_at` is epoch milliseconds; `i64::MIN` is `Date.distantPast`.
pub struct Flags {
    sync_requested: AtomicBool,
    code_sync_requested: AtomicBool,
    stop_requested: AtomicBool,
    backfill_allowed: AtomicBool,
    yield_at: AtomicI64,
    /// Issue #7a: the folders a scoped `sync [...]` asked to reconcile; empty means the
    /// whole set. A plain `sync` clears it — a full sync outranks a pending scope.
    sync_dirs: Mutex<Vec<String>>,
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(i64::MIN)
}

impl Flags {
    /// The flags' start shape: a fresh helper asks for the whole set of folders first
    /// (`syncRequested = true` — the first thing it does) with backfill as configured.
    /// The flags are public (issue #7a): the verb suite (`services/tests/verbs.rs`) pins
    /// the stdin seam.
    pub fn new(backfill: bool) -> Self {
        Self {
            sync_requested: AtomicBool::new(true), // the first thing it does
            code_sync_requested: AtomicBool::new(true),
            stop_requested: AtomicBool::new(false),
            backfill_allowed: AtomicBool::new(backfill),
            yield_at: AtomicI64::new(i64::MIN),
            sync_dirs: Mutex::new(Vec::new()),
        }
    }

    /// The engine's `{ defer { syncRequested = false }; return syncRequested }`.
    /// Issue #7a: a taken request answers its scope — the folders named by `sync [...]`
    /// lines that arrived since the last sync (merged, deduplicated), or an empty list for
    /// the whole set of folders.
    pub fn take_sync(&self) -> Option<Vec<PathBuf>> {
        if !self.sync_requested.swap(false, Ordering::SeqCst) {
            return None;
        }
        Some(
            self.sync_dirs
                .lock()
                .unwrap()
                .drain(..)
                .map(PathBuf::from)
                .collect(),
        )
    }

    pub fn take_code_sync(&self) -> bool {
        self.code_sync_requested.swap(false, Ordering::SeqCst)
    }

    pub fn stop(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst)
    }

    pub fn backfill(&self) -> bool {
        self.backfill_allowed.load(Ordering::SeqCst)
    }

    /// `Date() >= yieldAt`.
    fn yielded(&self) -> bool {
        epoch_ms() >= self.yield_at.load(Ordering::SeqCst)
    }

    /// The sync's `defer`: indexing gets at least as long as the sync before
    /// it, 2 s at the least.
    fn set_yield_after(&self, took: Duration) {
        let floor = took.max(Duration::from_secs(2));
        self.yield_at
            .store(epoch_ms() + floor.as_millis() as i64, Ordering::SeqCst);
    }

    /// `shouldStop` for a run: `stopRequested || (!isNew && !backfillAllowed)
    /// || Date() >= yieldAt && (syncRequested || part.code && codeSyncRequested)`.
    fn should_stop(&self, is_new: bool, code: bool) -> bool {
        let base = self.stop() || (!is_new && !self.backfill());
        let yield_to_sync = self.yielded()
            && (self.sync_requested.load(Ordering::SeqCst)
                || (code && self.code_sync_requested.load(Ordering::SeqCst)));
        base || yield_to_sync
    }

    /// `shouldStop` for `finishLongFiles` (the main index's only): `!backfill`
    /// joins the yield condition for the code sync only when a code index exists.
    fn should_stop_rest(&self, has_code: bool) -> bool {
        let yield_to_sync = self.yielded()
            && (self.sync_requested.load(Ordering::SeqCst)
                || (self.code_sync_requested.load(Ordering::SeqCst) && has_code));
        self.stop() || !self.backfill() || yield_to_sync
    }
}

/// The loop's lock + the wake pair. The engine's `Shared` shape: the flags
/// ride beside the lock (atomics, readable without it), the wake is a
/// condvar on its own little mutex — Swift's `wake` DispatchSemaphore, which
/// likewise never touches the state lock.
struct Shared {
    flags: Flags,
    lock: Mutex<Serve>,
    wake: Condvar,
    wake_lock: Mutex<()>,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

fn event(fields: Value) {
    emit(&fields);
}

/// `indexService(_:)` + `serve()`. Returns the process's exit code; the CLI
/// process-exits with it.
pub fn index_service(config: IndexServiceConfig) -> i32 {
    match run(config) {
        Ok(code) => code,
        Err(error) => {
            event(serde_json::json!({"event": "error", "message": error, "fatal": true}));
            1
        }
    }
}

fn run(config: IndexServiceConfig) -> Result<i32, String> {
    // Issue #155's shape knob: text-only keeps the 2048-token document budget
    // (only the media encoders are left out), so #155 changes no stored vector.
    let log = config.index.join(digup_core::paths::WORKER_LOG);
    // The Arc is a SAME-THREAD lifetime handle: the embedder wraps FFI
    // contexts that belong to the thread that opened them (the engine's
    // serial-queue confinement, reproduced), and the model never crosses a
    // thread — the daemon's own loop is its only user. clippy's Send/Sync
    // advice is about sharing; this Arc is never shared.
    #[allow(clippy::arc_with_non_send_sync)] // same-thread lifetime handle, never shared
    let model = Arc::new(SharedEmbedder::new(Arc::new(move |text_only: bool| {
        let models = LlamaModels::locate()?;
        let context = if text_only { Some(2048usize) } else { None };
        let embedder = LlamaEmbedder::new(&models, text_only, context, Some(&log))?;
        #[allow(clippy::arc_with_non_send_sync)] // same-thread lifetime handle, never shared
        Ok(Arc::new(embedder) as Arc<dyn Embedder>)
    })));
    let main = make_part(
        &config,
        &model,
        MAIN_FILE,
        &config.roots,
        config.options.clone(),
    )?;
    let code = if config.code_roots.is_empty() {
        None
    } else {
        Some(make_part(
            &config,
            &model,
            CODE_FILE,
            &config.code_roots,
            config.code_options.clone(),
        )?)
    };
    let serve = Serve {
        main,
        code,
        folder: config.index.clone(),
        model,
        unavailable: HashMap::new(),
        pass: HashMap::new(),
    };
    let shared = Arc::new(Shared {
        flags: Flags::new(config.backfill),
        lock: Mutex::new(serve),
        wake: Condvar::new(),
        wake_lock: Mutex::new(()),
    });
    serve_loop(&shared, config.new_files_limit, config.linger)
}

/// `IndexService.Part` + `indexService`'s `part(_:)`: the store, its indexer
/// (costs beside the index, the model's known source, the `reindex` event on a
/// model change), the folders, `--prune`'s keep-only.
fn make_part(
    config: &IndexServiceConfig,
    model: &Arc<SharedEmbedder>,
    file: &str,
    roots: &[PathBuf],
    options: IndexOptions,
) -> Result<Part, String> {
    let store = IndexStore::open(&config.index, file).map_err(|e| e.to_string())?;
    let embedder_owner = model.clone();
    let make_embedder: digup_runtime::indexer::MakeEmbedder = Box::new(move || {
        let owner = embedder_owner.clone();
        owner.embedder()
    });
    let mut indexer =
        Indexer::new(store, options.clone(), make_embedder).map_err(|e| e.to_string())?;
    indexer.costs_folder = Some(config.index.clone()); // measured costs, for later estimates
    indexer.embedder_source = Some(model_info().vector_source()); // known without starting one
    let code = options.code;
    indexer.reset_on_model_change = Some(Box::new(move |indexed: &str, now: &str| {
        event(serde_json::json!({"event": "reindex", "from": indexed, "to": now, "code": code}));
    }));
    Ok(Part {
        indexer,
        roots: roots.to_vec(),
        keep_only: if config.prune {
            Some(roots.to_vec())
        } else {
            None
        },
    })
}

/// `readCommands()`: the stdin thread. Sets the flags and signals the wake;
/// EOF means the app went away — stop.
fn read_commands(shared: &Arc<Shared>) {
    let shared = shared.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lines() {
            let Ok(line) = line else { break };
            apply_verb(&shared.flags, line.trim());
            shared.wake_signal();
        }
        shared.flags.stop_requested.store(true, Ordering::SeqCst); // the app went away
        shared.wake_signal();
    });
}

/// Issue #7a: one stdin line, applied to the flags. The verbs are the engine's pinned set;
/// the scoped `sync ["<dir>", …]` is the shell watcher's addition (dirs merged,
/// deduplicated, for the next sync — a line that is not a JSON array of strings is passed
/// over, as every unknown verb is). Public: the verb suite (`services/tests/verbs.rs`)
/// pins this exact seam.
pub fn apply_verb(flags: &Flags, line: &str) {
    match line {
        "sync" => {
            // Issue #7a: the whole set of folders outranks a pending scope.
            flags.sync_dirs.lock().unwrap().clear();
            flags.sync_requested.store(true, Ordering::SeqCst);
        }
        "sync code" => flags.code_sync_requested.store(true, Ordering::SeqCst),
        "stop" => flags.stop_requested.store(true, Ordering::SeqCst),
        "backfill on" => flags.backfill_allowed.store(true, Ordering::SeqCst),
        "backfill off" => flags.backfill_allowed.store(false, Ordering::SeqCst),
        _ => {
            if let Some(rest) = line.strip_prefix("sync ") {
                if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(rest) {
                    let dirs: Vec<String> = items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect();
                    if !dirs.is_empty() {
                        let mut pending = flags.sync_dirs.lock().unwrap();
                        for dir in dirs {
                            if !pending.contains(&dir) {
                                pending.push(dir);
                            }
                        }
                        flags.sync_requested.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }
}

impl Shared {
    fn wake_signal(&self) {
        let _guard = self.wake_lock.lock().expect("the wake lock");
        self.wake.notify_all();
    }
}

/// `serve()`'s main loop, on the thread that owns the stores. Swift takes
/// its lock per flag read; here the flags are atomics and the loop holds the
/// store lock for its stretches — the command thread never touches the
/// stores, so nothing waits on it.
fn serve_loop(shared: &Arc<Shared>, new_files_limit: i64, linger: f64) -> Result<i32, String> {
    // serve(): stale scratch folders first, then the startup pass (stopped
    // note, rescan, model check, prune). A startup failure is fatal: the
    // event says so and the helper ends.
    remove_stale_scratch_folders();
    let mut state = shared.lock.lock().expect("the serve lock");
    if let Err(error) = startup(&state) {
        finish_all(&mut state);
        event(serde_json::json!({"event": "error", "message": error, "fatal": true}));
        return Ok(1);
    }
    drop(state);

    // `Thread.detachNewThread { readCommands() }` (IndexService.swift:104):
    // the stdin thread starts AFTER the startup pass, and the loop's first
    // flag consumption (the `defer { syncRequested = false }` at the loop
    // top) lands before the reader thread can set the flag. A client that
    // writes `sync` right after spawning it — the server's `start(); sync()`
    // — is therefore a RE-request, not part of the first sync: the pass
    // yields at `yield_at`, re-syncs, and the batch summaries land as the
    // suite pins them. Measured 2026-11-13: a reader started before the
    // startup pass merged that line into the first latch — one batch, six
    // files, ONE summary (the suite asks for >= 2).
    read_commands(shared);

    let mut new_files: HashSet<i64> = HashSet::new();
    let mut new_code: HashSet<i64> = HashSet::new();
    let flags = &shared.flags;
    let mut state = shared.lock.lock().expect("the serve lock");
    let mut code_counted = state.code.is_none();
    loop {
        if let Some(scope) = flags.take_sync() {
            sync_part(
                &mut state,
                flags,
                false,
                &mut new_files,
                new_files_limit,
                &scope,
            );
        }
        // The code folders are crawled with the first sync, so their counts
        // show from the start; their files are read once the main index has
        // nothing waiting, below.
        if !code_counted && state.code.is_some() {
            code_counted = true;
            if flags.take_code_sync() {
                // The code index keeps its full-folder sync: the code watcher is
                // the cheap walk, and issue #7a scopes only the media one.
                sync_part(&mut state, flags, true, &mut new_code, new_files_limit, &[]);
            }
        }
        if flags.stop() {
            break;
        }
        if !model_ready() {
            // Nothing to embed with yet; the app starts another helper once
            // the download is done.
            let mut extra = Map::new();
            extra.insert("for".into(), Value::String("model".to_string()));
            emit_counts(&state, "waiting", extra);
            break;
        }
        if !new_files.is_empty() {
            if !run_part(&mut state, flags, false, Some(&new_files)) {
                break;
            }
            new_files = left(&state, flags, false, &new_files);
            continue;
        }
        if pending_count(&state, false) > 0 && flags.backfill() {
            if !run_part(&mut state, flags, false, None) {
                break;
            }
            if flags.sync_requested.load(Ordering::SeqCst) {
                continue;
            }
        }
        // The code index once the main one has nothing waiting: its own sync,
        // what it just queued at once, the rest under the backfill's rules.
        if state.code.is_some() && (pending_count(&state, false) == 0 || !flags.backfill()) {
            if flags.take_code_sync() {
                sync_part(&mut state, flags, true, &mut new_code, new_files_limit, &[]);
            }
            if !new_code.is_empty() {
                if !run_part(&mut state, flags, true, Some(&new_code)) {
                    break;
                }
                new_code = left(&state, flags, true, &new_code);
                continue;
            }
            if pending_count(&state, true) > 0 && flags.backfill() {
                if !run_part(&mut state, flags, true, None) {
                    break;
                }
                if flags.sync_requested.load(Ordering::SeqCst)
                    || flags.code_sync_requested.load(Ordering::SeqCst)
                {
                    continue;
                }
            }
        }
        // Once nothing is waiting: the rest of long PDFs and documents.
        if flags.backfill()
            && pending_count(&state, false) == 0
            && pending_count(&state, true) == 0
            && unfinished_files(&state) > 0
        {
            if !finish_long_files(&mut state, flags) {
                break;
            }
            if flags.sync_requested.load(Ordering::SeqCst)
                || flags.code_sync_requested.load(Ordering::SeqCst)
            {
                continue;
            }
        }
        if state.code.is_some() && flags.code_sync_requested.load(Ordering::SeqCst) {
            continue;
        }
        state.pass = HashMap::new(); // the pass is over: the next one counts from zero
        emit_counts(&state, "idle", Map::new());
        // Nothing warm to keep around: leave now. Else wait a little for more work.
        if !state.model.is_running() {
            break;
        }
        if !wait_for_work(shared, &state, flags, linger) {
            break;
        }
    }
    finish_all(&mut state);
    drop(state);
    event(serde_json::json!({"event": "exit"}));
    Ok(0)
}

/// `serve()`'s startup pass: the stopped-file note (a file an earlier helper
/// had to stop on isn't read again until it changes), the once-per-index
/// rescan, the model check, the `--prune` folders.
fn startup(state: &Serve) -> Result<(), String> {
    if let Some(stopped) = StoppedFile::take(&state.folder) {
        let parts: Vec<(&Part, bool)> = {
            let mut parts = vec![(&state.main, false)];
            if let Some(code) = state.code.as_ref() {
                parts.push((code, true));
            }
            parts
        };
        if let Some(path) = &stopped.path {
            for (part, code) in &parts {
                if part
                    .indexer
                    .store
                    .mark_stopped(path, &stopped.why)
                    .map_err(|e| e.to_string())?
                {
                    event(
                        serde_json::json!({"event": "stopped", "path": path, "why": stopped.why, "code": code}),
                    );
                }
            }
        } else {
            // A note without a path is "the helper died on the file it was
            // reading" (`CrashNotes`). The app names that file for the next
            // helper (`DIGUP_STOPPED_FILE`); the queue's head is *not*
            // necessarily it — marking the head stays for a spawn that
            // carries no name, and marks nothing when the queue has moved on.
            let mut marked = false;
            if let Ok(named) = std::env::var("DIGUP_STOPPED_FILE") {
                for (part, code) in &parts {
                    if part
                        .indexer
                        .store
                        .mark_stopped(&named, &stopped.why)
                        .map_err(|e| e.to_string())?
                    {
                        event(
                            serde_json::json!({"event": "stopped", "path": named, "why": stopped.why, "code": code}),
                        );
                        marked = true;
                    }
                }
            }
            if !marked {
                let head = state
                    .main
                    .indexer
                    .store
                    .pending(Some(1), None)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .next();
                if let Some(head) = head {
                    state
                        .main
                        .indexer
                        .store
                        .mark_stopped(&head.path, &stopped.why)
                        .map_err(|e| e.to_string())?;
                    event(
                        serde_json::json!({"event": "stopped", "path": head.path, "why": stopped.why}),
                    );
                }
            }
        }
    }
    let again = state
        .main
        .indexer
        .store
        .read_again_once()
        .map_err(|e| e.to_string())?;
    if again > 0 {
        event(serde_json::json!({"event": "rescan", "files": again}));
    }
    let parts: Vec<(&Part, bool)> = {
        let mut parts = vec![(&state.main, false)];
        if let Some(code) = state.code.as_ref() {
            parts.push((code, true));
        }
        parts
    };
    for (part, code) in parts {
        part.indexer.check_model().map_err(|e| e.to_string())?;
        if let Some(keep) = &part.keep_only {
            let keep: Vec<String> = keep
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let pruned = part
                .indexer
                .store
                .prune_keeping_under(&keep)
                .map_err(|e| e.to_string())?;
            if pruned > 0 {
                event(serde_json::json!({"event": "pruned", "files": pruned, "code": code}));
            }
        }
    }
    Ok(())
}

fn part(state: &Serve, code: bool) -> &Part {
    if code {
        state.code.as_ref().expect("the code part")
    } else {
        &state.main
    }
}

fn part_mut(state: &mut Serve, code: bool) -> &mut Part {
    if code {
        state.code.as_mut().expect("the code part")
    } else {
        &mut state.main
    }
}

/// `(try? part.store.pendingCount()) ?? 0` — a missing code index is 0 too
/// (`(try? code?.store.pendingCount()) ?? 0`).
fn pending_count(state: &Serve, code: bool) -> i64 {
    if code && state.code.is_none() {
        return 0;
    }
    part(state, code).indexer.store.pending_count().unwrap_or(0)
}

/// `(try? main.store.unfinishedCount().files) ?? 0`.
fn unfinished_files(state: &Serve) -> i64 {
    state
        .main
        .indexer
        .store
        .unfinished_count()
        .map(|(files, _)| files)
        .unwrap_or(0)
}

/// `modelReady`: `embedderSource() == nil || LlamaModels.complete` — the
/// portable source is always known (the in-process model), so it's the files.
fn model_ready() -> bool {
    LlamaModels::complete()
}

/// `availableRoots(_:)`: the part's folders that are there and readable right
/// now; also what `pending` and the counts look at. The missing report is
/// said again only when it changes (a drive ejected mid-run must not crawl
/// as "all deleted").
fn available_roots(state: &mut Serve, code: bool) -> Result<Vec<PathBuf>, String> {
    let roots: Vec<PathBuf> = part(state, code).roots.clone();
    let missing: Vec<PathBuf> = roots
        .iter()
        .filter(|folder| !folder.is_dir())
        .cloned()
        .collect();
    let unreadable: Vec<PathBuf> = roots
        .iter()
        .filter(|folder| {
            !missing.contains(folder) && !digup_core::crawler::can_list(&folder.to_string_lossy())
        })
        .cloned()
        .collect();
    let report = (
        missing
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        unreadable
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    );
    let say = match state.unavailable.get(&code) {
        Some(previous) => *previous != report,
        None => !(report.0.is_empty() && report.1.is_empty()),
    };
    if say {
        event(
            serde_json::json!({"event": "missing", "folders": report.0, "unreadable": report.1, "code": code}),
        );
    }
    state.unavailable.insert(code, report);
    let available: Vec<PathBuf> = roots
        .into_iter()
        .filter(|folder| !missing.contains(folder) && !unreadable.contains(folder))
        .collect();
    part(state, code)
        .indexer
        .store
        .set_scope(Some(
            available
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        ))
        .map_err(|e| e.to_string())?;
    Ok(available)
}

/// `sync(_ part:, into:)`: crawl the part's available folders, queue what
/// changed, report. A sync's failure is an event, not the run's end.
/// Issue #7a: `scope` is the watcher's folders when a scoped `sync [...]` asked
/// for them — the crawl walks those, and removals prune only the rows under them;
/// an empty scope is every folder, byte-for-byte the engine's original behavior.
fn sync_part(
    state: &mut Serve,
    flags: &Flags,
    code: bool,
    new_files: &mut HashSet<i64>,
    new_files_limit: i64,
    scope: &[PathBuf],
) {
    let started = SystemTime::now();
    let instant = Instant::now();
    let result = match available_roots(state, code) {
        Ok(roots) => {
            event(serde_json::json!({"event": "scanning", "checked": 0, "code": code}));
            let mut progress = move |p: &digup_core::crawler::CrawlProgress| {
                let mut fields = Map::new();
                fields.insert("event".into(), Value::String("scanning".to_string()));
                fields.insert("checked".into(), serde_json::json!(p.checked));
                fields.insert("code".into(), Value::Bool(code));
                if let Some(root) = &p.root {
                    fields.insert("root".into(), Value::String(root.clone()));
                }
                event(Value::Object(fields));
            };
            let walk: Vec<PathBuf> = if scope.is_empty() {
                roots.clone()
            } else {
                scope.to_vec()
            };
            part(state, code)
                .indexer
                .sync_scoped(&roots, &walk, &mut progress)
                .map_err(|e| e.to_string())
        }
        Err(error) => Err(error),
    };
    // The sync's `defer`: indexing gets at least as long as the sync took.
    flags.set_yield_after(instant.elapsed());
    match result {
        Ok(summary) => {
            if (summary.queued.len() as i64) <= new_files_limit {
                new_files.extend(summary.queued.iter().copied());
            }
            let mut extras = Map::new();
            extras.insert("code".into(), Value::Bool(code));
            extras.insert("new".into(), serde_json::json!(new_files.len()));
            extras.insert("added".into(), serde_json::json!(summary.added));
            extras.insert("changed".into(), serde_json::json!(summary.changed));
            extras.insert("moved".into(), serde_json::json!(summary.moved));
            extras.insert("removed".into(), serde_json::json!(summary.removed));
            extras.insert("unchanged".into(), serde_json::json!(summary.unchanged));
            extras.insert(
                "ms".into(),
                serde_json::json!((instant.elapsed().as_secs_f64() * 1000.0) as i64),
            );
            // The app checks its sync request came before this.
            extras.insert(
                "started".into(),
                serde_json::json!(started
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0)),
            );
            extras.insert("left_out".into(), serde_json::json!(summary.crawl.left_out));
            emit_counts(state, "sync", extras);
        }
        Err(error) => {
            event(
                serde_json::json!({"event": "error", "message": format!("sync failed: {error}"), "code": code}),
            );
        }
    }
}

/// `run(_ part:, only:)`: embed the part's pending files. False when
/// indexing has to stop (the model died, or the index was built with other
/// settings). Issue #155: the model takes its shape from the queue before any
/// file is read; a queue read that fails is answered conservatively: full.
fn run_part(state: &mut Serve, flags: &Flags, code: bool, ids: Option<&HashSet<i64>>) -> bool {
    let started = Instant::now();
    let is_new = ids.is_some();
    if !code {
        let kinds = part(state, code)
            .indexer
            .store
            .pending_kinds(ids)
            .unwrap_or_else(|_| {
                [
                    FileKind::Screenshot,
                    FileKind::Image,
                    FileKind::Pdf,
                    FileKind::Doc,
                    FileKind::Audio,
                    FileKind::Video,
                    FileKind::Code,
                ]
                .into_iter()
                .collect()
            });
        let needs_media = kinds.iter().any(FileKind::needs_media_encoders);
        state.model.note(needs_media);
    }
    // Cumulative over the pass: what earlier runs did, plus everything
    // pending now (what a sync just added too).
    let so_far = state.pass.get(&code).copied().unwrap_or((0, 0));
    let mut total = so_far.1.max(so_far.0 + pending_count(state, code));
    let mut done_this_run: i64 = 0;
    let on_event = &mut |event: &Event| {
        done_this_run = event.done as i64;
        total = total.max(so_far.0 + event.total as i64);
        emit(&serde_json::json!({
            "event": "file",
            "new": is_new,
            "done": so_far.0 + event.done as i64,
            "total": total,
            "path": event.path,
            "kind": event.kind.as_str(),
            "state": event.state,
            "seconds": event.seconds,
            "note": event.note.clone().unwrap_or_default(),
            "code": code,
        }));
    };
    let should_stop = &|| flags.should_stop(is_new, code);
    let result = part_mut(state, code)
        .indexer
        .run(None, ids, should_stop, on_event);
    state.pass.insert(code, (so_far.0 + done_this_run, total));
    match result {
        Ok(states) => {
            if !states.is_empty() {
                emit(&serde_json::json!({
                    "event": "summary",
                    "new": is_new,
                    "states": states,
                    "code": code,
                    "ms": (started.elapsed().as_secs_f64() * 1000.0) as i64,
                }));
            }
            true
        }
        Err(error) => {
            emit(
                &serde_json::json!({"event": "error", "message": error.to_string(), "fatal": true, "code": code}),
            );
            false
        }
    }
}

/// `finishLongFiles()`: the rest of long files, yielding as the backfill does.
fn finish_long_files(state: &mut Serve, flags: &Flags) -> bool {
    let started = Instant::now();
    let has_code = state.code.is_some();
    let on_step = &mut |step: &Step| {
        emit(&serde_json::json!({
            "event": "rest",
            "done": step.done,
            "total": step.total,
            "files": step.files,
            "path": step.path,
            "kind": step.kind.as_str(),
            "state": step.state,
            "seconds": step.seconds,
            "note": step.note.clone().unwrap_or_default(),
        }));
    };
    let should_stop = &|| flags.should_stop_rest(has_code);
    match state.main.indexer.finish_long_files(should_stop, on_step) {
        Ok(states) => {
            if !states.is_empty() {
                emit(&serde_json::json!({
                    "event": "summary",
                    "rest": true,
                    "states": states,
                    "ms": (started.elapsed().as_secs_f64() * 1000.0) as i64,
                }));
            }
            true
        }
        Err(error) => {
            emit(
                &serde_json::json!({"event": "error", "message": error.to_string(), "fatal": true}),
            );
            false
        }
    }
}

/// `left(_:of:)`: new files a run stopped before (a sync or stop came first),
/// kept for the next round. Done: anything still pending joins the backfill.
fn left(state: &Serve, flags: &Flags, code: bool, files: &HashSet<i64>) -> HashSet<i64> {
    let yielded = flags.sync_requested.load(Ordering::SeqCst)
        || flags.stop()
        || (code && flags.code_sync_requested.load(Ordering::SeqCst));
    if !yielded {
        return HashSet::new();
    }
    part(state, code)
        .indexer
        .store
        .pending(None, Some(files))
        .unwrap_or_default()
        .into_iter()
        .map(|file| file.id)
        .collect()
}

/// `waitForWork(_:of:)`: waits up to `linger` seconds for something to do.
/// False: time's up, or told to stop.
fn wait_for_work(shared: &Shared, state: &Serve, flags: &Flags, linger: f64) -> bool {
    let deadline = Instant::now() + Duration::from_secs_f64(linger);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let guard = shared.wake_lock.lock().expect("the wake lock");
        let (guard, timed_out) = shared
            .wake
            .wait_timeout(guard, remaining)
            .expect("the wake lock");
        drop(guard);
        if timed_out.timed_out() {
            return false;
        }
        let (sync, code_sync, stop, backfill) = (
            flags.sync_requested.load(Ordering::SeqCst),
            flags.code_sync_requested.load(Ordering::SeqCst),
            flags.stop(),
            flags.backfill(),
        );
        if stop {
            return false;
        }
        if sync || (code_sync && state.code.is_some()) {
            return true;
        }
        if backfill
            && (pending_count(state, false) > 0
                || pending_count(state, true) > 0
                || unfinished_files(state) > 0)
        {
            return true;
        }
    }
}

/// `emitCounts(_:_)`: an event with both indexes' counts — the main one's as
/// they are, the code index's prefixed "code_".
fn emit_counts(state: &Serve, event_name: &str, extra: Map<String, Value>) {
    let mut fields = extra;
    fields.insert("event".into(), Value::String(event_name.to_string()));
    let mut parts: Vec<(&Part, bool)> = vec![(&state.main, false)];
    if let Some(code) = state.code.as_ref() {
        parts.push((code, true));
    }
    for (part, code) in parts {
        let Ok(counts) = part.indexer.store.counts() else {
            continue;
        };
        let prefix = if code { "code_" } else { "" };
        let (mut files, mut searchable, mut pending) = (0i64, 0i64, 0i64);
        for states in counts.by_kind_and_state.values() {
            for (state_name, count) in states {
                files += count;
                if state_name == "done" || state_name == "partial" {
                    searchable += count;
                }
                if state_name == "pending" {
                    pending += count;
                }
            }
        }
        fields.insert(format!("{prefix}files"), serde_json::json!(files));
        fields.insert(format!("{prefix}searchable"), serde_json::json!(searchable));
        fields.insert(format!("{prefix}pending"), serde_json::json!(pending));
        fields.insert(
            format!("{prefix}vectors"),
            serde_json::json!(counts.vectors),
        );
        if !code {
            fields.insert("unfinished".into(), serde_json::json!(counts.unfinished));
        }
    }
    event(Value::Object(fields));
}

/// `finish()`: both indexers stop their worker and clear their scratch
/// folders, the shared model goes.
fn finish_all(state: &mut Serve) {
    state.main.indexer.finish();
    if let Some(code) = state.code.as_mut() {
        code.indexer.finish();
    }
    state.model.close();
}

/// `removeStaleScratchFolders()`: scratch folders ($TMPDIR/DigUp-<pid>) of
/// indexing processes that are gone (killed mid-batch).
fn remove_stale_scratch_folders() {
    let temp = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("DigUp-") else {
            continue;
        };
        let Ok(pid) = rest.parse::<i32>() else {
            continue;
        };
        if pid == std::process::id() as i32 || !is_gone(pid) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// `isGone(_:)`: no process has this id now (`kill(pid, 0)` fails with ESRCH).
#[cfg(unix)]
pub(crate) fn is_gone(pid: i32) -> bool {
    unsafe {
        libc::kill(pid, 0) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

/// Windows has no `kill`: the libc crate maps `kill` to SIGNAL DELIVERY
/// (TerminateProcess) there, so `kill(pid, 0)` would answer "gone" for
/// every live pid and delete the live helpers' temp dirs. The liveness
/// question, asked of the NT kernel directly: `OpenProcess` with the
/// query-limited right — a live pid answers a handle (closed here), a
/// dead one answers NULL. (The libc crate's own `OpenProcess` is gated to
/// msvc only — measured by the gnu cross-check — so the probe rides
/// windows-sys, which compiles on every windows target.)
#[cfg(windows)]
pub(crate) fn is_gone(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() {
            true
        } else {
            CloseHandle(handle);
            false
        }
    }
}
