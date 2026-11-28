//! Issue #182 (big tiled TIFFs and the memory preflight), pinned on the Rust side.
//!
//! The Swift-era watchdog KILLED the helper (app.log's `indexer: exited (5)`) when one file
//! needed more memory than its limit, and the same files were retried every restart — the
//! queue stalled behind them for a week. The engine's answer is the per-file preflight: the
//! file is refused by name, the process lives, indexing moves on. The refusal texts and the
//! wrapping-multiplication shapes are pinned in `exif.rs` (its "#182 budget" scenarios) and
//! `image.rs`; this file adds the shapes measured on the box 2026-11-28 with the pinned
//! ffmpeg: the tiled TIFFs a photo editor exports DECODE (stb reads the tile table), and the
//! preflight refuses the over-budget one without touching the process.
//!
//! The fixtures are PIL-made tiled TIFFs (tile table + LZW / Adobe-Deflate), held out of git
//! the way the RAW and SVG fixtures are: DIGUP_DECODE_FIXTURES names the folder. A tiled TIFF
//! whose layout stb cannot read (planar, 16-bit, CMYK tiles) keeps the pinned decline
//! (`tif-tiled.tif s1` in `exif.rs`): the engine's own answer, never an invented decode.

use std::path::Path;

use digup_native::image;

fn fixtures() -> Option<std::path::PathBuf> {
    match std::env::var("DIGUP_DECODE_FIXTURES") {
        Ok(folder) if Path::new(&folder).is_dir() => Some(Path::new(&folder).to_path_buf()),
        _ => {
            eprintln!("skipped: DIGUP_DECODE_FIXTURES is not set (tiled TIFF samples)");
            None
        }
    }
}

#[test]
fn a_tiled_tiff_decodes_and_the_budget_preflight_refuses_by_name() {
    // The #182 family, measured on this box: PIL's tiled LZW export (1200×900) is read by stb
    // from its tile table, and the tiled Adobe-Deflate export (6000×4000) — a header stb stops
    // at — goes through ffmpeg, the same path a huge HEIC takes. Both decode under no limit.
    // The SAME test then sets a 20 MB limit: the 6000×4000 picture (72 MB of PPM over the pipe)
    // is refused by name — the watchdog's job done as a refusal. The process answering this
    // test IS the assertion that nothing dies; the budget is a process-wide static, so the
    // decode and the refusal share one test, in order.
    let Some(samples) = fixtures() else { return };
    let work = tempfile::tempdir().expect("a work folder");
    image::set_decode_memory_budget(0);
    for (name, width, height) in [
        ("tiled_lzw.tif", 1200i64, 900i64),
        ("big_tiled.tif", 6000, 4000),
    ] {
        let path = samples.join(name);
        if !path.exists() {
            eprintln!("skipped: {name} is not in DIGUP_DECODE_FIXTURES");
            continue;
        }
        let prepared = image::prepare(&path, 400, 200, work.path(), false)
            .unwrap_or_else(|error| panic!("{name}: the tiled TIFF refused: {error}"));
        assert_eq!(
            (prepared.width, prepared.height),
            (width, height),
            "{name}: its own size"
        );
        assert!(
            Path::new(&prepared.path).exists(),
            "{name}: the prepared copy"
        );
    }
    let copies: usize = std::fs::read_dir(work.path())
        .expect("the work folder")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jpg"))
        .count();
    assert_eq!(copies, 2, "both tiled exports wrote their copy");

    let limit = 20 * 1_048_576;
    image::set_decode_memory_budget(limit);
    let outcome = image::prepare(&samples.join("big_tiled.tif"), 400, 200, work.path(), false);
    image::set_decode_memory_budget(0);
    let error = match outcome {
        Ok(_) => panic!("the 72 MB stream passed a 20 MB limit"),
        Err(error) => error,
    };
    let why = error.to_string();
    assert!(why.contains("needed more than"), "the named refusal: {why}");
    assert!(why.contains("20 MB"), "the helper's limit, named: {why}");
    assert_eq!(
        std::fs::read_dir(work.path())
            .expect("the work folder")
            .count(),
        2,
        "a refused picture writes no copy"
    );
}

#[test]
fn a_password_protected_office_file_is_refused_not_crashed() {
    // Issue #171: a document an Office "Encrypt with Password" made. Measured on this box:
    // the OLE-CFB container (the magic a password-encrypted .docx/.xlsx carries) answers
    // `password-protected` at this seam, and the same file through the indexer's doc reader
    // answers `unreadable: not a zip file` (the runtime test
    // `password_protected_files_fail_and_the_queue_moves_on` pins that row). The zip-masked
    // shape — bit 13 set on a real zip — answers `password-protected` from miniz's own
    // detection, pinned in `crates/core/tests/doc.rs`. All three are refusals: the reader
    // lives, and the queue moves on.
    let work = tempfile::tempdir().expect("a work folder");
    let path = work.path().join("encrypted.docx");
    let mut bytes = vec![0u8; 4096];
    bytes[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    std::fs::write(&path, &bytes).expect("the fixture");
    match digup_core::office::text(&path, "docx") {
        Ok(text) => panic!("a CFB container read as a document ({} chars)", text.len()),
        Err(error) => assert_eq!(error.to_string(), "password-protected", "the refusal text"),
    }
}
