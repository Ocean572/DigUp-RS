//! Schema v7 unit pins (#195, round ten S1): anchors + anchor-relative rows + answer
//! assembly + per-anchor scope + the v6 read-compat gate. The law these pins carry:
//! the stored row is RELATIVE, every answer is ASSEMBLED absolute (byte-identical to the
//! v6 contract), counters ride the anchor-prefixed bucket keys UNCHANGED, and a v6 store
//! opens for a verbatim read with its write verbs refused.

use digup_core::index_store::{IndexStore, StoreMode};
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

/// Raw (anchor, rel, bucket) rows straight off the table — the pins read storage shape,
/// not answers (answers are asserted on the record path).
fn raw_rows(store: &IndexStore) -> Vec<(String, String, String)> {
    open_conn(store)
        .prepare("SELECT a.abs, f.path, f.bucket FROM files f JOIN anchors a ON a.id = f.anchor ORDER BY f.id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The portable law: argv roots arrive as scope → anchors registered; rows under a
/// registered root anchor AT the root and store the FULL relative path, and every answer
/// assembles the absolute path byte-identically to v6's stored form.
#[test]
fn registered_roots_store_relative_rows_and_answer_absolute() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    store
        .insert(&cand("/data/photos/x.png", FileKind::Image))
        .unwrap();

    assert_eq!(
        raw_rows(&store),
        vec![(
            "/data".to_string(),
            "photos/x.png".to_string(),
            "1/photos".to_string()
        )],
        "the row is RELATIVE (photos/x.png) with the anchor-prefixed bucket key 1/photos"
    );
    let records = store
        .files_under(&["/data".to_string(), "/data/photos".to_string()])
        .unwrap();
    assert_eq!(
        records.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["/data/photos/x.png"],
        "the answer is ASSEMBLED absolute; /data/photos is covered by /data and skipped (v6 rule)"
    );
}

/// The assembly edge: the POSIX root anchor carries its own slash (no double slash), the
/// Windows drive-root gets the slash inserted between (`"C:" + "/" + rel`).
#[test]
fn root_anchor_assemblies_carry_one_slash() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.ensure_anchor("/").unwrap();
    store.ensure_anchor("C:").unwrap();
    let a = store.insert(&cand("/x.png", FileKind::Image)).unwrap();
    let b = store.insert(&cand("C:/x.png", FileKind::Image)).unwrap();
    // The covering rule quotes `/` under a listed root (v6 law), so the lookup rides a
    // filtered `pending` over the two ids — exact, no scope games.
    let by_id: std::collections::HashSet<i64> = [a, b].into_iter().collect();
    let paths: Vec<String> = store
        .pending(None, Some(&by_id))
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(paths.len(), 2);
    let joined = paths.join("\n");
    assert!(
        joined.contains("/x.png") && !joined.contains("//x.png"),
        "the POSIX root anchor assembles /x.png — not //x.png: {joined}"
    );
    assert!(
        joined.contains("C:/x.png"),
        "the drive-root anchor assembles C:/x.png — not C:x.png: {joined}"
    );
}

/// UNIQUE(anchor, path): the same relative name under two roots is legal (v6's global
/// UNIQUE(path) spoke of one store per tree; v7 carries the tree-relative name per root).
#[test]
fn the_same_relative_name_stands_under_two_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_scope(Some(vec!["/a".to_string(), "/b".to_string()]))
        .unwrap();
    store.insert(&cand("/a/x.png", FileKind::Image)).unwrap();
    store.insert(&cand("/b/x.png", FileKind::Image)).unwrap();
    let rows = raw_rows(&store);
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|(_, rel, _)| rel == "x.png"),
        "both rows keep the same rel x.png under their own anchors: {rows:?}"
    );
    assert_eq!(
        store
            .files_under(&["/a".to_string()])
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        vec!["/a/x.png".to_string()],
        "the /a listing answers only the /a row"
    );
    assert!(store.counters_are_consistent().unwrap());
}

/// A move across roots: the anchor relocates, counters ride `files_au_path` (the bucket
/// key moves), and the FTS name row re-tokenizes from the ASSEMBLED absolute path (the
/// s343 law — folder tokens quote the root spelling, not the relative form).
#[test]
fn moving_across_roots_homes_the_anchor_counters_and_name_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_scope(Some(vec!["/a".to_string(), "/b".to_string()]))
        .unwrap();
    let id = store.insert(&cand("/a/x.png", FileKind::Image)).unwrap();
    store
        .add_segment(
            id,
            SegmentKind::Image,
            Modality::Image,
            None,
            None,
            None,
            None,
            Some(&[0.1f32; 768]),
        )
        .unwrap();
    store.move_file(id, "/b/x.png").unwrap();

    let rows = raw_rows(&store);
    assert_eq!(
        rows,
        vec![("/b".to_string(), "x.png".to_string(), "2/".to_string())],
        "the row moved anchor and key, the rel stands (same name under the new root)"
    );
    assert!(
        store.counters_are_consistent().unwrap(),
        "counters moved with the bucket key (files_au_path)"
    );
    let counts = store.counts().unwrap();
    let full = store.full_counts().unwrap();
    assert_eq!(counts.by_kind_and_state, full.by_kind_and_state);
    assert_eq!(
        (counts.segments, counts.vectors),
        (full.segments, full.vectors)
    );
    let body: String = open_conn(&store)
        .query_row(
            "SELECT body FROM fts WHERE rowid IN (SELECT id FROM segments WHERE file = ? AND kind = 'name')",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(body, "b", "the folder token moved with the anchor (a → b)");
}

/// Scope in v7 spans anchors: scoped counter reads equal the full-scan oracle under a
/// one-anchor scope, a two-anchor scope, the root anchor (whole store), and a subfolder
/// scope inside one anchor. The z-relevant law: scoped == full over the same rows.
#[test]
fn scoped_reads_match_the_full_oracle_across_anchors() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_scope(Some(vec!["/a".to_string(), "/b".to_string()]))
        .unwrap();
    for path in ["/a/1.jpg", "/a/nest/2.jpg", "/b/3.jpg"] {
        let id = store.insert(&cand(path, FileKind::Image)).unwrap();
        store
            .add_segment(
                id,
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

    for scope in [
        vec!["/a".to_string()],
        vec!["/a".to_string(), "/b".to_string()],
        vec!["/".to_string()],
        vec!["/a/nest".to_string()],
    ] {
        store.set_scope(Some(scope.clone())).unwrap();
        let counts = store.counts().unwrap();
        let full = store.full_counts().unwrap();
        assert_eq!(
            counts.by_kind_and_state, full.by_kind_and_state,
            "scope {scope:?}: kind/state counts equal full aggregation"
        );
        assert_eq!(
            (counts.segments, counts.vectors),
            (full.segments, full.vectors),
            "scope {scope:?}: segment/vector counts equal full aggregation"
        );
        assert_eq!(
            store.pending_count().unwrap(),
            store.full_pending_count().unwrap(),
            "scope {scope:?}: pending equal"
        );
    }
    // The subfolder scope sees exactly its files: 1 of 3.
    store.set_scope(Some(vec!["/a/nest".to_string()])).unwrap();
    assert_eq!(store.pending_count().unwrap(), 1);
    store.set_scope(None).unwrap();
    assert_eq!(store.pending_count().unwrap(), 3);
}

/// Root-level files (rel = a bare name) get the prefix-alone bucket `"<anchor id>/"` and
/// the folder scope of their anchor sees them (the own-key range law, prefix space).
#[test]
fn root_level_files_get_the_prefix_bucket_and_scope_sees_them() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    store.insert(&cand("/data/x.png", FileKind::Image)).unwrap();
    let rows = raw_rows(&store);
    assert_eq!(
        rows[0].2, "1/",
        "the root-file bucket is the anchor prefix alone: {:?}",
        rows[0]
    );
    assert_eq!(store.pending_count().unwrap(), 1);
    let counts = store.counts().unwrap();
    assert_eq!(
        counts
            .by_kind_and_state
            .get("image")
            .map(|m| m.get("pending").copied()),
        Some(Some(1)),
        "the scoped counters see the root-level file"
    );
}

/// The dual-spelling law in the rel space: a registered root `L` whose realpath `R`
/// differs stores rows for walk paths under `R` at anchor `L` (the realpath spelling is
/// the split key), answers assemble the registered spelling, and `files_under` reaches
/// them from BOTH spellings (the `/var`↔`/private/var` rule, v7 shape).
#[test]
fn a_symlinked_root_stores_rows_for_the_walk_spelling_and_answers_the_registered_one() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join("photos")).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let link = digup_core::paths::to_slashes(&link.to_string_lossy()).to_string();
    let real = digup_core::paths::to_slashes(&real.to_string_lossy()).to_string();

    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec![link.clone()])).unwrap();
    let anchors = store.anchors().unwrap();
    assert_eq!(
        anchors[0].real.as_deref(),
        Some(real.as_str()),
        "the anchor carries the realpath spelling beside the registered abs"
    );
    // The walk reports the resolved spelling — the row splits on the realpath bound.
    store
        .insert(&cand(&format!("{real}/photos/y.png"), FileKind::Image))
        .unwrap();
    let rows = raw_rows(&store);
    assert_eq!(
        rows,
        vec![(
            link.clone(),
            "photos/y.png".to_string(),
            "1/photos".to_string()
        )],
        "the row anchors at the registered root with the full relative path"
    );
    for root in [link.as_str(), real.as_str()] {
        let found = store.files_under(&[root.to_string()]).unwrap();
        assert_eq!(
            found.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec![format!("{link}/photos/y.png").as_str()],
            "files_under reaches the row from the {root} spelling and answers the registered one"
        );
    }
}

/// The v6 read-compat gate + the S2 migration law: a byte-built v6 store (v5 stamp +
/// counters + the eight triggers + stamp 6 + raw rows) opens as `StoreMode::V6` at version
/// 6, answers its stored absolute paths VERBATIM through every reader, and the FIRST WRITE
/// verb migrates the store to v7 — rows re-relativized, counters re-keyed, answers
/// byte-identical. Reads never migrate; the conversion rides the writer entry
/// (`write_guard`). The migration marker clears on completion.
#[test]
fn a_v6_store_opens_reads_verbatim_and_the_first_write_migrates() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let conn = Connection::open(tmp.path().join(digup_core::MAIN_FILE)).unwrap();
        conn.execute_batch(&digup_core::schema::create_sql())
            .unwrap(); // stamps 5
        conn.execute_batch("ALTER TABLE files ADD COLUMN bucket TEXT NOT NULL DEFAULT ''")
            .unwrap(); // the first half of `migrate_counters` (the v6 column)
        conn.execute_batch(digup_core::schema::COUNTERS_TABLES_SQL)
            .unwrap();
        conn.execute_batch(&digup_core::schema::counter_triggers_sql())
            .unwrap();
        conn.execute("INSERT INTO files(path, kind, size, mtime, inode, device, bucket) VALUES('/data/a.jpg', 'image', 100, 1.0, 7, 3, '/data')", [])
            .unwrap();
        conn.execute_batch(&digup_core::schema::fill_counters_sql())
            .unwrap();
        conn.execute_batch("PRAGMA user_version = 6;").unwrap();
    }
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    assert_eq!(store.mode(), StoreMode::V6, "a v6 store stays v6 at open");
    assert_eq!(store.db.user_version().unwrap(), 6);
    assert!(store.counters_are_consistent().unwrap());
    let rows = store.files_under(&["/data".to_string()]).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["/data/a.jpg"],
        "v6 reads answer the STORED absolute path verbatim"
    );
    assert_eq!(
        store.pending_count().unwrap(),
        store.full_pending_count().unwrap()
    );
    assert_eq!(store.pending(None, None).unwrap().len(), 1);
    assert!(store.set_scope(Some(vec!["/data".to_string()])).is_ok());
    assert_eq!(store.pending_count().unwrap(), 1);

    // The first write verb migrates: the insert lands, the store is v7, the old row rides.
    let b = store.insert(&cand("/data/b.jpg", FileKind::Image)).unwrap();
    assert_eq!(store.mode(), StoreMode::V7, "the first write lands v7");
    assert_eq!(store.db.user_version().unwrap(), 7);
    let anchors = store.anchors().unwrap();
    assert_eq!(
        anchors.iter().map(|a| a.abs.as_str()).collect::<Vec<_>>(),
        vec!["/data"],
        "the scope root registered as the anchor (the argv-roots-to-anchors law)"
    );
    let raw = raw_rows(&store);
    assert_eq!(
        raw,
        vec![
            ("/data".to_string(), "a.jpg".to_string(), "1/".to_string()),
            ("/data".to_string(), "b.jpg".to_string(), "1/".to_string()),
        ],
        "rows RELATIVE with prefix bucket keys, ids kept — the v6 row migrated (root-level file)"
    );
    let answers = store.files_under(&["/data".to_string()]).unwrap();
    assert_eq!(
        answers.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["/data/a.jpg", "/data/b.jpg"],
        "the answers assemble byte-identically to the v6 contract"
    );
    assert!(
        store.counters_are_consistent().unwrap(),
        "counters re-keyed from /data to 1/ and consistent"
    );
    assert_eq!(store.pending_count().unwrap(), 2);
    // The marker clears on completion (the resume law runs only from a marker).
    let marker: String = open_conn(&store)
        .query_row(
            "SELECT coalesce((SELECT value FROM meta WHERE key = 'portable_migration'), '')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marker, "", "the migration marker is cleared on completion");
    // The eight triggers stand on the swapped table and keep the counters exact: a move
    // re-keys the counters between bucket keys (the files_au_path law, post-migration).
    store.move_file(b, "/data/nested/b.jpg").unwrap();
    assert!(store.counters_are_consistent().unwrap());
    assert_eq!(store.pending_count().unwrap(), 2);
}

/// The honest out-of-anchor answer: a stopped-file note naming a path under no root marks
/// no row (v6 answered false too — v7 keeps the false, through the split refusal).
#[test]
fn a_note_under_no_root_marks_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    assert!(
        !store.mark_stopped("/nowhere/x.png", "ran away").unwrap(),
        "the v7 split refusal answers false, like v6's no-row lookup"
    );
}

/// The #7a ride-over (#195): `file_by_inode` (the device+inode lookup, added on main while
/// this port ran — the scoped sync's move detection follows it) must answer the ASSEMBLED
/// path on a v7 store: the record's path is the shape consumers re-path and report, and the
/// relative form is not a client fact.
#[test]
fn file_by_inode_answers_the_assembled_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    store
        .insert(&cand("/data/photos/x.png", FileKind::Image))
        .unwrap();
    let found = store
        .file_by_inode(3, 7)
        .unwrap()
        .expect("the row sharing (device, inode)");
    assert_eq!(
        found.path, "/data/photos/x.png",
        "the inode lookup answers ASSEMBLED absolute, like every other reader"
    );
}

/// The v6 era's exact chain, byte-built: v5 stamp + the bucket column + counters tables +
/// the eight triggers + raw rows (`(path, kind)`, bucket = `bucket_of(path)` — the v6 law) +
/// the counters fill + stamp 6.
fn v6_fixture(dir: &std::path::Path, rows: &[(&str, &str)]) {
    let conn = Connection::open(dir.join(digup_core::MAIN_FILE)).unwrap();
    conn.execute_batch(&digup_core::schema::create_sql())
        .unwrap(); // stamps 5
    conn.execute_batch("ALTER TABLE files ADD COLUMN bucket TEXT NOT NULL DEFAULT ''")
        .unwrap();
    conn.execute_batch(digup_core::schema::COUNTERS_TABLES_SQL)
        .unwrap();
    conn.execute_batch(&digup_core::schema::counter_triggers_sql())
        .unwrap();
    for (path, kind) in rows {
        conn.execute(
            "INSERT INTO files(path, kind, size, mtime, inode, device, bucket) VALUES(?, ?, 100, 1.0, 7, 3, ?)",
            rusqlite::params![path, kind, digup_core::model::bucket_of(path)],
        )
        .unwrap();
    }
    conn.execute_batch(&digup_core::schema::fill_counters_sql())
        .unwrap();
    conn.execute_batch("PRAGMA user_version = 6;").unwrap();
}

fn counter_buckets(store: &IndexStore) -> Vec<String> {
    open_conn(store)
        .prepare("SELECT DISTINCT bucket FROM counters ORDER BY bucket")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn marker(store: &IndexStore) -> String {
    open_conn(store)
        .query_row(
            "SELECT coalesce((SELECT value FROM meta WHERE key = 'portable_migration'), '')",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn row_count(store: &IndexStore) -> i64 {
    open_conn(store)
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .unwrap()
}

/// The migration of a store WITH rows (S2, spec law (a)): anchors come from the STORED-row
/// clusters and the argv roots VALIDATE them — a cluster a root covers anchors its rows AT
/// the root with the FULL relative path; a cluster no root validates is PRUNED, its rows
/// COUNTED in the report (the ghost cure, reused: a ghost is a row no current root answers
/// under — the cure is the count, not a kept row). Counters re-key from the absolute buckets
/// to the anchor-prefixed ones, every answer assembles byte-identically, and the scope
/// re-applies in the v7 shape.
#[test]
fn migration_splits_rows_rekeys_counters_and_answers_identically() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(
        tmp.path(),
        &[
            ("/data/x.jpg", "image"),
            ("/data/photos/y.png", "screenshot"),
            ("/else/z.txt", "doc"),
        ],
    );
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    // The verb relays its counts verbatim (the thin-client law: the engine counts, the
    // report carries the counts — nothing invented).
    let report = store.migrate_portable().unwrap();
    assert_eq!(
        (
            report.rows,
            report.migrated,
            report.pruned,
            report.anchors,
            report.rewritten
        ),
        (3, 2, 1, 1, 0),
        "3 rows read, 2 migrated under the /data anchor, the /else cluster pruned with its count"
    );
    // The first write verb rides on the migrated store.
    store
        .insert(&cand("/data/new.png", FileKind::Image))
        .unwrap();
    assert_eq!(store.mode(), StoreMode::V7);
    assert_eq!(store.db.user_version().unwrap(), 7);

    let mut raw = raw_rows(&store);
    raw.sort();
    assert_eq!(
        raw,
        vec![
            ("/data".to_string(), "new.png".to_string(), "1/".to_string()),
            (
                "/data".to_string(),
                "photos/y.png".to_string(),
                "1/photos".to_string()
            ),
            ("/data".to_string(), "x.jpg".to_string(), "1/".to_string()),
        ],
        "the registered root anchors at the root with the FULL rel; the ghost row is pruned"
    );
    assert_eq!(
        counter_buckets(&store),
        vec!["1/".to_string(), "1/photos".to_string()],
        "counters re-keyed to the anchor-prefixed prefix space; the pruned cluster's key is gone"
    );
    assert!(store.counters_are_consistent().unwrap());
    assert_eq!(marker(&store), "", "the marker cleared");

    // Answers assemble byte-identically, per root. Order is the UNIQUE(anchor, path) index
    // order (rel-sorted per anchor) — the v6 per-root range scan answered the same way.
    assert_eq!(
        store
            .files_under(&["/data".to_string()])
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        vec![
            "/data/new.png".to_string(),
            "/data/photos/y.png".to_string(),
            "/data/x.jpg".to_string()
        ],
    );
    assert!(
        store
            .files_under(&["/else".to_string()])
            .unwrap()
            .is_empty(),
        "the pruned cluster answers nothing — the prune is the cure the report counts"
    );
    // Scope re-applied in the v7 shape; the ghost is gone store-wide (the prune).
    assert_eq!(store.pending_count().unwrap(), 3, "in-scope rows");
    assert_eq!(
        store.full_pending_count().unwrap(),
        3,
        "the scan oracle equals the counters — no ghost count survives"
    );
    let counts = store.counts().unwrap();
    assert_eq!(
        counts.by_kind_and_state.get("doc"),
        None,
        "the pruned doc is out of the counts"
    );
    assert_eq!(
        counts
            .by_kind_and_state
            .get("image")
            .and_then(|m| m.get("pending"))
            .copied(),
        Some(2),
        "the two in-scope images counted"
    );
    store.set_scope(None).unwrap();
    assert_eq!(
        store.pending_count().unwrap(),
        3,
        "the whole store: the ghost row was pruned, the count is honest"
    );
    assert_eq!(store.full_pending_count().unwrap(), 3);
    assert_eq!(row_count(&store), 3, "2 migrated + 1 inserted");
}

/// The resume law (S2): a run interrupted after the row copy (phase 1 alone) leaves the
/// store VERBATIM v6 — original table, original stamp, the marker set — and the next write
/// verb re-runs every phase idempotently: the copy adds nothing it already inserted, the
/// swap lands, the counters key once, and the store finishes v7 with exactly its rows.
#[test]
fn an_interrupted_migration_resumes_from_the_marker() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(tmp.path(), &[("/data/x.jpg", "image")]);
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    // Phase 1 only — the crash shape: rows copied, swap never ran.
    store.migrate_copy_rows(&["/data".to_string()]).unwrap();
    assert_eq!(store.db.user_version().unwrap(), 6, "the store is still v6");
    assert_eq!(
        store.mode(),
        StoreMode::V6,
        "reads stay verbatim mid-flight"
    );
    assert_eq!(
        marker(&store),
        "rows",
        "the marker says the verb is under way"
    );
    let new_exists: i64 = open_conn(&store)
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'files_new'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(new_exists, 1, "the copy table stands beside the original");
    assert_eq!(row_count(&store), 1, "the original rows stand");
    assert_eq!(
        store
            .files_under(&["/data".to_string()])
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        vec!["/data/x.jpg".to_string()],
        "a reader mid-flight answers the v6 bytes"
    );

    // The next write verb resumes: re-runs every phase, completes.
    store
        .insert(&cand("/data/n/y.png", FileKind::Image))
        .unwrap();
    assert_eq!(store.db.user_version().unwrap(), 7);
    assert_eq!(store.mode(), StoreMode::V7);
    assert_eq!(
        row_count(&store),
        2,
        "no row inserted twice (INSERT OR IGNORE keyed on the id)"
    );
    assert!(store.counters_are_consistent().unwrap());
    assert_eq!(marker(&store), "", "the marker cleared on completion");
    assert_eq!(
        store
            .files_under(&["/data".to_string()])
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        vec!["/data/n/y.png".to_string(), "/data/x.jpg".to_string()],
        "the resumed answers assemble identically (index order: rel-sorted)"
    );
}

/// The gate is the chain's law: it passes before the stamp, and the self-heal is the same
/// rebuild the v6 migration runs — tampered counters do NOT ship as v7 counters.
#[test]
fn migration_heals_tampered_counters_before_stamping() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(
        tmp.path(),
        &[("/data/x.jpg", "image"), ("/data/y.png", "image")],
    );
    {
        // Tamper: the counters lie about the row count (the v6-store counters, pre-migration).
        let conn = Connection::open(tmp.path().join(digup_core::MAIN_FILE)).unwrap();
        conn.execute("UPDATE counters SET n = 99", []).unwrap();
    }
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    assert!(
        !store.counters_are_consistent().unwrap(),
        "the tamper is visible on the v6 store"
    );
    store.insert(&cand("/data/z.png", FileKind::Image)).unwrap();
    assert_eq!(
        store.db.user_version().unwrap(),
        7,
        "the self-heal completed"
    );
    assert!(
        store.counters_are_consistent().unwrap(),
        "the migration rebuilt the counters from the new files table"
    );
    assert_eq!(store.full_pending_count().unwrap(), 3);
    let counts = store.full_counts().unwrap();
    assert_eq!(
        counts
            .by_kind_and_state
            .get("image")
            .and_then(|m| m.get("pending"))
            .copied(),
        Some(3),
        "the rebuilt counters count the rows, not the tamper"
    );
}

/// The dual spelling in the migration: a v6 store whose rows spell the WALK form (the
/// `/private/var` shape) under a registered argv root (the `/var` shape — the symlinked
/// root) splits on the realpath bound, anchors at the REGISTERED spelling, and answers the
/// registered form.
#[test]
fn migration_splits_a_walk_spelling_row_at_the_registered_root() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join("photos")).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let link = digup_core::paths::to_slashes(&link.to_string_lossy()).to_string();
    let real = digup_core::paths::to_slashes(&real.to_string_lossy()).to_string();

    v6_fixture(tmp.path(), &[(&format!("{real}/photos/y.png"), "image")]);
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec![link.clone()])).unwrap();
    store
        .insert(&cand(&format!("{real}/photos/n.png"), FileKind::Image))
        .unwrap();
    let anchors = store.anchors().unwrap();
    assert_eq!(anchors.len(), 1);
    assert_eq!(
        anchors[0].abs, link,
        "the anchor is the registered spelling"
    );
    assert_eq!(
        anchors[0].real.as_deref(),
        Some(real.as_str()),
        "the realpath spelling rides the anchor"
    );
    let raw = raw_rows(&store);
    assert!(
        raw.iter()
            .all(|(a, rel, _)| a == &link && rel.starts_with("photos/")),
        "rows split at the realpath bound and anchor at the registered one: {raw:?}"
    );
    for root in [link.as_str(), real.as_str()] {
        let found = store.files_under(&[root.to_string()]).unwrap();
        assert_eq!(found.len(), 2, "both spellings reach the rows ({root})");
        assert!(
            found.into_iter().all(|r| r.path.starts_with(&link)),
            "the answers assemble the REGISTERED spelling from the {root} lookup"
        );
    }
    assert!(store.counters_are_consistent().unwrap());
}

/// No registered roots: every row parent-anchors (the unregistered-writer law) — the same
/// shape a bare-CLI store takes — and the answers round-trip identically.
#[test]
fn a_migration_without_roots_parent_anchors_every_row() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(
        tmp.path(),
        &[("/x/a.png", "image"), ("/y/deep/b.png", "doc")],
    );
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.insert(&cand("/x/c.png", FileKind::Image)).unwrap();
    let raw = raw_rows(&store);
    assert_eq!(
        raw,
        vec![
            ("/x".to_string(), "a.png".to_string(), "1/".to_string()),
            ("/y/deep".to_string(), "b.png".to_string(), "2/".to_string()),
            ("/x".to_string(), "c.png".to_string(), "1/".to_string()),
        ],
        "anchors are the parent folders, rels the bare names, rows in id order"
    );
    assert_eq!(
        store
            .files_under(&["/x".to_string(), "/y".to_string()])
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        vec![
            "/x/a.png".to_string(),
            "/x/c.png".to_string(),
            "/y/deep/b.png".to_string()
        ],
        "answers assemble byte-identically, rootless store"
    );
    assert!(store.counters_are_consistent().unwrap());
}

/// Migration on a store WITH segments — the measured cascade law (matrix, 2026-11-27):
/// `DROP TABLE` with `foreign_keys = ON` cascade-DELETEs every child row (in-tx and
/// `defer_foreign_keys` alike), the emptied rowid restarts ids at 1, and the migration
/// silently loses every vector, name row and fts row. The swap runs with FK OFF around its
/// transaction. This pin is the seal: segment ids, vector BYTES, name rows and fts rows all
/// stand byte-identically after migration, and the ids the FK points at stand with them.
#[test]
fn migration_preserves_segments_vectors_and_name_rows() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(
        tmp.path(),
        &[("/data/a.jpg", "image"), ("/data/b.png", "image")],
    );
    {
        let conn = Connection::open(tmp.path().join(digup_core::MAIN_FILE)).unwrap();
        conn.execute(
            "INSERT INTO segments(id, file, kind, modality, excerpt) VALUES(1, 1, 'name', 'text', 'a jpg')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO segments(id, file, kind, modality, vec) VALUES(2, 1, 'image', 'image', ?1)",
            rusqlite::params![vec![0x55u8; 1536]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts(rowid, name, body, original) VALUES(2, '', 'alpha text', 'alpha text')",
            [],
        )
        .unwrap();
        conn.execute_batch(&digup_core::schema::fill_counters_sql())
            .unwrap();
    }
    let vec_before: Vec<u8> = Connection::open(tmp.path().join(digup_core::MAIN_FILE))
        .unwrap()
        .query_row("SELECT vec FROM segments WHERE id = 2", [], |r| r.get(0))
        .unwrap();
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    store.insert(&cand("/data/c.png", FileKind::Image)).unwrap();
    assert_eq!(store.db.user_version().unwrap(), 7);
    let conn = open_conn(&store);
    let segs: Vec<(i64, i64, String)> = conn
        .prepare("SELECT id, file, kind FROM segments ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        segs,
        vec![
            (1, 1, "name".to_string()),
            (2, 1, "image".to_string()),
            (3, 3, "name".to_string()),
        ],
        "the v6 segment rows stand with their ids; the inserted row adds its name row"
    );
    let vec_after: Vec<u8> = conn
        .query_row("SELECT vec FROM segments WHERE id = 2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        vec_after, vec_before,
        "the vector bytes stand verbatim (the v7 parity law: vec bytes pre/post migration are EXACT)"
    );
    let body: String = conn
        .query_row("SELECT body FROM fts WHERE rowid = 2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(body, "alpha text", "the fts row survives the swap");
    assert!(store.counters_are_consistent().unwrap());
    let full = store.full_counts().unwrap();
    assert_eq!(
        (full.segments, full.vectors),
        (1, 1),
        "content segments (name rows excluded from `segments`, the counts law) + vectors exact"
    );
    // FK came back ON (the connection contract) and the new writes enforce it.
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        fk, 1,
        "the swap's FK window closed — the connection is back on the contract"
    );
}

// ---- S2c: spec law (a) clusters + remount + prune, law (c) refusal-before-write ----

/// The portability promise (spec law (a), the remount): the folder MOVED — the rows spell
/// the old mount, the argv root is the new mount. The cluster's tail-component run matches
/// exactly one root, so the rows REBASE onto it: `anchor.abs` carries the CURRENT spelling,
/// rels ride the cluster tail, and the name rows re-tokenize from the new root spelling
/// (the s343 law: folder tokens quote the root).
#[test]
fn migration_rebases_a_remounted_tree_onto_the_new_root() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(
        tmp.path(),
        &[
            ("/mnt/old/photos/a.jpg", "image"),
            ("/mnt/old/photos/deep/b.png", "image"),
        ],
    );
    {
        let conn = Connection::open(tmp.path().join(digup_core::MAIN_FILE)).unwrap();
        // A name row + its fts row, tokenized from the OLD root spelling.
        conn.execute(
            "INSERT INTO segments(id, file, kind, modality, excerpt) VALUES(1, 1, 'name', 'text', 'a jpg')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts(rowid, name, body) VALUES(1, 'a.jpg', 'mnt old photos')",
            [],
        )
        .unwrap();
        conn.execute_batch(&digup_core::schema::fill_counters_sql())
            .unwrap();
    }
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_scope(Some(vec!["/mnt/new/photos".to_string()]))
        .unwrap();
    let report = store.migrate_portable().unwrap();
    assert_eq!(
        (
            report.rows,
            report.migrated,
            report.pruned,
            report.anchors,
            report.rewritten
        ),
        (2, 2, 0, 1, 2),
        "both rows rebase onto the one new root — the tail match names exactly one candidate"
    );
    let mut raw = raw_rows(&store);
    raw.sort();
    assert_eq!(
        raw,
        vec![
            (
                "/mnt/new/photos".to_string(),
                "a.jpg".to_string(),
                "1/".to_string()
            ),
            (
                "/mnt/new/photos".to_string(),
                "deep/b.png".to_string(),
                "1/deep".to_string()
            ),
        ],
        "the anchor rebases to the CURRENT spelling; rels carry the cluster tail"
    );
    assert_eq!(
        store
            .files_under(&["/mnt/new/photos".to_string()])
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        vec![
            "/mnt/new/photos/a.jpg".to_string(),
            "/mnt/new/photos/deep/b.png".to_string()
        ],
        "answers assemble the new mount"
    );
    // The name row re-tokenizes from the new root spelling (the verb's move_file pass).
    let conn = open_conn(&store);
    let (name, body): (String, String) = conn
        .query_row(
            "SELECT f.name, f.body FROM fts f JOIN segments s ON s.id = f.rowid \
             WHERE s.file = 1 AND s.kind = 'name'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, digup_core::search_text::normalized("a.jpg"));
    assert_eq!(
        body, "mnt new photos",
        "the folder tokens quote the NEW root — the moved tree searches under its new name"
    );
    assert_eq!(
        counter_buckets(&store),
        vec!["1/".to_string(), "1/deep".to_string()],
        "counters ride the new prefix keys"
    );
    assert!(store.counters_are_consistent().unwrap());
    assert_eq!(marker(&store), "", "the marker cleared");
}

/// Validation failure is the prune (spec law (a)): a cluster whose tail matches TWO roots
/// is ambiguous — no single new home exists — and an ambiguous tail counts as no validation.
/// Its rows are PRUNED, counted in the report; the store lands v7 with nothing anchored
/// under an invented home.
#[test]
fn migration_prunes_a_cluster_with_an_ambiguous_tail() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(tmp.path(), &[("/old/photos/a.jpg", "image")]);
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_scope(Some(vec!["/x/photos".to_string(), "/y/photos".to_string()]))
        .unwrap();
    let report = store.migrate_portable().unwrap();
    assert_eq!(
        (report.rows, report.migrated, report.pruned, report.anchors),
        (1, 0, 1, 2),
        "the tail `photos` matches both roots — ambiguous, pruned with its count"
    );
    assert_eq!(row_count(&store), 0, "the cluster's rows are gone");
    assert_eq!(store.db.user_version().unwrap(), 7);
    assert_eq!(
        counter_buckets(&store),
        Vec::<String>::new(),
        "no counter key survives the prune + the gate's heal"
    );
    assert!(store.counters_are_consistent().unwrap());
    assert!(store
        .files_under(&["/x/photos".to_string(), "/y/photos".to_string()])
        .unwrap()
        .is_empty());
    assert_eq!(marker(&store), "");
}

/// Law (c): the UNIQUE(anchor, path) pre-check runs BEFORE any write — the plan is built
/// read-only and the refusal fires there. v6's global UNIQUE(path) makes a containment
/// collision impossible, but a remount CAN land two clusters on one root: the refusal is
/// honest and permanent — the store stays verbatim v6 with its rows, and every re-run
/// refuses identically.
#[test]
fn migration_refuses_a_unique_collision_before_any_write() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(
        tmp.path(),
        &[("/x/photos/a.jpg", "image"), ("/y/photos/a.jpg", "image")],
    );
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_scope(Some(vec!["/x/photos".to_string()]))
        .unwrap();
    let err = store.migrate_portable().unwrap_err().to_string();
    assert!(
        err.contains("collide") && err.contains("a.jpg") && err.contains("/y/photos/a.jpg"),
        "the refusal quotes the colliding pair: {err}"
    );
    assert_eq!(
        store.db.user_version().unwrap(),
        6,
        "the refusal never stamps"
    );
    assert_eq!(store.mode(), StoreMode::V6, "reads stay verbatim");
    assert_eq!(row_count(&store), 2, "every v6 row stands");
    assert_eq!(
        marker(&store),
        "rows",
        "phase 0 ran (the resume marker) — the plan refused before any row write"
    );
    // Re-running refuses identically: the refusal is permanent, not a torn state.
    let again = store.migrate_portable().unwrap_err().to_string();
    assert!(
        again.contains("collide"),
        "the second call re-refuses: {again}"
    );
    assert_eq!(row_count(&store), 2);
}

/// Multiple argv roots = multiple anchors: each root registers its own anchor row, each
/// cluster validates against its root, and the bucket keys carry the anchor ids.
#[test]
fn migration_anchors_one_anchor_per_registered_root() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(tmp.path(), &[("/a/x.png", "image"), ("/b/y.png", "image")]);
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store
        .set_scope(Some(vec!["/a".to_string(), "/b".to_string()]))
        .unwrap();
    let report = store.migrate_portable().unwrap();
    assert_eq!(
        (report.rows, report.migrated, report.pruned, report.anchors),
        (2, 2, 0, 2),
        "two roots, two anchors, nothing pruned"
    );
    let mut raw = raw_rows(&store);
    raw.sort();
    assert_eq!(
        raw,
        vec![
            ("/a".to_string(), "x.png".to_string(), "1/".to_string()),
            ("/b".to_string(), "y.png".to_string(), "2/".to_string()),
        ],
        "anchors are the roots, buckets carry the anchor ids"
    );
    assert_eq!(
        store
            .files_under(&["/a".to_string(), "/b".to_string()])
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>(),
        vec!["/a/x.png".to_string(), "/b/y.png".to_string()],
        "per-anchor index order: anchor 1's rows, then anchor 2's"
    );
    assert_eq!(
        counter_buckets(&store),
        vec!["1/".to_string(), "2/".to_string()]
    );
    assert!(store.counters_are_consistent().unwrap());
}

/// A root-level file (`/x.png`, bucket `""`): the root cluster validates against the
/// registered root (the cluster CONTAINS it), but the ROW sits under no root bound — it
/// is a ghost, pruned with its count (the same ghost law as a row under no root).
#[test]
fn migration_prunes_a_root_level_file_outside_the_roots() {
    let tmp = tempfile::tempdir().unwrap();
    v6_fixture(tmp.path(), &[("/x.png", "image")]);
    let store = open(tmp.path(), digup_core::MAIN_FILE);
    store.set_scope(Some(vec!["/data".to_string()])).unwrap();
    let report = store.migrate_portable().unwrap();
    assert_eq!(
        (report.rows, report.migrated, report.pruned),
        (1, 0, 1),
        "the cluster validates, the row under no bound prunes with its count"
    );
    assert_eq!(row_count(&store), 0);
    assert_eq!(store.db.user_version().unwrap(), 7);
    assert!(store.counters_are_consistent().unwrap());
    assert_eq!(marker(&store), "");
}
