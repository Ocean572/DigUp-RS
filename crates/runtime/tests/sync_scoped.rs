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

#[test]
fn a_scope_the_chosen_folders_do_not_contain_is_answered_as_a_full_sync() {
    // The review's finding 2, engine side: the crawl and the prune run on exactly what the
    // verb names, so a scope above the chosen folders (`sync ["/"]`, `sync ["/home/secret"]`,
    // a helper written by anything that can reach its stdin) indexes trees the user never
    // chose and deletes rows for paths the sync has no business touching. It is answered as
    // the full sync: the roots, never less.
    let work = temp_folder("clamp");
    let corpus = work.join("corpus");
    let stray = work.join("stray");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "a/keep.jpg", b"not really a jpeg");
    write(&stray, "private.txt", b"never chosen");
    let indexer = model_free_indexer(index.clone());
    let in_corpus = |indexer: &Indexer| {
        indexer
            .store
            .files_under(&[slash(&corpus)])
            .expect("rows")
            .len()
    };
    let full = indexer
        .sync(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the full sync");
    assert_eq!(full.added, 1);
    assert_eq!(
        indexer
            .store
            .files_under(&[slash(&work)])
            .expect("rows")
            .len(),
        1,
        "the full sync never indexed the folder outside the chosen one"
    );
    // The out-of-roots scope, and the filesystem root itself: both answer the full sync.
    for escape in [work.clone(), PathBuf::from("/")] {
        let escaped = indexer
            .sync_scoped(std::slice::from_ref(&corpus), &[escape], &mut |_| {})
            .expect("the escaped scope");
        assert_eq!(
            (escaped.added, escaped.removed),
            (0, 0),
            "an out-of-roots scope is the whole set's job, and the whole set has nothing to do"
        );
        assert_eq!(in_corpus(&indexer), 1, "the chosen folder's rows stand");
        assert_eq!(
            indexer
                .store
                .files_under(&[slash(&work)])
                .expect("rows")
                .len(),
            1,
            "and nothing outside them entered"
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_scope_folder_keeps_the_full_crawls_dataset_and_project_verdict() {
    // The review's finding 3: the crawl treats what it walks as ITS roots, and the two
    // root-exempt rules key on that — a chosen root is never a dataset, a chosen root's own
    // files are not "inside a code project". A scope dir handed to `crawl` is therefore a
    // root: the recorder's frame dump and the media inside a code project the user chose —
    // folders the FULL crawl excludes — get indexed and embedded by the scoped sync (the
    // model is the expensive part) and deleted by the next full crawl: add, embed, prune,
    // repeat, every burst. The rules ride the folders the user chose.
    let work = temp_folder("rules");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "pile/clip_000000.mp4", b"not really a video");
    for i in 1..=300 {
        write(
            &corpus,
            &format!("pile/clip_{i:06}.mp4"),
            b"not really a video",
        );
    }
    // A code project inside the chosen folder, with media in it.
    std::fs::create_dir_all(corpus.join("repo/.git")).unwrap();
    write(&corpus, "repo/media/photo.jpg", b"not really a jpeg");
    write(&corpus, "own/photo.jpg", b"not really a jpeg");

    let indexer = model_free_indexer(index.clone());
    let full = indexer
        .sync(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the full sync");
    assert_eq!(
        full.added, 1,
        "the full crawl keeps the user's own photo and leaves out the dataset pile and the \
         project's media"
    );

    // The two churn shapes, scoped: the pile, and the project's media folder.
    let scoped = indexer
        .sync_scoped(
            std::slice::from_ref(&corpus),
            &[corpus.join("pile"), corpus.join("repo/media")],
            &mut |_| {},
        )
        .expect("the scoped sync");
    assert_eq!(
        (scoped.added, scoped.removed),
        (0, 0),
        "the scope dirs are treated as what they are: INSIDE the chosen folder, so the full \
         crawl's verdict rides and nothing is embedded to be pruned next time"
    );
    assert_eq!(
        indexer
            .store
            .files_under(&[slash(&corpus)])
            .expect("rows")
            .len(),
        1,
        "the dataset pile and the project's media stay out of the index"
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_scope_edge_is_a_folder_boundary_not_a_string_prefix() {
    // The reviewer verified the range scan's boundary (`bound` = the folder plus a trailing
    // slash, the upper bound `bound + char::MAX`) and asked for it PINNED, because the whole
    // no-over-prune argument rests on it: a scope of `/a/b` must never touch the rows of the
    // sibling `/a/bc`, which shares its string prefix.
    let work = temp_folder("boundary");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "b/one.jpg", b"not really a jpeg");
    write(&corpus, "bc/two.jpg", b"not really a jpeg");
    write(&corpus, "b/three.jpg", b"not really a jpeg");
    let indexer = model_free_indexer(index.clone());
    let full = indexer
        .sync(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the full sync");
    assert_eq!(full.added, 3);
    // A delete inside the SIBLING, with only `/a/b` scoped: the sibling's rows stand — the
    // scope's range ends at the component boundary.
    std::fs::remove_file(corpus.join("bc/two.jpg")).expect("unlink the sibling's file");
    let scoped = indexer
        .sync_scoped(
            std::slice::from_ref(&corpus),
            &[corpus.join("b")],
            &mut |_| {},
        )
        .expect("the scoped sync");
    assert_eq!(
        scoped.removed, 0,
        "the sibling's vanished row is not this scope's business — a full sync prunes it"
    );
    let rows = indexer.store.files_under(&[slash(&corpus)]).expect("rows");
    assert_eq!(
        rows.len(),
        3,
        "/a/bc's row survives a /a/b scope (the string-prefix leak would have deleted it)"
    );
    assert!(rows.iter().any(|row| row.path.ends_with("bc/two.jpg")));
    // And the scope's own delete prunes, exactly: the boundary cuts both ways.
    std::fs::remove_file(corpus.join("b/one.jpg")).expect("unlink the scoped file");
    let scoped = indexer
        .sync_scoped(
            std::slice::from_ref(&corpus),
            &[corpus.join("b")],
            &mut |_| {},
        )
        .expect("the scoped sync of the real delete");
    assert_eq!(scoped.removed, 1, "the scope's own delete prunes");
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn a_file_entering_the_scope_from_outside_repaths_its_row() {
    // The review's finding 7: the inode lookup rides the scope's rows, so a file that moves
    // INTO the scope from a folder the watcher never reported (notify adds watches lazily for
    // newly created folders; FSEvents coalesces on a Mac) has no row in scope: a second row
    // is inserted while the old path keeps standing inside the store's scope, so search
    // answers a path that no longer exists and the open fails until a full sync cleans it.
    // The schema indexes `(device, inode)` for exactly this lookup.
    let work = temp_folder("ghost");
    let corpus = work.join("corpus");
    let index = work.join("index");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::create_dir_all(&index).unwrap();
    write(&corpus, "seen/keep.jpg", b"not really a jpeg");
    write(&corpus, "dark/keep.jpg", b"not really a jpeg");
    let indexer = model_free_indexer(index.clone());
    let full = indexer
        .sync(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("the full sync");
    assert_eq!(full.added, 2);
    // A move from the unwatched folder into the scoped one: one burst, one side.
    std::fs::create_dir_all(corpus.join("lit")).unwrap();
    std::fs::rename(corpus.join("dark/keep.jpg"), corpus.join("lit/keep.jpg")).expect("the move");
    let scoped = indexer
        .sync_scoped(
            std::slice::from_ref(&corpus),
            &[corpus.join("lit")],
            &mut |_| {},
        )
        .expect("the scoped sync");
    assert_eq!(
        (scoped.added, scoped.moved, scoped.removed),
        (0, 1, 0),
        "the row rides the inode across the scope's edge: no second row, no embedding bill"
    );
    let rows = indexer.store.files_under(&[slash(&corpus)]).expect("rows");
    assert_eq!(rows.len(), 2, "no ghost row left behind");
    assert!(
        !rows.iter().any(|row| row.path.ends_with("dark/keep.jpg")),
        "the old path is gone from the index — search cannot answer a file that is not there"
    );
    assert!(rows.iter().any(|row| row.path.ends_with("lit/keep.jpg")));
    let _ = std::fs::remove_dir_all(&work);
}
