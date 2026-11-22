//! `EngineConfig` + the server's own settings (`engine/config.py`, `.env.example`):
//! everything a `digup` spawn needs, from the environment. Defaults follow
//! `main.swift`; the server always passes `--index` explicitly, because the CLI's
//! default (`AppPaths`: `~/.local/share/digup`) is the desktop app's folder, not a
//! server volume. `models_dir` publishes as `DIGUP_MODELS` — the *only* place
//! `ModelFiles.swift` looks, pinned to a volume so a stray cache can't feed the
//! engine a different model.

use std::path::PathBuf;

/// `DEFAULT_ENCODER_IDLE` (config.py:18): `main.swift`'s `--encoder-idle` default.
pub const DEFAULT_ENCODER_IDLE: f64 = 300.0;

/// `config.py:22` — one engine + one index + the folders it serves, all absolute.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub index: PathBuf,
    pub roots: Vec<String>,
    pub binary: String,
    pub excluded: Vec<String>,
    pub excluded_extensions: Vec<String>,
    pub models_dir: Option<PathBuf>,
    pub encoder_idle: f64,
}

impl EngineConfig {
    /// `search_argv()`: the argv array the daemon is spawned with — direct spawn,
    /// never a shell wrapper (ParentWatch kills children whose parent dies).
    pub fn search_argv(&self) -> Vec<String> {
        let mut argv = vec![self.binary.clone(), "search-service".to_string()];
        argv.extend(self.roots.iter().cloned());
        argv.push("--index".to_string());
        argv.push(self.index.to_string_lossy().into_owned());
        argv.push("--encoder-idle".to_string());
        argv.push(format_gf64(self.encoder_idle));
        argv.extend(self.exclude_args());
        argv
    }

    /// `index_argv(backfill:, prune:, …)` — the excludes ride along: the SAME list
    /// estimate and search get (stage-12 §11.3: the forecast must describe the tree
    /// the indexer WOULD walk).
    pub fn index_argv(
        &self,
        backfill: bool,
        prune: bool,
        memory_limit_mb: Option<i64>,
        time_limit_s: Option<i64>,
        new_limit: Option<i64>,
        linger_s: Option<f64>,
    ) -> Vec<String> {
        let mut argv = vec![self.binary.clone(), "index-service".to_string()];
        argv.extend(self.roots.iter().cloned());
        argv.push("--index".to_string());
        argv.push(self.index.to_string_lossy().into_owned());
        argv.extend(self.exclude_args());
        if backfill {
            argv.push("--backfill".to_string());
        }
        if prune {
            argv.push("--prune".to_string());
        }
        if let Some(mb) = memory_limit_mb {
            argv.push("--memory-limit".to_string());
            argv.push(mb.to_string());
        }
        if let Some(seconds) = time_limit_s {
            argv.push("--time-limit".to_string());
            argv.push(seconds.to_string());
        }
        if let Some(limit) = new_limit {
            argv.push("--new-limit".to_string());
            argv.push(limit.to_string());
        }
        if let Some(linger) = linger_s {
            argv.push("--linger".to_string());
            argv.push(format_gf64(linger));
        }
        argv
    }

    /// `status_argv()`.
    pub fn status_argv(&self) -> Vec<String> {
        vec![
            self.binary.clone(),
            "status".to_string(),
            "--index".to_string(),
            self.index.to_string_lossy().into_owned(),
        ]
    }

    /// `estimate_argv()`: `estimate --json`, the excludes with it (ParentWatch
    /// helper: the server is the long-lived parent, direct child, no shell).
    pub fn estimate_argv(&self) -> Vec<String> {
        let mut argv = vec![self.binary.clone(), "estimate".to_string()];
        argv.extend(self.roots.iter().cloned());
        argv.push("--index".to_string());
        argv.push(self.index.to_string_lossy().into_owned());
        argv.extend(self.exclude_args());
        argv.push("--json".to_string());
        argv
    }

    fn exclude_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for folder in &self.excluded {
            args.push("--exclude".to_string());
            args.push(folder.clone());
        }
        if !self.excluded_extensions.is_empty() {
            args.push("--exclude-ext".to_string());
            args.push(self.excluded_extensions.join(","));
        }
        args
    }

    /// `env()`: the inherited environment, with the model folder pinned.
    /// The name is the runtime's `digup_runtime::models::MODELS_ENV` ("DIGUP_MODELS") — the
    /// server has no `digup-runtime` dependency by design (it spawns the `digup` binary, it does
    /// not link llama.cpp), so the literal stays here and the const is the twin to keep in step.
    pub fn env(&self) -> Vec<(String, OsString)> {
        std::env::vars_os()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v))
            .chain(
                self.models_dir
                    .as_ref()
                    .map(|dir| ("DIGUP_MODELS".to_string(), dir.as_os_str().to_os_string())),
            )
            .collect()
    }
}

use std::ffi::OsString;

/// Python's `f"{x:g}"` for the values the server passes as seconds: an integer
/// prints without a decimal point, a fraction keeps the shortest form. Also the
/// shape of the timeout sentences (`{timeout:g}` in `protocol.py` / `cli.py`).
pub(crate) fn format_gf64(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// The server's own settings, `.env.example`'s names: bind host/port, the TTL of
/// the `/status` cache, prewarm and backfill defaults, the config folder, the
/// engine binary. Root and index paths are per target (targets.py, stage-14).
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub status_ttl_s: f64,
    pub prewarm: bool,
    pub backfill: bool,
    pub config: PathBuf,
    pub binary: String,
    pub models_dir: Option<PathBuf>,
    pub index: PathBuf,
    pub roots: Vec<String>,
    pub excluded: Vec<String>,
    pub excluded_extensions: Vec<String>,
    pub encoder_idle: f64,
    /// `DIGUP_SERVER_ESTIMATE_TIMEOUT_S` (120): the endpoint's honest ceiling
    /// for one full-tree walk; the engine keeps walking past it. The mirror only:
    /// the estimate handler reads the knob per request through
    /// `config::estimate_timeout()`, which is the knob's home.
    pub estimate_timeout_s: f64,
    /// `DIGUP_SERVER_JOB_TIMEOUT_S` (3600): a job's ceiling — an expired job
    /// records *interrupted* with its partial counters and the next resumes.
    pub job_timeout_s: f64,
    /// `DIGUP_SERVER_JOB_LOG_SIZE` (1 MiB): the jobs log rotates at size.
    pub job_log_size: u64,
    /// `DIGUP_SERVER_MINT_KEYS`: the standalone box mints its API key at boot.
    pub mint_keys: bool,
    /// `DIGUP_SERVER_UI`: where the built frontend lives (Stage 6's mount).
    pub ui: Option<PathBuf>,
    /// Faces (FACES.md Phase 3): the clustering knobs the faces-service daemon
    /// is spawned with — `DIGUP_SERVER_FACE_MERGE` (0.5) and
    /// `DIGUP_SERVER_FACE_COLLISION` (0.10); the fixture row in FACES.md pins
    /// what a threshold does (the 2008↔2013 pair joins at 0.45, splits at 0.5).
    pub faces_merge: f64,
    pub faces_collision: f64,
    /// `DIGUP_SERVER_FACE_MODEL_IDLE_S` (60): the daemon unloads the face
    /// models after this idle window — the encoder's rule.
    pub faces_model_idle: f64,
    /// `DIGUP_SERVER_FACE_SCAN_TIMEOUT_S` (3600): the ceiling of one
    /// synchronous scan call (detect is ~4.4 s/photo CPU — a big tree is hours).
    pub faces_scan_timeout_s: f64,
}

fn env_string(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(':')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// `int(s)` (measured on CPython 3.12, 2026-11-15): surrounding whitespace is
/// stripped, an optional sign, underscores BETWEEN digits (leading/trailing and
/// doubled runs are refused: `int("1__1")` raises), digits only.
pub(crate) fn py_int(text: &str) -> Option<i64> {
    let t = text.trim();
    let (sign, digits) = match t.strip_prefix('+') {
        Some(rest) => (1i64, rest),
        None => match t.strip_prefix('-') {
            Some(rest) => (-1i64, rest),
            None => (1i64, t),
        },
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_') {
        return None;
    }
    if digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let value: i64 = digits.replace('_', "").parse().ok()?;
    value.checked_mul(sign)
}

/// `float(s)` (measured, same probe): whitespace and sign as `int`, underscores
/// in the digit runs (doubled runs refused), `.5`/`5.` forms, an optional
/// `e` exponent (with sign and underscores), and the words `inf`/`infinity`/
/// `nan` (case-insensitive). Overflow prints `inf` (measured `float("1e999")`).
pub fn py_float(text: &str) -> Option<f64> {
    let t = text.trim();
    let (sign, rest) = match (t.strip_prefix('+'), t.strip_prefix('-')) {
        (Some(r), _) => (1.0f64, r),
        (None, Some(r)) => (-1.0f64, r),
        (None, None) => (1.0f64, t),
    };
    let lowered = rest.to_lowercase();
    if matches!(lowered.as_str(), "inf" | "infinity" | "nan") {
        return Some(sign * lowered.parse::<f64>().ok()?);
    }
    if !rest.bytes().all(|b| {
        b.is_ascii_digit()
            || b == b'_'
            || b == b'.'
            || b == b'e'
            || b == b'E'
            || b == b'+'
            || b == b'-'
    }) {
        return None;
    }
    // Underscore placement (measured `float("1_000.5") = 1000.5`, `float("1__1")`
    // raises): underscores sit BETWEEN the digits of each run. Check each digit
    // group (mantissa digits and exponent digits) for leading/trailing/doubled runs.
    let core = rest.replace("e", "E");
    let (mantissa, exponent) = match core.split_once('E') {
        Some((m, e)) => (m, Some(e)),
        None => (core.as_str(), None),
    };
    let run_ok = |run: &str| {
        !run.starts_with('_')
            && !run.ends_with('_')
            && !run.contains("__")
            && run.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    // An empty digit part is allowed on ONE side of the dot only ("5." / ".5").
    if int_part.is_empty() && frac_part.unwrap_or("1").is_empty() {
        return None;
    }
    if !int_part.is_empty() && !run_ok(int_part) {
        return None;
    }
    if let Some(f) = &frac_part {
        if !f.is_empty() && !run_ok(f) {
            return None;
        }
    }
    if let Some(e) = &exponent {
        let e = e
            .strip_prefix('+')
            .unwrap_or(e)
            .strip_prefix('-')
            .unwrap_or(e);
        if e.is_empty() || !run_ok(e) {
            return None;
        }
    }
    let cleaned = format!("{}{}", sign_str(sign), rest.replace('_', ""));
    fn sign_str(sign: f64) -> &'static str {
        if sign < 0.0 {
            "-"
        } else {
            ""
        }
    }
    match cleaned.parse::<f64>() {
        Ok(value) => Some(value),
        // Python prints overflow as inf (measured float("1e999") == inf).
        Err(_) => Some(sign * f64::INFINITY),
    }
}

/// A knob the operator owns must parse: Python raises at startup (measured
// `DIGUP_SERVER_JOB_TIMEOUT_S=abc`: `ValueError: could not convert string to
/// float` + `Application startup failed. Exiting.`). The refusal relays the
/// knob and its value; a default is a DEFAULT FOR AN UNSET knob, never a
/// rescue of a typo'd one.
pub(crate) fn knob_float(name: &str, default: f64) -> Result<f64, String> {
    match std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => py_float(&value)
            .ok_or_else(|| format!("{name}: '{}' is not a number", value))
            .map(|seconds| {
                if seconds.is_finite() {
                    seconds
                } else {
                    f64::MAX
                }
            }),
        None => Ok(default),
    }
}

pub(crate) fn knob_int(name: &str, default: i64) -> Result<i64, String> {
    match std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => {
            py_int(&value).ok_or_else(|| format!("{name}: '{}' is not an integer", value))
        }
        None => Ok(default),
    }
}

/// The ONE home of `DIGUP_SERVER_ESTIMATE_TIMEOUT_S` (the field `ServerConfig::estimate_timeout_s`
/// mirrors it, the estimate handler reads it through this): `api.py:642` reads the knob **per
/// request**, so the read belongs on the request path and not on the boot path — a knob the
/// operator typo'd fails that request (Starlette's plain 500), it never rescues itself with the
/// default and never kills the boot. `ServerConfig::from_env` keeps the mirror inert.
pub(crate) fn estimate_timeout() -> Result<f64, String> {
    knob_float("DIGUP_SERVER_ESTIMATE_TIMEOUT_S", 120.0)
}

impl ServerConfig {
    /// `.env.example` defaults: 127.0.0.1:8787, TTL 15 s, prewarm and backfill on.
    /// Startup dies on the facts the reference refuses on (`config_from_env`):
    /// `DIGUP_SERVER_INDEX` is REQUIRED (Python `os.environ[...]` — a KeyError
    /// kills the boot), roots must not be empty ("at least one media root is
    /// required" — the AGENTS.md "cost an hour" mis-deploy, silent zero hits
    /// with normal health, loud there and silent here is exactly the shape the
    /// reference refuses), and every numeric knob must parse (Python raises).
    pub fn from_env() -> Result<Self, String> {
        // Required like Python's subscript: an EMPTY value boots on both sides
        // (measured 2026-11-15: `DIGUP_SERVER_INDEX=""` boots the reference);
        // an UNSET one is the refusal.
        let index = PathBuf::from(
            std::env::var("DIGUP_SERVER_INDEX")
                .map_err(|_| "DIGUP_SERVER_INDEX: the index folder is required".to_string())?,
        );
        let roots = env_list("DIGUP_SERVER_ROOTS");
        if roots.is_empty() {
            return Err("DIGUP_SERVER_ROOTS: at least one media root is required".to_string());
        }
        // `config_from_env`'s config folder: env wins, else beside the index
        // (jobs.jsonl, tokens.json, targets.json — recreation-safe facts).
        let config_dir = match std::env::var("DIGUP_SERVER_CONFIG")
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            Some(value) => PathBuf::from(value),
            None => index.join("server-config"),
        };
        let models = std::env::var("DIGUP_SERVER_MODELS")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let binary = env_string("DIGUP_SERVER_BINARY", "digup");
        let idle = knob_float("DIGUP_SERVER_ENCODER_IDLE", DEFAULT_ENCODER_IDLE)?;
        Ok(ServerConfig {
            host: env_string("DIGUP_SERVER_HOST", "127.0.0.1"),
            port: match knob_int("DIGUP_SERVER_PORT", 8787)? {
                // Python int() accepts the wide value and uvicorn fails to bind;
                // the refusal lands at boot here - loud, exit 1, same shape.
                port if (0..=65535).contains(&port) => port as u16,
                _port => {
                    return Err(format!(
                        "DIGUP_SERVER_PORT: '{}' is not a valid port",
                        std::env::var("DIGUP_SERVER_PORT").unwrap_or_default()
                    ))
                }
            },
            status_ttl_s: knob_float("DIGUP_SERVER_STATUS_TTL_S", 15.0)?,
            prewarm: env_flag("DIGUP_SERVER_PREWARM", true),
            backfill: env_flag("DIGUP_SERVER_BACKFILL", true),
            config: config_dir,
            // NOT a startup knob: the reference reads it per request (api.py:642), so a
            // typo must not kill the boot - the estimate call that reads it fails.
            // The value lives only as the mirror; the estimate handler parses the
            // env per request like Python does, through `config::estimate_timeout()`.
            estimate_timeout_s: std::env::var("DIGUP_SERVER_ESTIMATE_TIMEOUT_S")
                .ok()
                .and_then(|value| py_float(&value))
                .unwrap_or(120.0),
            job_timeout_s: knob_float("DIGUP_SERVER_JOB_TIMEOUT_S", 3600.0)?,
            job_log_size: knob_int("DIGUP_SERVER_JOB_LOG_SIZE", 1_048_576)?.max(0) as u64,
            mint_keys: matches!(
                std::env::var("DIGUP_SERVER_MINT_KEYS")
                    .unwrap_or_default()
                    .to_lowercase()
                    .as_str(),
                "1" | "true" | "yes"
            ),
            ui: std::env::var("DIGUP_SERVER_UI")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            faces_merge: knob_float("DIGUP_SERVER_FACE_MERGE", 0.5)?,
            faces_collision: knob_float("DIGUP_SERVER_FACE_COLLISION", 0.10)?,
            faces_model_idle: knob_float("DIGUP_SERVER_FACE_MODEL_IDLE_S", 60.0)?,
            faces_scan_timeout_s: knob_float("DIGUP_SERVER_FACE_SCAN_TIMEOUT_S", 3600.0)?,
            encoder_idle: idle,
            binary,
            models_dir: models,
            index,
            roots,
            excluded: env_list("DIGUP_SERVER_EXCLUDE"),
            excluded_extensions: std::env::var("DIGUP_SERVER_EXCLUDE_EXT")
                .unwrap_or_default()
                .split(',')
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect(),
        })
    }

    /// The main target's engine config (targets.py's MAIN_TARGET).
    pub fn engine(&self) -> EngineConfig {
        EngineConfig {
            index: self.index.clone(),
            roots: self.roots.clone(),
            binary: self.binary.clone(),
            excluded: self.excluded.clone(),
            excluded_extensions: self.excluded_extensions.clone(),
            models_dir: self.models_dir.clone(),
            encoder_idle: self.encoder_idle,
        }
    }
}

/// `DIGUP_SERVER_PREWARM=0` turns the boot prewarm off; anything else (unset) is on.
fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name).ok() {
        Some(value) => value != "0",
        None => default,
    }
}

#[cfg(test)]
mod tests {
    use super::{knob_float, knob_int, py_float, py_int, ServerConfig};

    #[test]
    fn the_number_grammar_is_pythons_own_measured_shape() {
        // Every row measured with CPython 3.12 int()/float() on this box
        // (2026-11-15). The knobs are Python-parsed: whitespace is stripped,
        // signs accepted, underscores BETWEEN digits (doubled runs raise),
        // float takes .5 / 5. / exponents / inf / nan, and overflow prints inf.
        assert_eq!(py_int("12"), Some(12));
        assert_eq!(py_int(" 12 "), Some(12));
        assert_eq!(py_int("+5"), Some(5));
        assert_eq!(py_int("-5"), Some(-5));
        assert_eq!(py_int("1_1"), Some(11));
        assert_eq!(py_int("_1"), None);
        assert_eq!(py_int("1__1"), None); // doubled run: ValueError (measured)
        assert_eq!(py_int("12a"), None);
        assert_eq!(py_int(""), None);
        assert_eq!(py_int("1.5"), None);
        assert_eq!(py_int("1e3"), None);
        assert_eq!(py_float("1.5"), Some(1.5));
        assert_eq!(py_float(" 1.5 "), Some(1.5));
        assert_eq!(py_float("1_1"), Some(11.0));
        assert_eq!(py_float("1__1"), None);
        assert_eq!(py_float(".5"), Some(0.5));
        assert_eq!(py_float("5."), Some(5.0));
        assert_eq!(py_float("1e3"), Some(1000.0));
        assert_eq!(py_float("+1_0.5e1"), Some(105.0));
        assert_eq!(py_float("1e999"), Some(f64::INFINITY)); // overflow prints inf (measured)
        assert_eq!(py_float("12a"), None);
        assert_eq!(py_float("_1"), None);
        assert_eq!(py_float("1e"), None);
        // inf/nan parse in Python (measured) and hand a non-finite number to a
        // timer: the timer behaviour at the engine was NOT measured, so the
        // knobs relay the parse and clamp at the Duration seam (knob_float
        // maps non-finite to f64::MAX - the comparison "elapsed > inf/nan"
        // is never true, matching JobRunner's comparison against a Python nan).
        assert_eq!(py_float("inf"), Some(f64::INFINITY));
        assert!(py_float("nan").is_some_and(|v| v.is_nan()));
    }

    #[test]
    fn the_env_facts_the_reference_refuses_are_refused_here() {
        // The reference fail-fasts measured on 2026-11-15, in ONE test:
        // env vars are process-global and the harness runs tests in parallel, so
        // every scenario rides the same serialized walk.

        // Measured reference shape (2026-11-15): DIGUP_SERVER_JOB_TIMEOUT_S=abc
        // -> ValueError + "Application startup failed. Exiting." The port relays
        // the knob and its value; a default is for an UNSET knob only.
        std::env::set_var("DIGUP_SERVER_JOB_TIMEOUT_S", "abc");
        assert_eq!(
            knob_float("DIGUP_SERVER_JOB_TIMEOUT_S", 3600.0).unwrap_err(),
            "DIGUP_SERVER_JOB_TIMEOUT_S: 'abc' is not a number"
        );
        std::env::set_var("DIGUP_SERVER_JOB_LOG_SIZE", "1_000");
        assert_eq!(knob_int("DIGUP_SERVER_JOB_LOG_SIZE", 1).unwrap(), 1000);
        std::env::set_var("DIGUP_SERVER_JOB_LOG_SIZE", " 12 ");
        assert_eq!(knob_int("DIGUP_SERVER_JOB_LOG_SIZE", 1).unwrap(), 12);
        std::env::remove_var("DIGUP_SERVER_JOB_TIMEOUT_S");
        std::env::remove_var("DIGUP_SERVER_JOB_LOG_SIZE");
        assert_eq!(
            knob_float("DIGUP_SERVER_JOB_TIMEOUT_S", 3600.0).unwrap(),
            3600.0
        );

        // The three fail-fasts measured on the reference (2026-11-15):
        std::env::remove_var("DIGUP_SERVER_INDEX");
        std::env::set_var("DIGUP_SERVER_ROOTS", "/tmp/a");
        assert_eq!(
            ServerConfig::from_env().unwrap_err(),
            "DIGUP_SERVER_INDEX: the index folder is required"
        );
        std::env::set_var("DIGUP_SERVER_INDEX", "/tmp/f08-idx");
        std::env::remove_var("DIGUP_SERVER_ROOTS");
        assert_eq!(
            ServerConfig::from_env().unwrap_err(),
            "DIGUP_SERVER_ROOTS: at least one media root is required"
        );
        std::env::set_var("DIGUP_SERVER_ROOTS", "");
        assert_eq!(
            ServerConfig::from_env().unwrap_err(),
            "DIGUP_SERVER_ROOTS: at least one media root is required"
        );
        std::env::set_var("DIGUP_SERVER_ROOTS", "/tmp/a:/tmp/b");
        std::env::set_var("DIGUP_SERVER_STATUS_TTL_S", "1x5");
        assert_eq!(
            ServerConfig::from_env().unwrap_err(),
            "DIGUP_SERVER_STATUS_TTL_S: '1x5' is not a number"
        );
        std::env::remove_var("DIGUP_SERVER_STATUS_TTL_S");
        std::env::set_var("DIGUP_SERVER_PORT", "99999");
        assert_eq!(
            ServerConfig::from_env().unwrap_err(),
            "DIGUP_SERVER_PORT: '99999' is not a valid port"
        );
        std::env::remove_var("DIGUP_SERVER_PORT");
        // The healthy chain boots, and an EMPTY index boots on both sides
        // (measured: DIGUP_SERVER_INDEX="" is a booted server in the reference):
        let cfg = ServerConfig::from_env().unwrap();
        assert_eq!(cfg.roots, vec!["/tmp/a", "/tmp/b"]);
        std::env::set_var("DIGUP_SERVER_INDEX", "");
        assert!(ServerConfig::from_env().is_ok());
        std::env::set_var("DIGUP_SERVER_INDEX", "/tmp/f08-idx");

        // api.py:642 reads DIGUP_SERVER_ESTIMATE_TIMEOUT_S per request; a typo
        // must not kill the boot (the estimate call answers 500 - pinned at the
        // estimate handler test seam, tests/boot.rs shape).
        std::env::set_var("DIGUP_SERVER_ESTIMATE_TIMEOUT_S", "abc");
        std::env::set_var("DIGUP_SERVER_INDEX", "/tmp/f08-idx");
        std::env::set_var("DIGUP_SERVER_ROOTS", "/tmp/a");
        let cfg = ServerConfig::from_env().expect("the estimate knob is read per request");
        assert_eq!(cfg.estimate_timeout_s, 120.0); // the mirror defaults, no boot death
        std::env::remove_var("DIGUP_SERVER_ESTIMATE_TIMEOUT_S");
    }
}
