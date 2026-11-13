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
        argv.extend(self.exclude_args().into_iter());
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
        argv.extend(self.exclude_args().into_iter());
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
        argv.extend(self.exclude_args().into_iter());
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
    /// for one full-tree walk; the engine keeps walking past it.
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

impl ServerConfig {
    /// `.env.example` defaults: 127.0.0.1:8787, TTL 15 s, prewarm and backfill on.
    pub fn from_env() -> Self {
        let index = PathBuf::from(env_string("DIGUP_SERVER_INDEX", "/data/index"));
        let roots = env_list("DIGUP_SERVER_ROOTS");
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
        let idle = std::env::var("DIGUP_SERVER_ENCODER_IDLE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_ENCODER_IDLE);
        ServerConfig {
            host: env_string("DIGUP_SERVER_HOST", "127.0.0.1"),
            port: std::env::var("DIGUP_SERVER_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(8787),
            status_ttl_s: std::env::var("DIGUP_SERVER_STATUS_TTL_S")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(15.0),
            prewarm: env_flag("DIGUP_SERVER_PREWARM", true),
            backfill: env_flag("DIGUP_SERVER_BACKFILL", true),
            config: config_dir,
            estimate_timeout_s: std::env::var("DIGUP_SERVER_ESTIMATE_TIMEOUT_S")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(120.0),
            job_timeout_s: std::env::var("DIGUP_SERVER_JOB_TIMEOUT_S")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(3600.0),
            job_log_size: std::env::var("DIGUP_SERVER_JOB_LOG_SIZE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_048_576),
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
        }
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
