//! The faces privacy move (#195, S4): the faces' file rides the device-side home
//! `<faces-root>/<sha256(index path) hex[:16]>/face.sqlite`, NOT the index folder — an
//! in-tree `.digup` carries index + previews, never faces. The join law is untouched
//! (faces key on `(path, size, mtime)` of the store they belong to), so the device-side
//! folder key is the store's DEVICE-LOCAL identity: a store carried to another machine
//! computes its own home and the faces stay where they were computed.

use digup_core::face_store::FaceStore;
use digup_core::{Candidate, FileKind, IndexStore};
use digup_services::face_service::{faces_store_dir, move_legacy_faces};
use sha2::{Digest, Sha256};

fn cand(path: &str) -> Candidate {
    Candidate {
        path: path.to_string(),
        kind: FileKind::Image,
        size: 100,
        mtime: 1.0,
        inode: 7,
        device: 3,
    }
}

fn store_under(dir: &std::path::Path, root: &str, file: &str) -> IndexStore {
    let store = IndexStore::open(dir, digup_core::MAIN_FILE).unwrap();
    store.set_scope(Some(vec![root.to_string()])).unwrap();
    store.insert(&cand(file)).unwrap();
    store
}

/// The home law (#195): 16 hex of the sha256 over the INDEX FOLDER PATH — the store's
/// device-local identity. The anchor set is NOT a usable key, and the reason is measured
/// (2026-11-27, the app's live 173k store): the v6→v7 migration creates one anchor per
/// row cluster — dozens of deep folders — and every folder the user adds to the roots is
/// a new anchor, so an anchor-keyed home MOVES under the running app: the People tab
/// empties and 195k faces re-scan. The path key is stable across the migration and across
/// root changes, and it carries the privacy law by itself: the home lives under the faces
/// root, outside the `.digup`, so a store carried to another device gets a fresh home.
#[test]
fn the_faces_home_keys_the_index_path_and_survives_the_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let faces = tmp.path().join("faces");
    let a = tmp.path().join("idx-a");
    let b = tmp.path().join("idx-b");
    let store = store_under(&a, "/data", "/data/x.jpg");
    let dir_a = faces_store_dir(&a, &faces);
    let name = dir_a.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name.len(), 16, "16 hex of the path digest");
    assert!(
        name.chars().all(|ch| ch.is_ascii_hexdigit()),
        "the home name is hex: {name}"
    );
    let want: String = Sha256::digest(a.to_string_lossy().as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(faces.join(&want), dir_a, "the key is the index path");
    assert_eq!(faces_store_dir(&a, &faces), dir_a, "deterministic");
    // Anchors arrive (a root added — the migration shape, the set-scope shape): the home
    // STANDS. This is the pin the anchor key failed: the same store, more anchors, one home.
    store
        .set_scope(Some(vec!["/data".to_string(), "/elsewhere".to_string()]))
        .unwrap();
    assert_eq!(
        faces_store_dir(&a, &faces),
        dir_a,
        "anchors added — the home of a store never moves under its own app"
    );
    // The same roots in a DIFFERENT folder: a different home — the faces do not travel with
    // the folder, which is the privacy law (a carried `.digup` computes faces on its device).
    store_under(&b, "/data", "/data/x.jpg");
    assert_ne!(
        faces_store_dir(&b, &faces),
        dir_a,
        "the home keys the device path, not the root set"
    );
}

/// The store's own first home: an empty folder (no store file yet) hashes the same path —
/// the daemon opens a store and its faces file in the SAME home the empty folder named.
#[test]
fn an_empty_index_folder_and_the_store_it_holds_share_one_home() {
    let tmp = tempfile::tempdir().unwrap();
    let faces = tmp.path().join("faces");
    let index = tmp.path().join("idx");
    std::fs::create_dir_all(&index).unwrap();
    let want: String = Sha256::digest(index.to_string_lossy().as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        faces_store_dir(&index, &faces),
        faces.join(want),
        "no anchors — the folder path is the identity"
    );
}

/// The legacy move: a `face.sqlite` beside index.sqlite (the FACES.md §2 shape)
/// rides to the device home on first open; the index folder keeps NO faces file
/// (the privacy boundary). Never destructive: a file already at the new home
/// wins and the re-run is a no-op.
#[test]
fn a_legacy_face_file_moves_out_of_the_index_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let index = tmp.path().join("idx");
    std::fs::create_dir_all(&index).unwrap();
    drop(FaceStore::open(&index).unwrap()); // creates face.sqlite beside the index
    let home = tmp.path().join("faces").join("cafe00000000beef");
    move_legacy_faces(&index, &home);
    assert!(
        home.join(digup_core::face_store::FACE_FILE).exists(),
        "the faces moved to the device home"
    );
    assert!(
        !index.join(digup_core::face_store::FACE_FILE).exists(),
        "the index folder carries no faces — the privacy boundary"
    );
    // The store opens on at the new home.
    drop(FaceStore::open(&home).unwrap());
    // Non-destructive: a file already at the new home wins; the old copy stands.
    std::fs::write(
        index.join(digup_core::face_store::FACE_FILE),
        b"legacy-again",
    )
    .unwrap();
    move_legacy_faces(&index, &home);
    assert_eq!(
        std::fs::read(index.join(digup_core::face_store::FACE_FILE)).unwrap(),
        b"legacy-again",
        "the existing new home wins — the move never overwrites"
    );
}
