//! `digup faces-service` (FACES.md Phase 2 §2): the face verbs as a JSON-lines
//! daemon — the same seam as `search-service`/`index-service`: one JSON request
//! per line on stdin, one reply per line on stdout carrying the request's `id`,
//! lines without an `id` are events, ParentWatch guards the parent.
//!
//! The verbs are the compiled list in `handle`: `scan` (crawl roots → decode →
//! SCRFD detect → embed → record, streaming `face-scan` events, then one online
//! clustering pass), `cluster`, `persons`, `person`, `faces`, `name`, `merge`,
//! `delete`, `unload`, `confirm`, `pending`, `similar`, `prune`, `counts`, `quit`.
//! The store law (FACES.md §2/§3): piggyback freshness is `(path, size, mtime)`
//! — a file already recorded with those exact facts is not re-decoded; the
//! models load once per batch and unload after `--model-idle` seconds (FACES.md
//! Phase 2 §4). The consent gate lives server-side (`faces-consent.json`); the
//! daemon answers for whoever opened it, like every other engine daemon.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use digup_core::crawler;
use digup_core::face_store::{FaceRecord, FaceRow, FaceStore, ScanOf, FACE_FILE};
use digup_core::options::IndexOptions;
use digup_core::paths;
use digup_native::rgb::RgbImage;
use digup_runtime::face::{Embedder, Scrfd};
use sha2::{Digest, Sha256};

use crate::emit::emit;

/// The daemon's start shape. `index` is the index folder (its anchors hash the
/// device-side faces home — the #195 privacy move; the legacy beside-index.sqlite
/// shape moves on first open); `models` carries the ONNX
/// pins; the thresholds are the clustering knobs from the config.
#[derive(Clone)]
pub struct FaceServiceConfig {
    pub index: PathBuf,
    pub models: PathBuf,
    pub det_model: String,
    pub emb_model: String,
    pub merge_threshold: f32,
    pub collision_margin: f32,
    pub model_idle: f64,
    /// The consent record (FACES.md Phase 1 §5): the daemon refuses to serve
    /// without it — "what the UI gates, the engine gates". The server checks
    /// the same record server-side; the file is the deployment's fact.
    pub consent: Option<PathBuf>,
    /// The scan's worker pool (FACES.md 2026-11-22): parallel FILES, one shared
    /// session set per daemon — not per-op threading. 0 = auto (see `workers_for`).
    /// The daemon's own default; the shell and the server name no flag and inherit it.
    pub workers: usize,
    /// Folders the scan's crawl must not open (the app's Folders-tab skip list,
    /// `--exclude`, the main index's own law — the crawler applies a skip inside
    /// chosen folders and walks a folder chosen INSIDE a skipped one whole).
    pub excluded_folders: Vec<String>,
    /// The core law (FACES.md 2026-11-23 — Immich's `minFaces: 3`, the measured
    /// cure for the bench-library fragmentation: 195,604 faces → 37,513 cards at
    /// 0.5, of which 46 % were singleton/pair junk). A person exists from this
    /// many faces up; lower counts stay in the review queue. Default 3, Immich's
    /// shipped default; `--faces-min-faces` moves it.
    pub min_faces: usize,
    /// The detection-score floor for RECORDED faces (Immich runs its detector at
    /// 0.7, we record at the SCRFD gate's own 0.5). The detector seam stays at
    /// 0.5 — parity — this knob filters on the record side, so a deployment can
    /// skip background faces from low-confidence detections.
    /// `--faces-min-score` moves it; default 0.5 changes nothing.
    pub min_score: f32,
    /// The device-side faces root (#195, the privacy boundary): the faces' file
    /// rides `<faces_dir>/<anchor-hash>/face.sqlite`, NOT the index folder — an
    /// in-tree `.digup` carries index + previews only, so a folder carried away
    /// carries its index, never its faces. The store's anchors hash the folder
    /// (a v6 store's faces hash by index path); a legacy `face.sqlite` beside
    /// index.sqlite moves to the new home on first open.
    pub faces_dir: PathBuf,
}

struct State {
    /// The store behind its own lock (the scan runs its workers against it —
    /// FACES.md worker pool): per-file writes serialize on this mutex, the
    /// daemon state lock stays held for the whole scan, so the idle watch
    /// cannot unload models mid-scan.
    store: Arc<Mutex<FaceStore>>,
    config: FaceServiceConfig,
    /// Sessions shared with the scan workers: tract plans run from a shared
    /// `Arc` (tract-core `SimplePlan::run(self: &Arc<Self>)`, per-call scratch),
    /// so the models load once, not once per worker.
    scrfd: Option<Arc<Scrfd>>,
    embedder: Option<Arc<Embedder>>,
    last_use: Instant,
}

/// The daemon's whole state behind one lock — the search_service pattern: the
/// request thread and the idle watch share it, the lock is the safety.
struct Shared(Mutex<State>);
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

fn write_out(fields: Value) {
    emit(&fields);
}

pub fn face_service(config: FaceServiceConfig) -> i32 {
    match run(config) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

/// The faces' home on the device (#195, the privacy boundary): 16 hex of the sha256 of the
/// index folder's path — the store's DEVICE-LOCAL identity. The anchor set is NOT a usable
/// key (measured 2026-11-27 on the app's live 173k store: the v6→v7 migration creates one
/// anchor per row cluster — dozens of deep folders — and every folder the user adds to the
/// roots is a new anchor, so an anchor-keyed home MOVES under the running app: the People
/// tab empties and 195k faces re-scan). The privacy law needs no anchor: the home lives
/// under the faces root, OUTSIDE the `.digup`, so a store carried to another device computes
/// its own home from its own path and the faces never travel. The faces key rows by
/// `(path, size, mtime)`, which is the same spelling on the same device. `pub` for the
/// seam's pins (`tests/faces_move.rs`).
pub fn faces_store_dir(index: &Path, faces_root: &Path) -> PathBuf {
    let digest = Sha256::digest(index.to_string_lossy().as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    faces_root.join(hex)
}

/// The legacy move: a `face.sqlite` beside index.sqlite (the FACES.md §2 shape)
/// rides to the device-side home on first open — sidecars (`-wal`/`-shm`) with
/// it. Never destructive: a file already at the new home wins (the old copy
/// stands untouched for the operator), and the move is a rename on the same
/// volume when possible, a copy+delete across volumes. `pub` for the pins.
pub fn move_legacy_faces(index: &Path, dir: &Path) {
    let old = index.join(FACE_FILE);
    let new = dir.join(FACE_FILE);
    if !old.exists() || new.exists() {
        return;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    for suffix in ["", "-wal", "-shm"] {
        let from = index.join(format!("{FACE_FILE}{suffix}"));
        if !from.exists() {
            continue;
        }
        let to = dir.join(format!("{FACE_FILE}{suffix}"));
        if std::fs::rename(&from, &to).is_ok() {
            continue;
        }
        // Cross-volume fallback: copy, delete the source on success — the WAL
        // rides with the main file, so the trio moves whole.
        if std::fs::copy(&from, &to).is_ok() {
            let _ = std::fs::remove_file(&from);
        }
    }
}

fn run(config: FaceServiceConfig) -> Result<i32, String> {
    // The consent gate is the engine's own law (FACES.md Phase 1 §5 — the
    // license posture is "user-supplied-only, consented"): the daemon refuses
    // to open the store when `--faces-consent` names no accepted record.
    // The server checks the same file server-side; this is the seam below it.
    match &config.consent {
        None => {
            return Err(
                "faces-service needs --faces-consent (face recognition is user-consented)".into(),
            )
        }
        Some(path) => {
            let record: Value = serde_json::from_str(
                &std::fs::read_to_string(path)
                    .map_err(|e| format!("faces: no consent record at {}: {e}", path.display()))?,
            )
            .map_err(|e| format!("faces: unreadable consent record {}: {e}", path.display()))?;
            if record.get("consented").and_then(Value::as_bool) != Some(true) {
                return Err("faces: consent record does not consent".into());
            }
        }
    }
    // The privacy law (#195): faces ride the device, not the folder. The store's
    // anchors hash the device-side home; a legacy face.sqlite beside index.sqlite
    // moves there on first open (the daemon is the only opener).
    let faces_home = faces_store_dir(&config.index, &config.faces_dir);
    move_legacy_faces(&config.index, &faces_home);
    let store = Arc::new(Mutex::new(
        FaceStore::open(&faces_home).map_err(|e| e.to_string())?,
    ));
    let state = State {
        store,
        config: FaceServiceConfig {
            index: config.index.clone(),
            models: config.models.clone(),
            det_model: config.det_model.clone(),
            emb_model: config.emb_model.clone(),
            merge_threshold: config.merge_threshold,
            collision_margin: config.collision_margin,
            model_idle: config.model_idle,
            consent: config.consent.clone(),
            workers: config.workers,
            excluded_folders: config.excluded_folders.clone(),
            min_faces: config.min_faces,
            min_score: config.min_score,
            faces_dir: config.faces_dir.clone(),
        },
        scrfd: None,
        embedder: None,
        last_use: Instant::now(),
    };
    // The store's own model law (FACES.md tiers): a face.sqlite is embedded by
    // exactly ONE embedder pin; a scan under a different pin is a hard refusal.
    stamp_models(&state)?;
    write_out(serde_json::json!({
        "event": "ready",
        "index": config.index.to_string_lossy().into_owned(),
        "det_model": config.det_model,
        "emb_model": config.emb_model,
    }));
    // `Resource Management` (FACES.md Phase 2 §4): the models unload after the
    // idle window — the encoder's rule, the same lock-guarded watch as
    // search_service's idle_watch.
    let shared = Arc::new(Shared(Mutex::new(state)));
    std::thread::spawn({
        let shared = shared.clone();
        move || loop {
            std::thread::sleep(Duration::from_secs(2));
            let mut state = shared.0.lock().expect("the state lock");
            if state.scrfd.is_some()
                && state.last_use.elapsed().as_secs_f64() > state.config.model_idle
            {
                state.scrfd = None;
                state.embedder = None;
                write_out(serde_json::json!({"event": "model", "state": "stopped"}));
            }
        }
    });
    let stdin = std::io::stdin();
    for line in stdin.lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
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
            return Ok(0);
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
    Ok(0)
}

/// The model stamps ride in the store's meta (FACES.md §3 "dimension from the
/// pin"): the first scan writes them, every later scan must agree.
fn stamp_models(state: &State) -> Result<(), String> {
    // ONE guard for the whole stamping (the store is an Arc<Mutex<..>> since the
    // worker pool): a `match` on `lock().meta()` keeps the guard alive across the
    // arms, and the write arm would re-lock the same mutex from the same thread —
    // a fresh store deadlocked there (measured 2026-11-22: the daemon hung before
    // `ready`, strace futex-waiting).
    let store = state.store.lock().unwrap();
    let stamp = |key: &str, want: &str| -> Result<(), String> {
        match store.meta(key).map_err(|e| e.to_string())?.as_deref() {
            None => store.set_meta(key, want).map_err(|e| e.to_string()),
            Some(found) if found == want => Ok(()),
            Some(found) => Err(format!(
                "this face store was made by {found}; {want} would mix incomparable vectors (a tier switch is a full re-scan)"
            )),
        }
    };
    stamp("det_model", &state.config.det_model)?;
    stamp("emb_model", &state.config.emb_model)
}

/// Load the models on demand (they load once per batch — FACES.md Phase 2 §4).
fn use_models(state: &mut State) -> Result<(bool, bool), String> {
    let need_det = state.scrfd.is_none();
    let need_emb = state.embedder.is_none();
    if need_det {
        let path = state.config.models.join(&state.config.det_model);
        let scrfd = Scrfd::load(&path, (640, 640)).map_err(|e| e.to_string())?;
        state.scrfd = Some(Arc::new(scrfd));
    }
    if need_emb {
        let path = state.config.models.join(&state.config.emb_model);
        let embedder = Embedder::load(&path).map_err(|e| e.to_string())?;
        state
            .store
            .lock()
            .unwrap()
            .set_meta("embed_dim", &embedder.output_dim().to_string())
            .map_err(|e| e.to_string())?;
        state.embedder = Some(Arc::new(embedder));
    }
    if need_det || need_emb {
        write_out(serde_json::json!({
            "event": "model",
            "state": "loaded",
            "det_model": state.config.det_model,
            "emb_model": state.config.emb_model,
        }));
    }
    state.last_use = Instant::now();
    Ok((need_det, need_emb))
}

fn decode_photo(path: &str) -> Result<RgbImage, String> {
    let p = Path::new(path);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if let Some(note) = digup_native::image::not_read_yet(&ext) {
        return Err(note);
    }
    if ext == "heic" || ext == "heif" {
        // The ffmpeg path applies the orientation itself (engine seam).
        return digup_native::image::decode_with_ffmpeg(p).map_err(|e| e.to_string());
    }
    let Some(img) = RgbImage::load(p) else {
        // WebP/TIFF gaps on a stb without them: the ffmpeg seam (engine rule).
        return digup_native::image::decode_with_ffmpeg(p).map_err(|e| e.to_string());
    };
    let orientation = digup_native::exif::read(p)
        .map(|e| e.orientation)
        .unwrap_or(1);
    Ok(img.oriented(orientation))
}

fn row_json(r: &FaceRow) -> Value {
    serde_json::json!({
        "id": r.id,
        "path": r.path,
        "size": r.size,
        "mtime": r.mtime,
        "box": r.box_,
        "kps": r.kps,
        "score": r.score,
        // The source-pixel map of the crop (schema 3): box/kps are in these
        // coordinates, `/preview` is a scaled render — the thin client crops with
        // them. 0 = a row recorded before the map existed (the uncropped fallback).
        "width": r.width,
        "height": r.height,
        "person": r.person,
    })
}

fn handle(state: &mut State, op: &str, request: &Map<String, Value>) -> Result<Value, String> {
    match op {
        "scan" => scan(state, request),
        "cluster" => {
            // Multi-pass clustering (FACES.md 2026-11-23): the core law dissolves
            // sub-min_faces persons and requeues their faces; those faces merged
            // with centroids that ARE persons now, so the next pass absorbs them
            // (Immich re-runs its clusterer nightly for the same reason). Passes
            // repeat until the queue stops shrinking — bounded at 6.
            let mut assigned = 0usize;
            for _ in 0..6 {
                let n = state
                    .store
                    .lock()
                    .unwrap()
                    .run_clustering(
                        state.config.merge_threshold,
                        state.config.collision_margin,
                        state.config.min_faces,
                        usize::MAX,
                    )
                    .map_err(|e| e.to_string())?;
                assigned += n;
                if n == 0 {
                    break;
                }
            }
            Ok(serde_json::json!({"ok": true, "assigned": assigned}))
        }
        "persons" => {
            // `covers: true` (the People tab's cards): each person row gains
            // `cover_face` — its best-scoring face row (box/kps/path), so the
            // thin client can mint one preview handle per card from the same
            // answer. Without the flag the reply is the plain list.
            let covers = request
                .get("covers")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // `limit: N` (the People tab asks for the window it paints — GRID_CAP cards,
            // the most-photo people first): the verb answers the top N by photo count and
            // pays the cover query for exactly those rows. A store of 8,868 people at
            // covers:true costs one face query per person (measured 34 s cold on the live
            // store, over the thin client's 120 s read clock), and the panel never paints
            // more than its cap — the window is what it asked for. Without `limit` the
            // reply keeps its exact shape: every person, id order, covers on all of them.
            let window = request.get("limit").and_then(Value::as_u64);
            // `named: true` (the People tab's autocomplete list, 2026-11-23): answer
            // only the named people, ordered by photo count — the names the user has
            // given are the merge targets. Cheap: the named set is small; covers are
            // paid only for the rows returned.
            let named_only = request
                .get("named")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut all = state
                .store
                .lock()
                .unwrap()
                .list_persons()
                .map_err(|e| e.to_string())?;
            if named_only {
                all.retain(|p| p.name.as_deref().is_some_and(|s| !s.is_empty()));
                all.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
            }
            if let Some(n) = window {
                all.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
                all.truncate(n as usize);
            }
            let persons: Vec<Value> = all
                .iter()
                .map(|p| {
                    let mut row = serde_json::json!({"id": p.id, "name": p.name, "count": p.count, "cover": p.cover});
                    if covers {
                        let face = state.store.lock().unwrap().person_faces(p.id)
                            .map_err(|e| e.to_string())?
                            .into_iter()
                            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
                        row["cover_face"] = face.map(|f| row_json(&f)).unwrap_or(Value::Null);
                    }
                    Ok(row)
                })
                .collect::<Result<Vec<Value>, String>>()?;
            Ok(serde_json::json!({"ok": true, "persons": persons}))
        }
        "person" => {
            let id = need_num(request, "person")?;
            // The painted page (People-tab gallery, 2026-11-23): `order: "photo"` =
            // the photos' own dates newest first (`mtime`, id on ties); any other
            // value keeps the verb's row order. `limit`/`offset` is the window the
            // thin client scrolls. No fields = the full list, the exact shape the
            // verb has always answered.
            let order = request
                .get("order")
                .and_then(Value::as_str)
                .unwrap_or("scan");
            let limit = request
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n.min(i64::MAX as u64) as i64)
                .unwrap_or(i64::MAX);
            let offset = request.get("offset").and_then(Value::as_u64).unwrap_or(0) as i64;
            let faces: Vec<Value> = state
                .store
                .lock()
                .unwrap()
                .person_faces_page(id, order, limit, offset)
                .map_err(|e| e.to_string())?
                .iter()
                .map(row_json)
                .collect();
            Ok(serde_json::json!({"ok": true, "faces": faces}))
        }
        "faces" => {
            let path = need_str(request, "path")?;
            let faces: Vec<Value> = state
                .store
                .lock()
                .unwrap()
                .faces_of(&path)
                .map_err(|e| e.to_string())?
                .iter()
                .map(row_json)
                .collect();
            Ok(serde_json::json!({"ok": true, "faces": faces}))
        }
        "paths" => {
            // The search filter (round six, 2026-11-23, the user's design): the semantic
            // search answers its rows; the People filter asks which of THOSE paths carry
            // the person's face. The face rows are the engine's own facts — this relays
            // membership and invents nothing: the search's z-scores are untouched.
            let id = need_num(request, "person")?;
            let empty = Vec::new();
            let wanted: std::collections::HashSet<String> = request
                .get("paths")
                .and_then(Value::as_array)
                .unwrap_or(&empty)
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let hits = state
                .store
                .lock()
                .unwrap()
                .person_paths_among(id, &wanted)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true, "paths": hits}))
        }
        "name" => {
            // `person` names the row, not `id`: the protocol's reply key is `id`
            // (every JSON-lines daemon reserves it — SearchService/IndexService).
            let id = need_num(request, "person")?;
            let name = need_str(request, "name")?;
            let now = now();
            state
                .store
                .lock()
                .unwrap()
                .name_person(id, &name, now)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "merge" => {
            let into = need_num(request, "into")?;
            let from = need_num(request, "from")?;
            state
                .store
                .lock()
                .unwrap()
                .merge_persons(into, from)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "delete" => {
            let id = need_num(request, "person")?;
            state
                .store
                .lock()
                .unwrap()
                .delete_person(id)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "unload" => {
            // The bulk `delete` over the unnamed group (the app/server "Unload
            // unnamed" button, the user's law 2026-11-27): unnamed persons are
            // dropped, their faces requeue onto the named centroids' pass;
            // named persons are never touched. The reply relays the engine's
            // own counts — no invented numbers.
            let (persons, faces) = state
                .store
                .lock()
                .unwrap()
                .unload_unnamed()
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true, "persons": persons, "faces": faces}))
        }
        "confirm" => {
            let face = need_num(request, "face")?;
            let person = need_num(request, "person")?;
            let now = now();
            state
                .store
                .lock()
                .unwrap()
                .confirm_face(face, person, now)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "pending" => {
            // The collision guard's review queue, BEST MATCH FIRST (the user's
            // law 2026-11-23: "faces with close matches should be at the top —
            // I can't scroll through that many faces"). A window of the queue is
            // scored against every centroid (the engine's own cosines — the
            // evidence, not a score invented here), sorted by its best candidate
            // descending, ties broken by face identity — the pool's determinism
            // law: never the order things ran. Faces below the deployment's
            // min-score stay queued and unseen (Immich's minScore law — they are
            // background-face noise, not review material).
            const WINDOW: usize = 1_000;
            const PAGE: usize = 500;
            let queue = state
                .store
                .lock()
                .unwrap()
                .pending_faces(WINDOW)
                .map_err(|e| e.to_string())?;
            let centroids = state
                .store
                .lock()
                .unwrap()
                .centroids_with_ids()
                .map_err(|e| e.to_string())?;
            let names: std::collections::HashMap<i64, Option<String>> = state
                .store
                .lock()
                .unwrap()
                .list_persons()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|p| (p.id, p.name))
                .collect();
            let mut scored: Vec<ScoredFace> = Vec::with_capacity(queue.len());
            for (face, vector) in queue {
                let mut top: Vec<(i64, f32)> = Vec::with_capacity(4);
                for (id, c) in &centroids {
                    let cos = digup_core::face_cluster::cosine(&vector, &c.vec);
                    if top.len() < 3 || cos > top[2].1 {
                        top.push((*id, cos));
                        top.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        top.truncate(3);
                    }
                }
                scored.push((top.first().map(|t| t.1).unwrap_or(f32::MIN), face, top));
            }
            scored.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.cmp(&b.1))
            });
            let mut out = Vec::new();
            for (_, face, top) in scored {
                if out.len() == PAGE {
                    break;
                }
                let row = row_of(state, face)?;
                if row["score"]
                    .as_f64()
                    .map(|s| s < state.config.min_score as f64)
                    .unwrap_or(true)
                {
                    continue;
                }
                out.push(serde_json::json!({
                    "face": row,
                    "candidates": top.iter().map(|(pid, cos)| {
                        serde_json::json!({"person": pid, "name": names.get(pid).cloned().flatten(), "cos": cos})
                    }).collect::<Vec<_>>(),
                }));
            }
            Ok(serde_json::json!({"ok": true, "pending": out, "count": out.len()}))
        }
        "similar" => {
            // `find-similar-faces`: a face's own vector against the store,
            // brute force (FACES.md §2 — same law as the index's vectors).
            let face = need_num(request, "face")?;
            let limit = request
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(20)
                .clamp(1, 200) as usize;
            let vector = state
                .store
                .lock()
                .unwrap()
                .face_vector(face)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no face {face}"))?;
            let mut hits = state
                .store
                .lock()
                .unwrap()
                .find_similar(&vector, limit + 1)
                .map_err(|e| e.to_string())?;
            hits.retain(|(row, _)| row.id != face);
            hits.truncate(limit);
            let faces: Vec<Value> = hits
                .iter()
                .map(|(row, cos)| {
                    let mut v = row_json(row);
                    v["cos"] = serde_json::json!(cos);
                    v
                })
                .collect();
            Ok(serde_json::json!({"ok": true, "faces": faces}))
        }
        "prune" => {
            // Ghost-row cascade (FACES.md §2): faces whose file is out of the
            // target's roots leave with the main index's scope law.
            let roots: Vec<String> = request
                .get("roots")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(paths::standardize)
                        .collect()
                })
                .unwrap_or_default();
            let pruned = state
                .store
                .lock()
                .unwrap()
                .prune_outside(&|p: &str| roots.iter().any(|r| paths::is_inside(p, r)))
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true, "pruned": pruned}))
        }
        "counts" => {
            let c = state
                .store
                .lock()
                .unwrap()
                .counts()
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": true,
                "faces": c.faces,
                "assigned": c.assigned,
                "persons": c.persons,
                "named": c.named,
                "pending": c.pending,
                "models": {
                    "det": state.config.det_model,
                    "emb": state.config.emb_model,
                    "dim": state.store.lock().unwrap().meta("embed_dim").map_err(|e| e.to_string())?,
                },
            }))
        }
        _ => Err(format!("faces: no such op {op}")),
    }
}

/// `scan`: the crawl the index already runs (FACES.md §3 "piggyback sync" —
/// the same roots, the same crawler), filtered to still images; every file
/// whose `(path, size, mtime)` is NOT already recorded gets decode → detect →
/// embed → record, streaming `face-scan` events, then one clustering pass.
fn scan(state: &mut State, request: &Map<String, Value>) -> Result<Value, String> {
    let roots: Vec<PathBuf> = request
        .get("roots")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();
    if roots.is_empty() {
        return Err("scan needs roots".into());
    }
    let limit = request
        .get("limit")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .unwrap_or(i64::MAX) as usize;
    let images: Vec<&str> = digup_core::rules::IMAGE_EXTENSIONS.to_vec();
    // The scan's crawl honors the deployment's skip list (the app's Folders-tab
    // skips — the main index's own exclusion law, FACES.md 2026-11-22).
    let crawl_options = IndexOptions {
        excluded_folders: state.config.excluded_folders.clone(),
        ..IndexOptions::default()
    };
    let report = crawler::crawl(&roots, &crawl_options, &mut |progress| {
        write_out(
            serde_json::json!({"event": "crawl", "checked": progress.checked, "root": progress.root}),
        );
    });
    let candidates: Vec<digup_core::model::Candidate> = report
        .candidates
        .into_iter()
        .filter(|c| {
            let ext = Path::new(&c.path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            images.contains(&ext.as_str())
        })
        .collect();
    // Piggyback freshness (FACES.md §3): the files whose exact `(path, size, mtime)`
    // the store recorded are NOT re-decoded — including the faceless ones (the
    // scans table, schema 2), which is where a photo library's cost really lives.
    // `fresh` relays how many that skipped; `limit` bounds the decode work per scan.
    let photos: Vec<digup_core::model::Candidate> = candidates
        .iter()
        .filter(|c| {
            !state
                .store
                .lock()
                .unwrap()
                .is_fresh(&c.path, c.size, c.mtime)
                .unwrap_or(false)
        })
        .take(limit)
        .cloned()
        .collect();
    let fresh = candidates.len() - photos.len();
    // The worker pool (FACES.md 2026-11-22, measured on this box: the scan ran ONE
    // thread of 20 — pidstat 100 % on a single core — with detect 993 ms/photo + embed
    // on the critical path). Files are the unit of parallelism: each worker owns a
    // file's decode → detect → embed, and store writes serialize on the store lock.
    // The sessions are shared, not per-worker: tract plans run from `&Arc<Self>`
    // (tract-core plan.rs — per-call scratch), so the models load once. The daemon's
    // state lock stays held across the scan, so the idle watch cannot unload models
    // mid-scan. Events ride a channel to this thread — whole lines, streamed live.
    use_models(state)?;
    let det = Arc::clone(state.scrfd.as_ref().expect("the detector is loaded"));
    let emb = Arc::clone(state.embedder.as_ref().expect("the embedder is loaded"));
    let store = Arc::clone(&state.store);
    let config = state.config.clone();
    let workers = workers_for(state.config.workers, photos.len());
    let chunk = photos.len().div_ceil(workers).max(1);
    let sums = Arc::new(Mutex::new((0usize, 0usize, 0usize)));
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::scope(|scope| {
        for candidates in photos.chunks(chunk) {
            let (det, emb, store, tx, sums, config) = (
                Arc::clone(&det),
                Arc::clone(&emb),
                Arc::clone(&store),
                tx.clone(),
                Arc::clone(&sums),
                config.clone(),
            );
            scope.spawn(move || {
                let pool = Pool {
                    det: &det,
                    emb: &emb,
                    store: &store,
                    config: &config,
                };
                let mut scanned = 0usize;
                let mut faces_total = 0usize;
                let mut skipped = 0usize;
                for candidate in candidates {
                    match decode_and_scan(
                        &pool,
                        candidate.path.as_str(),
                        candidate.size,
                        candidate.mtime,
                        now(),
                    ) {
                        Ok(n) => {
                            scanned += 1;
                            faces_total += n;
                            let _ = tx.send(serde_json::json!({
                                "event": "face-scan", "path": candidate.path, "faces": n,
                            }));
                        }
                        Err(why) => {
                            skipped += 1;
                            let _ = tx.send(serde_json::json!({
                                "event": "face-skip", "path": candidate.path, "why": why,
                            }));
                        }
                    }
                }
                let mut total = sums.lock().expect("the sums lock");
                total.0 += scanned;
                total.1 += faces_total;
                total.2 += skipped;
            });
        }
        // This thread streams events as they arrive; the stream ends when every
        // worker's sender is dropped, and `scope` joins them on the way out.
        drop(tx);
        for event in rx {
            write_out(event);
        }
    });
    let (scanned, faces_total, skipped) = *sums.lock().expect("the sums lock");
    state.last_use = Instant::now();
    let mut assigned = 0usize;
    for _ in 0..6 {
        let n = state
            .store
            .lock()
            .unwrap()
            .run_clustering(
                state.config.merge_threshold,
                state.config.collision_margin,
                state.config.min_faces,
                usize::MAX,
            )
            .map_err(|e| e.to_string())?;
        assigned += n;
        if n == 0 {
            break;
        }
    }
    let c = state
        .store
        .lock()
        .unwrap()
        .counts()
        .map_err(|e| e.to_string())?;
    write_out(
        serde_json::json!({"event": "cluster", "assigned": assigned, "persons": c.persons, "pending": c.pending}),
    );
    Ok(serde_json::json!({
        "ok": true, "files": scanned, "faces": faces_total, "skipped": skipped,
        "fresh": fresh, "assigned": assigned,
    }))
}

/// A worker's view of the scan: the shared sessions, the lock-serialized store,
/// and the model pins (FACES.md worker pool). Bundled as one receiver so the
/// per-file routine reads as one unit — and stays inside clippy's argument gate.
struct Pool<'a> {
    det: &'a Scrfd,
    emb: &'a Embedder,
    store: &'a Mutex<FaceStore>,
    config: &'a FaceServiceConfig,
}

fn decode_and_scan(
    pool: &Pool,
    path: &str,
    size: i64,
    mtime: f64,
    now: f64,
) -> Result<usize, String> {
    let img = decode_photo(path)?;
    let faces = pool
        .det
        .detect(&img.pixels, img.width, img.height)
        .map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    for f in &faces {
        // The deployment's detection-score floor (Immich's minScore law): the
        // detector keeps its 0.5 SCRFD gate (parity), the record side applies
        // the knob — low-confidence detections (background faces, posters) never
        // become faces, so they never become cards or queue entries.
        if f.score < pool.config.min_score {
            continue;
        }
        let crop = digup_runtime::face::norm_crop(&img.pixels, img.width, img.height, f);
        let vector = pool.emb.embed(&crop).map_err(|e| e.to_string())?;
        records.push(FaceRecord {
            box_: f.box_,
            kps: f.kps,
            score: f.score,
            vector,
        });
    }
    pool.store
        .lock()
        .expect("the store lock")
        .record_scan(
            ScanOf {
                path,
                size,
                mtime,
                width: img.width as i64,
                height: img.height as i64,
                det_model: &pool.config.det_model,
                emb_model: &pool.config.emb_model,
            },
            &records,
            now,
        )
        .map_err(|e| e.to_string())
}

/// The scan's worker count (FACES.md 2026-11-22). 0 = auto: cores/2, capped at 8 —
/// measured on this box (20 cores): the face pipeline is one thread deep, and each
/// worker's decode spawns a multithreaded ffmpeg, so half the cores cover the
/// parallel files and leave the rest to the machine. An explicit number names the
/// pool; files cap it (never a worker with nothing to do).
fn workers_for(configured: usize, photos: usize) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let workers = if configured > 0 {
        configured
    } else {
        (cores / 2).clamp(1, 8)
    };
    workers.min(photos).max(1)
}

/// One queued face scored for the review queue: (best candidate cosine, face id,
/// top-3 candidates as (person id, cosine)) — the engine's cosines, relayed.
type ScoredFace = (f32, i64, Vec<(i64, f32)>);

fn row_of(state: &State, id: i64) -> Result<Value, String> {
    let mut row: Option<Value> = None;
    let rows = state
        .store
        .lock()
        .unwrap()
        .face_rows_by_id(id)
        .map_err(|e| e.to_string())?;
    if let Some(r) = rows.first() {
        row = Some(row_json(r));
    }
    row.ok_or_else(|| format!("no face {id}"))
}

fn need_num(request: &Map<String, Value>, key: &str) -> Result<i64, String> {
    request
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{key} needs a number"))
}

fn need_str(request: &Map<String, Value>, key: &str) -> Result<String, String> {
    request
        .get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| format!("{key} needs a string"))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod scan_workers_tests {
    use super::workers_for;

    #[test]
    fn auto_workers_is_cores_half_capped_at_eight() {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        assert_eq!(workers_for(0, 1000), (cores / 2).clamp(1, 8).min(1000));
        assert_eq!(workers_for(0, 3), (cores / 2).clamp(1, 8).min(3));
        assert_eq!(workers_for(3, 1000), 3);
        assert_eq!(workers_for(64, 1000), 64); // an explicit number names the pool
        assert_eq!(workers_for(8, 5), 5); // never more workers than files
        assert_eq!(workers_for(0, 0), 1); // zero files: one worker does nothing
    }
}
