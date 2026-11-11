//! Scoring parity tests (`Searcher.swift:305-372`, `Math+Portable.swift`): the block-scored
//! matrix, the per-modality means, the z-scale and the strong-cut, pinned to the engine's rules.
//!
//! The fp16 *format* is pinned by `tests/vectors.rs` (byte-for-byte against a Swift-made index);
//! these pin the *arithmetic* on top: what a block of stored vectors scores, and what a query's
//! scale does with those scores.

use digup_core::model::Modality;
use digup_core::scoring::{
    meaning, modality_means, strong_count, VectorMatrix, BLOCK_ROWS, Z_GATE,
};
use digup_core::vectors::{floats_from_halves, half_bytes, DIMENSION};

/// Deterministic pseudo-random unit vectors, the way a real index's rows look: distinct,
/// normalized, no special values.
fn fake_rows(count: usize) -> Vec<Vec<f32>> {
    let mut seed = 0x2545_F491_u64;
    let mut rand = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as f32) / (1u32 << 31) as f32 - 0.5
    };
    (0..count)
        .map(|_| {
            let raw: Vec<f32> = (0..DIMENSION).map(|_| rand()).collect();
            let norm = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
            raw.iter().map(|x| x / norm).collect()
        })
        .collect()
}

#[test]
fn a_matrix_needs_rows_and_full_blobs() {
    assert!(
        VectorMatrix::new(0).is_none(),
        "the engine's rows > 0 guard"
    );
    let mut matrix = VectorMatrix::new(3).unwrap();
    assert_eq!(matrix.rows(), 3);
    let short = [0u8; DIMENSION]; // 768 bytes, not a vector
    assert!(!matrix.set_row(0, &short), "a short blob must not load");
    assert!(
        !matrix.set_row(3, &[0u8; DIMENSION * 2]),
        "past the last row"
    );
    let blank = [0u8; DIMENSION * 2];
    assert!(matrix.set_row(2, &blank));
    let zero: Vec<f32> = (0..DIMENSION)
        .map(|_| 1.0 / (DIMENSION as f32).sqrt())
        .collect();
    let scores = matrix.score(&zero);
    assert_eq!(scores.len(), 3);
    for score in scores {
        assert!(score.abs() < 1e-6, "zero rows score zero: {score}");
    }
    // A query of the wrong width scores nothing, and never panics.
    assert_eq!(matrix.score(&[0.0; 8]).len(), 3);
    assert!(matrix.score(&[0.0; 8]).iter().all(|s| *s == 0.0));
}

#[test]
fn scores_match_the_plain_dot_product_across_block_borders() {
    // 2 × 1024 + 452 rows: the middle and the tail block, so the block border is exercised.
    let rows = fake_rows(2 * BLOCK_ROWS + 452);
    let mut matrix = VectorMatrix::new(rows.len()).unwrap();
    for (index, row) in rows.iter().enumerate() {
        assert!(matrix.set_row(index, &half_bytes(row)));
    }
    let query: Vec<f32> = rows[7].iter().map(|x| x + 0.001).collect();
    let query = {
        let norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        query.iter().map(|x| x / norm).collect::<Vec<_>>()
    };
    let scores = matrix.score(&query);
    // The plain product on the decoded (f16 → f32) rows: what the engine's f32 dot computes,
    // to f64 precision. The lane arithmetic is f32, so a small tolerance is the contract.
    let decoded_query = floats_from_halves(&half_bytes(&query));
    let mut worst = 0.0f64;
    for (index, row) in rows.iter().enumerate() {
        let decoded = floats_from_halves(&half_bytes(row));
        let expected: f64 = decoded
            .iter()
            .zip(&decoded_query)
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum();
        worst = worst.max((f64::from(scores[index]) - expected).abs());
    }
    assert!(worst < 1e-4, "worst |Δ| {worst} over {} rows", rows.len());
    // A row scores its own vector highest, and the query's own row (7) sits at the top.
    let top = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap()
        .0;
    assert_eq!(top, 7);
}

#[test]
fn meaning_centers_each_modality_on_a_shared_deviation() {
    // Text sits higher than image for this query, as the engine's comment says it can.
    let scores: Vec<f32> = std::iter::empty()
        .chain((0..30).map(|i| 0.50 + 0.010 * (i as f32))) // 30 text rows, mean 0.645
        .chain((0..30).map(|i| 0.30 + 0.004 * (i as f32))) // 30 image rows, mean 0.358
        .collect();
    let modality: Vec<Modality> = std::iter::empty()
        .chain((0..30).map(|_| Modality::Text))
        .chain((0..30).map(|_| Modality::Image))
        .collect();
    let means = modality_means(&scores, &modality, None);
    assert!((means[1] - 0.645).abs() < 1e-5, "text mean {}", means[1]);
    assert!((means[2] - 0.358).abs() < 1e-5, "image mean {}", means[2]);

    let z = meaning(&scores, &modality, None);
    // The scale is one deviation over all the centered scores, not each modality's own: scaling
    // per modality would inflate the narrow image group (the engine measured 91 → 94 of 105
    // queries keeping one shared deviation, 2026-10-08).
    // Order within a modality is the model's own: the top text row beats the top image row.
    assert!(z[29] > z[59]);
    // The centered set's mean is zero, so z is (score − mean) / deviation with one deviation.
    let text_dev = (z[29] - z[0]) / (scores[29] - scores[0]) as f64;
    let image_dev = (z[59] - z[30]) / (scores[59] - scores[30]) as f64;
    assert!(
        (text_dev - image_dev).abs() < 1e-6,
        "one shared deviation: text 1/{text_dev} vs image 1/{image_dev}"
    );
}

#[test]
fn small_modalities_take_the_all_average() {
    // The rule from Searcher.swift:365: groups under 20 vectors use the average of all.
    let scores: Vec<f32> = (0..19).map(|i| 0.4 + 0.01 * (i as f32)).collect();
    let modality = vec![Modality::Audio; 19];
    let means = modality_means(&scores, &modality, None);
    let all: f64 = scores.iter().map(|s| f64::from(*s)).sum::<f64>() / 19.0;
    assert!((means[3] - all).abs() < 1e-9);

    let scores2: Vec<f32> = (0..20).map(|i| 0.4 + 0.01 * (i as f32)).collect();
    let modality2 = vec![Modality::Audio; 20];
    let means2 = modality_means(&scores2, &modality2, None);
    let own = scores2.iter().map(|s| f64::from(*s)).sum::<f64>() / 20.0;
    assert!((means2[3] - own).abs() < 1e-9, "20 is not small");
}

#[test]
fn sets_scale_rows_only_move_the_scale() {
    // A book among ordinary files: its 200 later pages score on the scale, stay out of it
    // (`setsScale`), so they don't drag the scale's center down for every query (the engine's
    // 2026-10-09 note: pages pulled the text average down, documents rose over screenshots).
    // The book's best page (0.90) must pass the 2.0σ gate; an ordinary page (0.50) must not.
    let scores: Vec<f32> = std::iter::empty()
        .chain((0..19).map(|_| 0.50)) // the set: 19 ordinary pages
        .chain(std::iter::once(0.90)) // the query's page — the standout
        .chain((0..200).map(|_| 0.10)) // later pages of a long book
        .collect();
    let modality = vec![Modality::Text; 220];
    let scale: Vec<bool> = (0..220).map(|row| row < 20).collect();

    let z_with = meaning(&scores, &modality, Some(&scale));
    // On the set-only scale: centered mean 0, deviation 0.115 (19 × 0.095² + 0.305² of spread),
    // so the standout sits at 0.305 / 0.115 ≈ 2.65σ and an ordinary page at −0.83σ.
    assert!(
        z_with[19] >= Z_GATE,
        "the book's best page: {} σ",
        z_with[19]
    );
    assert!(
        z_with[0] < Z_GATE,
        "an ordinary page must not pass: {}",
        z_with[0]
    );
    assert!(
        z_with[21] < Z_GATE,
        "an excluded page must not pass: {}",
        z_with[21]
    );
    let passing = z_with.iter().filter(|z| **z >= Z_GATE).count();
    assert_eq!(passing, 1, "one standout in the field");

    // With the pages IN the scale, the center collapses toward them and the field widens:
    // the same page now sits ≈ 6σ out, the pages themselves sit at the center. The scale is the
    // contract — which rows move it is the measured fix.
    let z_all = meaning(&scores, &modality, None);
    assert!(
        (z_all[19] - z_with[19]).abs() > 1.0,
        "the pages moved the scale: {} vs {}",
        z_all[19],
        z_with[19]
    );
    assert!(
        z_all[21].abs() < 1.0,
        "the pages become the center: {}",
        z_all[21]
    );
}

#[test]
fn the_deviation_has_the_twenty_hundredths_floor() {
    // One vector, repeated: deviation 0 would divide by zero; the engine floors it at 0.02 and
    // every centered score is 0, so every z is 0 — nothing stands out of an empty field.
    let scores = vec![0.5f32; 50];
    let modality = vec![Modality::Text; 50];
    let z = meaning(&scores, &modality, None);
    assert!(z.iter().all(|v| *v == 0.0));
    // A spread a hair above the floor is measured at the floor, not the tiny deviation.
    let scores2: Vec<f32> = (0..50).map(|i| 0.5 + 0.000_1 * (i as f32)).collect();
    let z2 = meaning(&scores2, &modality, None);
    let full_spread = z2[49] - z2[0];
    let raw_spread = (scores2[49] - scores2[0]) as f64;
    assert!(
        (full_spread - raw_spread / 0.02).abs() < 1e-6,
        "floor deviation 0.02: {full_spread} vs {}",
        raw_spread / 0.02
    );
}

#[test]
fn strong_count_cuts_at_half_the_top() {
    // The engine's number: 0.5 × the top; the rest folds under "weaker matches". The cut is
    // inclusive, as Swift's `prefix { score >= top * ratio }`: 4.5 counts at 9 × 0.5.
    let scores = vec![9.0, 5.0, 4.999, 4.5, 1.0];
    assert_eq!(strong_count(&scores, 0.5), 4);
    assert_eq!(strong_count(&scores, 0.6), 1);
    // A non-positive top keeps the whole list (the engine's guard).
    assert_eq!(strong_count(&[0.0, -1.0], 0.5), 2);
    assert_eq!(strong_count(&[], 0.5), 0);
}

/// The whole scoring path against **a real index the Swift engine wrote**: its stored blobs,
/// the reference `query` vector as the query, and the arithmetic checked against plain f64
/// products. `DIGUP_SWIFT_INDEX` is the index folder, `DIGUP_REFERENCE_DIR` the folder with
/// `vectors.json`; without either the test says so and skips, as the engine's suites do.
///
/// This is the contract check on the data that matters: hand-made rows pin the shape, the
/// engine's blobs pin the reality — 20k rows of real vectors score with the same ranking the
/// engine's Accelerate/Portable product gives, and the z-scale on them matches the spec
/// line for line.
#[test]
fn swift_blobs_score_and_scale_on_a_real_index() {
    let Some(folder) = std::env::var_os("DIGUP_SWIFT_INDEX") else {
        eprintln!("skipped: set DIGUP_SWIFT_INDEX to a folder with an index.sqlite built by digup");
        return;
    };
    let file = std::path::Path::new(&folder).join("index.sqlite");
    if !file.is_file() {
        eprintln!("skipped: no {} (DIGUP_SWIFT_INDEX)", file.display());
        return;
    }
    let reference_folder = std::env::var_os("DIGUP_REFERENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../Tests/LlamaRuntimeTests/Reference")
        });
    let vectors_file = reference_folder.join("vectors.json");
    if !vectors_file.is_file() {
        eprintln!(
            "skipped: no {} (DIGUP_REFERENCE_DIR)",
            vectors_file.display()
        );
        return;
    }

    #[derive(serde::Deserialize)]
    struct Reference {
        vectors: std::collections::HashMap<String, Vec<f32>>,
    }
    let reference: Reference =
        serde_json::from_str(&std::fs::read_to_string(&vectors_file).unwrap()).unwrap();
    let query = reference.vectors["query"].clone();
    assert_eq!(query.len(), DIMENSION);

    // Read-only: the fixture may be a live index, and an opening write would create WAL sidecars.
    let conn =
        rusqlite::Connection::open_with_flags(&file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let stored: Vec<(Vec<u8>, Modality)> = conn
        .prepare(
            "SELECT vec, modality FROM segments WHERE vec IS NOT NULL \
             ORDER BY id LIMIT 20000",
        )
        .unwrap()
        .query_map([], |row| {
            let blob: Vec<u8> = row.get(0)?;
            let kind: String = row.get(1)?;
            let modality = match kind.as_str() {
                "text" => Modality::Text,
                "image" => Modality::Image,
                "audio" => Modality::Audio,
                _ => Modality::None,
            };
            Ok((blob, modality))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!stored.is_empty(), "the fixture index has no vectors");

    let mut matrix = VectorMatrix::new(stored.len()).unwrap();
    for (index, (blob, _)) in stored.iter().enumerate() {
        assert!(matrix.set_row(index, blob), "row {index} is not a vector");
    }
    let scores = matrix.score(&query);

    // The plain f64 product on the decoded rows is what the engine's f32 dot computes; the lane
    // arithmetic (f32, block of 1024) is the only thing between them.
    let plain: Vec<f64> = stored
        .iter()
        .map(|(blob, _)| {
            floats_from_halves(blob)
                .iter()
                .zip(&query)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum()
        })
        .collect();
    let worst = (0..stored.len())
        .map(|index| (f64::from(scores[index]) - plain[index]).abs())
        .fold(0.0f64, f64::max);
    assert!(
        worst < 1e-4,
        "worst |Δ| {worst} over {} real Swift vectors",
        stored.len()
    );

    // The best match is the same row either way: ranking is what the search shows.
    let best_matrix = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap()
        .0;
    let best_plain = plain
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap()
        .0;
    assert_eq!(best_matrix, best_plain, "the top hit must be the same row");

    // The z-scale on real rows, checked against the spec transcribed here line for line.
    let modality: Vec<Modality> = stored.iter().map(|(_, m)| *m).collect();
    let z = meaning(&scores, &modality, None);
    let expected = spec_meaning(&scores, &modality);
    let worst_z = z
        .iter()
        .zip(&expected)
        .map(|(a, b): (&f64, &f64)| (a - b).abs())
        .fold(0.0f64, f64::max);
    assert!(worst_z < 1e-9, "worst z Δ {worst_z}");
    println!(
        "scored {} real Swift vectors (best row {best_matrix}), z-scale within {worst_z:.1e}",
        stored.len()
    );
}

/// The spec of `Searcher.meaning` written out plainly, for the cross-check to lean on.
fn spec_meaning(scores: &[f32], modality: &[Modality]) -> Vec<f64> {
    // Modality means, with the < 20 groups rule, over the all-rows scale.
    let cases = [
        Modality::None,
        Modality::Text,
        Modality::Image,
        Modality::Audio,
    ];
    let mut means = std::collections::HashMap::new();
    let all_mean: f64 = scores.iter().map(|s| f64::from(*s)).sum::<f64>() / scores.len() as f64;
    for case in cases {
        let rows: Vec<f64> = scores
            .iter()
            .zip(modality)
            .filter(|(_, m)| **m == case)
            .map(|(s, _)| f64::from(*s))
            .collect();
        if rows.len() >= 20 {
            means.insert(case, rows.iter().sum::<f64>() / rows.len() as f64);
        } else {
            means.insert(case, all_mean);
        }
    }
    let centered: Vec<f64> = scores
        .iter()
        .zip(modality)
        .map(|(s, m)| f64::from(*s) - means[m])
        .collect();
    let mean = centered.iter().sum::<f64>() / centered.len() as f64;
    let squares: f64 = centered.iter().map(|v| (v - mean) * (v - mean)).sum();
    let deviation = (squares / centered.len() as f64).sqrt().max(0.02);
    centered.iter().map(|v| (v - mean) / deviation).collect()
}
