//! `digup search-service` (`SearchService`, SearchService.swift): the app's
//! searcher as a process, for shells that can't link DigUpKit. It keeps the
//! index open and the query encoder warm, so a search while typing costs what
//! it costs in the Mac app (tens of milliseconds), not a process start.
//!
//! Protocol: one JSON request per line on stdin, one JSON response per line on
//! stdout, carrying the request's `id` — the verbs, replies and events are as
//! `SearchService.swift` documents them (SEARCH_OPS in lib.rs).
//!
//! The encoder exits after `--encoder-idle` seconds without a query and the
//! vectors are released; keyword search keeps working and the next query
//! brings both back. After twice that time the process itself exits.
//!
//! The engine's `makeEmbedder` starts the mlx-vlm worker process when
//! `DIGUP_WORKER_DIR` is set — the Apple-Silicon eval reference the server
//! never uses; on this target the encoder is llama.cpp in this process
//! (text-only), as the portable engine's default is.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::{Map, Value};

use digup_core::model::{FileKind, NameQuery};
use digup_core::searcher::{SearchHit, SearchOptions, Searcher};
use digup_core::{folder_selection, IndexStore, CODE_FILE};
use digup_runtime::prompts::{code_query, query};
use digup_runtime::worker::Embedder;
use digup_runtime::{LlamaEmbedder, LlamaModels};

use crate::emit::emit;

/// The daemon's start shape (`searchService(_:)`'s Arguments). Paths arrive
/// engine-normalized (`folder_url`); `types` is `IndexOptions.extensions`' set.
pub struct SearchConfig {
    pub index: PathBuf,
    pub roots: Vec<String>,
    pub skipped: Vec<String>,
    pub types: HashSet<String>,
    pub code_roots: Vec<String>,
    pub code_skipped: Vec<String>,
    pub idle_seconds: f64,
}

/// `Arguments.folderURL(_:)` (main.swift:127): `~` expanded, the path
/// standardized (`standardizedFileURL` — `.` and `..` removed lexically, the
/// trailing slash dropped) — `paths::standardize`, the core's implementation.
pub fn folder_url(path: &str) -> String {
    let expanded = if path == "~" {
        home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        let joined = Path::new(&home()).join(rest);
        joined.to_string_lossy().into_owned()
    } else {
        path.to_string()
    };
    // `URL(fileURLWithPath:)` makes a RELATIVE argument absolute against the
    // process's CWD before standardizing (main.swift:127) — the daemon's root
    // strings ride that too, and the index rows carry them (AGENTS.md's
    // out-of-scope rule: rows carry the roots they were crawled from; a
    // relative launch must name the folder it resolves to).
    let absolute = if expanded.starts_with('/') {
        expanded
    } else {
        let joined = std::env::current_dir().unwrap_or_default().join(&expanded);
        joined.to_string_lossy().into_owned()
    };
    digup_core::paths::standardize(&absolute)
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// The daemon's whole state, behind one lock — the class's `@unchecked
/// Sendable` guarded by its `NSLock`. Every access goes through `lock()`,
/// main loop and idle-watch alike; the `unsafe` says what Swift's attribute
/// says: the lock is the safety, not the compiler.
struct Shared(Mutex<State>);
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

struct State {
    directory: PathBuf,
    store: &'static IndexStore,
    searcher: Searcher<'static>,
    code_store: Option<&'static IndexStore>,
    code_searcher: Option<Searcher<'static>>,
    roots: Vec<String>,
    skipped: Vec<String>,
    types: HashSet<String>,
    code_roots: Vec<String>,
    code_skipped: Vec<String>,
    encoder: Option<Box<dyn Embedder>>,
    encoder_state: String,
    vector_cache: HashMap<String, Vec<f32>>,
    last_query: Instant,
    warned_about_source: bool,
    log: PathBuf,
}

/// Events and replies alike go through the engine's `output` lock: one line at
/// a time on stdout, from the request loop, the idle watch and the render
/// threads alike.
fn write_out(fields: Value) {
    let _guard = OUTPUT.lock().expect("the output lock");
    emit(&fields);
}

static OUTPUT: Mutex<()> = Mutex::new(());

/// `stopEncoder()`: the model's memory back to the system, the query cache
/// with it. An `unavailable` state is the load's own, not a stop's.
fn stop_encoder(state: &mut State) {
    if let Some(encoder) = state.encoder.as_mut() {
        encoder.close();
    }
    state.encoder = None;
    state.vector_cache.clear();
    if state.encoder_state != "unavailable" {
        state.encoder_state = "stopped".to_string();
    }
}

/// `runningEncoder()`: the warm encoder, loading it once when needed and
/// saying so (the `encoder` event, its `seconds` to hundredths).
fn running_encoder(state: &mut State) -> bool {
    if state.encoder.is_some() {
        return true;
    }
    if state.encoder_state == "unavailable" {
        return false;
    }
    state.encoder_state = "starting".to_string();
    let started = Instant::now();
    let made = LlamaModels::locate()
        .and_then(|models| LlamaEmbedder::new(&models, true, None, Some(&state.log)));
    match made {
        Ok(worker) => {
            let source = worker.info().vector_source();
            state.encoder = Some(Box::new(worker));
            state.encoder_state = "ready".to_string();
            let seconds = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
            write_out(serde_json::json!({
                "event": "encoder", "state": "ready", "seconds": seconds, "source": source,
            }));
            true
        }
        Err(error) => {
            state.encoder_state = "unavailable".to_string();
            write_out(serde_json::json!({
                "event": "encoder", "state": "unavailable",
                "error": error.to_string().chars().take(300).collect::<String>(),
            }));
            false
        }
    }
}

/// `vector(for:code:)`: the query's vector, cached per shape; the encoder
/// failure prints and stops the encoder (the next query starts a new one).
fn vector_for(state: &mut State, text: &str, code: bool) -> Option<Vec<f32>> {
    state.last_query = Instant::now();
    let key = if code {
        format!("code\u{0}{text}")
    } else {
        text.to_string()
    };
    if let Some(cached) = state.vector_cache.get(&key) {
        return Some(cached.clone());
    }
    if !running_encoder(state) {
        return None;
    }
    let prompt = if code { code_query(text) } else { query(text) };
    let made = state
        .encoder
        .as_ref()
        .expect("the encoder running_encoder just made")
        .embed_texts(&[prompt]);
    match made {
        Ok(rows) => {
            let values = rows.into_iter().next().unwrap_or_default();
            if state.vector_cache.len() > 256 {
                state.vector_cache.clear();
            }
            state.vector_cache.insert(key, values.clone());
            Some(values)
        }
        Err(error) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "encoder: {error}");
            stop_encoder(state);
            None
        }
    }
}

/// `hitFields` (SearchService.swift:365): a hit as JSON — what `digup search
/// --json` prints, plus segment_id, modified, size and info.
pub fn hit_fields(hit: &SearchHit) -> Value {
    let mut row = Map::new();
    row.insert("path".into(), Value::String(hit.path.clone()));
    row.insert("kind".into(), Value::String(hit.kind.as_str().to_string()));
    row.insert(
        "segment".into(),
        Value::String(hit.segment.as_str().to_string()),
    );
    row.insert("segment_id".into(), serde_json::json!(hit.segment_id));
    row.insert("score".into(), serde_json::json!(hit.score));
    row.insert("modified".into(), serde_json::json!(hit.modified));
    row.insert("size".into(), serde_json::json!(hit.size));
    let mut info = Map::new();
    for (key, value) in &hit.info {
        info.insert(key.clone(), Value::String(value.clone()));
    }
    row.insert("info".into(), Value::Object(info));
    if let Some(z) = hit.z {
        row.insert("z".into(), serde_json::json!(z));
    }
    if let Some(cosine) = hit.cosine {
        row.insert("cosine".into(), serde_json::json!(cosine));
    }
    if let Some(rank) = hit.keyword_rank {
        row.insert("keyword_rank".into(), serde_json::json!(rank));
    }
    if let Some(loc) = hit.loc {
        row.insert("loc".into(), serde_json::json!(loc));
    }
    if let Some(end) = hit.loc_end {
        row.insert("loc_end".into(), serde_json::json!(end));
    }
    if let Some(focus) = hit.focus {
        row.insert("focus".into(), serde_json::json!(focus));
    }
    if let Some(excerpt) = &hit.excerpt {
        row.insert("excerpt".into(), Value::String(excerpt.clone()));
    }
    Value::Object(row)
}

/// `openCode()`: the code index is open while there are code folders; without
/// any, a shell can delete it (Windows won't delete an open file). The store
/// outlives every searcher the daemon makes from it (the class shape's
/// reference, made concrete by leaking: the process is the store's lifetime).
fn open_code(state: &mut State) -> Result<(), String> {
    if state.code_roots.is_empty() {
        return Ok(());
    }
    let store = IndexStore::open(&state.directory, CODE_FILE).map_err(|e| e.to_string())?;
    let store: &'static IndexStore = Box::leak(Box::new(store));
    state.code_searcher = Some(Searcher::new(store, false).map_err(|e| e.to_string())?);
    state.code_store = Some(store);
    Ok(())
}

/// `render(_:)`: a preview picture through the engine's `PreviewImages` — a
/// PDF's page, a video's frame at a moment, a picture a web view can't show,
/// as JPEG. Two at a time run beside the request loop; each file version
/// renders once (the cache key is the engine's, and daemon-internal).
fn render(
    request: &Map<String, Value>,
    previews: &Path,
    rendered: &Mutex<HashMap<String, String>>,
) -> Result<Value, String> {
    let path = request
        .get("path")
        .and_then(Value::as_str)
        .ok_or("which file?")?;
    let kind = request
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("image");
    let loc = number(request.get("loc"));
    let pixels = (request
        .get("pixels")
        .and_then(Value::as_i64)
        .unwrap_or(1024)
        .clamp(32, 2048)) as usize;
    let modified = std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let key = format!("{path}|{modified}|{kind}|{}|{pixels}", loc.unwrap_or(-1.0));
    let cached = rendered.lock().expect("the render lock").get(&key).cloned();
    if let Some(done) = cached {
        if Path::new(&done).exists() {
            return Ok(serde_json::json!({"image": done}));
        }
    }
    let out = previews.join(format!("{}.jpg", digup_core::uuid::uuid_string()));
    let made = match kind {
        "pdf" => digup_native::pdf::render_preview(
            loc.unwrap_or(1.0) as usize,
            Path::new(path),
            pixels,
            &out,
        )
        .map(|_| out),
        "video" => digup_native::media::frame_at(loc.unwrap_or(0.0), Path::new(path), pixels, &out)
            .map(|_| out),
        _ => digup_native::image::prepare(Path::new(path), pixels as i64, 0, previews, false)
            .map(|prepared| PathBuf::from(prepared.path)),
    }
    .map_err(|e| e.to_string())?;
    let image = made.to_string_lossy().into_owned();
    rendered
        .lock()
        .expect("the render lock")
        .insert(key, image.clone());
    Ok(serde_json::json!({"image": image}))
}

/// A JSON number as the engine's `Double` (`(request["loc"] as?
/// NSNumber)?.doubleValue`): integers and doubles alike.
fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// `summary(of:in:)`: what's indexed under one folder, as the Mac app's
/// `Engine.rootSummaries` counts it — its files by state and kind.
fn summary(root: &str, store: Option<&IndexStore>) -> Value {
    let folder = Path::new(root);
    let missing = !folder.is_dir();
    let unreadable = !missing && !digup_core::crawler::can_list(root);
    let mut states: HashMap<&str, i64> = HashMap::new();
    for key in ["indexed", "pending", "failed", "skipped", "unfinished"] {
        states.insert(key, 0);
    }
    let mut kinds: HashMap<String, i64> = HashMap::new();
    let mut skipped_reasons: HashMap<String, i64> = HashMap::new();
    let files = store
        .and_then(|store| store.files_under(&[root.to_string()]).ok())
        .unwrap_or_default();
    for file in &files {
        *kinds.entry(file.kind.as_str().to_string()).or_default() += 1;
        match file.state.as_str() {
            "done" => *states.get_mut("indexed").unwrap() += 1,
            "partial" => {
                *states.get_mut("indexed").unwrap() += 1;
                *states.get_mut("unfinished").unwrap() += 1;
            }
            "pending" => *states.get_mut("pending").unwrap() += 1,
            "failed" => *states.get_mut("failed").unwrap() += 1,
            _ => {
                *states.get_mut("skipped").unwrap() += 1;
                *skipped_reasons
                    .entry(
                        file.detail
                            .clone()
                            .unwrap_or_else(|| "not read".to_string()),
                    )
                    .or_default() += 1;
            }
        }
    }
    let mut fields = Map::new();
    fields.insert("path".into(), Value::String(root.to_string()));
    fields.insert("missing".into(), Value::Bool(missing));
    fields.insert("unreadable".into(), Value::Bool(unreadable));
    for (key, count) in states {
        fields.insert(key.to_string(), serde_json::json!(count));
    }
    fields.insert("files".into(), serde_json::json!(files.len()));
    let mut kinds_row = Map::new();
    for (kind, count) in kinds {
        kinds_row.insert(kind, serde_json::json!(count));
    }
    fields.insert("kinds".into(), Value::Object(kinds_row));
    let mut reasons = Map::new();
    for (reason, count) in skipped_reasons {
        reasons.insert(reason, serde_json::json!(count));
    }
    fields.insert("skipped_reasons".into(), Value::Object(reasons));
    Value::Object(fields)
}

/// `handle(_:…)` under the state lock: every op's reply fields (the `id` is
/// added by the loop).
fn handle(state: &mut State, op: &str, request: &Map<String, Value>) -> Result<Value, String> {
    match op {
        "search" => {
            let text = request.get("q").and_then(Value::as_str).unwrap_or("");
            let code = request
                .get("code")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let kinds: Option<HashSet<FileKind>> =
                request.get("kinds").and_then(Value::as_array).map(|rows| {
                    rows.iter()
                        .filter_map(|v| v.as_str())
                        .filter_map(FileKind::parse)
                        .collect()
                });
            let limit = request.get("limit").and_then(Value::as_i64).unwrap_or(40) as usize;
            let want_meaning = request
                .get("meaning")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let prefix_last = request
                .get("prefix_last")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let started = Instant::now();
            let mut vector: Option<Vec<f32>> = None;
            let mut warning: Option<String> = None;
            // "name: …" needs no model — keyword search, the encoder never starts.
            if want_meaning && !text.is_empty() && NameQuery::text_of(text).is_none() {
                vector = vector_for(state, text, code);
                // Vectors from another model or runtime must never meet:
                // keyword-only then, and say so once.
                if vector.is_some() {
                    let indexed = if code {
                        state
                            .code_searcher
                            .as_ref()
                            .and_then(|s| s.vector_source.clone())
                    } else {
                        state.searcher.vector_source.clone()
                    };
                    let source = state.encoder.as_ref().map(|e| e.info().vector_source());
                    if let (Some(indexed), Some(source)) = (indexed, source) {
                        if indexed != source {
                            vector = None;
                            if !state.warned_about_source {
                                warning = Some(format!(
                                    "meaning search is off: the index was built with {indexed}, the encoder is {source}"
                                ));
                                state.warned_about_source = true;
                            }
                        }
                    }
                }
            }
            let embedded = Instant::now();
            let opts = SearchOptions {
                kinds: kinds.as_ref(),
                limit,
                z_gate: 2.0,
                word_weight: None,
                prefix_last,
            };
            let hits: Vec<SearchHit> = if code {
                let Some(code_searcher) = state.code_searcher.as_mut() else {
                    // The engine's early return: no ms, no warning.
                    return Ok(serde_json::json!({"hits": [], "strong": 0, "meaning": false}));
                };
                let (roots, skipped) = (state.code_roots.clone(), state.code_skipped.clone());
                code_searcher
                    .search(text, vector.as_deref(), &opts)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|hit| {
                        folder_selection::is_searched(&hit.path, &roots, &skipped)
                            && Path::new(&hit.path).exists()
                    })
                    .collect()
            } else {
                let (roots, skipped, types) = (
                    state.roots.clone(),
                    state.skipped.clone(),
                    state.types.clone(),
                );
                state
                    .searcher
                    .search(text, vector.as_deref(), &opts)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|hit| {
                        folder_selection::is_searched(&hit.path, &roots, &skipped)
                            && !types
                                .contains(&digup_core::code::extension_of(&hit.path).to_lowercase())
                            && Path::new(&hit.path).exists()
                    })
                    .collect()
            };
            let scores: Vec<f64> = hits.iter().map(|hit| hit.score).collect();
            let mut reply = Map::new();
            reply.insert(
                "hits".into(),
                Value::Array(hits.iter().map(hit_fields).collect()),
            );
            reply.insert(
                "strong".into(),
                serde_json::json!(digup_core::scoring::strong_count(&scores, 0.5)),
            );
            reply.insert("meaning".into(), Value::Bool(vector.is_some()));
            let embed_ms = {
                // Int(_:): milliseconds, truncated. embed = embedded − started;
                // search = now − embedded (Date().timeIntervalSince(embedded)).
                let between = started.elapsed().as_secs_f64() - embedded.elapsed().as_secs_f64();
                (between * 1000.0) as i64
            };
            reply.insert(
                "ms".into(),
                serde_json::json!({
                    "embed": embed_ms,
                    "search": (embedded.elapsed().as_secs_f64() * 1000.0) as i64,
                }),
            );
            if let Some(warning) = warning {
                reply.insert("warning".into(), Value::String(warning));
            }
            Ok(Value::Object(reply))
        }
        "embed_ahead" => {
            let typed = request.get("q").and_then(Value::as_str).unwrap_or("");
            if NameQuery::text_of(typed).is_none() {
                // "name: …" is keyword-only: don't wake the encoder for it.
                let _ = vector_for(
                    state,
                    typed,
                    request
                        .get("code")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                );
            }
            Ok(serde_json::json!({"ok": true}))
        }
        "text" => {
            let segment = request
                .get("segment")
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|x| x as i64)));
            let Some(segment) = segment else {
                return Err("which segment?".to_string());
            };
            let code = request
                .get("code")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = if code {
                state.code_searcher.as_ref()
            } else {
                Some(&state.searcher)
            }
            .map(|searcher| searcher.text_of_segment(segment))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten();
            Ok(serde_json::json!({"text": text}))
        }
        "reload" => {
            let had_vectors = state.searcher.vectors_loaded;
            state.searcher = Searcher::new(state.store, had_vectors).map_err(|e| e.to_string())?;
            match state.code_store {
                Some(code_store) => {
                    let had = state
                        .code_searcher
                        .as_ref()
                        .map(|s| s.vectors_loaded)
                        .unwrap_or(false);
                    state.code_searcher =
                        Some(Searcher::new(code_store, had).map_err(|e| e.to_string())?);
                }
                None => open_code(state)?,
            }
            Ok(serde_json::json!({
                "ok": true,
                "vectors": state.searcher.vector_count(),
                "code_vectors": state.code_searcher.as_ref().map(|s| s.vector_count()).unwrap_or(0),
            }))
        }
        "summaries" => {
            let code = request
                .get("code")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let roots: Vec<String> = if code {
                state.code_roots.clone()
            } else {
                state.roots.clone()
            };
            let store = if code {
                state.code_store
            } else {
                Some(state.store)
            };
            Ok(
                serde_json::json!({"roots": roots.iter().map(|root| summary(root, store)).collect::<Vec<Value>>() }),
            )
        }
        "folders" => {
            if let Some(value) = request.get("roots").and_then(Value::as_array) {
                state.roots = value
                    .iter()
                    .filter_map(Value::as_str)
                    .map(folder_url)
                    .collect();
            }
            if let Some(value) = request.get("skipped").and_then(Value::as_array) {
                state.skipped = value
                    .iter()
                    .filter_map(Value::as_str)
                    .map(folder_url)
                    .collect();
            }
            if let Some(value) = request.get("types").and_then(Value::as_array) {
                let joined = value
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",");
                state.types = digup_core::options::IndexOptions::extensions(&joined);
            }
            if let Some(value) = request.get("code_roots").and_then(Value::as_array) {
                state.code_roots = value
                    .iter()
                    .filter_map(Value::as_str)
                    .map(folder_url)
                    .collect();
            }
            if let Some(value) = request.get("code_skipped").and_then(Value::as_array) {
                state.code_skipped = value
                    .iter()
                    .filter_map(Value::as_str)
                    .map(folder_url)
                    .collect();
            }
            if state.code_roots.is_empty() {
                state.code_searcher = None;
                state.code_store = None;
            } else if state.code_searcher.is_none() {
                open_code(state)?;
            }
            Ok(serde_json::json!({"ok": true}))
        }
        "prewarm" => {
            state.last_query = Instant::now();
            running_encoder(state);
            if !state.searcher.vectors_loaded {
                state.searcher.reload().map_err(|e| e.to_string())?;
            }
            Ok(serde_json::json!({
                "ok": true, "encoder": state.encoder_state, "vectors": state.searcher.vector_count(),
            }))
        }
        "status" => {
            let mut reply = Map::new();
            reply.insert("encoder".into(), Value::String(state.encoder_state.clone()));
            reply.insert(
                "vectors".into(),
                serde_json::json!(if state.searcher.vectors_loaded {
                    state.searcher.vector_count()
                } else {
                    0
                }),
            );
            reply.insert(
                "code_vectors".into(),
                serde_json::json!(match state.code_searcher.as_ref() {
                    Some(searcher) if searcher.vectors_loaded => searcher.vector_count(),
                    _ => 0,
                }),
            );
            reply.insert(
                "index".into(),
                Value::String(state.store.file.to_string_lossy().into_owned()),
            );
            if let Ok(counts) = state.store.counts() {
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
                reply.insert("files".into(), serde_json::json!(files));
                reply.insert("searchable".into(), serde_json::json!(searchable));
                reply.insert("pending".into(), serde_json::json!(pending));
                reply.insert("unfinished".into(), serde_json::json!(counts.unfinished));
            }
            Ok(Value::Object(reply))
        }
        "quit" => Ok(serde_json::json!({"ok": true})), // the loop answers quit before handle
        other => Err(format!("unknown op {other}")),
    }
}

/// `idleWatch()`: after `idleSeconds` without a query the model and the
/// vectors go; after twice that, the process goes too.
fn idle_watch(shared: Arc<Shared>, idle_seconds: f64) {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let mut state = shared.0.lock().expect("the state lock");
        let idle = state.last_query.elapsed().as_secs_f64();
        if idle > 2.0 * idle_seconds {
            write_out(serde_json::json!({"event": "exit", "reason": "idle"}));
            // Not std::process::exit(): on Linux it waits for stdin's lock,
            // which the main thread holds in the line read, for ever.
            unsafe { libc::_exit(0) };
        }
        if (state.encoder.is_some() || state.searcher.vectors_loaded) && idle > idle_seconds {
            stop_encoder(&mut state);
            state.searcher.release_vectors();
            if let Some(code_searcher) = state.code_searcher.as_mut() {
                code_searcher.release_vectors();
            }
            write_out(
                serde_json::json!({"event": "encoder", "state": "stopped", "reason": "idle"}),
            );
        }
    }
}

/// `serve()`'s loop; `searchService(_:)` emits `ready` before it. The engine
/// class returns the exit code; the CLI process-exits with it.
pub fn serve(config: SearchConfig) -> i32 {
    match run(config) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

fn run(config: SearchConfig) -> Result<i32, String> {
    let previews = config.index.join("previews");
    let _ = std::fs::remove_dir_all(&previews);
    std::fs::create_dir_all(&previews).map_err(|e| e.to_string())?;
    let store = IndexStore::main(&config.index).map_err(|e| e.to_string())?;
    let store: &'static IndexStore = Box::leak(Box::new(store));
    let searcher = Searcher::new(store, false).map_err(|e| e.to_string())?;
    let mut state = State {
        directory: config.index.clone(),
        store,
        searcher,
        code_store: None,
        code_searcher: None,
        roots: config.roots.clone(),
        skipped: config.skipped.clone(),
        types: config.types.clone(),
        code_roots: config.code_roots.clone(),
        code_skipped: config.code_skipped.clone(),
        encoder: None,
        encoder_state: "stopped".to_string(),
        vector_cache: HashMap::new(),
        last_query: Instant::now(),
        warned_about_source: false,
        log: config.index.join("worker-query.log"),
    };
    open_code(&mut state)?;
    write_out(
        serde_json::json!({"event": "ready", "index": config.index.to_string_lossy().into_owned()}),
    );
    let shared = Arc::new(Shared(Mutex::new(state)));
    std::thread::spawn({
        let shared = shared.clone();
        move || idle_watch(shared, config.idle_seconds)
    });
    let render_state = Arc::new(RenderState {
        previews,
        rendered: Mutex::new(HashMap::new()),
    });
    let slots = Arc::new(Slots::new(2));
    let stdin = std::io::stdin();
    for line in stdin.lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue; // `try? JSONSerialization … as? [String: Any]`
        };
        let Value::Object(request) = request else {
            continue;
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let op = match request.get("op").and_then(Value::as_str) {
            Some(op) => op.to_string(),
            None => {
                let mut reply = Map::new();
                reply.insert("ok".into(), Value::Bool(false));
                reply.insert("error".into(), Value::String("no op".to_string()));
                reply.insert("id".into(), id);
                write_out(Value::Object(reply));
                continue;
            }
        };
        if op == "quit" {
            write_out(serde_json::json!({"id": id, "ok": true}));
            stop_encoder(&mut shared.0.lock().expect("the state lock"));
            return Ok(0);
        }
        if op == "render" {
            // renderSlots.wait() runs on the request thread: a third render
            // waits for the first two; the thread returns the slot when it ends.
            slots.acquire();
            let slots = slots.clone();
            let render_state = render_state.clone();
            std::thread::spawn(move || {
                let reply = match render(&request, &render_state.previews, &render_state.rendered) {
                    Ok(reply) => reply,
                    Err(error) => serde_json::json!({
                        "ok": false, "error": error.chars().take(500).collect::<String>(),
                    }),
                };
                let mut reply = reply.as_object().expect("a render reply").clone();
                reply.insert("id".into(), id);
                write_out(Value::Object(reply));
                slots.release(); // the renderSlots.signal()
            });
            continue;
        }
        let reply = {
            let mut state = shared.0.lock().expect("the state lock");
            handle(&mut state, &op, &request)
        };
        let mut reply = match reply {
            Ok(reply) => reply.as_object().expect("a reply object").clone(),
            Err(error) => {
                let mut row = Map::new();
                row.insert("ok".into(), Value::Bool(false));
                row.insert(
                    "error".into(),
                    Value::String(error.chars().take(500).collect()),
                );
                row
            }
        };
        reply.insert("id".into(), id);
        write_out(Value::Object(reply));
    }
    stop_encoder(&mut shared.0.lock().expect("the state lock"));
    Ok(0)
}

struct RenderState {
    previews: PathBuf,
    rendered: Mutex<HashMap<String, String>>,
}

/// `DispatchSemaphore(value: 2)` — two renderings at a time. std's
/// `Semaphore` is not in this toolchain's `sync` (the stabilization
/// was pulled back), so the slot count is a lock and a condvar.
struct Slots {
    inner: Mutex<i32>,
    ready: Condvar,
}

impl Slots {
    fn new(count: i32) -> Self {
        Self {
            inner: Mutex::new(count),
            ready: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let mut count = self.inner.lock().expect("the render slots");
        while *count == 0 {
            count = self.ready.wait(count).expect("the render slots");
        }
        *count -= 1;
    }

    fn release(&self) {
        *self.inner.lock().expect("the render slots") += 1;
        self.ready.notify_one();
    }
}
