//! The read-only one-shot CLI: `status` (`engine/cli.py` — `estimate`/`speed`
//! land with the jobs stage). Short-lived processes, no protocol.
//!
//! `status` is parsed from the CLI's own lines (`main.swift: status(...)`) — the
//! server shows the engine's words, and anything it can't parse stays None, never
//! guessed. A nonzero exit is `CLIError` carrying the engine's own words — the API
//! answers 502 with them.
//!
//! This is a plain `subprocess.run` child (no pipes left open, no shell wrapper):
//! `estimate --json` is a ParentWatch helper and the server is the long-lived
//! parent the engine designed for; `status` reads no stdin.

use std::ffi::OsString;
use std::io::Read;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

use crate::config::format_gf64;
use crate::config::EngineConfig;

/// `CLIError`: the CLI said `error: …` and exited nonzero — the engine's own
/// words, kept. The API maps it to 502.
#[derive(Clone, Debug)]
pub struct CliError(pub String);

impl std::fmt::Display for CliError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{}", self.0)
    }
}

/// One `digup` process to completion; stdout returned, engine errors raised
/// verbatim (`cli.py:29`). The engine's one-shot outputs are small (a status
/// table, a JSON line), so the pipes never need concurrent draining: poll to
/// exit, then read what the kernel buffered.
pub fn run(
    argv: &[String],
    env: &[(String, OsString)],
    timeout: Duration,
) -> Result<String, CliError> {
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Err(CliError(format!("cannot start the engine helper: {error}"))),
    };
    let started = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(error) => return Err(CliError(format!("digup: {error}"))),
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            // Python: TimeoutExpired → CLIError, quoting the same words (`cli.py:33`
            // — "quote the engine instead", api.py:6's rule).
            return Err(CliError(format!(
                "digup {} timed out after {} s",
                argv.get(1).cloned().unwrap_or_else(|| "?".to_string()),
                format_gf64(timeout.as_secs_f64())
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    if code != Some(0) {
        // `(proc.stderr or proc.stdout).strip() or f"digup exited {returncode}"`.
        let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
        let stdout_text = String::from_utf8_lossy(&stdout).trim().to_string();
        let words = if !stderr.is_empty() {
            stderr
        } else if !stdout_text.is_empty() {
            stdout_text
        } else {
            format!(
                "digup exited {}",
                code.map(|c| c.to_string()).unwrap_or("None".into())
            )
        };
        return Err(CliError(words));
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// `cli.py`'s `line = lambda pat, text: re.search(pat, text, re.M)` — the first
/// match of a `(?m)` pattern. The lifetime names text explicitly: `Captures`
/// borrows it.
fn first_capture<'t>(pattern: &str, text: &'t str) -> Option<regex::Captures<'t>> {
    Regex::new(pattern)
        .expect("cli.py's patterns are valid")
        .captures(text)
}

/// `estimate_lines(config)`: `estimate --json` — every JSON line, one per root
/// folder, as the engine streams them. ParentWatch rule: this helper exits the
/// moment its parent dies and treats PID 1 as a dead app; the server is the
/// long-lived parent (direct children, no shell wrappers). No JSON line is a
/// CLIError quoting the head of the output — `api.py:6`'s rule (quote the
/// engine, never invent).
pub fn estimate_lines(config: &EngineConfig, timeout: Duration) -> Result<Vec<Value>, CliError> {
    let out = run(&config.estimate_argv(), &config.env(), timeout)?;
    let lines: Vec<Value> = out
        .lines()
        .filter(|line| line.trim().starts_with('{'))
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect();
    if lines.is_empty() {
        let head: String = out.chars().take(200).collect();
        return Err(CliError(format!(
            "estimate --json printed no JSON line: {head}"
        )));
    }
    Ok(lines)
}

/// `status(config)`: `digup status` parsed — index path, vectors, per-kind
/// counts, fingerprint, build tag, size. The patterns are `cli.py`'s, verbatim,
/// including the ones the engine's thousands separators defeat at >999 rows
/// (the server reports None there, never a guess).
pub fn status(config: &EngineConfig, timeout: Duration) -> Result<Value, CliError> {
    let out = run(&config.status_argv(), &config.env(), timeout)?;
    let search = first_capture;
    let mut info = json!({
        "index": null, "vectors": null, "segments": null, "fingerprints": null,
        "build_tag": null, "vectors_version": null, "index_size": null, "kinds": []
    });
    if let Some(c) = search(r"(?m)^index: (.+)$", &out) {
        info["index"] = json!(c[1]);
    }
    if let Some(c) = search(r"(?m)^vectors: (\d+) in (\d+) segments", &out) {
        info["vectors"] = json!(c[1].parse::<i64>().expect("digits"));
        info["segments"] = json!(c[2].parse::<i64>().expect("digits"));
    }
    if let Some(c) = search(r"(?m)^built with: (.+)$", &out) {
        info["fingerprints"] = json!(c[1]);
    }
    if let Some(c) = search(r"(?m)^this digup: llama\.cpp ([^,]+), vectors (\d+)", &out) {
        info["build_tag"] = json!(c[1]);
        info["vectors_version"] = json!(c[2].parse::<i64>().expect("digits"));
    }
    if let Some(c) = search(r"(?m)^size: (.+)$", &out) {
        info["index_size"] = json!(c[1]);
    }
    let kinds_re = Regex::new(r"(?m)^  (\S+)\s+(\d+)\s+(\d+ done(?: \(\d+ partial\))?)$")
        .expect("cli.py's pattern");
    let kinds = info["kinds"].as_array_mut().expect("kinds is a list");
    for c in kinds_re.captures_iter(&out) {
        kinds.push(json!({
            "kind": c[1].to_string(),
            "files": c[2].parse::<i64>().expect("digits"),
            "states": c[3].to_string()
        }));
    }
    Ok(info)
}
