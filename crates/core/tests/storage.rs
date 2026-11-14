//! Phase-1 storage tests (ARCHITECTURE_MAP.md §2): a rusqlite-built index must be byte-identical
//! to the Swift engine's at `user_version = 6`, and the counters + 8 triggers must never drift from
//! full aggregation.

use digup_core::db::Sql;
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

/// `Database.transaction`'s exits, each measured: Swift's `defer { transactionDepth -= 1 }`
/// (Database.swift:88) runs on EVERY exit, and its `catch` (95-103) ends the transaction —
/// `ROLLBACK` at depth 0, `ROLLBACK TO` + `RELEASE` deeper — before the error is rethrown.
///
/// The defect this pins (`audit-base`) incremented the depth BEFORE the `BEGIN` and restored
/// it only in the arms that reach the end of its own `match`, so a `BEGIN` that is refused
/// left the daemon at depth 1. The next top-level `transaction` then takes the SAVEPOINT
/// branch: no `BEGIN` runs, its writes ride the leaked transaction, and its `RELEASE digup1`
/// COMMITS everything — including the writes a failed body was supposed to discard. A
/// long-lived index helper would persist rows the engine discards, permanently, and never
/// say so.
///
/// The `COMMIT`-that-fails arm (`SQLITE_FULL`, a read-only mount, a share that drops a
/// handle mid-run) is the same code as the body-failure arm — `end_early` + the depth
/// guard — and is measured through it: in WAL a reader CANNOT produce a failing `COMMIT`
/// (readers read at their snapshot while the writer appends — that is the mode's point,
/// measured here: a reader mid-scan let the `COMMIT` land), and the frame write on an
/// already-open `-wal` handle ignores file permissions (measured: `chmod 444` on
/// `index.sqlite-wal` still committed). The reachable injections are the ones below.
#[test]
fn a_blocked_begin_leaves_no_depth_behind() {
    use digup_core::db::Database;
    use std::time::Duration;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("begins.sqlite");
    let db = Database::open(&path).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    db.set_busy_timeout_ms(0).unwrap();

    let writer = Connection::open(&path).unwrap();
    writer
        .busy_timeout(Duration::from_millis(0))
        .expect("a writer with no patience");
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let mut ran = false;
    let outcome = db.transaction(|| -> digup_core::db::Result<()> {
        ran = true;
        Ok(())
    });
    assert!(
        outcome.is_err() && !ran,
        "BEGIN IMMEDIATE is refused by the other writer, and no body runs"
    );
    assert_eq!(
        db.transaction_depth(),
        0,
        "a failed BEGIN does not keep the daemon at depth 1 (Database.swift:88's defer)"
    );
    drop(writer);

    db.set_busy_timeout_ms(digup_core::db::BUSY_TIMEOUT_MS)
        .unwrap();
    db.transaction(|| -> digup_core::db::Result<()> {
        db.execute("INSERT INTO t(v) VALUES('landed')")?;
        Ok(())
    })
    .expect("the next transaction is top-level again: BEGIN runs");
    let left: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 1, "and its write commits");
}

/// The depth-0 `catch` (`try? execute("ROLLBACK")`): a body's error discards its writes and
/// stands as the transaction's error.
#[test]
fn a_failing_body_at_depth_zero_rolls_back_and_its_error_stands() {
    use digup_core::db::{Database, Error};

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("bodies.sqlite")).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    db.execute("INSERT INTO t(v) VALUES('committed')").unwrap();

    let outcome = db.transaction(|| -> digup_core::db::Result<()> {
        db.execute("INSERT INTO t(v) VALUES('the write that must vanish')")?;
        Err(Error::Failure("a body that fails".to_string()))
    });
    assert!(
        matches!(&outcome, Err(Error::Failure(_))),
        "the body's error stands: {outcome:?}"
    );
    assert_eq!(db.transaction_depth(), 0);
    let left: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 1, "the ROLLBACK discards the body's write");
}

/// The savepoint arm (Database.swift:90-102): inside a transaction, a nested block is a
/// `SAVEPOINT digup1`, and its failure undoes ONLY its own writes — the outer transaction
/// goes on and commits its own. The nesting depth is what chooses the branch, so this is
/// also the test that the depth is 1 inside the body and home at the end.
#[test]
fn a_nested_transaction_undoes_only_its_own_writes() {
    use digup_core::db::{Database, Error};

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("nesting.sqlite")).unwrap();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();

    db.transaction(|| -> digup_core::db::Result<()> {
        assert_eq!(db.transaction_depth(), 1, "the body runs at depth 1");
        db.execute("INSERT INTO t(v) VALUES('outer')")?;
        let inner = db.transaction(|| -> digup_core::db::Result<()> {
            assert_eq!(
                db.transaction_depth(),
                2,
                "the nested body runs one deeper (the SAVEPOINT branch)"
            );
            db.execute("INSERT INTO t(v) VALUES('inner')")?;
            Err(Error::Failure("the nested body fails".to_string()))
        });
        assert!(inner.is_err(), "the nested error reaches its caller");
        assert_eq!(
            db.transaction_depth(),
            1,
            "and the depth is back at the outer body"
        );
        let seen: i64 =
            db.conn()
                .query_row("SELECT count(*) FROM t WHERE v = 'inner'", [], |row| {
                    row.get(0)
                })?;
        assert_eq!(seen, 0, "ROLLBACK TO digup1 undid the nested write");
        Ok(())
    })
    .expect("the outer transaction commits");

    let rows: Vec<String> = {
        let conn = db.conn();
        let mut stmt = conn
            .prepare("SELECT v FROM t ORDER BY v")
            .expect("a read-only scan of the table");
        stmt.query_map([], |row| row.get::<_, String>(0))
            .expect("the scan runs")
            .map(|r| r.expect("a row"))
            .collect()
    };
    assert_eq!(rows, vec!["outer".to_string()]);

    // A nested block that succeeds releases its own writes into the outer transaction: they
    // appear to the outer body, and vanish with the outer's ROLLBACK.
    let outcome = db.transaction(|| -> digup_core::db::Result<()> {
        let inner = db.transaction(|| -> digup_core::db::Result<()> {
            db.execute("INSERT INTO t(v) VALUES('released')")
        });
        assert!(inner.is_ok(), "RELEASE digup1 keeps the nested writes");
        let seen: i64 =
            db.conn()
                .query_row("SELECT count(*) FROM t WHERE v = 'released'", [], |row| {
                    row.get(0)
                })?;
        assert_eq!(seen, 1, "and the outer body sees them");
        Err(Error::Failure("the outer body fails".to_string()))
    });
    assert!(outcome.is_err());
    let left: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        left, 1,
        "the outer ROLLBACK takes the released nested write with it (Database.swift:96)"
    );
    assert_eq!(db.transaction_depth(), 0);
}

/// `pending`/`pendingKinds`' `only:` filter (IndexStore.swift:699, 737) — the narrowing the
/// index daemon uses for the just-added files, and the shape no suite on either engine ever
/// reached. The port's one bug class here is the SQL string itself: Swift's
/// `" AND id IN (\(ids…))"` interpolation emits ONE literal paren on each side (the
/// interpolation's own parens are syntax, not characters), and a transcription that counts
/// the source parens yields `" AND id IN (1))"`, which SQLite rejects — a fresh index's
/// first daemon sync dies on its first `only:` call. This test runs the filter on a fresh
/// store, so the SQL is executed, not read.
#[test]
fn the_only_filter_narrows_the_pending_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);

    // Five files, three states, distinct mtimes so the order is the ORDER BY's, not the
    // insert order: two pending photos, a pending doc, a done photo, a failed doc.
    let ids: Vec<i64> = [
        ("/data/p1.jpg", FileKind::Image, 5.0, "pending"),
        ("/data/p2.jpg", FileKind::Image, 9.0, "pending"),
        ("/data/note.txt", FileKind::Doc, 7.0, "pending"),
        ("/data/done.jpg", FileKind::Image, 8.0, "done"),
        ("/data/bad.txt", FileKind::Doc, 6.0, "failed"),
    ]
    .iter()
    .map(|(path, kind, mtime, state)| {
        let mut c = cand(path, *kind);
        c.mtime = *mtime;
        let id = store.insert(&c).unwrap();
        if *state != "pending" {
            store
                .finish(id, state, None, std::collections::HashMap::new())
                .unwrap();
        }
        id
    })
    .collect();
    let (p1, p2, note, _done, _bad) = (ids[0], ids[1], ids[2], ids[3], ids[4]);

    // The unfiltered queue: indexing order (images before docs), mtime DESC inside.
    let paths: Vec<String> = store
        .pending(None, None)
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(
        paths,
        vec!["/data/p2.jpg", "/data/p1.jpg", "/data/note.txt"],
        "images first, newest first — the queue the run indexes from"
    );

    // `only:` narrows to the requested pending ids, in the SAME order, and drops the rest:
    // asking for the doc plus one photo keeps photo+doc, and the done/failed rows the ids
    // name (p3 here is a done file) stay out on their state.
    let only: std::collections::HashSet<i64> = [note, p2, ids[3]].into_iter().collect();
    let paths: Vec<String> = store
        .pending(None, Some(&only))
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(
        paths,
        vec!["/data/p2.jpg", "/data/note.txt"],
        "the filter narrows the queue; the done file inside `only` is not pending"
    );

    // `only` with ids that are all done/failed: the state predicate keeps them out.
    let only: std::collections::HashSet<i64> = [ids[3], ids[4]].into_iter().collect();
    assert!(
        store.pending(None, Some(&only)).unwrap().is_empty(),
        "a `only:` over finished files answers nothing, not their rows"
    );

    // The empty set returns before any SQL runs (Swift: `if let ids, ids.isEmpty { return out }`).
    let empty: std::collections::HashSet<i64> = std::collections::HashSet::new();
    assert!(store.pending(None, Some(&empty)).unwrap().is_empty());
    assert!(store.pending_kinds(Some(&empty)).unwrap().is_empty());

    // limit rides after the filter — ` LIMIT ?` is the last clause, so the narrowed
    // queue caps at the limit, in queue order.
    let only: std::collections::HashSet<i64> = [p1, p2, note].into_iter().collect();
    let paths: Vec<String> = store
        .pending(Some(1), Some(&only))
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(paths, vec!["/data/p2.jpg"], "limit caps the narrowed queue");

    // pendingKinds with `only:` — the shared embedder's model-shape look (#155): asked
    // about the two photos it sees only `image`, asked about all three it sees `doc` too.
    let only: std::collections::HashSet<i64> = [p1, p2].into_iter().collect();
    assert_eq!(
        store.pending_kinds(Some(&only)).unwrap(),
        [FileKind::Image].into_iter().collect(),
        "the narrowed kinds answer image alone: a text run this round never loads the image encoder the other way, either"
    );
    let only: std::collections::HashSet<i64> = [p1, p2, note].into_iter().collect();
    assert_eq!(
        store.pending_kinds(Some(&only)).unwrap(),
        [FileKind::Image, FileKind::Doc].into_iter().collect(),
        "and the full queue answers both"
    );
    assert_eq!(
        store.pending_kinds(None).unwrap(),
        [FileKind::Image, FileKind::Doc].into_iter().collect(),
        "the unfiltered look is the queue's kinds"
    );
    let _ = (p1, p2, note);
}

/// `prune_zero_counter_rows` (IndexStore.swift:572) — Swift's maintenance verb, exercised
/// there by `zeroRowsPruneAndRegrow` (CounterTests.swift:663) and by nothing here. The
/// counts below are the same shape Swift pins: emptied (bucket, kind, state) groups keep
/// their rows at n = 0 (reads stay exact through them); the prune clears them; the
/// triggers' upserts re-create rows on demand, and nothing between the steps moves a number.
#[test]
fn zero_rows_prune_and_regrow() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);

    // 240 photos over six folders, then all done: the pending groups empty out.
    let ids: Vec<i64> = (0..240)
        .map(|i| {
            store
                .insert(&cand(&format!("/z{}/f{}.png", i % 6, i), FileKind::Image))
                .unwrap()
        })
        .collect();
    for id in &ids {
        store
            .finish(*id, "done", None, std::collections::HashMap::new())
            .unwrap();
    }
    let count_zeros = |store: &IndexStore| -> i64 {
        open_conn(store)
            .query_row(
                "SELECT COUNT(*) FROM counters WHERE n = 0 AND leftsum = 0",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let zeros: i64 = count_zeros(&store);
    assert_eq!(
        zeros, 6,
        "one emptied (bucket, image, pending) group per folder — Finding 9's residue"
    );

    // Half go back to pending; every third gets a vector (segcounters grow).
    for (i, id) in ids.iter().enumerate() {
        if i % 2 == 0 {
            store
                .finish(*id, "pending", None, std::collections::HashMap::new())
                .unwrap();
        }
        if i % 3 == 0 {
            store
                .add_segment(
                    *id,
                    SegmentKind::Image,
                    Modality::Image,
                    None,
                    None,
                    None,
                    None,
                    Some(&[0.1f32; 768]),
                )
                .unwrap();
        }
    }

    // An isolated folder whose segments all go away: its segcounters group empties to a
    // zero row. Deleted app-order — fts first, like the engine's deleteSegments.
    let qids: Vec<i64> = (0..3)
        .map(|i| {
            store
                .insert(&cand(&format!("/q/f{i}.png"), FileKind::Image))
                .unwrap()
        })
        .collect();
    for id in &qids {
        store
            .add_segment(
                *id,
                SegmentKind::Image,
                Modality::Image,
                None,
                None,
                None,
                None,
                Some(&[0.1f32; 768]),
            )
            .unwrap();
    }
    for id in &qids {
        store
            .db
            .run(
                "DELETE FROM fts WHERE rowid IN (SELECT id FROM segments WHERE file = ? AND kind != 'name')",
                [Sql::Int(*id)],
            )
            .unwrap();
        store
            .db
            .run(
                "DELETE FROM segments WHERE file = ? AND kind != 'name'",
                [Sql::Int(*id)],
            )
            .unwrap();
    }
    let seg_zeros_count = |store: &IndexStore| -> i64 {
        open_conn(store)
            .query_row("SELECT COUNT(*) FROM segcounters WHERE n = 0", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let seg_zeros_before: i64 = seg_zeros_count(&store);
    assert_eq!(
        seg_zeros_before, 1,
        "the /q hasvec group stands at zero — the emptied group's row"
    );

    store.prune_zero_counter_rows().expect("the prune runs");
    let count_zeros = |store: &IndexStore| -> i64 {
        open_conn(store)
            .query_row(
                "SELECT COUNT(*) FROM counters WHERE n = 0 AND leftsum = 0",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let zeros: i64 = count_zeros(&store);
    assert_eq!(zeros, 0, "pruned");
    let seg_zeros: i64 = seg_zeros_count(&store);
    assert_eq!(seg_zeros, 0, "segcounters pruned too");

    // Pruning changes no truth: every counter read still equals full aggregation,
    // unscoped and under a folder scope (the counters' seeks ride the pruned table).
    let match_all = |store: &IndexStore| {
        assert!(
            store.counters_are_consistent().unwrap(),
            "row-for-row consistency"
        );
        assert_eq!(
            store.counts().unwrap().by_kind_and_state,
            store.full_counts().unwrap().by_kind_and_state
        );
        assert_eq!(
            store.pending_count().unwrap(),
            store.full_pending_count().unwrap()
        );
        assert_eq!(
            store.unfinished_count().unwrap(),
            store.full_unfinished_count().unwrap()
        );
    };
    match_all(&store);
    store.set_scope(Some(vec!["/z1".to_string()])).unwrap();
    match_all(&store);
    store.set_scope(None).unwrap();

    // The triggers' upserts re-create pruned groups on demand.
    store
        .insert(&cand("/z1/regrown.png", FileKind::Image))
        .unwrap();
    match_all(&store);
}

/// `files_under` (IndexStore.swift:603): the covering rule and the each-file-once rule,
/// at the shapes the app's root lists produce. A root inside another listed root is
/// covered by it and skipped — Swift's filter is `isInside(root, $0)`, whose prefix rule
/// makes a root cover itself only for the filesystem root ("/" starts with "/"), so a
/// list of `["/"]` answers NOTHING, quoted rows included; the port reproduces the rule,
/// it does not improve on it.
#[test]
fn files_under_skips_covered_roots_and_answers_each_file_once() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    for path in ["/data/a.jpg", "/data/in/b.jpg", "/other/c.jpg"] {
        store
            .insert(&cand(path, FileKind::Image))
            .unwrap();
    }

    let mut paths: Vec<String> = store
        .files_under(&["/data".to_string(), "/data/in".to_string()])
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec!["/data/a.jpg", "/data/in/b.jpg"],
        "/data/in is covered by /data and skipped; the files come once each"
    );

    let mut paths: Vec<String> = store
        .files_under(&["/data".to_string(), "/data".to_string()])
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec!["/data/a.jpg", "/data/in/b.jpg"],
        "a repeated root answers once (the `seen` set)"
    );

    assert!(
        store.files_under(&["/".to_string()]).unwrap().is_empty(),
        "the root covers itself on Swift's prefix rule (isInside(\"/\",\"/\")), so the \
         listing answers nothing; the engine behaves the same"
    );
}
