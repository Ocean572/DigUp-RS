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

    // READ-ONLY, as `scoring.rs` opens this fixture: the file is a user's live index (the
    // cross-check's folder), the tests only ever read blobs from it. The measured shape
    // is `reading_the_fixture_never_recovers_the_users_index` below: a read-write open of
    // a crashed index (dirty `-wal`) recovers AND checkpoints as it opens and closes —
    // the fixture's bytes move under a test that claims to read it.
    let conn = rusqlite::Connection::open_with_flags(
        &file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
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

/// The fixture open is READ-ONLY, and this is the pin that measures it: a fixture file
/// the process cannot write. The shared Swift-built indexes of the cross-checks live on
/// shared storage (the box's fixture folder, another uid's file), and the tests only
/// ever read blobs out of them. With mode 0444 on the file, the body of
/// `swift_blobs_reencode_exactly` must run; a read-write open (`O_RDWR`) dies at open
/// with EACCES. The fixture here is written by this port — the re-encode and length
/// assertions the body makes are encoder facts, and a unit-length vector built here
/// exercises the same body, which is what the pin targets. (Measured 2026-11-13: the
/// read-write open panics `unable to open database file` on the 0444 fixture.)
/// The fixture keeps its bytes: a suite that READS a user's index must not be the one
/// that recovers it. The shape is the engine's own crash: `Database::open` runs with
/// `synchronous = NORMAL` (db.rs:127), so committed frames sit in `-wal` while the main
/// database is behind — the index helper SIGKILLed by the watchdog (its `_exit(75)`)
/// leaves exactly that on disk. The child below creates such an index and kills itself
/// with SIGKILL, no close, no checkpoint.
///
/// The two opens measured against that fixture (this box, 2026-11-13):
///
///   * READ-ONLY: the frames are readable (SQLite builds the wal-index in heap memory —
///     a read-only connection is barred from checkpointing by design), the blobs
///     re-encode, and the main database is BYTE-IDENTICAL afterwards, its `-wal`
///     standing as the crash left it;
///   * read-write (rusqlite's `Connection::open`): opening the file takes the recovery
///     write, and the connection's close CHECKPOINTS — the frames fold into the main
///     database, whose bytes change. The suite silently REBUILDS the user's fixture,
///     then reads what it just rewrote: the parity test of a file it repaired is not
///     the parity test of the file the cross-check handed over. Measured on this box
///     (2026-11-13, the test below with the read-write open installed): the main file
///     grew 4,096 → 81,920 bytes and the `-wal` was truncated to 0.
///
/// The pin is the fixture's own bytes: unchanged by a test that only reads.
#[cfg(unix)]
#[test]
fn reading_the_fixture_never_recovers_the_users_index() {
    use std::os::unix::process::ExitStatusExt;

    let tmp = tempfile::tempdir().expect("a temp folder");
    let dir = tmp.path().join("fixture");
    std::fs::create_dir(&dir).expect("the fixture folder");

    let exe = std::env::current_exe().expect("the test binary, re-run as the crashed writer");
    let status = std::process::Command::new(&exe)
        .args([
            "--ignored",
            "--exact",
            "writes_the_fixture_and_dies_mid_run",
            "--nocapture",
        ])
        .env("DIGUP_FIXTURE_DIR", &dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("the child starts");
    let signal = status
        .signal()
        .expect("the child was killed by the signal it sent itself");
    assert_eq!(signal, 9, "SIGKILL, as the watchdog's owner would send");

    let index = dir.join(digup_core::MAIN_FILE);
    let main_before = std::fs::read(&index).expect("the main database as the crash left it");
    let wal = dir.join("index.sqlite-wal");
    assert!(
        wal.is_file() && std::fs::metadata(&wal).unwrap().len() > 0,
        "the crash left a dirty -wal: the committed frames live there, not in the main file"
    );

    std::env::set_var("DIGUP_SWIFT_INDEX", &dir);
    swift_blobs_reencode_exactly();
    std::env::remove_var("DIGUP_SWIFT_INDEX");

    let main_after = std::fs::read(&index).expect("the main database after the read");
    assert_eq!(
        main_after.len(),
        main_before.len(),
        "a read-only open cannot checkpoint, so the main file keeps its size; the \
         read-write open folded the frames in (measured: 4,096 -> 81,920 bytes, -wal truncated)"
    );
    assert!(
        main_after == main_before,
        "the suite read the fixture, it did not RECOVER it: a read-write open runs WAL \
         recovery on opening and checkpoints on close, folding the crash's frames into \
         the user's main database ({} changed bytes)",
        main_after
            .iter()
            .zip(&main_before)
            .filter(|(a, b)| a != b)
            .count()
    );
    assert!(
        wal.is_file() && std::fs::metadata(&wal).unwrap().len() > 0,
        "the crash's -wal stands: the next engine open resumes the run, with its counters"
    );
}

/// The child half: create an index the crashed engine left behind — a committed vector
/// row whose frames sit in `-wal`, the main database behind — and die by SIGKILL mid-run
/// (the index helper's `_exit(75)` shape; `synchronous = NORMAL` writes frames to `-wal`
/// and lets the checkpoint come, which a killed process never runs). `#[ignore]` keeps
/// it out of a plain `cargo test` run.
#[cfg(unix)]
#[test]
#[ignore = "run by reading_the_fixture_never_recovers… as a child: writes, then SIGKILL"]
fn writes_the_fixture_and_dies_mid_run() {
    use digup_core::{Candidate, FileKind, Modality, SegmentKind};
    use std::collections::HashMap;

    let dir = std::env::var("DIGUP_FIXTURE_DIR").expect("the fixture folder");
    let store = digup_core::index_store::IndexStore::open(
        std::path::Path::new(&dir),
        digup_core::MAIN_FILE,
    )
    .expect("an index");
    let file = store
        .insert(&Candidate {
            path: "/data/a.jpg".to_string(),
            kind: FileKind::Image,
            size: 10,
            mtime: 1.0,
            inode: 1,
            device: 1,
        })
        .expect("a file row");
    store
        .finish(file, "done", None, HashMap::new())
        .expect("the file is done");
    let unit = {
        let mut v = [0.0f32; DIMENSION];
        v[0] = 1.0;
        v
    };
    store
        .add_segment(
            file,
            SegmentKind::Image,
            Modality::Image,
            None,
            None,
            Some("a unit vector"),
            Some("a unit vector"),
            Some(&unit),
        )
        .expect("a segment with a vector");
    std::io::Write::flush(&mut std::io::stdout()).ok();
    unsafe {
        // The crash: no close, no checkpoint, no atexit — the OS drops the locks and
        // leaves the WAL exactly as the commits left it.
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
    std::process::exit(0); // not reached: SIGKILL has no handler
}
