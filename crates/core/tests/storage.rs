//! Phase-1 storage tests (ARCHITECTURE_MAP.md §2): a rusqlite-built index must be byte-identical
//! to the Swift engine's at `user_version = 6`, and the counters + 8 triggers must never drift from
//! full aggregation.

use digup_core::index_store::IndexStore;
use digup_core::{Candidate, FileKind, Modality, SegmentKind};
use rusqlite::Connection;

fn cand(path: &str, kind: FileKind) -> Candidate {
    Candidate {
        path: path.to_string(),
        kind,
        size: 100,
        mtime: 1.0,
        inode: 7,
        device: 3,
    }
}

fn open(dir: &std::path::Path, name: &str) -> IndexStore {
    IndexStore::open(dir, name).unwrap()
}

fn open_conn(store: &IndexStore) -> Connection {
    Connection::open(&store.file).unwrap()
}

#[test]
fn fresh_open_is_v6_and_matches_the_engine_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    assert_eq!(store.db.user_version().unwrap(), digup_core::SCHEMA_VERSION);

    let conn = open_conn(&store);
    let names: Vec<(String, String)> = conn
        .prepare("SELECT name, type FROM sqlite_master")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let has = |t: &str, n: &str| names.iter().any(|(name, ty)| name == n && ty == t);

    // Core tables + the v6 counters tables.
    for t in ["meta", "files", "segments", "counters", "segcounters"] {
        assert!(has("table", t), "missing table {t}");
    }
    assert!(has("table", "fts"), "missing FTS5 table fts");
    // The exact tokenizer, verbatim, is on the fts CREATE statement.
    let fts_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        fts_sql.contains(r#"categories 'L* N* Co M*'"#),
        "fts tokenizer wrong: {fts_sql}"
    );
    assert!(fts_sql.contains("porter unicode61 remove_diacritics 2"));

    // Indexes: two on files, one on segments, the counters covering index.
    for i in [
        "files_inode",
        "files_state",
        "segments_file",
        "counters_state",
    ] {
        assert!(has("index", i), "missing index {i}");
    }

    // Exactly the eight maintenance triggers (CounterTests pins "eight, not nine").
    let triggers: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        triggers,
        vec![
            "files_ad",
            "files_ai",
            "files_au_path",
            "files_au_state",
            "files_bd",
            "seg_ad",
            "seg_ai",
            "seg_au_vec",
        ]
    );
}

#[test]
fn connection_contract_matches_database_swift() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    // Read the pragmas from the store's OWN connection: journal_mode persists in the file, but
    // foreign_keys and mmap_size are per-connection settings the engine sets on open.
    let conn = store.db.conn();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal", "journal_mode must be WAL");
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        fk, 1,
        "foreign_keys must be ON (the counters' cascade scheme needs it)"
    );
    let mmap: i64 = conn
        .query_row("PRAGMA mmap_size", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mmap, digup_core::db::MMAP_SIZE);
}

#[test]
fn counters_match_full_aggregation_across_the_write_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);

    // A pending file, a done file with a text segment (no vector) and a media segment (with vector),
    // and a partial file whose info carries read_to/read_end (the counters' leftsum).
    let a = store.insert(&cand("/data/a.jpg", FileKind::Image)).unwrap();
    store
        .finish(a, "done", None, std::collections::HashMap::new())
        .unwrap();
    store
        .add_segment(
            a,
            SegmentKind::Image,
            Modality::Image,
            None,
            None,
            None,
            Some("a dog on the beach"),
            Some(&[0.1f32; 768]),
        )
        .unwrap();
    store
        .add_segment(
            a,
            SegmentKind::Ocr,
            Modality::None,
            None,
            None,
            None,
            Some("receipt total"),
            None,
        )
        .unwrap();

    let b = store
        .insert(&cand("/data/doc/notes.pdf", FileKind::Pdf))
        .unwrap();
    // partial: pages 30 read, 45 total → 15 pages left.
    store
        .finish(
            b,
            "partial",
            Some("read to page 30"),
            [
                ("pages".to_string(), "45".to_string()),
                ("read_to".to_string(), "30".to_string()),
                ("read_end".to_string(), "45".to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap();

    // Insert, update-of-state, segment insert (with + without a vector): all trigger sites fire.
    assert!(
        store.counters_are_consistent().unwrap(),
        "counters must equal full aggregation"
    );

    let counts = store.counts().unwrap();
    let full = store.full_counts().unwrap();
    assert_eq!(counts.by_kind_and_state, full.by_kind_and_state);
    assert_eq!(
        (counts.segments, counts.vectors),
        (full.segments, full.vectors)
    );
    assert_eq!(
        (counts.unfinished, counts.left),
        (full.unfinished, full.left)
    );
    assert_eq!(counts.left, 15, "leftsum = pages left on the partial row");

    assert_eq!(
        store.pending_count().unwrap(),
        store.full_pending_count().unwrap()
    );
    assert_eq!(
        store.unfinished_count().unwrap(),
        store.full_unfinished_count().unwrap()
    );
}

#[test]
fn move_and_delete_keep_the_counters_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    let a = store
        .insert(&cand("/data/x/img.png", FileKind::Image))
        .unwrap();
    store
        .add_segment(
            a,
            SegmentKind::Image,
            Modality::Image,
            None,
            None,
            None,
            None,
            Some(&[0.2f32; 768]),
        )
        .unwrap();

    // files_au_path: moving across folders re-homes the counters (bucket changes).
    store.move_file(a, "/other/x/img.png").unwrap();
    assert!(
        store.counters_are_consistent().unwrap(),
        "counters exact after a move"
    );

    // files_bd is the only effective segcounters decrement on a parent delete (foreign_keys ON).
    store.delete(a).unwrap();
    assert!(
        store.counters_are_consistent().unwrap(),
        "one decrement per segment, not two"
    );
    let counts = store.counts().unwrap();
    assert_eq!(counts.segments, 0);
    assert_eq!(counts.vectors, 0);
}

#[test]
fn scoped_counts_match_the_full_form_under_a_folder_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .insert(&cand("/data/in/a.jpg", FileKind::Image))
        .unwrap();
    store
        .insert(&cand("/data/in/b.jpg", FileKind::Image))
        .unwrap();
    store
        .insert(&cand("/data/out/c.jpg", FileKind::Image))
        .unwrap();

    store.set_scope(Some(vec!["/data/in".to_string()])).unwrap();
    assert_eq!(
        store.pending_count().unwrap(),
        2,
        "scope sees only files under it"
    );
    assert_eq!(
        store.pending_count().unwrap(),
        store.full_pending_count().unwrap()
    );
    let counts = store.counts().unwrap();
    let full = store.full_counts().unwrap();
    assert_eq!(counts.by_kind_and_state, full.by_kind_and_state);

    // Empty scope → the all-zero answer, matching the old ` AND 0` form.
    store.set_scope(Some(vec![])).unwrap();
    assert_eq!(store.pending_count().unwrap(), 0);
    // Clearing the scope restores the whole-library answer.
    store.set_scope(None).unwrap();
    assert_eq!(store.pending_count().unwrap(), 3);
}

#[test]
fn rebuild_recovers_countertamper_and_reopen_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    let a = store.insert(&cand("/data/a.jpg", FileKind::Image)).unwrap();
    store
        .add_segment(
            a,
            SegmentKind::Image,
            Modality::Image,
            None,
            None,
            None,
            None,
            Some(&[0.5f32; 768]),
        )
        .unwrap();
    assert!(store.counters_are_consistent().unwrap());

    // Tamper a counter row that exists (the file is pending; no finish ran), then rebuild: the
    // self-heal re-derives truth from full aggregation.
    {
        let conn = Connection::open(&store.file);
        let conn = conn.unwrap();
        conn.execute("UPDATE counters SET n = n + 7 WHERE state = 'pending'", [])
            .unwrap();
    }
    assert!(
        !store.counters_are_consistent().unwrap(),
        "tamper must be detected"
    );
    store.rebuild_counters().unwrap();
    assert!(
        store.counters_are_consistent().unwrap(),
        "rebuild restores exactness"
    );

    // Reopening the existing v6 file is a no-op (no re-migration), and its counters stand.
    drop(store);
    let reopened = open(tmp.path(), digup_core::MAIN_FILE);
    assert_eq!(
        reopened.db.user_version().unwrap(),
        digup_core::SCHEMA_VERSION
    );
    assert!(reopened.counters_are_consistent().unwrap());
}

#[test]
fn rejects_a_newer_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join(digup_core::MAIN_FILE);
    {
        let conn = Connection::open(&file).unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
    }
    let err = match IndexStore::open(tmp.path(), digup_core::MAIN_FILE) {
        Ok(_) => panic!("a newer index must be refused"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("newer DigUp"), "refuse a newer index: {err}");
}

#[test]
fn meta_roundtrips() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_meta(
            "fingerprint",
            Some("photo140 shot280 page280 frame70/3.0s audio30.0/25.0s"),
        )
        .unwrap();
    assert_eq!(
        store.meta("fingerprint").unwrap().as_deref(),
        Some("photo140 shot280 page280 frame70/3.0s audio30.0/25.0s")
    );
    store.set_meta("fingerprint", None).unwrap();
    assert_eq!(store.meta("fingerprint").unwrap(), None);
}
