//! `digup search-service` (`SearchService`, SearchService.swift): the app's
//! searcher as a process, for shells that can't link DigUpKit. It keeps the
//! index open and the query encoder warm, so a search while typing costs what
//! it costs in the Mac app (tens of milliseconds), not a process start.
//!
//! Protocol: one JSON request per line on stdin, one JSON response per line on
//! stdout, carrying the request's `id` — the verbs, replies and events are as
//! `SearchService.swift:9-32` documents them (this module's `handle` is the compiled list).
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
    /// The faces home (the #195 device-side root; `None` = the store is never read).
    /// Only the `person:` field of `search` opens it, and only through the read-only
    /// window — the daemon's plain searches never touch the store (round twelve).
    pub faces_dir: Option<PathBuf>,
    /// The consent record path (the faces daemon's own gate, `--faces-consent`): every
    /// People-scoped request re-reads it engine-side, GUI walls and engine walls alike.
    pub faces_consent: Option<PathBuf>,
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
    // The engine's ONE path form: slashes on Windows (identity on unix) — this value
    // feeds bytewise comparisons with crawl paths.
    digup_core::paths::to_slashes(&std::env::var("HOME").unwrap_or_default())
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
    /// `find like this` (#162/#121, 2026-11-25): the FULL-shape media encoder
    /// (text + mmproj), loaded by the first picture query and kept warm — issue
    /// #155's stickiness law, mirrored on this daemon: the 555 MB picture model
    /// is paid ONCE, the idle watch releases it with the text model's memory.
    media_encoder: Option<Box<dyn Embedder>>,
    media_encoder_state: String,
    vector_cache: HashMap<String, Vec<f32>>,
    last_query: Instant,
    warned_about_source: bool,
    log: PathBuf,
    /// The faces home and consent record from the launch (round twelve): `person:` reads
    /// the store through `face_reader`, the read-only window opened lazily on the first
    /// People-scoped request that cleared the consent check.
    #[cfg_attr(not(feature = "faces"), allow(dead_code))]
    // the plain-build stub refuses before reading
    faces_dir: Option<PathBuf>,
    #[cfg_attr(not(feature = "faces"), allow(dead_code))]
    faces_consent: Option<PathBuf>,
    #[cfg_attr(not(feature = "faces"), allow(dead_code))]
    face_reader: Option<digup_core::db::Database>,
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
    // `find like this` (#162/#121): the media encoder rides the same idle law —
    // the picture memory goes back to the system together with the text model's.
    stop_media_encoder(state);
}

/// The media encoder's own stop. The embed-error path uses it alone (the
/// vector_for law: a failed embed stops the encoder, the next call starts a
/// new one); the idle watch stops both through `stop_encoder`.
fn stop_media_encoder(state: &mut State) {
    if let Some(encoder) = state.media_encoder.as_mut() {
        encoder.close();
    }
    state.media_encoder = None;
    if state.media_encoder_state != "unavailable" {
        state.media_encoder_state = "stopped".to_string();
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

/// `runningMediaEncoder()` (#162/#121): the FULL shape — text + mmproj media
/// encoders with the 2048-token context, because a picture takes up to 1120
/// tokens and the text-only 512 context cannot serve it (embedder.rs:101-104).
/// Loaded once by the first picture query, kept warm by the stickiness law,
/// stopped by the same idle watch as the text encoder. The `media: true` field
/// marks the new event lines; the text encoder's lines keep their pre-round bytes.
fn running_media_encoder(state: &mut State) -> bool {
    if state.media_encoder.is_some() {
        return true;
    }
    if state.media_encoder_state == "unavailable" {
        return false;
    }
    state.media_encoder_state = "starting".to_string();
    let started = Instant::now();
    let made = LlamaModels::locate()
        .and_then(|models| LlamaEmbedder::new(&models, false, Some(2048), Some(&state.log)));
    match made {
        Ok(worker) => {
            let source = worker.info().vector_source();
            state.media_encoder = Some(Box::new(worker));
            state.media_encoder_state = "ready".to_string();
            let seconds = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
            write_out(serde_json::json!({
                "event": "encoder", "state": "ready", "seconds": seconds, "source": source,
                "media": true,
            }));
            true
        }
        Err(error) => {
            state.media_encoder_state = "unavailable".to_string();
            write_out(serde_json::json!({
                "event": "encoder", "state": "unavailable", "media": true,
                "error": error.to_string().chars().take(300).collect::<String>(),
            }));
            false
        }
    }
}

/// `find like this` (#162/#121): the reference picture's query vector, embedded
/// at the media budget its stored twin used at index time — photo 140 /
/// screenshot 280, the `IndexOptions` pair the indexer embeds with
/// (`vector_for`'s cache law rides: the same reference re-clicked re-embeds
/// nothing). Same L2-normalized 768-d space as the pool, so the dots the matrix
/// scores are cosines and the 2.0σ gate means what it always means. An embed
/// failure stops the media encoder (the vector_for law) — the next picture
/// query starts a new one.
fn reference_vector(state: &mut State, path: &str, kind: FileKind) -> Option<Vec<f32>> {
    state.last_query = Instant::now();
    let options = digup_core::options::IndexOptions::default();
    let budget = match kind {
        FileKind::Screenshot => options.screenshot_budget,
        _ => options.photo_budget,
    };
    let key = format!("media\u{0}{}\u{0}{path}", kind.as_str());
    if let Some(cached) = state.vector_cache.get(&key) {
        return Some(cached.clone());
    }
    if !running_media_encoder(state) {
        return None;
    }
    let made = state
        .media_encoder
        .as_ref()
        .expect("the encoder running_media_encoder just made")
        .embed_images(&[path.to_string()], budget as i32);
    match made {
        Ok(rows) => rows
            .into_iter()
            .next()
            .and_then(|row| row.vector)
            .inspect(|values| {
                if state.vector_cache.len() > 256 {
                    state.vector_cache.clear();
                }
                state.vector_cache.insert(key, values.clone());
            }),
        Err(error) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "media encoder: {error}");
            stop_media_encoder(state);
            None
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

/// `hitFields` (SearchService.swift:365, #178): a hit as JSON — what `digup search
/// `--json` prints, plus segment_id, modified, size and info. `strong` and `text` ride
/// in only when the caller knows them (the CLI's `--json` with `--strong`/`--text`);
/// the daemon's replies keep the pre-#178 bytes by passing nil.
pub fn hit_fields(hit: &SearchHit) -> Value {
    hit_fields_strong_text(hit, None, None)
}

/// The #178 shape: `strong` (`--strong`'s cutoff, per row) and `text` (`--text`'s
/// stored segment text) ride in only when the caller knows them.
pub fn hit_fields_text(hit: &SearchHit, strong: Option<bool>, text: Option<&str>) -> Value {
    hit_fields_strong_text(hit, strong, text)
}

fn hit_fields_strong_text(hit: &SearchHit, strong: Option<bool>, text: Option<&str>) -> Value {
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
    if let Some(strong) = strong {
        row.insert("strong".into(), Value::Bool(strong));
    }
    if let Some(text) = text {
        row.insert("text".into(), Value::String(text.to_string()));
    }
    Value::Object(row)
}

/// `segmentFields` (SearchService.swift #178): a `show` row — the segment's metadata
/// and its full stored text (nil for a segment no FTS row covers).
pub fn segment_fields(detail: &digup_core::searcher::SegmentDetail) -> Value {
    let mut row = Map::new();
    row.insert("path".into(), Value::String(detail.path.clone()));
    row.insert(
        "kind".into(),
        Value::String(detail.kind.as_str().to_string()),
    );
    row.insert(
        "segment".into(),
        Value::String(detail.segment.as_str().to_string()),
    );
    row.insert("segment_id".into(), serde_json::json!(detail.segment_id));
    row.insert("modified".into(), serde_json::json!(detail.modified));
    row.insert("size".into(), serde_json::json!(detail.size));
    let mut info = Map::new();
    for (key, value) in &detail.info {
        info.insert(key.clone(), Value::String(value.clone()));
    }
    row.insert("info".into(), Value::Object(info));
    if let Some(loc) = detail.loc {
        row.insert("loc".into(), serde_json::json!(loc));
    }
    if let Some(end) = detail.loc_end {
        row.insert("loc_end".into(), serde_json::json!(end));
    }
    if let Some(excerpt) = &detail.excerpt {
        row.insert("excerpt".into(), Value::String(excerpt.clone()));
    }
    if let Some(text) = &detail.text {
        row.insert("text".into(), Value::String(text.clone()));
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

/// `name_only:` (#66, 2026-11-25): the GUIs' "Names only" control rides this verb
/// field. The seam composes the EXACT spelling a human types in the box —
/// `NameQuery::PREFIX` + a space + the text — and only then: the field true, the
/// text non-empty, and the text not already carrying the prefix. Every other
/// combination passes through unchanged, so a field-less reply stays byte-parity
/// and a typed `name:` is never doubled. The names-only path itself
/// (`NameQuery::text_of`) is the engine's own; this field adds no second keyword
/// search and never touches the display text the thin clients show.
fn effective_query(text: &str, name_only: bool) -> String {
    if name_only && !text.is_empty() && NameQuery::text_of(text).is_none() {
        format!("{} {text}", NameQuery::PREFIX)
    } else {
        text.to_string()
    }
}

/// One more member of the post-ranking filter chain (the `ext:` comment above):
/// the rows the engine ranked ride verbatim, the filter KEEPS, it never re-scores.
fn keeps_extension(extension: &str, ext: &std::collections::HashSet<String>) -> bool {
    ext.is_empty() || ext.contains(extension)
}

/// The person's photo paths (round twelve, 2026-11-27, the user's pool law): the face
/// store read through its READ-ONLY window (`db::open_read_only` — the search daemon
/// never writes the store, the faces writer never blocks, and the two daemons coexist
/// as SQLite WAL readers/writer alike). The consent record is checked on EVERY
/// People-scoped request, engine-side — the faces daemon's own gate quoted at the
/// store's edge (FACES.md Phase 1 §5): what the UI walls gate, the engine gates too,
/// and a consent granted after this daemon started takes effect on the next request,
/// not at the next daemon restart. The store's home is the path-keyed #195 home,
/// computed by the faces daemon's own law — the daemon keeps no private copy of it.
#[cfg(feature = "faces")]
fn faces_pool_paths(
    state: &mut State,
    person: i64,
) -> Result<std::collections::HashSet<String>, String> {
    let consent = state
        .faces_consent
        .as_ref()
        .ok_or("search: person scope needs --faces-consent (face recognition is user-consented)")?;
    let record: Value = std::fs::read_to_string(consent)
        .map_err(|e| format!("search: no consent record at {}: {e}", consent.display()))
        .and_then(|text| {
            serde_json::from_str(&text).map_err(|e| {
                format!(
                    "search: unreadable consent record {}: {e}",
                    consent.display()
                )
            })
        })?;
    if record.get("consented").and_then(Value::as_bool) != Some(true) {
        return Err("search: consent record does not consent".to_string());
    }
    let faces_root = state
        .faces_dir
        .as_ref()
        .ok_or("search: person scope needs --faces-dir (the faces home)")?;
    if state.face_reader.is_none() {
        let store = crate::face_service::faces_store_dir(&state.directory, faces_root)
            .join(digup_core::face_store::FACE_FILE);
        state.face_reader = Some(
            digup_core::db::Database::open_read_only(&store)
                .map_err(|e| format!("search: the faces store is not there to read: {e}"))?,
        );
    }
    let reader = state.face_reader.as_ref().expect("the open above ran");
    let mut paths = std::collections::HashSet::new();
    reader
        .query(
            "SELECT DISTINCT path FROM faces WHERE person = ?1",
            [digup_core::db::Sql::Int(person)],
            |row| {
                if let Some(path) = row.text(0) {
                    paths.insert(path);
                }
                Ok(())
            },
        )
        .map_err(|e| format!("search: the faces store refused: {e}"))?;
    Ok(paths)
}

#[cfg(not(feature = "faces"))]
fn faces_pool_paths(
    _state: &mut State,
    _person: i64,
) -> Result<std::collections::HashSet<String>, String> {
    Err("search: person scope needs a build with the faces feature".to_string())
}

/// `handle(_:…)` under the state lock: every op's reply fields (the `id` is
/// added by the loop).
fn handle(state: &mut State, op: &str, request: &Map<String, Value>) -> Result<Value, String> {
    match op {
        "search" => {
            let text = effective_query(
                request.get("q").and_then(Value::as_str).unwrap_or(""),
                request
                    .get("name_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            let text = text.as_str();
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
            // `under:` (#206/#132/#188, 2026-11-25): the GUI's folder scope rides this
            // field and the searcher's proven `under:` machinery (#178/#1789) — the
            // scope's canonical spellings (realpath, worktree→main-repo) resolve inside
            // the searcher, the vector loop, FTS scan and coverage all answer scoped.
            // Absent/empty = unscoped, byte-parity; an in-scope-empty answer is no hits.
            let under: Vec<String> = request
                .get("under")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .filter(|path| !path.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            // `ext:` — an extension ALLOW-list: keep only rows whose file carries one
            // of these extensions. Normalized the way the engine spells extensions
            // (lowercase, leading dots stripped); absent/empty = no filter. It joins
            // the post-ranking filter chain beside the root/scope/type filters, so the
            // engine's z rows stay verbatim — the filter keeps, never re-scores.
            let ext: std::collections::HashSet<String> = request
                .get("ext")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(|value| value.trim().trim_start_matches('.').to_lowercase())
                        .filter(|value| !value.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            // `person:` (round twelve, 2026-11-27, the user's pool law): the People-scoped
            // search. The face store answers the person's photo paths and the searcher
            // POOLS the search into them — candidates are the pool's files and the z-gate
            // is computed within the pool, the engine's honest numbers for the query among
            // THAT person's photos (round six's post-filter shape kept only the global
            // field's winners through the intersection — the crowd crowding the user named).
            // The consent gate runs engine-side in `faces_pool_paths`; the GUI walls stay.
            let person_pool: Option<std::collections::HashSet<String>> =
                match request.get("person").and_then(Value::as_i64) {
                    Some(person) if person > 0 => {
                        if code {
                            return Err(
                                "search: person scope is a People search, never a code one"
                                    .to_string(),
                            );
                        }
                        Some(faces_pool_paths(state, person)?)
                    }
                    Some(person) => {
                        return Err(format!(
                            "search: person must be a number greater than 0, got {person}"
                        ))
                    }
                    None => None,
                };
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
                z_gate: digup_core::scoring::Z_GATE,
                word_weight: None,
                prefix_last,
                under: (!under.is_empty()).then_some(under.as_slice()),
                person_files: person_pool.as_ref(),
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
                            && keeps_extension(
                                &digup_core::code::extension_of(&hit.path).to_lowercase(),
                                &ext,
                            )
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
                        let extension = digup_core::code::extension_of(&hit.path).to_lowercase();
                        folder_selection::is_searched(&hit.path, &roots, &skipped)
                            && !types.contains(&extension)
                            && keeps_extension(&extension, &ext)
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
        "find_similar" => {
            // `find like this` (#162/#121, 2026-11-25): the reference picture
            // rides the daemon as a server-supplied `path` — the `render` op's
            // law: REST never carries paths; the thin client sends its minted
            // media id, the server resolves it to the path and supplies it here.
            // The picture's own vector (full media encoder, its indexed budget)
            // replaces the query text: `search("")` skips the FTS scan, the
            // pool's cosines and the 2.0σ gate mean exactly what they mean for
            // a text query, and the same post-ranking filter chain rides
            // verbatim. The reference's own row drops out of its answer (the
            // faces `similar` law — you don't want the photo you clicked).
            // `found_from` names what the answer was built from.
            let path = request.get("path").and_then(Value::as_str).unwrap_or("");
            if path.is_empty() {
                return Err("find like this needs a reference path".to_string());
            }
            let kinds: Option<HashSet<FileKind>> =
                request.get("kinds").and_then(Value::as_array).map(|rows| {
                    rows.iter()
                        .filter_map(|v| v.as_str())
                        .filter_map(FileKind::parse)
                        .collect()
                });
            let limit = request.get("limit").and_then(Value::as_i64).unwrap_or(40) as usize;
            let under: Vec<String> = request
                .get("under")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .filter(|path| !path.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let ext: std::collections::HashSet<String> = request
                .get("ext")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(|value| value.trim().trim_start_matches('.').to_lowercase())
                        .filter(|value| !value.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let started = Instant::now();
            let mut vector: Option<Vec<f32>> = None;
            let mut warning: Option<String> = None;
            let mut found_from: Option<(String, &'static str)> = None;
            // The reference must be a picture: the kind from its index row when
            // it is indexed (the budget its stored twin embedded at), the
            // engine's own screenshot name/folder law for an off-index file.
            let kind = if !Path::new(path).is_file() {
                warning = Some(format!("find like this: no such file: {path}"));
                None
            } else {
                match state.searcher.indexed_kind(path) {
                    Some(kind @ (FileKind::Image | FileKind::Screenshot)) => Some(kind),
                    Some(other) => {
                        warning = Some(format!(
                            "find like this: a {} is not a picture",
                            other.as_str()
                        ));
                        None
                    }
                    None if digup_core::rules::is_screenshot(path) => Some(FileKind::Screenshot),
                    None => Some(FileKind::Image),
                }
            };
            if let Some(kind) = kind {
                found_from = Some((path.to_string(), kind.as_str()));
                vector = reference_vector(state, path, kind);
                // The same source-match gate as text search: vectors from
                // another model must never meet the pool — no hits, and say so.
                if vector.is_some() {
                    let indexed = state.searcher.vector_source.clone();
                    let source = state
                        .media_encoder
                        .as_ref()
                        .map(|e| e.info().vector_source());
                    if let (Some(indexed), Some(source)) = (indexed, source) {
                        if indexed != source {
                            vector = None;
                            if !state.warned_about_source {
                                warning = Some(format!(
                                    "find like this is off: the index was built with {indexed}, the encoder is {source}"
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
                z_gate: digup_core::scoring::Z_GATE,
                word_weight: None,
                prefix_last: false,
                under: (!under.is_empty()).then_some(under.as_slice()),
                // Find like this answers PICTURES across the field — the People pool is a
                // text-search act (round twelve), and the merged bar law sends no pool here
                // on any front (the shell and web panels document the same law).
                person_files: None,
            };
            let reference = path.to_string();
            let (roots, skipped, types) = (
                state.roots.clone(),
                state.skipped.clone(),
                state.types.clone(),
            );
            let hits: Vec<SearchHit> = state
                .searcher
                .search("", vector.as_deref(), &opts)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|hit| {
                    let extension = digup_core::code::extension_of(&hit.path).to_lowercase();
                    hit.path != reference
                        && folder_selection::is_searched(&hit.path, &roots, &skipped)
                        && !types.contains(&extension)
                        && keeps_extension(&extension, &ext)
                        && Path::new(&hit.path).exists()
                })
                .collect();
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
            reply.insert(
                "ms".into(),
                serde_json::json!({
                    "embed": (started.elapsed().as_secs_f64() - embedded.elapsed().as_secs_f64()) as i64,
                    "search": (embedded.elapsed().as_secs_f64() * 1000.0) as i64,
                }),
            );
            if let Some((path, kind)) = found_from {
                reply.insert(
                    "found_from".into(),
                    serde_json::json!({"path": path, "kind": kind}),
                );
            }
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
            // `find like this` (#162/#121): the media encoder's own state rides
            // as an additive field — field-less replies stay byte-parity, the
            // server's status parser reads keys, not positions.
            reply.insert(
                "media_encoder".into(),
                Value::String(state.media_encoder_state.clone()),
            );
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
        if (state.encoder.is_some()
            || state.media_encoder.is_some()
            || state.searcher.vectors_loaded)
            && idle > idle_seconds
        {
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
        media_encoder: None,
        media_encoder_state: "stopped".to_string(),
        vector_cache: HashMap::new(),
        last_query: Instant::now(),
        warned_about_source: false,
        log: config.index.join(digup_core::paths::WORKER_QUERY_LOG),
        faces_dir: config.faces_dir,
        faces_consent: config.faces_consent,
        face_reader: None,
    };
    // `init` (SearchService.swift:61-73) in order: store, searcher, the
    // fields, then the previews folder is REBUILT — `try? removeItem` (the
    // removal's failure is dropped, as here) then `try createDirectory`
    // (its failure throws: startup dies) — and only then `openCode()`.
    // The order is a fact, not a style: the wipe rides AFTER the store
    // open, so a start that fails to open the index leaves the operator's
    // preview cache UNTOUCHED. Measured 2026-11-15 on a folder holding a
    // non-database `index.sqlite` + `previews/marker.jpg` (this box,
    // ai-ubuntu-computer): Swift exits 1 with the marker intact; the
    // audit-base had already deleted the whole previews tree — a daemon
    // start against a broken (or mid-migration) index destroyed rendered
    // previews that survive the reference's identical failure.
    let previews = config.index.join("previews");
    let _ = std::fs::remove_dir_all(&previews);
    std::fs::create_dir_all(&previews).map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::{run, IndexStore, SearchConfig};
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// The F-13c probe folder: a `index.sqlite` that cannot open (not a
    /// database) and a `previews` tree holding one rendered file — the
    /// scenario measured 2026-11-15 against the Swift twin on this box.
    fn scratch_index(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("digup-f13c-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("previews")).unwrap();
        std::fs::write(dir.join("index.sqlite"), b"not a database at all").unwrap();
        std::fs::write(dir.join("previews/marker.jpg"), b"keep me").unwrap();
        dir
    }

    fn probe_config(index: PathBuf) -> SearchConfig {
        SearchConfig {
            index,
            roots: vec![],
            skipped: vec![],
            types: HashSet::new(),
            code_roots: vec![],
            code_skipped: vec![],
            idle_seconds: 300.0,
            faces_dir: None,
            faces_consent: None,
        }
    }

    #[test]
    fn name_only_composes_the_typed_spelling_and_passes_the_rest_through() {
        // #66 (2026-11-25): the GUI chip rides the verb field; the seam must
        // compose the byte-identical spelling a human types — never double a
        // typed prefix (any casing), never prefix an empty text, and never
        // touch a field-less or already-prefixed query.
        assert_eq!(
            super::effective_query("passport scan", true),
            "name: passport scan"
        );
        assert_eq!(
            super::effective_query("name: passport", true),
            "name: passport"
        );
        assert_eq!(
            super::effective_query("NAME: passport", true),
            "NAME: passport"
        );
        assert_eq!(super::effective_query("  name: x", true), "  name: x");
        assert_eq!(super::effective_query("passport", false), "passport");
        assert_eq!(super::effective_query("", true), "");
    }

    #[test]
    fn keeps_extension_keeps_the_allow_list_rows_only() {
        // #206/#132/#188 (2026-11-25): one member of the post-ranking filter chain —
        // an empty allow-list keeps everything (the field-less verb, byte-parity),
        // a named list keeps only its extensions. The filter drops rows, it never
        // re-scores: the engine's z rows ride verbatim through it.
        let empty: HashSet<String> = HashSet::new();
        assert!(super::keeps_extension("jpg", &empty));
        let allow: HashSet<String> = ["mp4", "pdf"].iter().map(|s| s.to_string()).collect();
        assert!(super::keeps_extension("mp4", &allow));
        assert!(!super::keeps_extension("jpg", &allow));
    }

    #[test]
    fn a_failed_daemon_start_leaves_the_preview_cache_where_the_engine_leaves_it() {
        // SearchService.swift:61-73 opens the store FIRST, builds the fields,
        // and rebuilds the previews folder after them — a start that fails at
        // the store open never reaches the wipe, so the operator's rendered
        // previews survive. Measured on the Swift twin (search-service, a
        // non-database index.sqlite): exit 1, stderr `file is not a database
        // (in: PRAGMA …)`, stdout silent, previews/marker.jpg INTACT.
        let dir = scratch_index("keep");
        let outcome = run(probe_config(dir.clone()));
        assert!(
            outcome.is_err(),
            "the probe needs the store open to fail: {outcome:?}"
        );
        assert_eq!(
            std::fs::read(dir.join("previews/marker.jpg")).unwrap(),
            b"keep me",
            "the failed start must not touch the previews tree"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_audit_base_wiped_the_previews_before_opening_the_store() {
        // F-13c's fails-before, recorded at the fixed tip: the audit-base
        // statements verbatim (remove, create, THEN open). On the folder the
        // Swift twin leaves untouched, the audit-base deletes the whole
        // previews tree first and fails at the open after it — measured on
        // the daemon binary: the marker survived in Swift, was gone in the
        // audit-base (`previews/` an empty dir).
        let dir = scratch_index("wipe");
        let previews = dir.join("previews");
        let _ = std::fs::remove_dir_all(&previews);
        let _ = std::fs::create_dir_all(&previews);
        let store_open = IndexStore::main(&dir).map(|_| ());
        assert!(
            store_open.is_err(),
            "the probe needs the store open to fail"
        );
        assert!(
            std::fs::read(dir.join("previews/marker.jpg")).is_err(),
            "the audit-base: the marker is already gone"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The daemon's `person:` field (round twelve, 2026-11-27, the user's pool law),
    /// pinned at the seam the GUIs hit: the face store answers the person's photo
    /// paths, the searcher POOLS the verb into them (the core pool math is pinned in
    /// `searcher::tests` — here the join and the walls), and every People-scoped
    /// request re-reads the consent record engine-side: the faces daemon's gate
    /// quoted at the store's edge, the daemon reading the store READ-ONLY.
    #[cfg(feature = "faces")]
    #[test]
    fn person_scope_pools_the_verb_into_the_faces_store_and_gates_on_consent() {
        use super::{handle, Searcher, State};
        use digup_core::face_store::{FaceRecord, FaceStore, ScanOf};
        use digup_core::{Candidate, FileKind, Modality, SegmentKind};
        use serde_json::{json, Map};
        use std::collections::{HashMap, HashSet};
        use std::time::Instant;

        let tmp = tempfile::tempdir().expect("a temp folder");
        let index = tmp.path().join("index");
        let media = tmp.path().join("media");
        std::fs::create_dir_all(&index).expect("the index folder");
        std::fs::create_dir_all(&media).expect("the media folder");
        let laura = ["laura1.jpg", "laura2.jpg"];
        let paths: Vec<String> = laura
            .iter()
            .chain(std::iter::once(&"crowd.jpg"))
            .map(|name| media.join(name).to_string_lossy().into_owned())
            .collect();
        for path in &paths {
            std::fs::write(path, b"a photo").expect("the photo is on disk");
        }

        let store: &'static IndexStore = Box::leak(Box::new(
            IndexStore::open(&index, digup_core::MAIN_FILE).expect("an index"),
        ));
        for path in &paths {
            let file = store
                .insert(&Candidate {
                    path: path.clone(),
                    kind: FileKind::Image,
                    size: 7,
                    mtime: 1.0,
                    inode: 7,
                    device: 3,
                })
                .expect("a photo row");
            store
                .finish(file, "done", None, HashMap::new())
                .expect("the row is done");
            store
                .add_segment(
                    file,
                    SegmentKind::Ocr,
                    Modality::Text,
                    None,
                    None,
                    Some("wakeboarding on the lake"),
                    Some("wakeboarding on the lake"),
                    None,
                )
                .expect("the words ride the FTS row");
        }

        // The face store: Laura's two photos carry one identity each (identical
        // face vectors), clustered into person 1 by the store's own pass.
        let faces_root = tmp.path().join("faces");
        let home = crate::face_service::faces_store_dir(&index, &faces_root);
        let face_store = FaceStore::open(&home).expect("the store opens");
        for name in laura {
            let path = media.join(name).to_string_lossy().into_owned();
            face_store
                .record_scan(
                    ScanOf {
                        path: &path,
                        size: 7,
                        mtime: 1.0,
                        width: 100,
                        height: 100,
                        det_model: "scrfd_10g_bnkps.onnx",
                        emb_model: "adaface_ir_101.onnx",
                    },
                    &[FaceRecord {
                        box_: [10.0, 10.0, 40.0, 40.0],
                        kps: [
                            [20.0, 20.0],
                            [30.0, 20.0],
                            [25.0, 28.0],
                            [21.0, 34.0],
                            [29.0, 34.0],
                        ],
                        score: 0.9,
                        vector: vec![0.5f32; 512],
                    }],
                    1.0,
                )
                .expect("the scan records Laura's face");
        }
        face_store
            .run_clustering(0.3, 0.1, 2, usize::MAX)
            .expect("the pass clusters");
        let persons = face_store.list_persons().expect("the people");
        assert_eq!(persons.len(), 1, "one unnamed cluster from the two photos");
        drop(face_store);

        let consent = tmp.path().join("faces-consent.json");
        std::fs::write(
            &consent,
            serde_json::to_vec(&json!({"consented": true, "accepted_at": 1.0,
                "det_model": "scrfd_10g_bnkps.onnx", "emb_model": "adaface_ir_101.onnx"}))
            .expect("the record is written"),
        )
        .expect("the consent file");

        let mut state = State {
            directory: index.clone(),
            store,
            searcher: Searcher::new(store, false).expect("the searcher opens"),
            code_store: None,
            code_searcher: None,
            roots: vec![media.to_string_lossy().into_owned()],
            skipped: vec![],
            types: HashSet::new(),
            code_roots: vec![],
            code_skipped: vec![],
            encoder: None,
            encoder_state: "stopped".to_string(),
            // The find-like media seam rides the same law as the text one: the pool test
            // never asks a picture, so both stay unloaded.
            media_encoder: None,
            media_encoder_state: "stopped".to_string(),
            vector_cache: HashMap::new(),
            last_query: Instant::now(),
            warned_about_source: false,
            log: index.join("worker.log"),
            faces_dir: Some(faces_root),
            faces_consent: Some(consent.clone()),
            face_reader: None,
        };
        let request = |extra: Vec<(&str, serde_json::Value)>| -> Map<String, serde_json::Value> {
            let mut fields = Map::new();
            fields.insert("q".into(), json!("wakeboarding"));
            for (key, value) in extra {
                fields.insert(key.into(), value);
            }
            fields
        };
        let paths_of = |reply: &serde_json::Value| -> Vec<String> {
            reply["hits"]
                .as_array()
                .expect("hits")
                .iter()
                .map(|hit| hit["path"].as_str().expect("a path").to_string())
                .collect()
        };

        // The field-less verb: every photo carrying the words answers (byte-parity).
        let plain = handle(&mut state, "search", &request(vec![])).expect("the plain answer");
        assert_eq!(paths_of(&plain).len(), 3, "the whole field answers");

        // `person: 1` — the pool: Laura's photos only.
        let pooled = handle(&mut state, "search", &request(vec![("person", json!(1))]))
            .expect("the pooled answer");
        let mut hits = paths_of(&pooled);
        hits.sort();
        assert_eq!(
            hits,
            vec![
                media.join("laura1.jpg").to_string_lossy().into_owned(),
                media.join("laura2.jpg").to_string_lossy().into_owned(),
            ],
            "the pool is the face store's rows — the crowd never enters it"
        );

        // A person with no faces pools to nothing — the empty-pool law (like an
        // empty scope), an honest empty answer, not an unscoped pass.
        let none = handle(&mut state, "search", &request(vec![("person", json!(999))]))
            .expect("the empty pool answers");
        assert!(paths_of(&none).is_empty(), "{none}");

        // The walls, in the daemon's own words.
        let bad = handle(&mut state, "search", &request(vec![("person", json!(0))]))
            .expect_err("id 0 is no person");
        assert!(bad.contains("greater than 0"), "{bad}");
        let code = handle(
            &mut state,
            "search",
            &request(vec![("person", json!(1)), ("code", json!(true))]),
        )
        .expect_err("the code index has no People");
        assert!(code.contains("never a code one"), "{code}");

        std::fs::write(
            &consent,
            serde_json::to_vec(&json!({"consented": false})).expect("the record"),
        )
        .expect("the record is revoked");
        let wall = handle(&mut state, "search", &request(vec![("person", json!(1))]))
            .expect_err("the engine gates on the record too");
        assert!(wall.contains("does not consent"), "{wall}");

        state.faces_consent = None;
        let wall = handle(&mut state, "search", &request(vec![("person", json!(1))]))
            .expect_err("no consent flag, no People scope");
        assert!(wall.contains("needs --faces-consent"), "{wall}");

        state.faces_consent = Some(consent.clone());
        std::fs::write(
            &consent,
            serde_json::to_vec(&json!({"consented": true})).expect("the record"),
        )
        .expect("the record is restored");
        state.faces_dir = None;
        let wall = handle(&mut state, "search", &request(vec![("person", json!(1))]))
            .expect_err("no faces home, no People scope");
        assert!(wall.contains("needs --faces-dir"), "{wall}");
    }
}
