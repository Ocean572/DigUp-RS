//! Vector storage format (ARCHITECTURE_MAP.md §2): 768 × float16, L2-normalized, scored as f32.
//!
//! The engine keeps vectors as `Float16` on disk (1.5 KB each) and in memory, and scores them as
//! float32 (`Vectors.swift:6-13`). The bit layout is the contract — an existing index's `vec`
//! blobs must read identically — so the encoder is IEEE 754 binary16 with round-ties-to-even, the
//! one rule `Float16(_:)` follows. Tests in `tests/storage.rs` pin the ties, the subnormal edges
//! and the overflow against hand-computed values, and against the `half` crate over a wide sweep.

/// `Vectors.dimension`: EmbeddingGemma 2's output width.
pub const DIMENSION: usize = 768;

/// f32 → IEEE 754 binary16 (round-ties-to-even), the layout Swift's `Float16` produces.
pub fn to_half_bits(value: f32) -> u16 {
    half::f16::from_f32(value).to_bits()
}

/// The `vec` blob bytes: one u16 per component, little-endian (the order `Float16` lays out).
pub fn half_bytes(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 2);
    for &x in vector {
        out.extend_from_slice(&to_half_bits(x).to_le_bytes());
    }
    out
}

/// f16 bits → f32, the conversion the scorer does a block of rows at a time
/// (`Vectors.floats(fromHalf:)`): 1024 rows × 768 at a time, into f32.
pub fn from_half_bits(bits: u16) -> f32 {
    half::f16::from_bits(bits).to_f32()
}

/// A `vec` blob → its 768 f32s, the reading `VectorStorage` does per block
/// (`Math+Portable.swift:6-8` converts with `Float(source[index])`).
pub fn floats_from_halves(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| from_half_bits(u16::from_le_bytes([pair[0], pair[1]])))
        .collect()
}
