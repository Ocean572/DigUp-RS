//! The People tab's engine surface (docs/implementation/FACES.md), shell-side: the consent record, the
//! faces writer lock, and calls to `digup faces-service`. The same thin-client law as every other route
//! through the helper: the engine answers the face verbs over JSON lines, the shell relays them, and
//! invents nothing — counts, crops and cosines all ride the rows.
//!
//! This is the shape of the server's own seam (`crates/server/src/faces.rs`, ported for the app): the
//! record is this install's durable fact beside `settings.json` (0600 like the server's tokens file),
//! EVERY faces verb refuses before a daemon spawns when the record is absent, and the daemon enforces
//! the same file engine-side (`--faces-consent`: what the UI gates, the engine gates). The writer lock
//! keeps the server's file name beside the index — a shell scan and a server scan of the same index
//! exclude each other, and a scan is a writer of face.sqlite only, never of index.sqlite.
//!
//! The verb timeouts ride the server's pins (config.rs): counts 60 s, the reads 120 s, the scan 3600 s
//! (DIGUP_SERVER_FACE_SCAN_TIMEOUT_S's default). The scan is resumable the same way — files already
//! recorded are skipped, so a scan that ends at the ceiling continues on the next press. The model
//! knobs (merge 0.5, collision 0.10, model idle 60 s) ride the daemon's own defaults, which equal the
//! server's pins, so the shell names no flags the daemon would answer with its defaults.

use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};

use crate::helper::Launch;
use crate::Shell;

/// The faces writer lock, beside the index — the server's own inode name (faces.rs:35), so a shell
/// scan and a server scan of one index folder take turns exactly like two servers would.
pub const FACES_LOCK_NAME: &str = ".digup-server.faces.writer.lock";

/// The six user-supplied packs, name for name with `FaceModelFiles::ALL`
/// (crates/runtime/src/models.rs:145-190). The payload ships code, never weights: these arrive only
/// through `download-model --faces`, and only the engine checks their hashes — here it is presence.
pub const FACE_MODELS: [&str; 6] = [
    "scrfd_10g_bnkps.onnx",
    "w600k_mbf.onnx",
    "w600k_r50.onnx",
    "adaface_ir_18.onnx",
    "adaface_ir_50.onnx",
    "adaface_ir_101.onnx",
];
/// The detector pin (the record's default, the daemon's default — the parity tier).
pub const DETECTOR: &str = "scrfd_10g_bnkps.onnx";
/// The embedder the acceptance names when the picker says nothing (the server's consent default,
/// crates/server/src/main.rs:2025).
pub const DEFAULT_EMBEDDER: &str = "w600k_mbf.onnx";

/// The verb clocks. Reads ride the server's pins (counts 60 s, the reads 120 s). The SCAN is
/// unbounded shell-side (2026-11-22, the user's law: one press runs the whole folder): the app
/// streams events and has no HTTP request to outlive — the 30-day wall is a watchdog, not a
/// bound; a whole library finishes inside it. Kill-safety and the fresh-skip resume are
/// unchanged, so the wall being far away costs nothing. The SERVER keeps its 3600 s ceiling
/// (DIGUP_SERVER_FACE_SCAN_TIMEOUT_S): a synchronous scan rides an HTTP request, and the
/// ceiling is a pause there, not a loss — the next press resumes.
pub fn verb_timeout(op: &str) -> Duration {
    match op {
        "scan" => Duration::from_secs(30 * 24 * 3600),
        "counts" => Duration::from_secs(60),
        _ => Duration::from_secs(120),
    }
}

/// The record's file: beside settings.json. The shell has one index (the server's named targets are
/// its own law), so the record is the main target's bare name (faces.rs:38-44).
pub fn consent_path(config_dir: &Path) -> PathBuf {
    config_dir.join("faces-consent.json")
}

/// The consent record, read: None = not consented (absent file). A record that parses but is not the
/// server's shape (no `consented: true`) reads as absent — a malformed record is no consent, never a
/// partial one (the server's exact rule, faces.rs:46-58).
pub fn read_consent(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let record: Value = serde_json::from_str(&text).ok()?;
    let accepted = record
        .get("consented")
        .and_then(Value::as_bool)
        .filter(|yes| *yes)
        .is_some();
    accepted.then_some(record)
}

pub struct ConsentRecord {
    pub det_model: String,
    pub emb_model: String,
}

impl ConsentRecord {
    /// The record's model stamps; the detector pin defaults to the parity tier when the record
    /// predates the field (the daemon's own default matches, crates/cli/src/main.rs:244-247).
    pub fn of(record: &Value) -> ConsentRecord {
        ConsentRecord {
            det_model: record
                .get("det_model")
                .and_then(Value::as_str)
                .unwrap_or(DETECTOR)
                .to_string(),
            emb_model: record
                .get("emb_model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }
}

/// Persist the record atomically, 0600 — the server's shape (faces.rs:84-105: write `.new`, rename,
/// chmod): recreation never half-writes a consent.
pub fn write_consent(path: &Path, record: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = format!("{}.new", path.display());
    std::fs::write(&tmp, serde_json::to_vec(record).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", tmp))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Opting out deletes the record: the verbs start refusing, the face.sqlite stays on disk (the user's
/// data; deleting it is the user's act — a consent withdrawal never mass-deletes).
pub fn remove_consent(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The faces writer lock: flock(LOCK_EX|LOCK_NB) beside the index — busy is the sentence the server
/// answers 409 with, relayed here as the command's error. The guard releases on Drop (jobs.rs' order).
pub struct FacesWriterGuard {
    file: std::fs::File,
}

impl FacesWriterGuard {
    pub fn acquire(index: &Path) -> Result<FacesWriterGuard, String> {
        let _ = std::fs::create_dir_all(index);
        let lock_path = index.join(FACES_LOCK_NAME);
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) => return Err(format!("faces: {}: {error}", lock_path.display())),
        };
        if !try_lock(&file) {
            return Err(format!(
                "faces: {}: a faces scan is already running",
                index.display()
            ));
        }
        Ok(FacesWriterGuard { file })
    }
}

impl Drop for FacesWriterGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            let _ = self.file.unlock();
        }
    }
}

#[cfg(unix)]
fn try_lock(file: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(windows)]
fn try_lock(file: &std::fs::File) -> bool {
    file.try_lock().is_ok()
}

/// Where the packs live: the `--models` launch argument, else the platform folder `storage()` names
/// (the models line of Settings → Storage — the same folder `download-model --faces` fills).
pub fn models_folder(shell: &Shell) -> Option<PathBuf> {
    shell
        .options
        .models
        .clone()
        .or_else(|| crate::storage(shell)["models"].as_str().map(PathBuf::from))
}

/// The acceptance's embedder must be a pinned pack, never the detector (the server's tier law,
/// crates/server/src/main.rs:2032-2050).
pub fn valid_embedder(name: &str) -> bool {
    FACE_MODELS.contains(&name) && name != DETECTOR
}

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The consent wall, shell-side: no accepted record → the verbs refuse, before any daemon spawns.
pub fn consent_guard(shell: &Shell) -> Result<(PathBuf, ConsentRecord), String> {
    let config = shell
        .settings_path
        .parent()
        .ok_or("faces: no config folder for the consent record")?;
    let consent = consent_path(config);
    match read_consent(&consent) {
        Some(record) => Ok((consent, ConsentRecord::of(&record))),
        None => Err(
            "faces: face recognition is not consented — accept it in the People tab first"
                .to_string(),
        ),
    }
}

/// The daemon's argv, exact (crates/cli/src/main.rs:220-264, the server's order, faces.rs:196-217):
/// the consent file, the record's tier, the packs' folder, and the Folders-tab skips — every
/// `settings.skipped` folder rides `--exclude` (2026-11-22), the same list the search daemons get
/// (main.rs `folder_arguments`): the face scan must not open what the user skipped. The knob flags
/// ride the daemon's defaults, which equal the server's pins — naming them would re-say numbers
/// the engine already owns.
pub fn faces_arguments(
    index: Option<&Path>,
    consent: &Path,
    record: &ConsentRecord,
    models: Option<&Path>,
    faces_dir: &Path,
    skipped: &[String],
) -> Vec<String> {
    let mut args = vec!["faces-service".to_string()];
    if let Some(index) = index {
        args.extend(["--index".to_string(), index.display().to_string()]);
    }
    // The #195 privacy move: the faces ride the device-side faces home (the app's
    // config folder), never the index folder — a portable `.digup` in a carried
    // folder carries its index, never its faces.
    args.extend([
        "--faces-dir".to_string(),
        faces_dir.join("faces").display().to_string(),
    ]);
    args.extend([
        "--faces-consent".to_string(),
        consent.display().to_string(),
        "--det-model".to_string(),
        record.det_model.clone(),
        "--emb-model".to_string(),
        record.emb_model.clone(),
    ]);
    for folder in skipped {
        args.extend(["--exclude".to_string(), folder.clone()]);
    }
    if let Some(models) = models {
        args.extend(["--faces-models".to_string(), models.display().to_string()]);
    }
    args
}

/// One call to `faces-service`: spawn, the `ready` handshake, one request, the reply — events on the
/// way (the scan streams crawl/face-scan/cluster lines) go to `on_event` — then `quit` and reap.
/// Per-call, like the server: the store open is the daemon's only boot work, the models load on the
/// verbs that ask (faces.rs:183-186).
pub fn run_faces(
    launch: Launch,
    record: &ConsentRecord,
    op: &str,
    mut fields: Map<String, Value>,
    timeout: Duration,
    on_event: &mut dyn FnMut(Value),
) -> Result<Value, String> {
    if record.emb_model.is_empty() {
        return Err("faces: the consent record names no embedder".to_string());
    }
    let (mut child, mut stdin, stdout) = launch.spawn()?;
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(message);
            }
        }
    });
    // The handshake: faces.rs:227 `start(Some("ready"), 60 s)`. A payload built without the faces
    // feature answers faces-service with the usage text and exit 2 — its stdout closes without a
    // line, which lands as the "stopped" refusal, relayed, not dressed up.
    let ready_by = Instant::now() + Duration::from_secs(60).min(timeout);
    loop {
        match rx.recv_timeout(ready_by.saturating_duration_since(Instant::now())) {
            Ok(message) if message["event"] == "ready" => {
                // The home rides the ready line (round eleven): the caller scopes its
                // asset reader to the cover folder — the shell keeps no private copy
                // of the engine's home law.
                on_event(message);
                break;
            }
            Ok(message) => on_event(message),
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("faces: {op} — the helper never said ready"));
            }
            Err(RecvTimeoutError::Disconnected) => {
                crate::helper::reap(&mut child);
                return Err(
                    "faces: the helper is built without face recognition (upgrade DigUp)"
                        .to_string(),
                );
            }
        }
    }
    fields.insert("op".to_string(), Value::String(op.to_string()));
    fields.insert("id".to_string(), json!(1));
    let request = Value::Object(fields);
    writeln!(stdin, "{request}")
        .and_then(|_| stdin.flush())
        .map_err(|e| format!("faces: {op} — the helper isn't answering: {e}"))?;
    let deadline = Instant::now() + timeout;
    let reply = loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(message) => match message.get("id").and_then(Value::as_u64) {
                Some(1) => break message,
                Some(_) => {}
                None => on_event(message),
            },
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "faces: {op} ran past {:?} and was stopped — files recorded so far are kept, press again to continue",
                    timeout
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                crate::helper::reap(&mut child);
                return Err(format!("faces: the helper stopped during {op}"));
            }
        }
    };
    crate::helper::stop(child, stdin, &json!({"id": 2, "op": "quit"}).to_string());
    if reply.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(match reply.get("error") {
            Some(Value::String(text)) => format!("faces: {text}"),
            Some(value) => format!("faces: {value}"),
            None => "faces: the daemon refused with no reason".to_string(),
        });
    }
    Ok(reply)
}

/// Drop the protocol `id` and the daemon's own `ok` from a relayed answer (the server's
/// `engine_answer`, crates/server/src/main.rs:1937-1946) — the command adds its own envelope.
pub fn engine_answer(mut row: Value) -> Value {
    if let Some(map) = row.as_object_mut() {
        map.remove("id");
        map.remove("ok");
    }
    row
}

/// One faces verb: the launch built from this install's index, consent and packs, the reply relayed
/// with its protocol fields stripped. Reads and mutations never take the writer lock (the server's
/// rule — only the scan writes); the `ready` line scopes the cover reader (round eleven).
pub fn verb(shell: &Shell, op: &str, fields: Map<String, Value>) -> Result<Value, String> {
    let (_consent, record) = consent_guard(shell)?;
    let launch = faces_launch(shell, &record, op)?;
    let timeout = verb_timeout(op);
    let reply = run_faces(launch, &record, op, fields, timeout, &mut |event| {
        if let Some(app) = shell.app.lock().unwrap().clone() {
            scope_faces(&app, &event);
        }
    })?;
    Ok(engine_answer(reply))
}

/// The asset reader's faces scope (round eleven): the cover cards are files beside
/// `face.sqlite` inside the device-side home — the daemon's `ready` names that home,
/// and only its `covers` folder is opened to the webview. The store itself never
/// enters the asset scope; the panel reads the same rows it always read.
pub fn scope_faces(app: &tauri::AppHandle, event: &Value) {
    if event["event"] != "ready" {
        return;
    }
    let Some(faces) = event.get("faces").and_then(Value::as_str) else {
        return;
    };
    let _ = app
        .asset_protocol_scope()
        .allow_directory(Path::new(faces).join("covers"), false);
}

/// The Launch for one verb: the helper binary and env every helper gets (`DIGUP_MODELS`, the chosen
/// processor), the scan below the person's own programs (background: true — nice, as indexing is).
pub fn faces_launch(shell: &Shell, record: &ConsentRecord, op: &str) -> Result<Launch, String> {
    let consent = consent_path(
        shell
            .settings_path
            .parent()
            .ok_or("faces: no config folder")?,
    );
    let skipped = shell.settings.lock().unwrap().skipped.clone();
    let args = faces_arguments(
        shell.options.index.as_deref(),
        &consent,
        record,
        models_folder(shell).as_deref(),
        shell
            .settings_path
            .parent()
            .ok_or("faces: no config folder")?,
        &skipped,
    );
    let mut launch = shell.launch(args, "faces.log");
    launch.background = op == "scan";
    Ok(launch)
}

/// GET /faces/status, shell shape: consented + record + the six pins' presence + the engine's counts
/// (zeroes before consent — the app answers them itself, no daemon behind the wall).
pub fn status(shell: &Shell) -> Result<Value, String> {
    let config = shell
        .settings_path
        .parent()
        .ok_or("faces: no config folder")?;
    let consent = consent_path(config);
    let record = read_consent(&consent);
    let models: Vec<Value> = FACE_MODELS
        .iter()
        .map(|name| {
            json!({"name": name,
                   "present": models_folder(shell).is_some_and(|dir| dir.join(name).is_file())})
        })
        .collect();
    let counts = match &record {
        // The counts verb re-reads the record itself (consent_guard): zero daemon work behind an
        // absent one, and one spawn when it's there.
        Some(_) => verb(shell, "counts", Map::new())?,
        None => json!({"faces": 0, "assigned": 0, "persons": 0, "named": 0, "pending": 0}),
    };
    Ok(json!({
        "ok": true,
        "consented": record.is_some(),
        "record": record.unwrap_or(Value::Null),
        "models": models,
        "counts": counts,
    }))
}

/// The affirmative act (POST consent) and the opt-out (DELETE): the record names the tier the user
/// accepted; the model pins ride the record, so the daemon serves exactly what the acceptance names.
pub fn set_consent(shell: &Shell, accept: bool, emb_model: Option<&str>) -> Result<Value, String> {
    let config = shell
        .settings_path
        .parent()
        .ok_or("faces: no config folder")?;
    let consent = consent_path(config);
    if !accept {
        // Opting out removes the record; the face store stays (mass deletion is the user's act).
        let removed = remove_consent(&consent)?;
        return Ok(json!({"ok": true, "consented": false, "removed": removed}));
    }
    let emb = emb_model.unwrap_or(DEFAULT_EMBEDDER);
    if !valid_embedder(emb) {
        return Err(
            "faces: the embedder must be one of the pinned face packs (see the People tab)"
                .to_string(),
        );
    }
    let record = json!({
        "consented": true,
        "accepted_at": now_epoch(),
        "det_model": DETECTOR,
        "emb_model": emb,
    });
    write_consent(&consent, &record)?;
    Ok(json!({"ok": true, "consented": true, "record": record}))
}

/// POST scan, shell shape: the consent wall, the writer lock (busy is "a faces scan is already
/// running"), the roots — this app's own folders, never client-supplied — then the engine's own
/// counts relayed. Events stream as the scan works (crawl/face-scan/face-skip/cluster): each goes to
/// the pages as the `faces` event, the panel shows the engine's numbers, never its own.
pub fn scan(app: &tauri::AppHandle, shell: &Shell, limit: Option<i64>) -> Result<Value, String> {
    let (_consent, record) = consent_guard(shell)?;
    let roots = shell.folders();
    if roots.is_empty() {
        return Err("faces: no folders are chosen — add folders to index first".to_string());
    }
    let index = shell
        .options
        .index_folder()
        .ok_or("faces: no index folder for the faces writer lock")?;
    let _writer = FacesWriterGuard::acquire(&index)?;
    let mut fields = Map::new();
    fields.insert(
        "roots".to_string(),
        Value::Array(roots.into_iter().map(Value::String).collect()),
    );
    if let Some(n) = limit {
        if n <= 0 {
            return Err("faces: limit must be greater than 0".to_string());
        }
        fields.insert("limit".to_string(), json!(n));
    }
    let launch = faces_launch(shell, &record, "scan")?;
    let timeout = verb_timeout("scan");
    let reply = run_faces(launch, &record, "scan", fields, timeout, &mut |event| {
        let _ = app.emit("faces", json!({"from": "faces-service", "event": event}));
        scope_faces(app, &event);
    })?;
    Ok(json!({"ok": true, "scan": engine_answer(reply)}))
}

#[cfg(test)]
mod faces_port_tests {
    use super::*;

    fn record() -> ConsentRecord {
        ConsentRecord {
            det_model: DETECTOR.to_string(),
            emb_model: DEFAULT_EMBEDDER.to_string(),
        }
    }

    /// The daemon's argv, pinned: the consent file and the record's tier are the gate's whole
    /// contract; the packs' folder rides only when one is named; the Folders-tab skips ride
    /// `--exclude`, one pair per skipped folder (2026-11-22 — the face scan must not open them).
    #[test]
    fn faces_arguments_carry_the_consent_and_the_tier() {
        let args = faces_arguments(
            Some(Path::new("/home/me/digup-index")),
            Path::new("/home/me/.config/DigUp/faces-consent.json"),
            &record(),
            Some(Path::new("/home/me/digup-models")),
            Path::new("/home/me/.config/DigUp"),
            &["/home/me/photos/appdata_secret".to_string()],
        );
        let joined = args.join("|");
        assert_eq!(args.first().unwrap(), "faces-service");
        assert!(joined.contains("--index|/home/me/digup-index"), "{joined}");
        assert!(
            joined.contains("--faces-dir|/home/me/.config/DigUp/faces"),
            "the #195 privacy move: faces ride the device config folder: {joined}"
        );
        assert!(
            joined.contains("--faces-consent|/home/me/.config/DigUp/faces-consent.json"),
            "{joined}"
        );
        assert!(
            joined.contains("--det-model|scrfd_10g_bnkps.onnx"),
            "{joined}"
        );
        assert!(joined.contains("--emb-model|w600k_mbf.onnx"), "{joined}");
        assert!(
            joined.contains("--exclude|/home/me/photos/appdata_secret"),
            "{joined}"
        );
        assert_eq!(
            &args[args.len() - 2..],
            ["--faces-models", "/home/me/digup-models"]
        );
    }

    #[test]
    fn faces_arguments_without_an_index_or_models_folder() {
        let args = faces_arguments(
            None,
            Path::new("/cfg/faces-consent.json"),
            &record(),
            None,
            Path::new("/cfg"),
            &[],
        );
        assert_eq!(args.first().unwrap(), "faces-service");
        assert!(!args
            .iter()
            .any(|a| a == "--index" || a == "--faces-models" || a == "--exclude"));
        assert!(args.iter().any(|a| a == "--faces-consent"));
        assert!(args.iter().any(|a| a == "--faces-dir"));
    }

    /// A record that parses but is not the server's shape reads as absent — a malformed record is
    /// no consent (the wall must not open on a file full of `{}`).
    #[test]
    fn a_malformed_record_is_no_consent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("faces-consent.json");
        std::fs::write(&path, "{\"consented\": false}").unwrap();
        assert!(read_consent(&path).is_none());
        std::fs::write(&path, "not json").unwrap();
        assert!(read_consent(&path).is_none());
        std::fs::write(
            &path,
            "{\"consented\": true, \"emb_model\": \"w600k_mbf.onnx\"}",
        )
        .unwrap();
        assert!(read_consent(&path).is_some());
    }

    /// The tier law: only the pinned embedders, never the detector, never a name the engine
    /// has never heard of (the server refuses the same shapes with 422).
    #[test]
    fn only_the_pinned_embedders_are_accepted() {
        assert!(valid_embedder("w600k_mbf.onnx"));
        assert!(valid_embedder("adaface_ir_101.onnx"));
        assert!(!valid_embedder(DETECTOR));
        assert!(!valid_embedder("embeddinggemma-2-Q8_0.gguf"));
        assert!(!valid_embedder(""));
    }

    /// An empty embedder never reaches the daemon: the consent record must name the tier
    /// (the server refuses at faces.rs:218-222 the same way).
    #[test]
    fn a_record_without_an_embedder_refuses_the_daemon() {
        let record = ConsentRecord {
            det_model: DETECTOR.to_string(),
            emb_model: String::new(),
        };
        let launch = Launch {
            program: PathBuf::from("/nonexistent/digup"),
            args: Vec::new(),
            env: Vec::new(),
            log: None,
            background: false,
        };
        let error = run_faces(
            launch,
            &record,
            "counts",
            Map::new(),
            verb_timeout("counts"),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(error, "faces: the consent record names no embedder");
    }

    /// The verb clocks are the server's pins (crates/server/src/config.rs:429, faces.rs endpoints).
    #[test]
    fn the_verb_clocks_match_the_servers_pins() {
        assert_eq!(
            verb_timeout("scan"),
            Duration::from_secs(30 * 24 * 3600),
            "the shell's scan is unbounded — one press runs the whole folder (2026-11-22)"
        );
        assert_eq!(verb_timeout("counts"), Duration::from_secs(60));
        assert_eq!(verb_timeout("persons"), Duration::from_secs(120));
    }
}
