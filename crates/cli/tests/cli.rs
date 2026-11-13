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
    db.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        [key],
        |row| row.get::<_, String>(0),
    )
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
    if LlamaModels::locate().map(|models| models.media.is_none()).unwrap_or(true) {
        eprintln!("skipped: the pinned model files are not installed (docs/implementation/gpu-handoff.md).");
        return;
    }
    let work = tempfile::tempdir().expect("a work folder");
    let corpus = work.path().join("corpus");
    std::fs::create_dir_all(&corpus).expect("the corpus folder");
    std::fs::write(corpus.join("notes.txt"), "alpha beta\nretry the gateway twice\n")
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
