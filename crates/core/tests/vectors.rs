//! The fp16 vector format, checked against blobs the **Swift engine wrote**.
//!
//! `vectors.rs` says the contract: `vec` is 768 × float16, little-endian, L2-normalized. A decoder
//! round-trip (`f16 → f32 → f16`) is the strict test of that contract: every f16 value is exactly
//! representable as f32, so re-encoding a decoded value must give the same two bytes it came from.
//! Any other rounding — ties away from zero, flush-to-zero subnormals, a NaN payload rewrite —
//! changes at least one of the 1536 bytes of a real blob.
//!
//! The fixture is an index built by the engine itself. Point `DIGUP_SWIFT_INDEX` at its folder
//! (the cross-check in `docs/ARCHITECTURE_MAP.md` builds one); without it the test says so and
//! skips, as `ReferenceVectorTests` does when the model files are absent.

use digup_core::vectors::{floats_from_halves, half_bytes, to_half_bits, DIMENSION};
use rusqlite::Connection;

/// The engine's blobs, re-encoded byte for byte.
#[test]
fn swift_blobs_reencode_exactly() {
    let Some(folder) = std::env::var_os("DIGUP_SWIFT_INDEX") else {
        eprintln!("skipped: set DIGUP_SWIFT_INDEX to a folder with an index.sqlite built by digup");
        return;
    };
    let file = std::path::Path::new(&folder).join("index.sqlite");
    if !file.is_file() {
        eprintln!("skipped: no {} (DIGUP_SWIFT_INDEX)", file.display());
        return;
    }

    let conn = Connection::open(&file).unwrap();
    let blobs: Vec<Vec<u8>> = conn
        .prepare("SELECT vec FROM segments WHERE vec IS NOT NULL")
        .unwrap()
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!blobs.is_empty(), "the fixture index has no vectors");

    let mut checked = 0;
    for (index, blob) in blobs.iter().enumerate() {
        assert_eq!(
            blob.len(),
            DIMENSION * 2,
            "vector {}: {} bytes, not {} × 2",
            index,
            blob.len(),
            DIMENSION
        );
        let decoded = floats_from_halves(blob);
        assert_eq!(
            half_bytes(&decoded),
            *blob,
            "vector {index}: the encoder is not the engine's Float16"
        );

        // The engine stores L2-normalized vectors: the decoded length is 1, within fp16 rounding.
        let length: f64 = decoded
            .iter()
            .map(|x| (*x as f64) * (*x as f64))
            .sum::<f64>()
            .sqrt();
        assert!(
            (length - 1.0).abs() < 1e-3,
            "vector {index}: length {length}, the engine's are normalized"
        );
        checked += 1;
    }
    println!("re-encoded {checked} Swift-written vectors, byte for byte");
}

/// Rounding, at the edges a round-to-nearest-even rule shows up at: ties, subnormals, the overflow
/// edge, and the NaN/Inf layout. Values are f32s and the expected half is written by hand.
#[test]
fn rounding_is_round_to_nearest_even() {
    let cases: [(f32, u16); 12] = [
        (1.0, 0x3c00),  // 1.0
        (-1.0, 0xbc00), // -1.0
        (0.0, 0x0000),
        (-0.0, 0x8000),
        (1.0 / 2.0f32.powi(14), 0x0400), // the smallest normal half
        (1.0 / 2.0f32.powi(24), 0x0001), // the smallest subnormal half
        (1.0 + 2f32.powi(-11), 0x3c00),  // 1 + 2⁻¹¹: a tie, rounds to the even 1.0
        (1.0 + 3.0 * 2f32.powi(-11), 0x3c02), // 1 + 3·2⁻¹¹: a tie, rounds to the even mantissa
        (2f32.powi(-25), 0x0000),        // tie between 0 and the smallest subnormal → 0
        (65504.0, 0x7bff),               // the largest finite half
        (65520.0, 0x7c00),               // the overflow edge: ties-to-even → Inf
        ((1.0f32 * 2.0f32.powi(16)).powi(2), 0x7c00), // a square that overflows f32, then f16
    ];
    for (value, expected) in cases {
        assert_eq!(to_half_bits(value), expected, "the half of {value}");
    }

    // Inf and NaN keep their sign and stay non-finite.
    assert_eq!(to_half_bits(f32::INFINITY), 0x7c00);
    assert_eq!(to_half_bits(f32::NEG_INFINITY), 0xfc00);
    let nan = to_half_bits(f32::NAN);
    assert_eq!(nan & 0x7c00, 0x7c00, "a NaN stays a NaN: {nan:#06x}");
    assert!(nan & 0x3ff != 0, "a NaN keeps a payload: {nan:#06x}");

    // Round-tripping a value that is already a half is the identity, across the whole exponent range.
    // Normals: (1 + m/1024) × 2^e is exactly a half, so the encoder must return the bits it came from.
    for exponent in -14..=15i32 {
        for mantissa in [0u32, 1, 512, 1023] {
            let value = (1.0 + mantissa as f32 / 1024.0) * 2f32.powi(exponent);
            let expected = (((exponent + 15) as u16) << 10) | mantissa as u16;
            assert_eq!(
                to_half_bits(value),
                expected,
                "the half of {value} (2^{exponent} · 1 + {mantissa}/1024)"
            );
            assert_eq!(to_half_bits(-value), expected | 0x8000, "and its mirror");
        }
    }
    // Subnormals: m × 2⁻²⁴ is exactly a half with mantissa m, down to the smallest.
    for mantissa in 1..1024u32 {
        let value = mantissa as f32 * 2f32.powi(-24);
        assert_eq!(
            to_half_bits(value),
            mantissa as u16,
            "the subnormal half of {value}"
        );
    }
}

/// The scorer's conversion: the blob bytes → 768 f32s, and back.
#[test]
fn halves_round_trip() {
    let vector: Vec<f32> = (0..DIMENSION).map(|i| (i as f32 - 384.0) / 512.0).collect();
    let blob = half_bytes(&vector);
    assert_eq!(blob.len(), DIMENSION * 2);
    let decoded = floats_from_halves(&blob);
    assert_eq!(decoded.len(), DIMENSION);
    assert_eq!(
        half_bytes(&decoded),
        blob,
        "f16 → f32 → f16 is the identity"
    );
    for (a, b) in vector.iter().zip(&decoded) {
        assert!(
            (a - b).abs() <= a.abs() * 6e-4 + 6e-8,
            "{a} vs {b}: half precision keeps ~3 decimals"
        );
    }
}
