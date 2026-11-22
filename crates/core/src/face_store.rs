//! The face store (FACES.md Phase 1 §2): `face.sqlite`, the faces' own database,
//! beside the main index — faces and persons never share a table with
//! EmbeddingGemma's rows, and the main index's schema v6 is byte-untouched by the
//! faces feature. The join to the files table is by `(path, size, mtime)` (the
//! FACES.md join key), resolved at query time like every other thin-client read;
//! a face row whose file fell out of scope is a ghost row and prunes with it.
//!
//! Vectors are the same layout as the index's: float16 little-endian, L2-normalized
//! (`vectors.rs`), scored as float32, brute force. `det_model`/`emb_model` carry
//! the model stamps — a model change means re-scan, never a silent mix of spaces.

use std::path::{Path, PathBuf};

use crate::db::{Database, Result, Sql};
use crate::vectors;

/// The face store's own layout version (its file, its lineage — FACES.md §2).
pub const FACE_SCHEMA_VERSION: i64 = 1;
/// The faces' file in the index folder, beside `index.sqlite`.
pub const FACE_FILE: &str = "face.sqlite";

/// The base layout. `faces` carries the scan facts and the embedding; `persons`
/// the online centroid; `pending` the clustering queue (a face is queued the
/// moment it is recorded, and leaves it when it joins a person, a person is
/// deleted, or a human decides an ambiguous merge).
pub fn create_sql() -> String {
    "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE persons(
    id INTEGER PRIMARY KEY,
    name TEXT,                        -- NULL = unnamed
    centroid BLOB NOT NULL,           -- dim × float16, L2-normalized mean of members
    count INTEGER NOT NULL DEFAULT 0,
    cover REAL NOT NULL DEFAULT 100,  -- tightest member margin (1 − cosine), 100 = none yet
    created_at REAL NOT NULL,
    named_at REAL
);
CREATE TABLE faces(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,               -- the file it was found in (join key: path+size+mtime)
    size INTEGER NOT NULL,
    mtime REAL NOT NULL,
    box TEXT NOT NULL,                -- JSON [x0,y0,x1,y1], source px
    kps TEXT NOT NULL,                -- JSON [[x,y]×5], source px
    score REAL NOT NULL,              -- detection score
    det_model TEXT NOT NULL,          -- detector stamp (re-scan when it changes)
    emb_model TEXT NOT NULL,          -- embedder stamp
    vec BLOB NOT NULL,                -- dim × float16, L2-normalized
    person INTEGER REFERENCES persons(id) ON DELETE SET NULL,
    scanned_at REAL NOT NULL
);
CREATE INDEX faces_path ON faces(path, size, mtime);
CREATE INDEX faces_person ON faces(person);
CREATE TABLE pending(
    face INTEGER PRIMARY KEY REFERENCES faces(id) ON DELETE CASCADE,
    ts REAL NOT NULL
);
PRAGMA user_version = 1;"
        .to_string()
}

/// One detected face as the scan hands it over: box, 5 landmarks, score, unit embedding.
#[derive(Debug, Clone)]
pub struct FaceRecord {
    pub box_: [f32; 4],
    pub kps: [[f32; 2]; 5],
    pub score: f32,
    pub vector: Vec<f32>,
}

/// One file's scan facts: the join key (`path + size + mtime`, never crawl rowids)
/// and the model stamps — a model change means re-scan, never a silent mix.
#[derive(Debug, Clone, Copy)]
pub struct ScanOf<'a> {
    pub path: &'a str,
    pub size: i64,
    pub mtime: f64,
    pub det_model: &'a str,
    pub emb_model: &'a str,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FaceCounts {
    pub faces: i64,
    pub assigned: i64,
    pub persons: i64,
    pub named: i64,
    pub pending: i64,
}

#[derive(Debug, Clone)]
pub struct PersonRow {
    pub id: i64,
    pub name: Option<String>,
    pub centroid: Vec<f32>,
    pub count: i64,
    pub cover: f64,
}

#[derive(Debug, Clone)]
pub struct FaceRow {
    pub id: i64,
    pub path: String,
    pub size: i64,
    pub mtime: f64,
    pub box_: [f32; 4],
    pub kps: [[f32; 2]; 5],
    pub score: f32,
    pub person: Option<i64>,
}

fn json_box(b: &[f32; 4]) -> String {
    format!("[{}, {}, {}, {}]", b[0], b[1], b[2], b[3])
}

fn json_kps(k: &[[f32; 2]; 5]) -> String {
    let parts: Vec<String> = k.iter().map(|p| format!("[{}, {}]", p[0], p[1])).collect();
    format!("[{}]", parts.join(", "))
}

fn parse_box(s: &str) -> [f32; 4] {
    let nums: Vec<f32> = s
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    [
        nums.first().copied().unwrap_or(0.0),
        nums.get(1).copied().unwrap_or(0.0),
        nums.get(2).copied().unwrap_or(0.0),
        nums.get(3).copied().unwrap_or(0.0),
    ]
}

fn parse_kps(s: &str) -> [[f32; 2]; 5] {
    let inner = s.trim().trim_start_matches('[').trim_end_matches(']');
    let mut out = [[0.0f32; 2]; 5];
    let mut i = 0;
    for part in inner.split(']') {
        let nums: Vec<f32> = part
            .trim_start_matches(['[', ','])
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect();
        if nums.len() >= 2 && i < 5 {
            out[i] = [nums[0], nums[1]];
            i += 1;
        }
    }
    out
}

pub struct FaceStore {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub db: Database,
}

impl FaceStore {
    pub fn open(directory: &Path) -> Result<Self> {
        std::fs::create_dir_all(directory).map_err(|e| crate::db::Error::Failure(e.to_string()))?;
        let file = directory.join(FACE_FILE);
        let db = Database::open(&file)?;
        let store = Self {
            directory: directory.to_path_buf(),
            file,
            db,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version = self.db.user_version()?;
        if version > FACE_SCHEMA_VERSION {
            return Err(crate::db::Error::Failure(format!(
                "this face store was made by a newer DigUp (schema {version})"
            )));
        }
        if version == FACE_SCHEMA_VERSION {
            return Ok(());
        }
        self.db
            .set_busy_timeout_ms(crate::db::MIGRATION_BUSY_TIMEOUT_MS)?;
        self.db.transaction(|| {
            self.db.execute(&create_sql())?;
            self.db.set_user_version(FACE_SCHEMA_VERSION)
        })?;
        self.db.set_busy_timeout_ms(crate::db::BUSY_TIMEOUT_MS)?;
        Ok(())
    }

    // MARK: meta (model stamps, consent stamps)

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let mut out = None;
        self.db.query(
            "SELECT value FROM meta WHERE key = ?1",
            [Sql::Text(key.to_string())],
            |r| {
                out = r.text(0);
                Ok(())
            },
        )?;
        Ok(out)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.db.run(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [Sql::Text(key.to_string()), Sql::Text(value.to_string())],
        )
    }

    // MARK: recording scans

    /// A completed scan of one file: replace its faces wholesale (a re-scan of a
    /// file re-places its rows — the same file never contributes twice), queue the
    /// new faces for clustering, and return how many were recorded.
    pub fn record_scan(&self, file: ScanOf, faces: &[FaceRecord], now: f64) -> Result<usize> {
        self.db.transaction(|| {
            self.delete_file_rows(file.path)?;
            for f in faces {
                self.db.run(
                    "INSERT INTO faces(path, size, mtime, box, kps, score, det_model, emb_model, vec, scanned_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    [
                        Sql::Text(file.path.to_string()),
                        Sql::Int(file.size),
                        Sql::Double(file.mtime),
                        Sql::Text(json_box(&f.box_)),
                        Sql::Text(json_kps(&f.kps)),
                        Sql::Double(f.score as f64),
                        Sql::Text(file.det_model.to_string()),
                        Sql::Text(file.emb_model.to_string()),
                        Sql::Blob(vectors::half_bytes(&f.vector)),
                        Sql::Double(now),
                    ],
                )?;
                let id = self.db.last_insert_rowid();
                self.db.run(
                    "INSERT INTO pending(face, ts) VALUES (?1, ?2)",
                    [Sql::Int(id), Sql::Double(now)],
                )?;
            }
            Ok(faces.len())
        })
    }

    /// The file is gone (or re-scanned): its faces and the person membership they
    /// carried leave with it; counts drop with them.
    fn delete_file_rows(&self, path: &str) -> Result<()> {
        self.db.run(
            "DELETE FROM faces WHERE path = ?1",
            [Sql::Text(path.to_string())],
        )?;
        self.recount_persons()
    }

    /// Ghost-row cascade (FACES.md §2): faces whose file fell out of the index's
    /// scope (no `files` row answers for the path) are deleted. `scope_sql` is
    /// the searcher's scope predicate over `files` — the faces store mirrors the
    /// main index's root law, it does not invent its own.
    pub fn prune_outside(&self, existing_paths: &dyn Fn(&str) -> bool) -> Result<usize> {
        let mut rows: Vec<(i64, String)> = Vec::new();
        self.db.query("SELECT id, path FROM faces", [], |r| {
            let id = r.int(0);
            if let Some(p) = r.text(1) {
                rows.push((id, p));
            }
            Ok(())
        })?;
        let doomed: Vec<i64> = rows
            .into_iter()
            .filter(|(_, p)| !existing_paths(p))
            .map(|(id, _)| id)
            .collect();
        if doomed.is_empty() {
            return Ok(0);
        }
        self.db.transaction(|| {
            let ids: Vec<String> = doomed.iter().map(|i| i.to_string()).collect();
            self.db.run(
                &format!("DELETE FROM faces WHERE id IN ({})", ids.join(",")),
                [],
            )?;
            self.recount_persons()
        })?;
        Ok(doomed.len())
    }

    /// Piggyback freshness (FACES.md §3): the file is scanned when a face row
    /// carries its exact `(path, size, mtime)` facts — the join key, never a
    /// rowid — and no face of it is still queued (a queued face means the
    /// clustering has work on it, not the scanner).
    pub fn is_fresh(&self, path: &str, size: i64, mtime: f64) -> Result<bool> {
        let mut rows = 0i64;
        let mut queued = 0i64;
        self.db.query(
            "SELECT (SELECT COUNT(*) FROM faces WHERE path = ?1 AND size = ?2 AND mtime = ?3),
                    (SELECT COUNT(*) FROM pending p JOIN faces f ON f.id = p.face
                     WHERE f.path = ?1 AND f.size = ?2 AND f.mtime = ?3)",
            [
                Sql::Text(path.to_string()),
                Sql::Int(size),
                Sql::Double(mtime),
            ],
            |r| {
                rows = r.int(0);
                queued = r.int(1);
                Ok(())
            },
        )?;
        Ok(rows > 0 && queued == 0)
    }

    /// One face row by id (the `similar`/`pending` verbs).
    pub fn face_rows_by_id(&self, id: i64) -> Result<Vec<FaceRow>> {
        self.face_rows("WHERE id = ?1", [Sql::Int(id)])
    }

    /// A face's stored unit vector (fp16 round trip, the store's own numbers).
    pub fn face_vector(&self, id: i64) -> Result<Option<Vec<f32>>> {
        let mut out = None;
        self.db
            .query("SELECT vec FROM faces WHERE id = ?1", [Sql::Int(id)], |r| {
                if let Some(blob) = r.blob(0) {
                    out = Some(vectors::floats_from_halves(&blob));
                }
                Ok(())
            })?;
        Ok(out)
    }

    // MARK: clustering

    /// The pending queue, oldest first, capped.
    pub fn pending_faces(&self, limit: usize) -> Result<Vec<(i64, Vec<f32>)>> {
        let mut out = Vec::new();
        self.db.query(
            "SELECT f.id, f.vec FROM pending p JOIN faces f ON f.id = p.face
             ORDER BY p.ts, f.id LIMIT ?1",
            [Sql::Int(limit as i64)],
            |r| {
                let id = r.int(0);
                if let Some(blob) = r.blob(1) {
                    out.push((id, vectors::floats_from_halves(&blob)));
                }
                Ok(())
            },
        )?;
        Ok(out)
    }

    /// The persons as the clustering sees them: unit centroid + member count.
    pub fn centroids(&self) -> Result<Vec<crate::face_cluster::Centroid>> {
        let mut out = Vec::new();
        self.db.query(
            "SELECT centroid, count, cover FROM persons ORDER BY id",
            [],
            |r| {
                let blob = r.blob(0).unwrap_or_default();
                out.push(crate::face_cluster::Centroid {
                    vec: vectors::floats_from_halves(&blob),
                    count: r.int(1) as usize,
                    cover: r.double(2) as f32,
                });
                Ok(())
            },
        )?;
        Ok(out)
    }

    /// One clustering pass over the pending queue (FACES.md §2: online centroid,
    /// collision guard). A face that reaches no person opens a new one; an
    /// ambiguous face stays queued for `pending-clusters` to show. Assignments,
    /// centroids, counts and covers persist in one transaction. Returns the faces
    /// that left the queue.
    pub fn run_clustering(
        &self,
        merge_threshold: f32,
        collision_margin: f32,
        limit: usize,
    ) -> Result<usize> {
        let queue: Vec<(usize, Vec<f32>)> = self
            .pending_faces(limit)?
            .into_iter()
            .map(|(id, v)| (id as usize, v))
            .collect();
        if queue.is_empty() {
            return Ok(0);
        }
        let mut centroids = self.centroids()?;
        let res =
            crate::face_cluster::cluster(&queue, &mut centroids, merge_threshold, collision_margin);
        let mut done = 0usize;
        self.db.transaction(|| {
            let ts = now();
            for ((face, _vector), assignment) in queue.iter().zip(res.iter().map(|(_, a)| a)) {
                let face = *face as i64;
                match assignment {
                    crate::face_cluster::Assignment::Merge(p) => {
                        // persons are 1-based rows; the clustering indexed 0-based.
                        let c = &centroids[*p];
                        self.db.run(
                            "UPDATE persons SET centroid = ?1, count = ?2, cover = ?3 WHERE id = ?4",
                            [
                                Sql::Blob(vectors::half_bytes(&c.vec)),
                                Sql::Int(c.count as i64),
                                Sql::Double(if c.cover == f32::MAX {
                                    100.0
                                } else {
                                    c.cover as f64
                                }),
                                Sql::Int(*p as i64 + 1),
                            ],
                        )?;
                        self.db.run(
                            "UPDATE faces SET person = ?1 WHERE id = ?2",
                            [Sql::Int(*p as i64 + 1), Sql::Int(face)],
                        )?;
                        self.db.run("DELETE FROM pending WHERE face = ?1", [Sql::Int(face)])?;
                        done += 1;
                    }
                    crate::face_cluster::Assignment::New(i) => {
                        // Reaches no person: opens its own, centroid = itself.
                        // Existing persons are rows 1..=m; this pass appends
                        // m+1, m+2… in index order, so the row id is i + 1.
                        let c = &centroids[*i];
                        self.db.run(
                            "INSERT INTO persons(id, centroid, count, cover, created_at) VALUES (?1, ?2, 1, 100, ?3)",
                            [Sql::Int(*i as i64 + 1), Sql::Blob(vectors::half_bytes(&c.vec)), Sql::Double(ts)],
                        )?;
                        self.db.run(
                            "UPDATE faces SET person = ?1 WHERE id = ?2",
                            [Sql::Int(*i as i64 + 1), Sql::Int(face)],
                        )?;
                        self.db.run("DELETE FROM pending WHERE face = ?1", [Sql::Int(face)])?;
                        done += 1;
                    }
                    crate::face_cluster::Assignment::Deferred => {
                        // The collision guard's decision: the human decides
                        // (pending-clusters → confirm-person), the engine does not.
                    }
                }
            }
            Ok(done)
        })
    }

    // MARK: persons

    pub fn list_persons(&self) -> Result<Vec<PersonRow>> {
        let mut out = Vec::new();
        self.db.query(
            "SELECT id, name, centroid, count, cover FROM persons ORDER BY id",
            [],
            |r| {
                let blob = r.blob(2).unwrap_or_default();
                out.push(PersonRow {
                    id: r.int(0),
                    name: r.text(1),
                    centroid: vectors::floats_from_halves(&blob),
                    count: r.int(3),
                    cover: r.double(4),
                });
                Ok(())
            },
        )?;
        Ok(out)
    }

    pub fn name_person(&self, id: i64, name: &str, now: f64) -> Result<()> {
        self.db.run(
            "UPDATE persons SET name = ?1, named_at = ?2 WHERE id = ?3",
            [Sql::Text(name.to_string()), Sql::Double(now), Sql::Int(id)],
        )
    }

    /// `merge-persons`: `b`'s faces move to `a`, `a`'s centroid recomputes as the
    /// normalized weighted mean, its cover is the min of both, `b` is gone.
    pub fn merge_persons(&self, a: i64, b: i64) -> Result<()> {
        self.db.transaction(|| {
            let (va, na) = self.person_vector(a)?;
            let (vb, nb) = self.person_vector(b)?;
            if let (Some(va), Some(na), Some(vb), Some(nb)) = (va, na, vb, nb) {
                let nsum = (na + nb).max(1) as f32;
                let merged: Vec<f32> = va
                    .iter()
                    .zip(&vb)
                    .map(|(x, y)| (x * na as f32 + y * nb as f32) / nsum)
                    .collect();
                let norm = merged.iter().map(|x| x * x).sum::<f32>().sqrt();
                let merged: Vec<f32> = if norm > 0.0 {
                    merged.iter().map(|x| x / norm).collect()
                } else {
                    merged
                };
                self.db.run(
                    "UPDATE persons SET centroid = ?1, count = ?2 WHERE id = ?3",
                    [
                        Sql::Blob(vectors::half_bytes(&merged)),
                        Sql::Int(na + nb),
                        Sql::Int(a),
                    ],
                )?;
            }
            self.db.run(
                "UPDATE faces SET person = ?1 WHERE person = ?2",
                [Sql::Int(a), Sql::Int(b)],
            )?;
            self.db
                .run("DELETE FROM persons WHERE id = ?1", [Sql::Int(b)])
        })
    }

    /// `delete-person`: the person is gone; its faces return to the pending
    /// queue (they are evidence — the engine keeps them, clustering re-decides).
    pub fn delete_person(&self, id: i64) -> Result<()> {
        self.db.transaction(|| {
            self.db.run(
                "INSERT INTO pending(face, ts) SELECT id, ?1 FROM faces WHERE person = ?2",
                [Sql::Double(now()), Sql::Int(id)],
            )?;
            self.db.run(
                "UPDATE faces SET person = NULL WHERE person = ?1",
                [Sql::Int(id)],
            )?;
            self.db
                .run("DELETE FROM persons WHERE id = ?1", [Sql::Int(id)])
        })
    }

    /// A human decides an ambiguous face (the collision guard's queue): it joins
    /// `person` and the centroid grows.
    pub fn confirm_face(&self, face: i64, person: i64, _now: f64) -> Result<()> {
        self.db.transaction(|| {
            let mut blob = None;
            self.db.query(
                "SELECT vec FROM faces WHERE id = ?1",
                [Sql::Int(face)],
                |r| {
                    blob = r.blob(0);
                    Ok(())
                },
            )?;
            let Some(blob) = blob else {
                return Err(crate::db::Error::Failure(format!("no face {face}")));
            };
            let v = vectors::floats_from_halves(&blob);
            let (Some(c), Some(n)) = self.person_vector(person)? else {
                return Err(crate::db::Error::Failure(format!("no person {person}")));
            };
            let nc = crate::face_cluster::centroid_merge(&c, n as usize, &v).unwrap_or(c);
            self.db.run(
                "UPDATE persons SET centroid = ?1, count = ?2 WHERE id = ?3",
                [
                    Sql::Blob(vectors::half_bytes(&nc)),
                    Sql::Int(n + 1),
                    Sql::Int(person),
                ],
            )?;
            self.db.run(
                "UPDATE faces SET person = ?1 WHERE id = ?2",
                [Sql::Int(person), Sql::Int(face)],
            )?;
            self.db
                .run("DELETE FROM pending WHERE face = ?1", [Sql::Int(face)])
        })
    }

    fn person_vector(&self, id: i64) -> Result<(Option<Vec<f32>>, Option<i64>)> {
        let mut v = None;
        let mut n = None;
        self.db.query(
            "SELECT centroid, count FROM persons WHERE id = ?1",
            [Sql::Int(id)],
            |r| {
                v = r.blob(0).map(|b| vectors::floats_from_halves(&b));
                n = Some(r.int(1));
                Ok(())
            },
        )?;
        Ok((v, n))
    }

    // MARK: faces and photos

    /// The faces of one photo (thin-client crop coordinates live server-side;
    /// the engine answers with the box/kps it recorded).
    pub fn faces_of(&self, path: &str) -> Result<Vec<FaceRow>> {
        self.face_rows(
            "WHERE path = ?1 ORDER BY score DESC",
            [Sql::Text(path.to_string())],
        )
    }

    /// A person's photos: one row per face, score-desc, newest scans first.
    pub fn person_faces(&self, person: i64) -> Result<Vec<FaceRow>> {
        self.face_rows(
            "WHERE person = ?1 ORDER BY scanned_at DESC, score DESC",
            [Sql::Int(person)],
        )
    }

    /// `find-similar-faces`: brute-force cosine over the store (FACES.md §2 —
    /// brute force, no ANN, same law as the main index).
    pub fn find_similar(&self, vector: &[f32], limit: usize) -> Result<Vec<(FaceRow, f64)>> {
        let mut out = Vec::new();
        self.db.query(
            "SELECT id, path, size, mtime, box, kps, score, person, vec FROM faces",
            [],
            |r| {
                let blob = r.blob(8).unwrap_or_default();
                let v = vectors::floats_from_halves(&blob);
                let cos = crate::face_cluster::cosine(vector, &v) as f64;
                out.push((
                    FaceRow {
                        id: r.int(0),
                        path: r.text(1).unwrap_or_default(),
                        size: r.int(2),
                        mtime: r.double(3),
                        box_: parse_box(&r.text(4).unwrap_or_default()),
                        kps: parse_kps(&r.text(5).unwrap_or_default()),
                        score: r.double(6) as f32,
                        person: if r.is_null(7) { None } else { Some(r.int(7)) },
                    },
                    cos,
                ));
                Ok(())
            },
        )?;
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        out.truncate(limit);
        Ok(out)
    }

    fn face_rows(&self, tail: &str, values: impl IntoIterator<Item = Sql>) -> Result<Vec<FaceRow>> {
        let mut out = Vec::new();
        self.db.query(
            &format!("SELECT id, path, size, mtime, box, kps, score, person FROM faces {tail}"),
            values,
            |r| {
                out.push(FaceRow {
                    id: r.int(0),
                    path: r.text(1).unwrap_or_default(),
                    size: r.int(2),
                    mtime: r.double(3),
                    box_: parse_box(&r.text(4).unwrap_or_default()),
                    kps: parse_kps(&r.text(5).unwrap_or_default()),
                    score: r.double(6) as f32,
                    person: if r.is_null(7) { None } else { Some(r.int(7)) },
                });
                Ok(())
            },
        )?;
        Ok(out)
    }

    // MARK: counts

    pub fn counts(&self) -> Result<FaceCounts> {
        let mut c = FaceCounts::default();
        self.db.query(
            "SELECT (SELECT COUNT(*) FROM faces),
                    (SELECT COUNT(*) FROM faces WHERE person IS NOT NULL),
                    (SELECT COUNT(*) FROM persons),
                    (SELECT COUNT(*) FROM persons WHERE name IS NOT NULL),
                    (SELECT COUNT(*) FROM pending)",
            [],
            |r| {
                c.faces = r.int(0);
                c.assigned = r.int(1);
                c.persons = r.int(2);
                c.named = r.int(3);
                c.pending = r.int(4);
                Ok(())
            },
        )?;
        Ok(c)
    }

    /// The count repair after memberships dropped (faces deleted, persons merged):
    /// counts follow the live rows, never a drift.
    fn recount_persons(&self) -> Result<()> {
        self.db.run(
            "UPDATE persons SET count = (SELECT COUNT(*) FROM faces WHERE faces.person = persons.id)",
            [],
        )?;
        self.db.run(
            "DELETE FROM persons WHERE (SELECT COUNT(*) FROM faces WHERE faces.person = persons.id) = 0",
            [],
        )
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
