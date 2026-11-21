//! `digup mcp` — the stdio front-end of the MCP peer (`digup-services::mcp`).
//!
//! One JSON-RPC 2.0 message per line on stdin, one per line on stdout — the shape
//! Claude Desktop's command-block config speaks (`command: …/digup, args: ["mcp"]`).
//! The engine window is the daemon this command spawns: `search-service` on the same
//! folders `digup search` would read (roots positional, `--index DIR`, the exclude
//! lists, `--encoder-idle`), so a tool call lands on the hot searcher: index open,
//! encoder warm, answers as the verbs answer them — relayed verbatim into
//! `structuredContent`, with a short skimming text beside it. The daemon's idle-exit
//! is a restart, not an error (the server's rule, service.rs: spawn again, request
//! once).
//!
//! `estimate` runs in-process on `--allow-admin` (the CLI's own `estimate --json`,
//! one JSON line per folder, shaped as the REST estimate answers: `{"folders": […]}`);
//! `index` is server-only — a stdio run shares the user's own file permissions, so
//! there is no gate here to speak of, and no gate-shaped tool to advertise.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde_json::{json, Map, Value};

use digup_services::mcp::{self, CallOutcome, Step, Surface};

use crate::Arguments;

/// The daemon's answer window: `recv_timeout` bounds a wedged child (the first
/// search pays the model load — the server's READY_TIMEOUT measures the same
/// warm at tens of seconds on a card; 120 s covers the slowest measured boot).
const ANSWER_WAIT: Duration = Duration::from_secs(120);

fn spawn_reader(child: &mut Child) -> Receiver<Value> {
    let (sender, receiver) = mpsc::channel();
    let Some(stdout) = child.stdout.take() else {
        return receiver;
    };
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(text) = line else { break };
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if sender.send(value).is_err() {
                    break; // the reader is gone; the next send() learns EOF
                }
            }
        }
    });
    receiver
}

/// The `search-service` child, lazy: `initialize`/`tools/list` never touch it, the
/// first verb call spawns it, and its idle-exit is a restart (the AGENTS-pinned
/// daemon semantics: EOF on stdin mid-request = spawn again, request once).
struct Daemon {
    argv: Vec<String>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    lines: Option<Receiver<Value>>,
    id: i64,
}

impl Daemon {
    fn start(&mut self) -> Result<(), String> {
        let mut child = Command::new(&self.argv[0])
            .args(&self.argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start search-service: {e}"))?;
        let lines = spawn_reader(&mut child);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "search-service has no stdin".to_string())?;
        self.child = Some(child);
        self.stdin = Some(stdin);
        self.lines = Some(lines);
        Ok(())
    }

    /// Graceful stop: quit releases the model, closing stdin ends the loop (the
    /// daemon's EOF rule); the child is reaped, never leaked.
    fn stop(&mut self) {
        if let Some(stdin) = self.stdin.as_mut() {
            self.id += 1;
            let _ = writeln!(stdin, "{}", json!({"op": "quit", "id": self.id}));
            let _ = stdin.flush();
        }
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        self.lines = None;
    }

    fn send(&mut self, mut fields: Map<String, Value>) -> Result<Value, String> {
        self.id += 1;
        let want = json!(self.id);
        fields.insert("id".into(), want.clone());
        let line = serde_json::to_string(&Value::Object(fields)).expect("a built object");
        for attempt in 0..2 {
            if self.child.is_none() {
                self.start()?;
            }
            let write = match self.stdin.as_mut() {
                Some(stdin) => writeln!(stdin, "{line}")
                    .and_then(|()| stdin.flush())
                    .map_err(|e| format!("cannot reach search-service: {e}")),
                None => Err("search-service has no stdin".to_string()),
            };
            if write.is_err() && attempt == 0 {
                // The child died between respawn and write: the next attempt restarts.
                self.child = None;
                self.stdin = None;
                self.lines = None;
                continue;
            }
            write?;
            loop {
                // The borrow of `self.lines` ends with the recv result (owned Value),
                // so the arms below may take `&mut self` (the restart path).
                let received = self
                    .lines
                    .as_ref()
                    .ok_or("search-service has no reader")?
                    .recv_timeout(ANSWER_WAIT);
                match received {
                    Ok(value) => {
                        if value.get("id") == Some(&want) {
                            let mut reply = value;
                            if let Some(row) = reply.as_object_mut() {
                                row.remove("id"); // the transport id, not the engine's words
                            }
                            return Ok(reply);
                        }
                        // Events (ready/encoder/…) ride the same stream: routed away,
                        // exactly like the server's client does.
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        return Err("search-service did not answer in time".to_string())
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        // Idle-exit (or a death): reap, restart, re-request once.
                        self.stop_clean();
                        break;
                    }
                }
            }
        }
        Err("search-service stopped".to_string())
    }

    fn stop_clean(&mut self) {
        self.stdin = None;
        self.lines = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

/// One tool call, executed on the daemon (search/status/show are the daemon's verbs;
/// estimate is the CLI's own `estimate --json` in process). `is_error` is the engine's
/// own refusal (`ok:false`) — the relay rule, not an invention.
fn execute(
    daemon: &mut Daemon,
    estimate_args: &[String],
    name: &str,
    call: &Map<String, Value>,
) -> Result<CallOutcome, (i64, String)> {
    let refused = |reply: &Value| reply.get("ok").and_then(Value::as_bool) == Some(false);
    match name {
        "search" => {
            let Some(query) = call.get("q").and_then(Value::as_str) else {
                return Err((mcp::INVALID_PARAMS, "search needs q".to_string()));
            };
            let mut fields = Map::new();
            fields.insert("op".into(), json!("search"));
            fields.insert("q".into(), json!(query));
            // The tool's cap (schema-declared 1..100, default 10) clamped here —
            // the engine caps at 100 itself; a wild number is clamped, never obeyed.
            let limit = call
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(10)
                .clamp(1, 100);
            fields.insert("limit".into(), json!(limit));
            if let Some(code) = call.get("code").and_then(Value::as_bool) {
                fields.insert("code".into(), json!(code));
            }
            if let Some(kinds) = call.get("kinds").and_then(Value::as_array) {
                fields.insert("kinds".into(), json!(kinds));
            }
            let reply = daemon.send(fields).map_err(|e| (mcp::INTERNAL_ERROR, e))?;
            Ok(CallOutcome {
                is_error: refused(&reply),
                value: reply,
            })
        }
        "status" => {
            let mut fields = Map::new();
            fields.insert("op".into(), json!("status"));
            let reply = daemon.send(fields).map_err(|e| (mcp::INTERNAL_ERROR, e))?;
            Ok(CallOutcome {
                is_error: refused(&reply),
                value: reply,
            })
        }
        "show" => {
            let Some(segment) = call.get("segment_id").and_then(Value::as_i64) else {
                return Err((mcp::INVALID_PARAMS, "show needs segment_id".to_string()));
            };
            let mut fields = Map::new();
            fields.insert("op".into(), json!("text"));
            fields.insert("segment".into(), json!(segment));
            if let Some(code) = call.get("code").and_then(Value::as_bool) {
                fields.insert("code".into(), json!(code));
            }
            let reply = daemon.send(fields).map_err(|e| (mcp::INTERNAL_ERROR, e))?;
            Ok(CallOutcome {
                is_error: refused(&reply),
                value: reply,
            })
        }
        "estimate" => {
            let exe = std::env::current_exe().map_err(|e| (mcp::INTERNAL_ERROR, e.to_string()))?;
            let output = Command::new(exe)
                .arg("estimate")
                .arg("--json")
                .args(estimate_args)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .map_err(|e| (mcp::INTERNAL_ERROR, e.to_string()))?;
            // The contract: `estimate --json` answers one JSON line per folder; the
            // REST surface shapes them as {"folders": […]} (server cli.rs
            // estimate_lines) — the stdio tool mirrors that answer, verbatim rows.
            let mut folders = Vec::new();
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(row) if row.get("folder").is_some() => folders.push(row),
                    Ok(row) if row.get("folders").is_some() => {
                        // A self-reliant future emitter that answers the bundle
                        // whole: relay it whole.
                        return Ok(CallOutcome {
                            is_error: refused(&row),
                            value: row,
                        });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        return Err((
                            mcp::INTERNAL_ERROR,
                            format!("estimate --json answered unparseable: {e}"),
                        ))
                    }
                }
            }
            Ok(CallOutcome {
                is_error: false,
                value: json!({"folders": folders}),
            })
        }
        other => Err((mcp::INVALID_PARAMS, format!("no such tool: {other}"))),
    }
}

pub fn mcp(args: &Arguments, argv: &[String]) -> Result<(), String> {
    let allow_admin = args.flag("--allow-admin");
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    // The daemon reads the folders exactly as this run names them: the argv after
    // the `mcp` token, verbatim (roots positional, --index, --exclude…, --code-root…,
    // --encoder-idle) — no second option vocabulary to drift.
    let mut argv_daemon = vec![exe, "search-service".to_string()];
    argv_daemon.extend(argv.iter().skip(1).cloned());
    let estimate_args: Vec<String> = argv.iter().skip(1).cloned().collect();
    let mut daemon = Daemon {
        argv: argv_daemon,
        child: None,
        stdin: None,
        lines: None,
        id: 0,
    };
    let mut stdin = BufReader::new(std::io::stdin());
    let mut out = BufWriter::new(std::io::stdout());
    let mut line = String::new();
    loop {
        line.clear();
        if stdin.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            break; // EOF: the client is gone (stdin close is the daemon's stop verb)
        }
        let text = line.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&text) {
            Ok(req) => req,
            Err(e) => {
                let envelope = mcp::error(Value::Null, mcp::PARSE_ERROR, &e.to_string());
                writeln!(out, "{envelope}").map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                continue;
            }
        };
        match mcp::step(&req, Surface::Stdio, allow_admin, env!("CARGO_PKG_VERSION")) {
            Step::Reply(value) => {
                if !value.is_null() {
                    writeln!(out, "{value}").map_err(|e| e.to_string())?;
                    out.flush().map_err(|e| e.to_string())?;
                }
            }
            Step::ToolCall { id, name, args } => {
                let answer = match execute(&mut daemon, &estimate_args, &name, &args) {
                    Ok(outcome) => mcp::tool_result(&name, id, outcome),
                    Err((code, message)) => mcp::error(id, code, &message),
                };
                writeln!(out, "{answer}").map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
            }
        }
    }
    daemon.stop();
    Ok(())
}
