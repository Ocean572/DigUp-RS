//! Face clustering (FACES.md Phase 1 §2): online centroid clustering with a
//! collision guard. Pure functions over unit vectors — no SQLite, no I/O, so the
//! thresholds and the guard are testable in isolation.
//!
//! The law of the store: a face joins a person when its cosine to the person's
//! centroid reaches `merge_threshold`; the *collision guard* holds the face back
//! when two persons are within `collision_margin` of each other at the same face
//! (the merge is ambiguous — a human decides, the engine does not guess). A
//! person's centroid is the L2-normalized mean of its members' vectors, kept
//! online: `centroid ← normalize(centroid·count + face)`.

/// Cosine of two vectors (both expected L2-normalized; the dot product *is* the
/// cosine then, and the guard normalizes the sum, so no norm is recomputed).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// `normalize(c·n + f)`: the online centroid update. Returns the new unit vector;
/// `None` when the sum is nil (c·n + f ≈ 0 — a face exactly cancelling the
/// centroid, which normalization cannot survive).
pub fn centroid_merge(centroid: &[f32], n: usize, face: &[f32]) -> Option<Vec<f32>> {
    let sum: Vec<f32> = centroid
        .iter()
        .zip(face)
        .map(|(c, f)| c * n as f32 + f)
        .collect();
    let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        Some(sum.iter().map(|x| x / norm).collect())
    } else {
        None
    }
}

/// Outcome of one face's clustering turn (FACES.md §2): it joins a person, it
/// opens its own (index into the centroid list the store appends in this pass),
/// or the collision guard defers it to human review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assignment {
    /// Joins person index (0-based into the centroid slice).
    Merge(usize),
    /// Reaches no person: opens its own at this centroid index (the face's own
    /// vector, count 1 — the slice grows as the pass goes).
    New(usize),
    /// Two persons collide at this face (within `collision_margin` of each
    /// other): ambiguous, the face stays queued for `pending-clusters`.
    Deferred,
}

/// A greedy online pass over `pending` (face vectors) against `centroids`
/// (person centroids with their member counts): each face sees the persons the
/// earlier faces opened, each merge updates the centroid it used, each opening
/// appends. Order is the contract — faces arrive in scan order.
pub fn cluster(
    pending: &[(usize, Vec<f32>)], // (face id, unit vector)
    centroids: &mut Vec<Centroid>, // persons, indexed by person id
    merge_threshold: f32,
    collision_margin: f32,
) -> Vec<(usize, Assignment)> {
    let mut out = Vec::with_capacity(pending.len());
    for (face, v) in pending {
        let mut best: Option<(f32, usize)> = None;
        let mut second = f32::MIN;
        for (pid, c) in centroids.iter().enumerate() {
            let cos = cosine(v, &c.vec);
            if cos >= merge_threshold {
                if best.map(|(b, _)| cos > b).unwrap_or(true) {
                    second = best.map(|(b, _)| b).unwrap_or(f32::MIN);
                    best = Some((cos, pid));
                } else {
                    second = cos;
                }
            }
        }
        match best {
            // The collision guard: two persons within `collision_margin` at this
            // face → ambiguous, the face stays pending.
            Some((b, _)) if second >= merge_threshold && b - second < collision_margin => {
                out.push((*face, Assignment::Deferred));
            }
            Some((_, pid)) => {
                let c = &mut centroids[pid];
                if let Some(nc) = centroid_merge(&c.vec, c.count, v) {
                    c.cover = c.cover.min(cosine(v, &nc));
                    c.vec = nc;
                    c.count += 1;
                }
                out.push((*face, Assignment::Merge(pid)));
            }
            None => {
                centroids.push(Centroid::new(v));
                out.push((*face, Assignment::New(centroids.len() - 1)));
            }
        }
    }
    out
}

/// A person cluster in the making: unit centroid, member count, and the
/// tightest margin measured at a member (`cover`, `f32::MAX` before the first
/// merge) — the margin guard's evidence.
#[derive(Debug, Clone)]
pub struct Centroid {
    pub vec: Vec<f32>,
    pub count: usize,
    pub cover: f32,
}

impl Centroid {
    pub fn new(first: &[f32]) -> Self {
        Self {
            vec: first.to_vec(),
            count: 1,
            cover: f32::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn identical_vectors_merge_into_one() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let mut centroids = vec![Centroid::new(&v)];
        let res = cluster(&[(7, v.clone())], &mut centroids, 0.45, 0.05);
        assert_eq!(res, vec![(7, Assignment::Merge(0))]);
        assert_eq!(centroids[0].count, 2);
        // Centroid stays the same unit vector.
        assert!(cosine(&centroids[0].vec, &v) > 0.9999);
    }

    #[test]
    fn distant_faces_open_a_new_person() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let w = unit(&[0.0, 1.0, 0.0]);
        let mut centroids = vec![Centroid::new(&v)];
        let res = cluster(&[(1, w.clone())], &mut centroids, 0.45, 0.05);
        assert_eq!(res, vec![(1, Assignment::New(1))]);
        assert_eq!(centroids.len(), 2);
        assert_eq!(centroids[0].count, 1);
    }

    #[test]
    fn collision_guard_defers_the_ambiguous_face() {
        // Two persons near-collinear (cos between them 0.9), face right between:
        // both ≥ threshold, top − second < margin → deferred.
        let a = unit(&[1.0, 0.0, 0.0]);
        let b = unit(&[0.9, 0.4, 0.0]);
        let f = unit(&[0.95, 0.2, 0.0]);
        let mut centroids = vec![Centroid::new(&a), Centroid::new(&b)];
        let res = cluster(&[(1, f)], &mut centroids, 0.45, 0.10);
        assert_eq!(res[0].1, Assignment::Deferred);
        assert_eq!(centroids[0].count, 1);
        assert_eq!(centroids[1].count, 1);
    }

    #[test]
    fn clear_winner_merges_despite_a_far_second() {
        let a = unit(&[1.0, 0.0, 0.0]);
        let b = unit(&[0.5, 0.87, 0.0]);
        let f = unit(&[0.99, 0.14, 0.0]);
        let mut centroids = vec![Centroid::new(&a), Centroid::new(&b)];
        let res = cluster(&[(1, f)], &mut centroids, 0.45, 0.10);
        assert_eq!(res[0].1, Assignment::Merge(0));
    }

    #[test]
    fn online_growth_sees_the_new_face_belong() {
        // Faces arrive in order: a, b, c where a≈b and c joins the GROWN centroid
        // — the online property the store relies on.
        let a = unit(&[1.0, 0.0, 0.0]);
        let b = unit(&[0.99, 0.14, 0.0]);
        let c = unit(&[0.93, 0.37, 0.0]);
        let mut centroids = Vec::new();
        let res = cluster(
            &[(1, a.clone()), (2, b), (3, c.clone())],
            &mut centroids,
            0.45,
            0.05,
        );
        assert_eq!(res[0].1, Assignment::New(0));
        assert_eq!(res[1].1, Assignment::Merge(0));
        assert_eq!(res[2].1, Assignment::Merge(0));
        assert_eq!(centroids[0].count, 3);
        assert!(cosine(&centroids[0].vec, &a) > 0.9);
        assert!(cosine(&c, &a) < 0.96); // c alone is clearly its own neighbourhood
    }

    #[test]
    fn a_second_person_opened_in_the_same_pass_is_visible_to_the_rest() {
        // Two clear people arriving in one pass: the second one's opening must
        // be visible to a face that comes after it.
        let a = unit(&[1.0, 0.0]);
        let b = unit(&[1.0, 0.1]);
        let w = unit(&[0.0, 1.0]);
        let w2 = unit(&[0.1, 1.0]);
        let mut centroids = Vec::new();
        let res = cluster(
            &[(1, a), (2, b), (3, w), (4, w2)],
            &mut centroids,
            0.45,
            0.10,
        );
        assert_eq!(res[0].1, Assignment::New(0));
        assert_eq!(res[1].1, Assignment::Merge(0));
        assert_eq!(res[2].1, Assignment::New(1));
        assert_eq!(res[3].1, Assignment::Merge(1));
        assert_eq!(centroids[1].count, 2);
    }

    #[test]
    fn centroid_update_is_the_normalized_mean() {
        let a = unit(&[1.0, 0.0]);
        let b = unit(&[0.0, 1.0]);
        let c = centroid_merge(&a, 1, &b).unwrap();
        let m = unit(&[0.5f32.sqrt(), 0.5f32.sqrt()]);
        for (x, y) in c.iter().zip(&m) {
            assert!((x - y).abs() < 1e-6);
        }
    }
}
