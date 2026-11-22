//! The faces surface (FACES.md Phase 3): the People-tab endpoints, riding the
//! same thin-client law as every other server surface — the engine answers the
//! face verbs (`digup faces-service`, JSON-lines), the server relays them, and
//! invents nothing.
//!
//! The consent gate (FACES.md Phase 1 §5) is this deployment's durable record:
//! `<config>/faces-consent.json` for the main media index, `faces-consent-<name>.json`
//! for a named target, 0600 like `tokens.json` (it carries a legal acceptance,
//! not a secret, but it is the user's private fact). The record is written only
//! by the affirmative UI act (POST consent, admin); EVERY faces endpoint —
//! scan and reads alike — refuses with 400 before it spawns a daemon when the
//! record is absent. The daemon enforces the same record engine-side
//! (`--faces-consent`, FACES.md: "what the UI gates, the engine gates").
//!
//! The scan is synchronous on the blocking pool with its own writer lock
//! (`<index>/.digup-server.faces.writer.lock`) — busy is 409, the same law as
//! the index writer. Roots are the target's own roots from the deployment
//! config, never client-supplied (§7). Face rows carry the engine's box/kps;
//! the frontend crops them out of the existing `/preview` handle — the server
//! mints the handle for each face's photo (same HitCache as search hits).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::config::{format_gf64, EngineConfig, ServerConfig};
use crate::protocol::JsonLinesProcess;
use crate::targets::MAIN_TARGET;

/// The faces writer lock, beside the index — the index writer's law, its own
/// inode (FACES.md Phase 3: a scan is a writer of face.sqlite, never of
/// index.sqlite; the two jobs coexist, the locks are per file).
pub const FACES_LOCK_NAME: &str = ".digup-server.faces.writer.lock";

/// The record's own file name for a target (main: bare, named: suffixed).
pub fn consent_path(config_dir: &Path, target: &str) -> PathBuf {
    if target == MAIN_TARGET {
        config_dir.join("faces-consent.json")
    } else {
        config_dir.join(format!("faces-consent-{target}.json"))
    }
}

/// The consent record, read: None = not consented (absent file). A record that
/// parses but is not the server's shape (no `consented: true`) reads as absent —
/// a malformed record is no consent, never a partial one.
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
    /// The record's model stamps; the detector pin defaults to the parity tier
    /// when the record predates the field (the daemon's own default matches).
    pub fn of(record: &Value) -> ConsentRecord {
        ConsentRecord {
            det_model: record
                .get("det_model")
                .and_then(Value::as_str)
                .unwrap_or("scrfd_10g_bnkps.onnx")
                .to_string(),
            emb_model: record
                .get("emb_model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }
}

/// Persist the record atomically, 0600 — `security.rs`'s tokens-file shape
/// (write `.new`, rename, chmod): recreation never half-writes a consent.
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

/// Opting out deletes the record: the endpoints start refusing at 400, the
/// face.sqlite stays on disk (the user's data; deleting it is the user's act —
/// `prune`/`delete` verbs answer it, a consent withdrawal never mass-deletes).
pub fn remove_consent(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The faces writer lock: flock(LOCK_EX|LOCK_NB) beside the index — busy is
/// the engine writer's sentence shape (jobs.rs), 409. The guard releases on
/// Drop, and the handle close releases too (the same order as jobs.rs).
pub struct FacesWriterGuard {
    file: File,
}

impl FacesWriterGuard {
    pub fn acquire(index: &Path) -> Result<FacesWriterGuard, FacesBusy> {
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
            Err(error) => return Err(FacesBusy(format!("{}: {error}", lock_path.display()))),
        };
        if !try_lock(&file) {
            return Err(FacesBusy(format!(
                "{}: a faces scan is already running",
                index.display()
            )));
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
fn try_lock(file: &File) -> bool {
    use std::os::unix::io::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(windows)]
fn try_lock(file: &File) -> bool {
    file.try_lock().is_ok()
}

#[derive(Clone, Debug)]
pub struct FacesBusy(pub String);

impl std::fmt::Display for FacesBusy {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{}", self.0)
    }
}

/// Spawn the daemon for ONE call (the store open is its only boot work — the
/// models load on the verbs that ask, and unload after the idle window).
/// `ready` handshake, one request, `quit`, join: the same supervision law as
/// SearchService, per-call because the face verbs are a click, not a keystroke.
pub fn faces_call(
    scope: &EngineConfig,
    server: &ServerConfig,
    consent: &Path,
    record: &ConsentRecord,
    op: &str,
    mut fields: Map<String, Value>,
    timeout: Duration,
) -> Result<Value, FacesCall> {
    let mut argv = vec![
        server.binary.clone(),
        "faces-service".to_string(),
        "--index".to_string(),
        scope.index.to_string_lossy().into_owned(),
        "--faces-consent".to_string(),
        consent.to_string_lossy().into_owned(),
        "--det-model".to_string(),
        record.det_model.clone(),
        "--emb-model".to_string(),
        record.emb_model.clone(),
        "--merge-threshold".to_string(),
        format_gf64(server.faces_merge),
        "--collision-margin".to_string(),
        format_gf64(server.faces_collision),
        "--model-idle".to_string(),
        format_gf64(server.faces_model_idle),
    ];
    if let Some(models) = &server.models_dir {
        argv.push("--faces-models".to_string());
        argv.push(models.to_string_lossy().into_owned());
    }
    if record.emb_model.is_empty() {
        return Err(FacesCall::Refused(
            "faces: the consent record names no embedder".to_string(),
        ));
    }
    let proc = JsonLinesProcess::new(argv, "faces-service", scope.env());
    // A payload built without the `faces` feature answers faces-service with the
    // usage text and exit 2 — that lands as the daemon's own EngineExited (503),
    // relayed, not dressed up.
    proc.start(Some("ready"), Duration::from_secs(60))
        .map_err(FacesCall::from)?;
    fields.insert("op".into(), Value::String(op.to_string()));
    let reply = proc.request(&fields, timeout);
    let _ = proc.request(
        &{
            let mut quit = Map::new();
            quit.insert("op".into(), Value::String("quit".to_string()));
            quit
        },
        Duration::from_secs(10),
    );
    proc.close(Duration::from_secs(5));
    let reply = reply.map_err(FacesCall::from)?;
    if reply.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(FacesCall::Refused(match reply.get("error") {
            Some(Value::String(text)) => format!("faces: {text}"),
            Some(value) => format!("faces: {value}"),
            None => "faces: the daemon refused with no reason".to_string(),
        }));
    }
    Ok(reply)
}

/// The call's failures, mapped like the server maps every engine fact:
/// refused/exited → 503 (the engine's words), timeout → 504, CLI → 502.
pub enum FacesCall {
    Refused(String),
    Engine(String),
    Timeout(String),
    Cli(String),
}

impl From<crate::protocol::ProtocolError> for FacesCall {
    fn from(error: crate::protocol::ProtocolError) -> Self {
        use crate::protocol::ProtocolError;
        match error {
            ProtocolError::Exited(exited) => FacesCall::Engine(exited.to_string()),
            ProtocolError::Timeout(what) => FacesCall::Timeout(what),
            ProtocolError::Cli(what) => FacesCall::Cli(what),
            ProtocolError::Internal(what) => FacesCall::Engine(what),
        }
    }
}

/// Which of the six user-supplied pins are present in the models folder —
/// the People tab's "the tier is installed" fact, read off the pin names
/// (the pins live in `crates/runtime/src/models.rs`; only the engine checks
/// hashes, here it is presence, the same fact `ModelFiles.missing` reports).
pub fn model_presence(server: &ServerConfig) -> Vec<(&'static str, bool)> {
    use digup_runtime::models::FaceModelFiles;
    let folder = match &server.models_dir {
        Some(dir) => dir.as_path(),
        None => Path::new(""),
    };
    FaceModelFiles::ALL
        .iter()
        .map(|file| (file.name, folder.join(file.name).is_file()))
        .collect()
}
