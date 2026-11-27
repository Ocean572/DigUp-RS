//! The faces privacy move (#195, S4): the faces' file rides the device-side home
//! `<faces-root>/<anchor-hash>/face.sqlite`, NOT the index folder — an in-tree
//! `.digup` carries index + previews, never faces. The join law is untouched
//! (faces key on `(path, size, mtime)` of the store they belong to), so the
//! device-side folder key IS the store's identity, spelled device-locally.

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

/// The anchor law: the home is 16 hex of the sha256 over the store's anchor
/// spellings — deterministic, per root set, and keyed by the ANCHORS, not the
/// index folder (the same roots in two folders share one home: the faces belong
/// to the store's identity, not to where the file sits on this device).
#[test]
fn the_faces_home_is_hashed_by_the_store_anchors() {
    let tmp = tempfile::tempdir().unwrap();
    let faces = tmp.path().join("faces");
    let a = tmp.path().join("idx-a");
    let b = tmp.path().join("idx-b");
    let c = tmp.path().join("idx-c");
    store_under(&a, "/data", "/data/x.jpg");
    let dir_a = faces_store_dir(&a, &faces);
    let name = dir_a.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name.len(), 16, "16 hex of the anchor digest");
    assert!(
        name.chars().all(|ch| ch.is_ascii_hexdigit()),
        "the home name is hex: {name}"
    );
    assert_eq!(faces_store_dir(&a, &faces), dir_a, "deterministic");
    // Same anchors, different folder: the same home (the key is the store's identity).
    store_under(&c, "/data", "/data/x.jpg");
    assert_eq!(
        faces_store_dir(&c, &faces),
        dir_a,
        "the anchors name the home, not the index folder"
    );
    // A different root set: a different home.
    store_under(&b, "/elsewhere", "/elsewhere/x.jpg");
    assert_ne!(
        faces_store_dir(&b, &faces),
        dir_a,
        "different anchors, different faces home"
    );
}

/// A store with NO anchors (the fresh-store shape, and the v6 shape where the
/// anchors table is absent): the index folder path is the device-side identity,
/// hashed the same 16-hex way.
#[test]
fn an_anchorless_store_hashes_the_index_path() {
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
