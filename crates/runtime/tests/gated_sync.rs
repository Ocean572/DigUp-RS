//! Issue #110 — the gated sync: the folder snapshot (mtime, entries) decides which
//! folders the walk enumerates, and the row-check (the file-level law) answers the rows
//! under the folders it skips. The transitions, pinned in order:
//!
//! 1. the first gated sync fills the snapshot from an honest FULL crawl; the second
//!    gated sync on an unchanged tree enumerates the ROOTS ONLY — every folder's
//!    (mtime, entries) pair stands, so the deep files are answered by the row-check
//!    (unchanged == total, added/changed/removed 0);
//! 2. content-edit-in-place (the Gemini inverse — a file's content moves, its PARENT
//!    folder's mtime does not) requeues through the row-check, with its folder gated
//!    out of the walk;
//! 3. a folder RENAME (POSIX: the parent's pair moves, the folder's old spelling is
//!    gone, the new spelling has no snapshot row) rides the move law: moved == 1 —
//!    the row-check's ENOENT does not eat the row before the inode pairing runs;
//! 4. a deep new file descends through its CLOSED (unchanged) ancestors: added == 1;
//! 5. the honest edge: a delete disguised by an mtime restore (remove one file, add a
//!    dummy, restore the folder's mtime — the pair is byte-identical) → the row-check
//!    removes the deleted file (ENOENT on a stored path), and the dummy enters the
//!    index only at the next FULL crawl — the gate's stated limitation, pinned;
//! 6. the rules fingerprint: change the excludes → the snapshot is not honest for the
//!    new rules, the sync runs FULL and refills (the excluded folder's rows prune).
//!
//! The model-free shape of the #7a suite (the embedder factory never runs). Unix-only
//! (the utimes fixture; the suite measures the Linux engine box, as #7a's and #2's are).

#![cfg(unix)]

use std::path::{Path, PathBuf};

use digup_core::index_store::IndexStore;
use digup_core::options::IndexOptions;
use digup_runtime::indexer::Indexer;
use digup_runtime::worker::Embedder;

fn temp_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-gated-{name}"));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn write(folder: &Path, name: &str, body: &[u8]) {
    let path = folder.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The model-free indexer (the `crawl_line_matches` factory: `sync` never asks for a model).
fn model_free_indexer(index: PathBuf) -> Indexer {
    let store = IndexStore::main(&index).expect("open the store");
    let nowhere = index;
    let make = move || -> digup_runtime::Result<Box<dyn Embedder + 'static>> {
        Err(digup_runtime::Error::io(
            &nowhere,
            std::io::Error::other("no model here"),
        ))
    };
    Indexer::new(store, IndexOptions::default(), Box::new(make)).expect("the indexer")
}

/// Restore a folder's mtime with `utimes(2)`: µs resolution, so the restored pair
/// lands within the gate's 1e-6 tolerance of the snapshot's ns-precision stamp.
fn restore_mtime(path: &Path, mtime: f64) {
    let sec = mtime.floor() as i64;
    let usec = ((mtime - sec as f64) * 1e6).floor().clamp(0.0, 999_999.0) as i64;
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap();
    let times = [
        libc::timeval {
            tv_sec: sec,
            tv_usec: usec,
        },
        libc::timeval {
            tv_sec: sec,
            tv_usec: usec,
        },
    ];
    let rc = unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) };
    assert_eq!(rc, 0, "utimes restores the folder's mtime");
}

#[test]
fn the_gate_skips_unchanged_subtrees_and_the_row_check_answers_them() {
    let work = temp_folder("skip");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "top.jpg", b"not really a jpeg");
    write(&corpus, "deep/b.jpg", b"not really a jpeg");
    write(&corpus, "deep/deeper/c.txt", b"hello deep");
    let corpus_root = slash(&corpus);

    let indexer = model_free_indexer(index.clone());
    let rows = |indexer: &Indexer| -> usize {
        indexer
            .store
            .files_under(std::slice::from_ref(&corpus_root))
            .expect("the rows")
            .len()
    };

    // 1 — the first gated sync: no snapshot, so the honest FULL crawl runs and the
    // snapshot fills from the prefilter.
    let first = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the first gated sync");
    assert_eq!(first.added, 3, "the full crawl indexes the whole tree");
    let snapshot = indexer.store.dir_snapshots().expect("the snapshot");
    assert!(
        snapshot.contains_key(&corpus_root)
            && snapshot.contains_key(&slash(&corpus.join("deep")))
            && snapshot.contains_key(&slash(&corpus.join("deep/deeper"))),
        "every folder the walk reached rides its pair"
    );
    assert!(
        indexer.store.dirs_rules().expect("meta").is_some(),
        "the rules fingerprint rides with the snapshot"
    );

    // 2 — an unchanged tree: the gate skips every folder whose pair stands. Only the
    // roots enumerate; the deep rows are answered by the row-check.
    let second = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the steady-state gated sync");
    assert_eq!(
        (second.added, second.changed, second.moved, second.removed),
        (0, 0, 0, 0),
        "nothing moved: the gate skipped the deep folders, the row-check confirmed the rows"
    );
    assert_eq!(
        second.unchanged, 3,
        "every row answered — the enumerated root file plus the row-checked deep files"
    );
    assert_eq!(rows(&indexer), 3);
    let enumerated: Vec<String> = second
        .crawl
        .candidates
        .iter()
        .map(|c| c.path.clone())
        .collect();
    assert_eq!(
        enumerated,
        vec![corpus_root.clone() + "/top.jpg"],
        "the walk enumerated the ROOT ONLY — deep/ and deep/deeper/ ride their unchanged pairs"
    );

    // 3 — a file added in an EXISTING folder: its pair moves, the folder descends, the
    // new file enters through the candidates (not the row-check).
    write(&corpus, "deep/new.jpg", b"not really a jpeg");
    let third = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the sync after a folder change");
    assert_eq!(
        (third.added, third.changed, third.moved, third.removed),
        (1, 0, 0, 0),
        "deep/'s pair moved: the folder descends, the file enters"
    );
    assert!(
        third
            .crawl
            .candidates
            .iter()
            .any(|c| c.path == corpus_root.clone() + "/deep/new.jpg"),
        "the changed folder was enumerated"
    );
    assert_eq!(rows(&indexer), 4);

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_content_edit_in_a_gated_out_folder_requeues_through_the_row_check() {
    let work = temp_folder("content");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "top.jpg", b"not really a jpeg");
    write(&corpus, "deep/b.jpg", b"short body");
    let indexer = model_free_indexer(index.clone());
    indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the filling sync");

    // The Gemini inverse: content changes IN PLACE (the directory entry is untouched,
    // so the PARENT folder's mtime does NOT move) — the dir pair cannot see this.
    let edited_id = indexer
        .store
        .files_under(&[slash(&corpus)])
        .expect("rows")
        .into_iter()
        .find(|r| r.path.ends_with("deep/b.jpg"))
        .expect("the deep row")
        .id;
    write(&corpus, "deep/b.jpg", b"a much longer body than before");
    let sync = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the sync over a content-edited file");
    assert_eq!(
        (sync.added, sync.changed, sync.moved, sync.removed),
        (0, 1, 0, 0),
        "the row-check stats the stored path: size + mtime moved → requeue"
    );
    assert_eq!(
        sync.queued,
        vec![edited_id],
        "the edited row joins the embed queue — the sync queues exactly what it requeued"
    );
    assert!(
        !sync
            .crawl
            .candidates
            .iter()
            .any(|c| c.path.ends_with("deep/b.jpg")),
        "deep/'s pair never moved — the walk never enumerated the file the row-check caught"
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_folder_rename_moves_the_row_under_the_gate() {
    let work = temp_folder("rename");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "top.jpg", b"not really a jpeg");
    write(&corpus, "deep/b.jpg", b"not really a jpeg");
    let indexer = model_free_indexer(index.clone());
    indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the filling sync");

    // POSIX rename: the PARENT's pair moves (the root — always enumerated), the old
    // spelling is gone, the new spelling has no snapshot row (the never-seen shape).
    std::fs::rename(corpus.join("deep"), corpus.join("renamed")).expect("rename the folder");
    let sync = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the sync after the rename");
    assert_eq!(
        (sync.added, sync.changed, sync.moved, sync.removed),
        (0, 0, 1, 0),
        "the row rides the inode to the new spelling — no delete, no re-add, no embedding bill"
    );
    let rows = indexer.store.files_under(&[slash(&corpus)]).expect("rows");
    assert_eq!(rows.len(), 2);
    assert!(
        !rows.iter().any(|r| r.path.contains("deep/b.jpg")),
        "the old spelling is gone from the index — search cannot answer a path that is not there"
    );
    assert!(rows.iter().any(|r| r.path.ends_with("renamed/b.jpg")));
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_deep_new_file_descends_through_its_unchanged_ancestors() {
    let work = temp_folder("deepadd");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "top.jpg", b"not really a jpeg");
    write(&corpus, "a/b/c/keep.jpg", b"not really a jpeg");
    let indexer = model_free_indexer(index.clone());
    indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the filling sync");

    // The leaf's pair moves; a/ and a/b/ keep their pairs (new folders — never seen —
    // would be active too; here the leaf folder is the only change).
    write(&corpus, "a/b/c/new.jpg", b"not really a jpeg");
    let sync = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the sync after the deep add");
    assert_eq!(
        (sync.added, sync.changed, sync.moved, sync.removed),
        (1, 0, 0, 0),
        "a/b/c/'s pair moved: the walk reaches it through its unchanged ancestors"
    );
    // A brand-new folder chain (never seen): the never-seen shape descends too.
    write(&corpus, "brand/new/deep/x.txt", b"hello brand");
    let sync = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the sync after the brand-new chain");
    assert_eq!(
        (sync.added, sync.changed, sync.moved, sync.removed),
        (1, 0, 0, 0),
        "no snapshot rows for brand/, brand/new/, brand/new/deep/ — the never-seen shape descends"
    );
    assert_eq!(
        indexer
            .store
            .files_under(&[slash(&corpus)])
            .expect("rows")
            .len(),
        4
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_delete_masked_by_an_mtime_restore_is_caught_and_the_dummy_waits_for_a_full_crawl() {
    let work = temp_folder("mask");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "top.jpg", b"not really a jpeg");
    write(&corpus, "deep/keep.jpg", b"not really a jpeg");
    let corpus_root = slash(&corpus);
    let indexer = model_free_indexer(index.clone());
    indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the filling sync");
    let deep_mtime = indexer
        .store
        .dir_snapshots()
        .expect("the snapshot")
        .get(&slash(&corpus.join("deep")))
        .expect("deep/ rides its pair")
        .0;

    // The mask: delete one file, add a dummy (entry count back to what it was),
    // restore the folder's mtime — the pair the gate compares is byte-identical.
    std::fs::remove_file(corpus.join("deep/keep.jpg")).expect("unlink the file");
    write(&corpus, "deep/dummy.jpg", b"not really a jpeg");
    restore_mtime(&corpus.join("deep"), deep_mtime);

    let sync = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the masked sync");
    assert_eq!(
        (sync.added, sync.changed, sync.moved, sync.removed),
        (0, 0, 0, 1),
        "the row-check stats the stored path: ENOENT on a checked folder is a delete — the mask \
         never hides it (no enumeration needed, the stat answers)"
    );
    let rows = indexer
        .store
        .files_under(std::slice::from_ref(&corpus_root))
        .expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "the dummy is NOT indexed — an unchanged pair is the gate's stated limitation, honest: \
         it enters at the next FULL crawl"
    );
    assert!(rows[0].path.ends_with("top.jpg"));

    // The full crawl owns the dummy: the next FULL sync sees it.
    let full = indexer
        .sync_scoped(
            std::slice::from_ref(&corpus),
            std::slice::from_ref(&corpus),
            &mut |_| {},
        )
        .expect("the full sync");
    assert_eq!(
        (full.added, full.changed, full.moved, full.removed),
        (1, 0, 0, 0),
        "the full crawl adds what the gate could not see"
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_rules_change_forces_a_full_sync_and_refills_the_snapshot() {
    let work = temp_folder("rules");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "top.jpg", b"not really a jpeg");
    write(&corpus, "excl/keep.jpg", b"not really a jpeg");
    let indexer = model_free_indexer(index.clone());
    let first = indexer
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the filling sync");
    assert_eq!(first.added, 2, "both photos index under the default rules");

    // New excludes on the SAME store: the snapshot rides the old rules — it is not
    // honest for the new ones. The sync runs FULL and refills.
    let options = IndexOptions {
        excluded_folders: vec![slash(&corpus.join("excl"))],
        ..IndexOptions::default()
    };
    let store = IndexStore::main(&index).expect("open the store");
    let nowhere = index.clone();
    let make = move || -> digup_runtime::Result<Box<dyn Embedder + 'static>> {
        Err(digup_runtime::Error::io(
            &nowhere,
            std::io::Error::other("no model here"),
        ))
    };
    let indexer2 = Indexer::new(store, options, Box::new(make)).expect("the reconfigured indexer");
    let second = indexer2
        .sync_gated(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the sync under new rules");
    assert_eq!(
        (second.added, second.changed, second.moved, second.removed),
        (0, 0, 0, 1),
        "the excluded folder's row prunes — the full crawl ran because the fingerprint moved"
    );
    assert!(
        !second
            .crawl
            .candidates
            .iter()
            .any(|c| c.path.contains("excl/keep.jpg")),
        "the excluded folder is skipped whole, as every crawl skips it"
    );
    let rows = indexer2.store.files_under(&[slash(&corpus)]).expect("rows");
    assert_eq!(rows.len(), 1, "only the root file's row stands");
    let _ = std::fs::remove_dir_all(&work);
}
