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

/// The v6 read-compat gate: a byte-built v6 store (v5 stamp + counters + the eight
/// triggers + stamp 6 + raw rows) opens as `StoreMode::V6` at version 6, answers its
/// stored absolute paths verbatim through every reader, and REFUSES writes with the
/// migration sentence. The v6 → v7 conversion is the migration verb (S2), never a read.
#[test]
fn a_v6_store_opens_reads_verbatim_and_refuses_writes() {
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

    let refusal = store
        .insert(&cand("/data/b.jpg", FileKind::Image))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("schema 6"),
        "the writer on a v6 store refuses with the migration sentence: {refusal}"
    );
    let refusal = store
        .move_file(1, "/data/moved.jpg")
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("schema 6"),
        "moves refuse alike: {refusal}"
    );
    // The refusal changed nothing: the row and counters stand.
    assert_eq!(store.pending_count().unwrap(), 1);
    assert!(store.counters_are_consistent().unwrap());
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
