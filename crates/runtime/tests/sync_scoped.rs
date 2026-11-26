//! Issue #7a — the scoped sync: reconcile ONLY the folders the watcher named.
//!
//! The #2 suite's model-free shape (the `crawl_line_matches` pattern: the embedder factory
//! never runs — `sync` only crawls and reconciles rows). The transitions, pinned in order:
//!
//! 1. a full first sync indexes the whole tree;
//! 2. a scoped add enters the file under its dir and leaves the rows OUTSIDE the scope
//!    untouched — the crawl never saw them and the sync does not care;
//! 3. a scoped delete prunes exactly the rows under the scope;
//! 4. a folder deleted out from under a live root prunes its rows (the scope was fired by
//!    filesystem events, and the walk's failure at a GONE path is the delete, not blindness);
//! 5. a VANISHED chosen root (the ejected drive) keeps its rows — the root law outranks
//!    the scope, at every sync;
//! 6. a sealed folder inside the scope keeps its rows (the #2 law rides scoped too: the
//!    path EXISTS, so "not seen" is the walk's blindness);
//! 7. a move across two scoped dirs re-paths the row — no second embedding bill;
//! 8. `sync_scoped(roots, roots)` answers EXACTLY like `sync(roots)`: the full path is
//!    byte-for-byte the engine's original behavior.
//!
//! Unix-only (the mode-000 fixtures; the suite measures the Linux engine box, as #2's is).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use digup_core::index_store::IndexStore;
use digup_core::options::IndexOptions;
use digup_runtime::indexer::Indexer;
use digup_runtime::worker::Embedder;

fn temp_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-sync-scoped-{name}"));
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

#[test]
fn a_scoped_sync_reconciles_only_the_directories_the_watcher_named() {
    let work = temp_folder("sweep");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "a/one.txt", b"hello one");
    write(&corpus, "a/x.jpg", b"not really a jpeg");
    write(&corpus, "b/two.txt", b"hello two");
    write(&corpus, "deep/c/three.txt", b"hello three");

    let indexer = model_free_indexer(index.clone());
    let corpus_root = slash(&corpus);
    let rows = |indexer: &Indexer| -> Vec<String> {
        indexer
            .store
            .files_under(std::slice::from_ref(&corpus_root))
            .expect("the rows")
            .into_iter()
            .map(|row| row.path)
            .collect::<Vec<_>>()
    };
    let scoped = |indexer: &Indexer, dirs: &[PathBuf]| {
        indexer
            .sync_scoped(std::slice::from_ref(&corpus), dirs, &mut |_| {})
            .expect("the scoped sync")
    };
    let dir = |name: &str| corpus.join(name);

    // 1 — the whole tree indexes under the full sync.
    let first = indexer
        .sync(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the full sync");
    assert_eq!(first.added, 4, "all four files enter the index");
    assert_eq!(rows(&indexer).len(), 4);

    // 2 — a scoped add: the changed dir enters its file; the rows outside the scope stand
    // untouched although this crawl never walked a/, b/ … — they are a full sync's business.
    write(&corpus, "a/new.txt", b"hello new");
    let second = scoped(&indexer, &[dir("a")]);
    assert_eq!(
        (second.added, second.changed, second.moved, second.removed),
        (1, 0, 0, 0),
        "the scope adds the new file and prunes nothing outside it"
    );
    assert_eq!(
        second.unchanged, 2,
        "a/'s own files are checked as they were"
    );
    assert_eq!(
        rows(&indexer).len(),
        5,
        "b/ and deep/c/ stand untouched: not seen is not deleted OUTSIDE the scope"
    );

    // 3 — a scoped delete prunes exactly the rows under the scope.
    std::fs::remove_file(corpus.join("b/two.txt")).expect("unlink the file");
    let third = scoped(&indexer, &[dir("b")]);
    assert_eq!(
        third.removed, 1,
        "a successful scan of the scope that lacks the file is a delete"
    );
    assert_eq!(rows(&indexer).len(), 4);

    // 4 — a folder deleted out from under a live root: the scope fired by its remove events
    // walks a GONE path; the rows under it prune (the ejected-drive case is rule 5, above
    // this one at the root level).
    std::fs::remove_dir_all(corpus.join("deep")).expect("delete the folder");
    let fourth = scoped(&indexer, &[dir("deep")]);
    assert_eq!(
        fourth.removed, 1,
        "a deleted folder under a live root prunes its rows — the events named it"
    );
    assert_eq!(rows(&indexer).len(), 3);

    // 5 — the ejected drive: a VANISHED chosen root keeps its rows at every sync, scoped
    // included. The root rule speaks before the gone-path rule can.
    let corpus2 = work.join("drive");
    std::fs::create_dir_all(&corpus2).unwrap();
    write(&corpus2, "photos/keep.jpg", b"not really a jpeg");
    let second_root = indexer
        .sync(&[corpus.clone(), corpus2.clone()], &mut |_| {})
        .expect("the two-root sync");
    assert_eq!(second_root.added, 1, "the second root's file enters");
    std::fs::remove_dir_all(&corpus).expect("the first root goes away (a mount ejected)");
    let fifth = indexer
        .sync_scoped(
            &[corpus.clone(), corpus2.clone()],
            &[corpus.join("a")],
            &mut |_| {},
        )
        .expect("the scoped sync under a vanished root");
    assert_eq!(
        fifth.removed, 0,
        "an ejected drive's rows are not deletes: the files live elsewhere, the root is gone"
    );
    let drive_rows = indexer
        .store
        .files_under(&[slash(&corpus)])
        .expect("the rows");
    assert_eq!(
        drive_rows.len(),
        3,
        "the vanished root keeps its rows whole"
    );

    // 6 — a sealed folder INSIDE the scope: the path EXISTS, so the #2 law rides: the row
    // under it survives the scoped prune.
    write(&corpus2, "seal/photo.jpg", b"not really a jpeg");
    indexer
        .sync_scoped(
            &[corpus.clone(), corpus2.clone()],
            &[corpus2.join("seal")],
            &mut |_| {},
        )
        .expect("the seal-first sync");
    seal(&corpus2.join("seal"), 0o000);
    let sixth = indexer
        .sync_scoped(
            std::slice::from_ref(&corpus2),
            &[corpus2.join("seal")],
            &mut |_| {},
        )
        .expect("the scoped sync over the sealed folder");
    seal(&corpus2.join("seal"), 0o755);
    assert_eq!(
        sixth.removed, 0,
        "an unlistable folder is the walk's blindness, not an empty folder — scoped too"
    );
    let sealed_rows = indexer
        .store
        .files_under(&[slash(&corpus2.join("seal"))])
        .expect("the rows");
    assert_eq!(
        sealed_rows.len(),
        1,
        "the row under the sealed folder survives"
    );

    // 7 — a move across two scoped dirs re-paths the row: no delete, no re-add, no second
    // embedding bill (both sides rode the same burst, as the watcher fires both).
    std::fs::create_dir_all(corpus2.join("moved")).unwrap();
    std::fs::rename(
        corpus2.join("photos/keep.jpg"),
        corpus2.join("moved/keep.jpg"),
    )
    .expect("the move");
    let seventh = indexer
        .sync_scoped(
            std::slice::from_ref(&corpus2),
            &[corpus2.join("photos"), corpus2.join("moved")],
            &mut |_| {},
        )
        .expect("the move's scoped sync");
    assert_eq!(
        (
            seventh.added,
            seventh.changed,
            seventh.moved,
            seventh.removed
        ),
        (0, 0, 1, 0),
        "the row rides the inode to its new path"
    );

    // 8 — the full path rides the scoped entry point byte-for-byte: `sync_scoped(roots,
    // roots)` is the original full sync (the engine's pinned full behavior, untouched).
    let twin = temp_folder("twin");
    let twin_corpus = twin.join("corpus");
    let twin_index = twin.join("index");
    std::fs::create_dir_all(&twin_corpus).unwrap();
    std::fs::create_dir_all(&twin_index).unwrap();
    write(&twin_corpus, "photo.jpg", b"not really a jpeg");
    write(&twin_corpus, "notes.txt", b"hello");
    let pair = temp_folder("pair");
    let plain = model_free_indexer(pair.join("plain"));
    let via_scoped = model_free_indexer(pair.join("via"));
    let full = plain
        .sync(std::slice::from_ref(&twin_corpus), &mut |_| {})
        .expect("the plain full sync");
    let as_scoped = via_scoped
        .sync_scoped(
            std::slice::from_ref(&twin_corpus),
            std::slice::from_ref(&twin_corpus),
            &mut |_| {},
        )
        .expect("the same tree through the scoped entry point");
    assert_eq!(full.added, as_scoped.added);
    assert_eq!(full.changed, as_scoped.changed);
    assert_eq!(full.moved, as_scoped.moved);
    assert_eq!(full.removed, as_scoped.removed);
    assert_eq!(full.unchanged, as_scoped.unchanged);
    let full_rows = plain
        .store
        .files_under(&[slash(&twin_corpus)])
        .expect("rows");
    let scoped_rows = via_scoped
        .store
        .files_under(&[slash(&twin_corpus)])
        .expect("rows");
    assert_eq!(full_rows.len(), scoped_rows.len());

    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_dir_all(&twin);
    let _ = std::fs::remove_dir_all(&pair);
}
