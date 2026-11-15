//! The estimate (`Estimator.swift`): the M4 Pro's fixed costs, the per-kind math, the long-file
//! split, the code index's size, and the subfolder grouping the folder lists show.
//!
//! Media durations come from ffprobe (the engine's `Media.duration`), so the audio and video
//! cases run only where ffprobe is installed; they write a real WAV and pin what ffprobe says.

use std::collections::BTreeMap;

use digup_core::estimator::{
    self, Costs, Estimate, CHARACTERS_PER_STRETCH, PER_CHARACTER, PER_STRETCH,
};
use digup_core::model::{Candidate, FileKind};
use digup_core::options::IndexOptions;

fn temp_tree(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn candidate(path: &str, kind: FileKind, size: i64) -> Candidate {
    Candidate {
        path: path.to_string(),
        kind,
        size,
        mtime: 0.0,
        inode: 0,
        device: 0,
    }
}

fn write(root: &std::path::Path, name: &str, body: &[u8]) -> String {
    let file = root.join(name);
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::fs::write(&file, body).unwrap();
    file.to_string_lossy().into_owned()
}

/// A PDF whose trailer's page tree carries `/Count pages`.
fn pdf_with_page_count(root: &std::path::Path, name: &str, pages: i64) -> String {
    write(
        root,
        name,
        format!(
            "%PDF-1.4\n1 0 obj<</Type /Catalog /Pages 2 0 R>>endobj\n2 0 obj<</Type /Pages /Kids [] /Count {pages}>>endobj\ntrailer\n<</Size 3 /Root 1 0 R /Count {pages}>>\n%%EOF\n"
        )
        .as_bytes(),
    )
}

/// A PCM 16-bit mono WAV of `seconds` at `rate` samples a second.
fn wav(root: &std::path::Path, name: &str, rate: u32, seconds: u32) -> String {
    let samples = rate * seconds;
    let data = samples * 2;
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.resize((44 + data) as usize, 0);
    write(root, name, &bytes)
}

#[test]
fn the_fixed_costs_are_the_m4_pros_measured_numbers() {
    let costs = Costs::default();
    assert_eq!(
        [
            costs.image,
            costs.screenshot,
            costs.pdf_page,
            costs.doc_chunk,
            costs.audio_second,
            costs.video_second,
            costs.code_stretch,
        ],
        [0.12, 0.32, 0.09, 0.045, 0.0089, 0.019, 0.037]
    );
    // `MeasuredCosts` scales these by the field names `costs.json` carries.
    let mut scaled = Costs::default();
    for (name, value) in [
        ("image", 1.0),
        ("screenshot", 2.0),
        ("pdfPage", 3.0),
        ("docChunk", 4.0),
        ("audioSecond", 5.0),
        ("videoSecond", 6.0),
        ("codeStretch", 7.0),
    ] {
        scaled.set(name, value);
        assert_eq!(scaled.get(name), value);
    }
}

#[test]
fn the_estimate_multiplies_the_costs_by_kind() {
    let root = temp_tree("digup-estimate-kinds");
    let options = IndexOptions::default();
    let costs = Costs::default();
    let small_pdf = pdf_with_page_count(&root, "short.pdf", 40);
    let big_pdf = pdf_with_page_count(&root, "long.pdf", 60);

    let files = vec![
        candidate("/x/a.jpg", FileKind::Image, 1000),
        candidate("/x/b.jpg", FileKind::Image, 1000),
        candidate("/x/Screenshot one.png", FileKind::Screenshot, 1000),
        candidate(&small_pdf, FileKind::Pdf, 50_000),
        candidate(&big_pdf, FileKind::Pdf, 90_000),
        candidate("/x/notes.txt", FileKind::Doc, 200_000),
        candidate("/x/short.txt", FileKind::Doc, 1_000),
        candidate("/x/loop.mp3", FileKind::Audio, 10_000),
        candidate("/x/movie.mp4", FileKind::Video, 10_000),
        candidate("/x/src/main.rs", FileKind::Code, 3_000),
        candidate("/x/note.ipynb", FileKind::Code, 8_000),
    ];
    let estimate = estimator::estimate(&files, &options, &costs);

    // Counts per kind, and the media totals the folders show.
    assert_eq!(estimate.files.get(&FileKind::Image), Some(&2));
    assert_eq!(estimate.files.get(&FileKind::Screenshot), Some(&1));
    assert_eq!(estimate.files.get(&FileKind::Pdf), Some(&2));
    assert_eq!(estimate.files.get(&FileKind::Doc), Some(&2));
    assert_eq!(estimate.files.get(&FileKind::Code), Some(&2));

    // A PDF of 40 pages (≤ `bigPDFPages` 50) is read whole in the first pass; a 60-page one
    // takes `pdfStep` 30 pages now and the rest later.
    assert_eq!(estimate.pdf_pages_total, 100);
    assert_eq!(estimate.pdf_pages, 40 + 30);

    // A 200 KB document: 100 chunks, `docStep` 40 of them in the first pass.
    assert_eq!(estimate.doc_chunks, 40 + 1);
    assert_eq!(estimate.long_files, 2); // the 60-page PDF and the 100-chunk document
    assert!((estimate.later_seconds - (30.0 * 0.09 + 60.0 * 0.045)).abs() < 1e-9);

    // The code index: 3,000 characters are 3 stretches (1,400 a stretch, rounded up); a
    // notebook's file is mostly its outputs, so a third of the size is the guess at its cells.
    assert_eq!(estimate.code_stretches, 3 + 1);
    let expected_bytes =
        3_000.0 * PER_CHARACTER + 3.0 * PER_STRETCH + 1_000.0 * PER_CHARACTER + 1.0 * PER_STRETCH;
    assert!((estimate.bytes - expected_bytes).abs() < 1e-6);

    // The seconds add up per kind, and `kindSeconds` sums to the total.
    let expected = 2.0 * costs.image
        + costs.screenshot
        + (40.0 * costs.pdf_page)
        + (30.0 * costs.pdf_page)
        + (40.0 * costs.doc_chunk)
        + costs.doc_chunk
        + 3.0 * costs.code_stretch
        + costs.code_stretch;
    assert!(
        (estimate.seconds - expected).abs() < 1e-9,
        "{} vs {expected}",
        estimate.seconds
    );
    let kinds: f64 = estimate.kind_seconds.values().sum();
    assert!((kinds - estimate.seconds).abs() < 1e-9);
    assert!((estimate.kind_seconds[&FileKind::Image] - 2.0 * costs.image).abs() < 1e-9);

    // Media durations are ffprobe's; the totals follow them.
    if estimator::find_program("ffprobe").is_some() {
        let one = wav(&root, "one.wav", 8_000, 1);
        let two = wav(&root, "two.wav", 8_000, 2);
        let media = vec![
            candidate(&one, FileKind::Audio, 16_044),
            candidate(&two, FileKind::Video, 32_044),
        ];
        let media = estimator::estimate(&media, &options, &costs);
        assert!(
            (media.audio_seconds - 1.0).abs() < 0.02,
            "ffprobe said {}",
            media.audio_seconds
        );
        assert!(
            (media.video_seconds - 2.0).abs() < 0.02,
            "ffprobe said {}",
            media.video_seconds
        );
        assert!(
            (media.seconds - (1.0 * costs.audio_second + 2.0 * costs.video_second)).abs() < 1e-6
        );
    } else {
        eprintln!("skipped: ffprobe is not installed (DIGUP_FFMPEG or PATH)");
        let media = vec![candidate("/x/loop.mp3", FileKind::Audio, 10_000)];
        let media = estimator::estimate(&media, &options, &costs);
        assert_eq!(media.audio_seconds, 0.0);
    }

    // A file ffprobe can't answer for is 0 s, as the engine's `?? 0` says.
    let ghost_path = root.join("nope.mp4");
    let ghost = vec![candidate(
        &ghost_path.to_string_lossy(),
        FileKind::Video,
        10,
    )];
    assert_eq!(
        estimator::estimate(&ghost, &options, &costs).video_seconds,
        0.0
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn page_counts_come_from_the_file_header() {
    let root = temp_tree("digup-estimate-pages");
    assert_eq!(
        estimator::pages_of(&pdf_with_page_count(&root, "a.pdf", 12)),
        12
    );
    assert_eq!(
        estimator::pages_of(&pdf_with_page_count(&root, "b.pdf", 1)),
        1
    );
    assert_eq!(
        estimator::pages_of(&pdf_with_page_count(&root, "c.pdf", 3_000)),
        3_000
    );

    // An xref-stream PDF has no `trailer` keyword: the page objects are the count.
    let objects = write(
        &root,
        "stream.pdf",
        b"%PDF-1.7\n1 0 obj<</Type /Pages /Kids [2 0 R 3 0 R 4 0 R]>>endobj\n\
           2 0 obj<</Type /Page /Parent 1 0 R>>endobj\n\
           3 0 obj<</Type /Page /Parent 1 0 R>>endobj\n\
           4 0 obj<</Type /Page /Parent 1 0 R>>endobj\n\
           5 0 obj<</Type /ObjStm /N 3>>stream\nxref 0 6\ntrailer<</Size 6>>\n%%EOF\n",
    );
    assert_eq!(estimator::pages_of(&objects), 3);

    // A file PDFium can't read counts as one page, and so does one with nothing in it.
    let junk = write(&root, "junk.pdf", b"%PDF-1.0\nnot a pdf at all\n");
    assert_eq!(estimator::pages_of(&junk), 1);
    assert_eq!(
        estimator::pages_of(&root.join("absent.pdf").to_string_lossy()),
        1
    );

    // Past `maxPages` the estimate stops counting: only machine-made giants get that far.
    let options = IndexOptions::default();
    let costs = Costs::default();
    let huge = pdf_with_page_count(&root, "huge.pdf", 9_000);
    let estimate =
        estimator::estimate(&[candidate(&huge, FileKind::Pdf, 10_000)], &options, &costs);
    assert_eq!(estimate.pdf_pages_total, 9_000);
    assert_eq!(estimate.pdf_pages, options.pdf_step); // 30 of them, in the first pass
    assert!((estimate.later_seconds - (5_000.0 - 30.0) * costs.pdf_page).abs() < 1e-9);

    // A document of a few bytes is one chunk, never zero.
    let tiny = write(&root, "tiny.txt", b"hi");
    let estimate = estimator::estimate(&[candidate(&tiny, FileKind::Doc, 2)], &options, &costs);
    assert_eq!(estimate.doc_chunks, 1);
    assert_eq!(estimate.long_files, 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn estimates_group_by_the_subfolder_one_level_down() {
    let options = IndexOptions::default();
    let costs = Costs::default();
    let files = vec![
        candidate("/data/a.txt", FileKind::Doc, 2_000),
        candidate("/data/one/b.txt", FileKind::Doc, 2_000),
        candidate("/data/one/c.txt", FileKind::Doc, 2_000),
        candidate("/data/one/deep/d.txt", FileKind::Doc, 2_000),
        candidate("/data/two/e.txt", FileKind::Doc, 2_000),
    ];
    let parts = estimator::estimates_by_subfolder(&files, "/data", &options, &costs);
    let counts: BTreeMap<String, i64> = parts
        .iter()
        .map(|(key, value)| (key.clone(), value.files.values().sum()))
        .collect();
    assert_eq!(
        counts,
        [
            ("/data".to_string(), 1),
            ("/data/one".to_string(), 3),
            ("/data/two".to_string(), 1),
        ]
        .into_iter()
        .collect()
    );

    // Adding the parts is the folder's total (the CLI's `parts.values.reduce(Estimate(), +)`).
    let mut total = Estimate::default();
    for part in parts.values() {
        total.add(part);
    }
    assert_eq!(total.files.values().sum::<i64>(), 5);
    let whole = estimator::estimate(&files, &options, &costs);
    assert!(
        (total.seconds - whole.seconds).abs() < 1e-9,
        "{} vs {}",
        total.seconds,
        whole.seconds
    );

    // A trailing slash on the folder names the same groups, with the folder's own key as
    // given (`key = folder`, so "/data/" for the files right in it).
    let parts = estimator::estimates_by_subfolder(&files, "/data/", &options, &costs);
    assert_eq!(
        parts
            .iter()
            .map(|(key, value)| (key.clone(), value.files.values().sum::<i64>()))
            .collect::<BTreeMap<_, _>>(),
        [
            ("/data/".to_string(), 1),
            ("/data/one".to_string(), 3),
            ("/data/two".to_string(), 1),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn the_code_size_constants_are_the_engine_numbers() {
    assert_eq!(CHARACTERS_PER_STRETCH, 1_400);
    assert_eq!(PER_CHARACTER, 2.4);
    assert_eq!(PER_STRETCH, 1_600.0);
}

/// A PDF built the way the tail-window scan reads it: `pages` page objects at the head of
/// the file, a padding stream of `pad` bytes after them, `extra` page objects after the
/// padding (inside the last 32 KiB — the linearized shape, where page 1 lives at the file's
/// head), and a trailer that carries `/Count` only when `count` says so.
fn pdf_with_pages_at(
    root: &std::path::Path,
    name: &str,
    pages: usize,
    pad: usize,
    extra: usize,
    count: Option<usize>,
) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.4\n1 0 obj<</Type /Catalog /Pages 2 0 R>>endobj\n2 0 obj<</Type /Pages /Kids []>>endobj\n");
    for n in 0..pages {
        bytes.extend_from_slice(
            format!("{} 0 obj<</Type /Page /Parent 2 0 R>>endobj\n", 3 + n).as_bytes(),
        );
    }
    if pad > 0 {
        bytes.extend_from_slice(
            format!("{} 0 obj<</Length {}>>stream\n", 3 + pages, pad).as_bytes(),
        );
        bytes.extend_from_slice(&vec![b'a'; pad]);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    for n in 0..extra {
        bytes.extend_from_slice(
            format!(
                "{} 0 obj<</Type /Page /Parent 2 0 R>>endobj\n",
                4 + pages + n
            )
            .as_bytes(),
        );
    }
    bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f\n");
    match count {
        Some(c) => bytes.extend_from_slice(
            format!(
                "trailer\n<</Size {} /Root 1 0 R /Count {}>>\nstartxref\n0\n%%EOF\n",
                3 + pages + extra,
                c
            )
            .as_bytes(),
        ),
        None => bytes.extend_from_slice(
            format!(
                "trailer\n<</Size {} /Root 1 0 R>>\nstartxref\n0\n%%EOF\n",
                3 + pages + extra
            )
            .as_bytes(),
        ),
    }
    write(root, name, &bytes)
}

/// The fallback page count (no PDFium hook in a core-only run): what the trailer's
/// `/Count` and the tail-window token scan each answer. The trailer's `/Count` is the
/// page tree's own number and sits in the file's tail — exact whatever the file size.
/// The token scan sees only the last 32 KiB: all objects inside the window (a small
/// file) is exact; objects above the window are invisible, so a padded, trailer-less
/// PDF counts 0 and answers 1 (the "PDFium can't read it" shape), and a linearized
/// file — page 1 at the head, one page in the tail — counts only the tail's pages.
/// The shipped helper installs the PDFium hook (`native::pdf::install_pages_hook`),
/// which reads the tree and cannot undercount; this is the core-only consumer's number.
#[test]
fn the_fallback_page_count_sees_the_tail_window() {
    let root = temp_tree("digup-estimate-tail");
    let pages = 30usize;

    let exact = pdf_with_pages_at(&root, "counted.pdf", pages, 40_000, 0, Some(pages));
    assert_eq!(
        estimator::pages_of(&exact),
        pages as i64,
        "the trailer's /Count is in the tail: exact on a big file"
    );

    let small = pdf_with_pages_at(&root, "small.pdf", pages, 0, 0, None);
    assert_eq!(
        estimator::pages_of(&small),
        pages as i64,
        "a file that fits the window whole: the token count is exact"
    );

    let above = pdf_with_pages_at(&root, "above-window.pdf", pages, 40_000, 0, None);
    assert_eq!(
        estimator::pages_of(&above),
        1,
        "objects above the 32 KiB window are invisible: 0 counted answers 1, not 30"
    );

    let linear = pdf_with_pages_at(&root, "linearized.pdf", pages, 40_000, 3, None);
    assert_eq!(
        estimator::pages_of(&linear),
        3,
        "the linearized shape — pages at the head, three in the tail — counts the tail's 3"
    );
}

#[test]
fn a_probe_that_fails_after_printing_counts_as_zero() {
    // F12: Swift's `FFmpeg.run` throws on a non-zero terminationStatus AFTER
    // reading stdout (Media+Portable.swift:49-51), `try?` swallows it, and the
    // estimator's `duration(of:)` lands on `?? 0` (Media+Portable.swift:82-84).
    // A probe that prints a duration and exits non-zero (the shape of a file
    // whose header parses and whose body fails) counts as 0 to the engine.
    // The audit-base relayed the printed number: it never checked the status.
    // The scripted tool stands in for ffprobe (find_program's cache is
    // process-global, so the probe function is called directly).
    let root = temp_tree("probe-fail");
    let script = root.join("ffprobe");
    std::fs::write(&script, "#!/bin/sh\nprintf '123.4\n'\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, PermissionsExt::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    {
        // Windows: the shell script needs no exec bit; .sh is not a program, so
        // the pin runs it through cmd-visible shims - the pin is unix-shaped and
        // the Windows runner skips it (the gate box is Linux).
    }
    let fake = script.to_string_lossy().into_owned();
    assert_eq!(estimator::duration_with(&fake, "any.mp4"), 0.0);
    // A zero exit that prints the number still measures it (the same script,
    // flipped) - the fix checks the status, it does not mute the probe:
    std::fs::write(&script, "#!/bin/sh\nprintf '123.4\n'\nexit 0\n").unwrap();
    assert_eq!(estimator::duration_with(&fake, "any.mp4"), 123.4);
    let _ = std::fs::remove_dir_all(&root);
}
