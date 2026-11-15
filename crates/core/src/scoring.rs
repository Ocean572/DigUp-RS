//! Brute-force meaning scoring (`Searcher.swift:305-372`, `Math+Portable.swift`): the fp16
//! vector matrix, the 1024-row f32 blocks, the SIMD8 dot products, the per-modality means and
//! the z-scale the 2.0σ gate reads (`Searcher.zGate`).
//!
//! The contract from ARCHITECTURE_MAP.md §2: every stored vector is scored against every query
//! — brute force, no ANN / sidecar index — fp16 storage (half the footprint), f32 scoring, a
//! block of 1024 rows converted at a time. The matrix is mmap'd anonymous memory, as the engine
//! mmaps it: a `Vec` big enough for 70k vectors (108 MB) stays in the process footprint after
//! being freed (measured 2026-10-07, on the engine's side); `munmap` returns it at once, which
//! is what `MmapMut::drop` does.
//!
//! `meaning` puts one query's scores on one scale: each vector's cosine minus its modality's
//! average (text, image and audio sit at slightly different levels for the same query), in
//! standard deviations of all of them together. Within a modality that's the model's own order.

use std::cell::RefCell;

use crate::model::Modality;
use crate::vectors::DIMENSION;
use memmap2::MmapMut;

/// `VectorStorage.blockRows`: rows converted to float32 at a time.
pub const BLOCK_ROWS: usize = 1024;

/// `Searcher.zGate`: how far a segment must stand out to count as a meaning match.
pub const Z_GATE: f64 = 2.0;

/// `Searcher.wordWeight`: how much finding the query's words counts against meaning.
pub const WORD_WEIGHT: f64 = 2.0;

/// The stored vectors (`VectorStorage`): `rows × 768` float16 in mmap'd anonymous memory, plus
/// room to turn `BLOCK_ROWS` of them into float32 at a time.
pub struct VectorMatrix {
    rows: usize,
    halves: MmapMut,
    block: RefCell<Vec<f32>>,
}

impl VectorMatrix {
    /// The allocation: rows must be positive, as the engine's `VectorStorage.init(rows:)` asks.
    /// The pages arrive zeroed, so an unloaded row scores as a zero vector until `set_row`.
    pub fn new(rows: usize) -> Option<Self> {
        if rows == 0 {
            return None;
        }
        let halves = MmapMut::map_anon(rows * DIMENSION * 2).ok()?;
        let block = RefCell::new(vec![0.0f32; BLOCK_ROWS * DIMENSION]);
        Some(Self {
            rows,
            halves,
            block,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Copies one stored `vec` blob (`Vectors.half_bytes` output, 1536 bytes) into row `row`
    /// (`matrix.halves.copyBlob` in `Searcher.load`). False when the row is out of range or the
    /// blob is not a full vector.
    pub fn set_row(&mut self, row: usize, bytes: &[u8]) -> bool {
        if row >= self.rows || bytes.len() != DIMENSION * 2 {
            return false;
        }
        let start = row * DIMENSION * 2;
        self.halves[start..start + DIMENSION * 2].copy_from_slice(bytes);
        true
    }

    /// Cosine similarity of `query` with every stored vector (all are L2-normalized, so the dot
    /// product is the cosine): a matrix-vector product, done a block of rows at a time, each
    /// block turned into float32 first (`Searcher.score`). The block buffer is allocated once
    /// per query, as the engine's `VectorStorage` carries one for the whole matrix.
    pub fn score(&self, query: &[f32]) -> Vec<f32> {
        let mut scores = vec![0.0f32; self.rows];
        if self.rows == 0 || query.len() != DIMENSION {
            return scores;
        }
        let lanes = query_lanes(query);
        let mut block_all = self.block.borrow_mut();
        let mut row = 0;
        while row < self.rows {
            let count = BLOCK_ROWS.min(self.rows - row);
            // `Vectors.floats(fromHalf:)`: the block's f16 rows into f32, then `Vectors.scores`:
            // (count × 768) · (768 × 1).
            let block = &mut block_all[..count * DIMENSION];
            // `halves` is indexed in bytes: a row is DIMENSION * 2 of them.
            for (i, bits) in self.halves[row * DIMENSION * 2..(row + count) * DIMENSION * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .enumerate()
            {
                block[i] = crate::vectors::from_half_bits(u16::from_le_bytes([bits[0], bits[1]]));
            }
            dot_rows(block, count, &lanes, &mut scores[row..row + count]);
            row += count;
        }
        scores
    }
}

/// `Vectors.scores` (`Math+Portable.swift:11-25`): 768 = 96 lanes of 8 float32s; the row's dot
/// with `query` is the sum of the lane products, lane-wise, exactly Swift's SIMD8 loop. Rust
/// vectorizes the 8-wide inner loop into the same 256-bit code on x86-64/ARM.
fn dot_rows(rows: &[f32], count: usize, lanes: &[[f32; 8]], out: &mut [f32]) {
    for (row, score) in out.iter_mut().take(count).enumerate() {
        let start = &rows[row * DIMENSION..][..DIMENSION];
        let mut sum = [0.0f32; 8];
        for (lane, chunk) in lanes.iter().zip(start.as_chunks::<8>().0.iter()) {
            for k in 0..8 {
                sum[k] += lane[k] * chunk[k];
            }
        }
        *score = sum.iter().sum();
    }
}

/// The query as 8-float lanes (`queryLanes`).
fn query_lanes(query: &[f32]) -> Vec<[f32; 8]> {
    query
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| {
            [
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ]
        })
        .collect()
}

/// `Searcher.meaning`: each score minus its modality's average, in standard deviations of all
/// centered scores together (`sets_scale` rows only — the later pages of long files are scored
/// on the scale but don't move it). The deviation floor of 0.02 is the engine's.
pub fn meaning(scores: &[f32], modality: &[Modality], sets_scale: Option<&[bool]>) -> Vec<f64> {
    let means = modality_means(scores, modality, sets_scale);
    let centered: Vec<f64> = scores
        .iter()
        .zip(modality)
        .map(|(&score, kind)| f64::from(score) - means[modality_index(*kind)])
        .collect();
    let mut count = 0.0f64;
    let mut sum = 0.0f64;
    for (row, value) in centered.iter().enumerate() {
        if sets_scale.is_none_or(|scale| scale[row]) {
            count += 1.0;
            sum += value;
        }
    }
    let mean = sum / count.max(1.0);
    let mut squares = 0.0f64;
    for (row, value) in centered.iter().enumerate() {
        if sets_scale.is_none_or(|scale| scale[row]) {
            squares += (value - mean) * (value - mean);
        }
    }
    let deviation = (squares / count.max(1.0)).sqrt().max(0.02);
    centered
        .iter()
        .map(|value| (value - mean) / deviation)
        .collect()
}

/// `Searcher.modalityMeans`: the average score of each modality for this query. Groups smaller
/// than 20 vectors take the average of all (`Modality.allCases` order: none, text, image, audio).
pub fn modality_means(
    scores: &[f32],
    modality: &[Modality],
    sets_scale: Option<&[bool]>,
) -> [f64; 4] {
    let mut sum = [0.0f64; 4];
    let mut count = [0usize; 4];
    for (row, &score) in scores.iter().enumerate() {
        if sets_scale.is_none_or(|scale| scale[row]) {
            let index = modality_index(modality[row]);
            sum[index] += f64::from(score);
            count[index] += 1;
        }
    }
    let total: f64 = sum.iter().sum::<f64>();
    let rows: f64 = count.iter().sum::<usize>().max(1) as f64;
    let all = total / rows;
    let mut out = [all; 4];
    for index in 0..4 {
        if count[index] >= 20 {
            out[index] = sum[index] / count[index] as f64;
        }
    }
    out
}

/// `Searcher.strongCount`: how many hits (best first) score at least `ratio` × the top — the
/// rest the app folds under "weaker matches". A non-positive top keeps the whole list, as the
/// engine's `guard let top … else { return hits.count }` does.
pub fn strong_count(scores_best_first: &[f64], ratio: f64) -> usize {
    match scores_best_first.first() {
        Some(top) if *top > 0.0 => scores_best_first
            .iter()
            .take_while(|score| **score >= top * ratio)
            .count(),
        _ => scores_best_first.len(),
    }
}

fn modality_index(modality: Modality) -> usize {
    match modality {
        Modality::None => 0,
        Modality::Text => 1,
        Modality::Image => 2,
        Modality::Audio => 3,
    }
}
