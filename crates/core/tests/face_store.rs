//! The face store gate (FACES.md Phase 1 §2): schema, scan recording, online
//! clustering with the collision guard, the person verbs' store semantics
//! (name/merge/delete/confirm), counts, and the ghost-row cascade. Runs on a
//! temp `face.sqlite` — no model, no GPU, pure store arithmetic.

use digup_core::face_store::{FaceRecord, FaceStore, ScanOf};

fn v(x: f32, y: f32) -> Vec<f32> {
    // 8-d unit vectors, enough to exercise the store's blob layout (fp16 round
    // trip is exact for these small integers-and-halves values).
    let n = (x * x + y * y).sqrt();
    let mut v = vec![x / n, y / n];
    v.resize(8, 0.0);
    v
}

fn rec(x: f32, y: f32) -> FaceRecord {
    FaceRecord {
        box_: [0.0, 0.0, 10.0, 10.0],
        kps: [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0], [9.0, 10.0]],
        score: 0.9,
        vector: v(x, y),
    }
}

fn tmp_store(tag: &str) -> FaceStore {
    let dir = std::env::temp_dir().join(format!("digup-face-gate-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    FaceStore::open(&dir).expect("face.sqlite opens and migrates")
}

fn scan_of(path: &str) -> ScanOf<'_> {
    ScanOf {
        path,
        size: 10,
        mtime: 100.0,
        det_model: "det",
        emb_model: "emb",
    }
}

#[test]
fn schema_lands_at_version_one_with_the_faces_layout() {
    let s = tmp_store("schema");
    assert_eq!(s.db.user_version().unwrap(), 1);
    // The faces' file is its own — the main index's name never appears here.
    assert!(s.file.to_str().unwrap().ends_with("face.sqlite"));
    let c = s.counts().unwrap();
    assert_eq!((c.faces, c.persons, c.pending), (0, 0, 0));
}

#[test]
fn scan_recording_queues_faces_and_replaces_by_file() {
    let s = tmp_store("record");
    let faces = vec![rec(1.0, 0.0), rec(0.0, 1.0)];
    let n = s.record_scan(scan_of("/p/a.jpg"), &faces, 1.0).unwrap();
    assert_eq!(n, 2);
    let c = s.counts().unwrap();
    assert_eq!((c.faces, c.pending), (2, 2));
    // A re-scan of the same file replaces wholesale — never double rows.
    let n = s
        .record_scan(scan_of("/p/a.jpg"), &faces[..1], 2.0)
        .unwrap();
    assert_eq!(n, 1);
    let c = s.counts().unwrap();
    assert_eq!((c.faces, c.pending), (1, 1));
}

#[test]
fn clustering_groups_faces_by_their_vectors() {
    let s = tmp_store("cluster");
    // Two people: (1,0)-ish and (0,1)-ish; one ambiguous face between them.
    let faces = vec![
        rec(1.0, 0.05),
        rec(1.0, -0.05),
        rec(0.05, 1.0),
        rec(0.7, 0.7),
    ];
    s.record_scan(scan_of("/p/a.jpg"), &faces, 1.0).unwrap();
    let done = s.run_clustering(0.5, 0.10, 100).unwrap();
    // The ambiguous face (0.7,0.7) sits ~0.707 from P0 and ~0.742 from P1 — above
    // threshold at both, inside the collision margin: the guard defers it.
    let c = s.counts().unwrap();
    assert_eq!(c.faces, 4);
    assert_eq!(c.persons, 2);
    assert_eq!(done, 3, "two opens and one merge leave the queue");
    assert_eq!(c.pending, 1, "the collision-deferred face stays queued");
    assert_eq!(c.assigned, 3);
    let persons = s.list_persons().unwrap();
    assert_eq!(persons.len(), 2);
    let counts: Vec<i64> = persons.iter().map(|p| p.count).collect();
    assert!(counts.contains(&2), "the pair merged: counts {counts:?}");
    // The deferred face answers in the pending queue, and confirm-person decides it.
    let q = s.pending_faces(10).unwrap();
    assert_eq!(q.len(), 1);
    s.confirm_face(q[0].0, persons[0].id, 2.0).unwrap();
    let c = s.counts().unwrap();
    assert_eq!((c.pending, c.assigned), (0, 4));
}

#[test]
fn pending_faces_carry_their_unit_vectors_back() {
    let s = tmp_store("pending");
    s.record_scan(scan_of("/p/a.jpg"), &[rec(3.0, 4.0)], 1.0)
        .unwrap();
    let q = s.pending_faces(10).unwrap();
    assert_eq!(q.len(), 1);
    let n: f32 = q[0].1.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((n - 1.0).abs() < 1e-3, "fp16 round trip of the unit vector");
}

#[test]
fn verbs_the_person_surface_needs_work_on_the_store() {
    let s = tmp_store("verbs");
    s.record_scan(
        scan_of("/p/a.jpg"),
        &[rec(1.0, 0.02), rec(1.0, -0.02), rec(0.0, 1.0)],
        1.0,
    )
    .unwrap();
    s.run_clustering(0.5, 0.10, 100).unwrap();
    let persons = s.list_persons().unwrap();
    let (a, b) = (persons[0].id, persons[1].id);

    // name-person
    s.name_person(a, "Angela", 2.0).unwrap();
    let persons = s.list_persons().unwrap();
    assert_eq!(
        persons.iter().find(|p| p.id == a).unwrap().name.as_deref(),
        Some("Angela")
    );
    assert_eq!(s.counts().unwrap().named, 1);

    // person-faces / faces-of answer the join the UI needs
    let pf = s.person_faces(a).unwrap();
    assert_eq!(pf.len(), 2);
    assert_eq!(pf[0].path, "/p/a.jpg");
    let fo = s.faces_of("/p/a.jpg").unwrap();
    assert_eq!(fo.len(), 3);

    // merge-persons: b moves into a, counts sum, b is gone
    s.merge_persons(a, b).unwrap();
    let persons = s.list_persons().unwrap();
    assert_eq!(persons.len(), 1);
    assert_eq!(persons[0].id, a);
    assert_eq!(persons[0].count, 3);
    assert_eq!(s.counts().unwrap().assigned, 3);

    // delete-person: faces return to the queue, the person is gone
    s.delete_person(a).unwrap();
    let c = s.counts().unwrap();
    assert_eq!((c.persons, c.assigned, c.pending), (0, 0, 3));

    // confirm-person: a queued face joins a named person
    let q = s.pending_faces(1).unwrap();
    s.run_clustering(0.5, 0.10, 1).unwrap();
    let persons = s.list_persons().unwrap();
    let p = persons[0].id;
    let rest: Vec<_> = s
        .pending_faces(10)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    for face in rest {
        s.confirm_face(face, p, 3.0).unwrap();
    }
    let c = s.counts().unwrap();
    assert_eq!((c.faces, c.assigned, c.pending), (3, 3, 0));
    let _ = q;
}

#[test]
fn find_similar_is_a_brute_force_cosine_ranking() {
    let s = tmp_store("similar");
    s.record_scan(
        scan_of("/p/a.jpg"),
        &[rec(1.0, 0.0), rec(0.5, 0.5), rec(0.0, 1.0)],
        1.0,
    )
    .unwrap();
    let hits = s.find_similar(&v(1.0, 0.1), 3).unwrap();
    assert_eq!(hits.len(), 3);
    assert!(hits[0].1 > hits[1].1 && hits[1].1 > hits[2].1);
    assert!(hits[0].0.path == "/p/a.jpg");
}

#[test]
fn ghost_rows_prune_with_the_scope_the_main_index_keeps() {
    let s = tmp_store("prune");
    s.record_scan(scan_of("/p/in.jpg"), &[rec(1.0, 0.0)], 1.0)
        .unwrap();
    s.record_scan(scan_of("/p/out.jpg"), &[rec(0.0, 1.0)], 1.0)
        .unwrap();
    let removed = s.prune_outside(&|p: &str| p == "/p/in.jpg").unwrap();
    assert_eq!(removed, 1);
    let c = s.counts().unwrap();
    assert_eq!((c.faces, c.pending), (1, 1));
    // Person counts follow the live rows after a cascade (recount ran inside).
    s.run_clustering(0.5, 0.10, 100).unwrap();
    assert_eq!(s.counts().unwrap().persons, 1);
    assert_eq!(s.list_persons().unwrap()[0].count, 1);
}

#[test]
fn face_vectors_are_fp16_blobs_like_the_indexs() {
    let s = tmp_store("fp16");
    s.record_scan(scan_of("/p/a.jpg"), &[rec(3.0, 4.0)], 1.0)
        .unwrap();
    let mut blob_len = 0usize;
    s.db.query("SELECT length(vec) FROM faces LIMIT 1", [], |r| {
        blob_len = r.int(0) as usize;
        Ok(())
    })
    .unwrap();
    assert_eq!(blob_len, 8 * 2, "8 dims × float16");
    let face = &s.faces_of("/p/a.jpg").unwrap()[0];
    let v = v(3.0, 4.0);
    let q = s.pending_faces(10).unwrap();
    let stored = &q[0].1;
    for (a, b) in stored.iter().zip(&v) {
        assert!((a - b).abs() < 1e-3);
    }
    let _ = face;
}
