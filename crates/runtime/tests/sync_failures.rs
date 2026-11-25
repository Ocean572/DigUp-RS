//! Issue #2 — the sync must not delete rows for a folder the crawl could not read.
//!
//! The ⑨ harness shape at sync scale, model-free (the `crawl_line_matches` pattern: the
//! embedder factory never runs — `sync` only crawls and reconciles rows). The transitions,
//! pinned in order:
//!
//! 1. a fresh tree indexes (two rows);
//! 2. the subfolder goes unreadable (mode 000): the walk reports the failure, the row SURVIVES;
//! 3. access returns: the row is UNCHANGED — not re-added, the embedding is not paid twice;
//! 4. a real delete (file unlinked, folder readable): the row IS pruned — the fix keeps
//!    failure-blindness, not the deletes;
//! 5. the whole root goes unreadable (it still exists): every row of the root SURVIVES.
//!
//! Unix-only (the mode-000 fixtures; the suite measures the Linux engine box, as ⑨ is).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use digup_core::index_store::IndexStore;
use digup_core::options::IndexOptions;
use digup_runtime::indexer::Indexer;
use digup_runtime::worker::Embedder;

fn temp_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-sync-failures-{name}"));
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

fn seal(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("set the mode");
}

/// The model-free indexer (the `crawl_line_matches` factory: `sync` never asks for a model).
/// Takes the index folder BY VALUE: a `&Path` argument would seed an elided `'1` that the
/// trait-object chain in `Indexer::new` inherits, and `Box::new` demands `'static`.
fn model_free_indexer(index: PathBuf) -> Indexer {
    let store = IndexStore::main(&index).expect("open the store");
    let nowhere = index;
    // `+ 'static` spelled out: in a fn with reference arguments the elided trait-object
    // lifetime inherits that argument's `'1`, and `Box::new` demands `'static`.
    let make = move || -> digup_runtime::Result<Box<dyn Embedder + 'static>> {
        Err(digup_runtime::Error::io(
            &nowhere,
            std::io::Error::other("no model here"),
        ))
    };
    Indexer::new(store, IndexOptions::default(), Box::new(make)).expect("the indexer")
}

#[test]
fn rows_survive_an_unreadable_folder_and_a_real_delete_still_prunes() {
    let work = temp_folder("sweep");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "sub/photo.jpg", b"not really a jpeg");
    write(&corpus, "top.txt", &b"hello"[..]);
    let root = corpus.to_string_lossy().into_owned();
    let indexer = model_free_indexer(index.clone());
    let rows = |indexer: &Indexer| -> Vec<String> {
        indexer
            .store
            .files_under(std::slice::from_ref(&root))
            .expect("the rows")
            .into_iter()
            .map(|row| row.path)
            .collect::<Vec<_>>()
    };
    let sync = |indexer: &Indexer| {
        indexer
            .sync(std::slice::from_ref(&corpus), &mut |_| {})
            .expect("sync")
    };

    // 1 — the fresh tree indexes.
    let first = sync(&indexer);
    assert_eq!(first.added, 2, "both files enter the index");
    assert_eq!(rows(&indexer).len(), 2);

    // 2 — the subfolder goes unreadable; photo.jpg is on disk the whole time.
    seal(&corpus.join("sub"), 0o000);
    let second = sync(&indexer);
    seal(&corpus.join("sub"), 0o755);
    assert_eq!(
        (second.added, second.changed, second.moved, second.removed),
        (0, 0, 0, 0),
        "the sync must take the walk's failure as its own blindness, not as a delete"
    );
    assert_eq!(second.unchanged, 1);
    assert_eq!(
        rows(&indexer).len(),
        2,
        "the row under the unreadable folder survives: the file is still there"
    );

    // 3 — access returns: the SAME row, unchanged — no re-add, no second embedding bill.
    let third = sync(&indexer);
    assert_eq!(
        (third.added, third.changed, third.moved, third.removed),
        (0, 0, 0, 0),
        "the restored folder's row is kept as it was"
    );
    assert_eq!(third.unchanged, 2);

    // 4 — a real delete: file unlinked, folder readable — the row IS pruned.
    std::fs::remove_file(corpus.join("sub/photo.jpg")).expect("unlink the file");
    let fourth = sync(&indexer);
    assert_eq!(
        fourth.removed, 1,
        "a successful scan that lacks the file is a delete"
    );
    assert_eq!(rows(&indexer).len(), 1);

    // 5 — the whole root goes unreadable (it still exists): its rows survive.
    seal(&corpus, 0o000);
    let fifth = sync(&indexer);
    seal(&corpus, 0o755);
    assert_eq!(
        (fifth.added, fifth.changed, fifth.moved, fifth.removed),
        (0, 0, 0, 0),
        "an unlistable root is the crawl's blindness, not the root's emptiness"
    );
    // `unchanged` counts files the walk CHECKED and found as they were — an unlistable
    // root checked nothing, and its row survived the prune untouched.
    assert_eq!(fifth.unchanged, 0);
    assert_eq!(rows(&indexer).len(), 1);

    let _ = std::fs::remove_dir_all(&work);
}
