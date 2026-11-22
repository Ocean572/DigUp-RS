//! `digup faces-service` (FACES.md Phase 2 §2): the face verbs as a JSON-lines
//! daemon — the same seam as `search-service`/`index-service`: one JSON request
//! per line on stdin, one reply per line on stdout carrying the request's `id`,
//! lines without an `id` are events, ParentWatch guards the parent.
//!
//! The verbs are the compiled list in `handle`: `scan` (crawl roots → decode →
//! SCRFD detect → embed → record, streaming `face-scan` events, then one online
//! clustering pass), `cluster`, `persons`, `person`, `faces`, `name`, `merge`,
//! `delete`, `confirm`, `pending`, `similar`, `prune`, `counts`, `quit`.
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
use digup_core::face_store::{FaceRecord, FaceRow, FaceStore, ScanOf};
use digup_core::options::IndexOptions;
use digup_core::paths;
use digup_native::rgb::RgbImage;
use digup_runtime::face::{Embedder, Scrfd};

use crate::emit::emit;

/// The daemon's start shape. `index` is the index folder (face.sqlite rides
/// beside index.sqlite, its own file — FACES.md §2); `models` carries the ONNX
/// pins; the thresholds are the clustering knobs from the config.
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
}

struct State {
    store: FaceStore,
    config: FaceServiceConfig,
    scrfd: Option<Scrfd>,
    embedder: Option<Embedder>,
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
    let store = FaceStore::open(&config.index).map_err(|e| e.to_string())?;
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
    let stamp = |key: &str, want: &str| -> Result<(), String> {
        match state
            .store
            .meta(key)
            .map_err(|e| e.to_string())?
            .as_deref()
        {
            None => state
                .store
                .set_meta(key, want)
                .map_err(|e| e.to_string()),
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
        state.scrfd = Some(scrfd);
    }
    if need_emb {
        let path = state.config.models.join(&state.config.emb_model);
        let embedder = Embedder::load(&path).map_err(|e| e.to_string())?;
        state
            .store
            .set_meta("embed_dim", &embedder.output_dim().to_string())
            .map_err(|e| e.to_string())?;
        state.embedder = Some(embedder);
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
        "person": r.person,
    })
}

fn handle(state: &mut State, op: &str, request: &Map<String, Value>) -> Result<Value, String> {
    match op {
        "scan" => scan(state, request),
        "cluster" => {
            let assigned = state
                .store
                .run_clustering(
                    state.config.merge_threshold,
                    state.config.collision_margin,
                    usize::MAX,
                )
                .map_err(|e| e.to_string())?;
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
            let persons: Vec<Value> = state
                .store
                .list_persons()
                .map_err(|e| e.to_string())?
                .iter()
                .map(|p| {
                    let mut row = serde_json::json!({"id": p.id, "name": p.name, "count": p.count, "cover": p.cover});
                    if covers {
                        let face = state
                            .store
                            .person_faces(p.id)
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
            let faces: Vec<Value> = state
                .store
                .person_faces(id)
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
                .faces_of(&path)
                .map_err(|e| e.to_string())?
                .iter()
                .map(row_json)
                .collect();
            Ok(serde_json::json!({"ok": true, "faces": faces}))
        }
        "name" => {
            // `person` names the row, not `id`: the protocol's reply key is `id`
            // (every JSON-lines daemon reserves it — SearchService/IndexService).
            let id = need_num(request, "person")?;
            let name = need_str(request, "name")?;
            let now = now();
            state
                .store
                .name_person(id, &name, now)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "merge" => {
            let into = need_num(request, "into")?;
            let from = need_num(request, "from")?;
            state
                .store
                .merge_persons(into, from)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "delete" => {
            let id = need_num(request, "person")?;
            state.store.delete_person(id).map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "confirm" => {
            let face = need_num(request, "face")?;
            let person = need_num(request, "person")?;
            let now = now();
            state
                .store
                .confirm_face(face, person, now)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true}))
        }
        "pending" => {
            // The collision guard's review queue: each queued face with its top
            // candidates (the engine's own cosine against the centroids — the
            // evidence, not a score invented here).
            let queue = state.store.pending_faces(500).map_err(|e| e.to_string())?;
            let centroids = state.store.centroids().map_err(|e| e.to_string())?;
            let persons = state.store.list_persons().map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for (face, vector) in queue {
                let mut candidates: Vec<(i64, f64)> = centroids
                    .iter()
                    .enumerate()
                    .map(|(pid, c)| {
                        (
                            pid as i64 + 1,
                            digup_core::face_cluster::cosine(&vector, &c.vec) as f64,
                        )
                    })
                    .collect();
                candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                let row = row_of(state, face)?;
                out.push(serde_json::json!({
                    "face": row,
                    "candidates": candidates.iter().take(3).map(|(pid, cos)| {
                        serde_json::json!({"person": pid, "name": persons.iter().find(|p| p.id == *pid).and_then(|p| p.name.clone()), "cos": cos})
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
                .face_vector(face)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no face {face}"))?;
            let mut hits = state
                .store
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
                .prune_outside(&|p: &str| roots.iter().any(|r| paths::is_inside(p, r)))
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true, "pruned": pruned}))
        }
        "counts" => {
            let c = state.store.counts().map_err(|e| e.to_string())?;
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
                    "dim": state.store.meta("embed_dim").map_err(|e| e.to_string())?,
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
    let report = crawler::crawl(&roots, &IndexOptions::default(), &mut |progress| {
        write_out(
            serde_json::json!({"event": "crawl", "checked": progress.checked, "root": progress.root}),
        );
    });
    let photos: Vec<_> = report
        .candidates
        .into_iter()
        .filter(|c| {
            let ext = Path::new(&c.path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            images.contains(&ext.as_str())
        })
        .filter(|c| {
            !state
                .store
                .is_fresh(&c.path, c.size, c.mtime)
                .unwrap_or(false)
        })
        .take(limit)
        .collect();
    let mut scanned = 0usize;
    let mut faces_total = 0usize;
    let mut skipped = 0usize;
    for candidate in &photos {
        let now = now();
        match decode_and_scan(
            state,
            candidate.path.as_str(),
            candidate.size,
            candidate.mtime,
            now,
        ) {
            Ok(n) => {
                scanned += 1;
                faces_total += n;
                write_out(serde_json::json!({
                    "event": "face-scan", "path": candidate.path, "faces": n,
                }));
            }
            Err(why) => {
                skipped += 1;
                write_out(serde_json::json!({
                    "event": "face-skip", "path": candidate.path, "why": why,
                }));
            }
        }
    }
    let assigned = state
        .store
        .run_clustering(
            state.config.merge_threshold,
            state.config.collision_margin,
            usize::MAX,
        )
        .map_err(|e| e.to_string())?;
    let c = state.store.counts().map_err(|e| e.to_string())?;
    write_out(
        serde_json::json!({"event": "cluster", "assigned": assigned, "persons": c.persons, "pending": c.pending}),
    );
    Ok(serde_json::json!({
        "ok": true, "files": scanned, "faces": faces_total, "skipped": skipped, "assigned": assigned,
    }))
}

fn decode_and_scan(
    state: &mut State,
    path: &str,
    size: i64,
    mtime: f64,
    now: f64,
) -> Result<usize, String> {
    use_models(state)?;
    let img = decode_photo(path)?;
    let faces = state
        .scrfd
        .as_ref()
        .expect("the detector is loaded")
        .detect(&img.pixels, img.width, img.height)
        .map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    for f in &faces {
        let crop = digup_runtime::face::norm_crop(&img.pixels, img.width, img.height, f);
        let vector = state
            .embedder
            .as_ref()
            .expect("the embedder is loaded")
            .embed(&crop)
            .map_err(|e| e.to_string())?;
        records.push(FaceRecord {
            box_: f.box_,
            kps: f.kps,
            score: f.score,
            vector,
        });
    }
    state
        .store
        .record_scan(
            ScanOf {
                path,
                size,
                mtime,
                det_model: &state.config.det_model,
                emb_model: &state.config.emb_model,
            },
            &records,
            now,
        )
        .map_err(|e| e.to_string())
}

fn row_of(state: &State, id: i64) -> Result<Value, String> {
    let mut row: Option<Value> = None;
    let rows = state.store.face_rows_by_id(id).map_err(|e| e.to_string())?;
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
