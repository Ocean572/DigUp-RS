//! The CLI's own verbs, measured against the binary this package builds.
//!
//! Regression (found by `scripts/rust/twin-gate.sh`, measured 2026-11-13): `--code` is the
//! CODE store's run — `Arguments.store()` (main.swift:101-103) picks `code.sqlite` when the
//! flag is there. The port opened `index.sqlite` for `index --code`, so a code run into a
//! folder that already holds a media index refused its own index on the MEDIA fingerprint
//! ("index: … photo140 shot280 … now: … code1 lines1800", exit 1) where the reference answers
//! exit 0 with its rows in `code.sqlite`. The pinned suites pass each store explicitly through
//! the library, so only the verb path reaches this.
//!
//! Needs the pinned model files: prints the reason and returns, the way `reference_vectors.rs`
//! does. `scripts/rust/audit.sh` requires the suite's toolchain env, so the verb runs there.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

use digup_runtime::LlamaModels;

fn run(store: &Path, args: &[&str]) -> (i32, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_digup"))
        .args(args)
        .arg("--index")
        .arg(store)
        .output()
        .expect("the digup binary runs (CARGO_BIN_EXE_digup)");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// One `meta` row, read straight out of the SQLite file the verb wrote.
fn meta(store: &Path, file: &str, key: &str) -> Option<String> {
    let db = rusqlite::Connection::open_with_flags(
        store.join(file),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the store opens read-only");
    db.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .ok()
}

fn row_count(store: &Path, file: &str) -> i64 {
    let db = rusqlite::Connection::open_with_flags(
        store.join(file),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("the store opens read-only");
    db.query_row("SELECT count(*) FROM files", [], |row| row.get::<_, i64>(0))
        .expect("files counts")
}

#[test]
fn code_indexing_is_the_code_stores_own_run() {
    if LlamaModels::locate()
        .map(|models| models.media.is_none())
        .unwrap_or(true)
    {
        eprintln!("skipped: the pinned model files are not installed (docs/implementation/gpu-handoff.md).");
        return;
    }
    let work = tempfile::tempdir().expect("a work folder");
    let corpus = work.path().join("corpus");
    std::fs::create_dir_all(&corpus).expect("the corpus folder");
    std::fs::write(
        corpus.join("notes.txt"),
        "alpha beta\nretry the gateway twice\n",
    )
    .expect("notes.txt");
    std::fs::write(corpus.join("list.txt"), "one two three\n").expect("list.txt");
    let corpus = corpus.to_string_lossy().into_owned();
    let store = work.path().join("index");
    std::fs::create_dir_all(&store).expect("the store folder");

    // The media run first: `index.sqlite` carries the media fingerprint (`photo140 …`).
    let (exit, out) = run(&store, &["index", &corpus]);
    assert_eq!(exit, 0, "the media run refused: {out}");
    let media_fingerprint = meta(&store, "index.sqlite", "fingerprint")
        .expect("the media run stamps its fingerprint in index.sqlite");
    assert!(
        media_fingerprint.contains("photo140"),
        "not the media fingerprint: {media_fingerprint}"
    );

    // The code run on the SAME index folder: exit 0, its rows and fingerprint in code.sqlite.
    let (exit, out) = run(&store, &["index", &corpus, "--code"]);
    assert_eq!(
        exit, 0,
        "index --code refused a store that holds a media index: {out}"
    );
    let code_fingerprint = meta(&store, "code.sqlite", "fingerprint")
        .expect("the code run stamps the code fingerprint in code.sqlite");
    assert!(
        code_fingerprint.contains("code1 lines1800"),
        "not the code fingerprint: {code_fingerprint}"
    );
    assert_eq!(
        row_count(&store, "code.sqlite"),
        2,
        "both text files are indexed as code"
    );
    assert_eq!(
        meta(&store, "index.sqlite", "fingerprint").as_deref(),
        Some(media_fingerprint.as_str()),
        "the code run left index.sqlite's fingerprint alone"
    );

    // `status` reports the store its flag names (`main.swift:403` reads `args.store()`).
    let (exit, out) = run(&store, &["status", "--code"]);
    assert_eq!(exit, 0, "status --code: {out}");
    assert!(
        out.starts_with(&format!("index: {}", store.join("code.sqlite").display())),
        "status --code named the main store: {out}"
    );
    let (exit, out) = run(&store, &["status"]);
    assert_eq!(exit, 0, "status: {out}");
    assert!(
        out.starts_with(&format!("index: {}", store.join("index.sqlite").display())),
        "status named the code store: {out}"
    );
}

/// `digup mcp` over stdio (the Claude Desktop shape): one JSON-RPC line in, one out.
/// Model-free by design — `initialize`/`tools/list`/`ping` never start an encoder,
/// `status` answers from the counters, and the search call is a `name:` keyword query
/// (name queries start no model — search_service.rs "needs no model — keyword search").
/// The fresh store folder is created by the searcher's own schema pass; the daemon's
/// answers ride the stdio stream as routed events, never as protocol output, and EOF
/// on stdin is the loop's stop rule (quit the child, exit 0 — no leaked children).
#[test]
fn mcp_stdio_answers_initialize_tools_and_verb_calls() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    use serde_json::{json, Value};

    let work = tempfile::tempdir().expect("a work folder");
    let corpus = work.path().join("corpus");
    std::fs::create_dir_all(&corpus).expect("the corpus folder");
    let store = work.path().join("index");
    std::fs::create_dir_all(&store).expect("the store folder");

    let mut child = Command::new(env!("CARGO_BIN_EXE_digup"))
        .args([
            "mcp".to_string(),
            "--index".to_string(),
            store.to_string_lossy().into_owned(),
            corpus.to_string_lossy().into_owned(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("digup mcp spawns");
    let mut stdin = child.stdin.take().expect("stdin");
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                          "clientInfo": {"name": "test", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), // no answer, spec rule
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "search", "arguments": {"q": "name: zzznothing"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
               "params": {"name": "status", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
               "params": {"name": "index", "arguments": {}}}), // stdio-invisible tool
    ];
    for request in &requests {
        writeln!(stdin, "{request}").expect("the request line lands");
    }
    stdin.flush().expect("flush");
    drop(stdin); // EOF: the loop quits its daemon and exits.
    let stdout = child.stdout.take().expect("stdout");
    let mut replies = std::collections::BTreeMap::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("a stdout line");
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).expect("one JSON object per line");
        let id = value
            .get("id")
            .and_then(Value::as_i64)
            .expect("the reply carries the request id");
        assert_eq!(value["jsonrpc"], "2.0", "the envelope is JSON-RPC 2.0");
        replies.insert(id, value);
    }
    let status = child.wait().expect("the mcp run ends");
    assert_eq!(status.code(), Some(0), "digup mcp exit");

    // Six answered requests, six lines: the notification answered nothing.
    assert_eq!(replies.len(), 6, "{replies:?}",);
    let initialize = &replies[&1];
    assert_eq!(initialize["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(initialize["result"]["serverInfo"]["name"], "digup");
    assert!(initialize["result"]["capabilities"]["tools"].is_object());

    let tools = replies[&2]["result"]["tools"]
        .as_array()
        .expect("tools/list answers an array")
        .iter()
        .map(|def| def["name"].as_str().expect("a named tool").to_string())
        .collect::<Vec<_>>();
    // The stdio surface: the three read tools; `index` has no job runner to reach,
    // `estimate` waits for --allow-admin.
    assert_eq!(tools, vec!["search", "status", "show"]);

    let search = &replies[&3];
    assert_eq!(search["result"]["isError"], json!(false), "{search}");
    let answer = &search["result"]["structuredContent"];
    // The daemon's answer, relayed: the transport id is stripped, the rest is the
    // engine's own reply (an honest empty is an empty).
    assert!(
        answer.get("id").is_none(),
        "the relay strips the id: {answer}"
    );
    assert_eq!(answer["hits"], json!([]), "{answer}");
    assert!(search["result"]["content"][0]["text"]
        .as_str()
        .expect("a skimming text")
        .starts_with("0 hits, 0 strong"));

    let status_reply = &replies[&4];
    assert_eq!(status_reply["result"]["isError"], json!(false));
    assert_eq!(
        status_reply["result"]["structuredContent"]["files"],
        json!(0)
    );

    assert_eq!(
        replies[&5]["result"],
        json!({}),
        "ping answers an empty result"
    );

    let refused = &replies[&6];
    assert_eq!(
        refused["error"]["code"],
        json!(-32602),
        "the stdio surface has no index tool: {refused}"
    );
}
