//! Vector storage format (ARCHITECTURE_MAP.md §2): 768 × float16, L2-normalized, scored as f32.
//!
//! Phase 1 only needs the storage contract — the on-disk blob layout. The ranking itself (brute
//! force over the mmap'd matrix) is a later phase; the encoder here is IEEE 754 half precision so a
//! stored `vec` is the same 1536 bytes the Swift engine writes.

pub const DIMENSION: usize = 768;

/// f32 → IEEE 754 binary16 (round-to-nearest-even), the layout `Float16` produces.
pub fn to_half_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let mut exp = ((bits >> 23) & 0xff) as i32;
    let mut mant = bits & 0x7f_ffff;
    if exp == 0xff {
        // Inf / NaN: keep a NaN payload, drop the trailing bit that would alias to Inf.
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    exp -= 127 - 15;
    if exp >= 0x1f {
        return sign | 0x7c00; // overflow → Inf
    }
    if exp <= 0 {
        // subnormal / zero
        if exp < -10 {
            return sign;
        }
        mant |= 0x80_0000;
        let shift = (14 - exp) as u32;
        let mut m = mant >> shift;
        let round = (mant >> (shift - 1)) & 1;
        if round == 1 {
            m += 1;
        }
        return sign | (m as u16);
    }
    let mut m = mant >> 13;
    let round = (mant >> 12) & 1;
    m += round; // round-to-nearest (carry into exponent is automatic)
    let mut result = ((exp as u16) << 10) | (m as u16 & 0x3ff);
    if m > 0x3ff {
        result = ((exp + 1) as u16) << 10; // mantissa carried into the exponent
    }
    sign | result
}

/// The `vec` blob bytes: one u16 per component, little-endian (the order `Float16` lays out).
pub fn half_bytes(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 2);
    for &x in vector {
        out.extend_from_slice(&to_half_bits(x).to_le_bytes());
    }
    out
}
