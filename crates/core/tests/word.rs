//! F-D (the legacy Word `.doc` reader): the piece-table decode for both measured layouts,
//! the named declines, and — gated on the box's fixture folder — the four real NIST files.
//!
//! Fixture law (user, 2026-11-28): NO `.doc` fixture is in git. The synthetic files below
//! are built IN CODE (the `cfb` writer lays the container out), so CI runs them with zero
//! downloads. The real-corpus suite reads `$DIGUP_DOCFIX` and SKIPS with a printed note
//! when the env is unset — it fetches nothing.
//!
//! The layouts are the measured ones (crates/core/src/word.rs header): Word 97 keeps its
//! CLX in the table stream at fcClx 0x1A2; Word 6/95 stores contiguous 8-bit text from
//! fcMin 0x18 (fast-saved: a CLX at 0x160 INSIDE WordDocument).

use std::io::Write;
use std::path::Path;

use digup_core::doc::text_of;
use digup_core::word;

/// The synthetic Word 97 file writer: FibBase (wIdent 0xA5EC, nFib 0x00C1, flags bit 9 =
/// 1Table) + `ccpText@0x4C` + `fcClx@0x1A2/lcbClx@0x1A6`; the CLX (a Prc and a Pcdt) laid
/// at fcClx inside 1Table; text pieces in WordDocument from 0x600. `pieces` are
/// (bytes as stored in WordDocument, fCompressed) — the character count is the byte length
/// for a compressed piece, half of it for a UTF-16LE piece; a compressed piece's `fc` is
/// stored as 2× the byte offset, the engine's own doubling.
fn build_word97(path: &Path, pieces: &[(&[u8], bool)]) {
    let clx = build_clx(pieces, 0x600, true);
    let lcb_clx = clx.len();

    let mut word_document = vec![0u8; 0x800];
    word_document[0x00..2].copy_from_slice(&0xA5ECu16.to_le_bytes()); // wIdent
    word_document[0x02..4].copy_from_slice(&0x00C1u16.to_le_bytes()); // nFib: Word 97
    word_document[0x0A..12].copy_from_slice(&0x0200u16.to_le_bytes()); // 1Table
    word_document[0x0C..14].copy_from_slice(&1033u16.to_le_bytes()); // code page 1252
    let text_area = pieces_area(pieces, 0x600);
    word_document[0x600..0x600 + text_area.len()].copy_from_slice(&text_area);
    word_document[0x4C..0x50].copy_from_slice(&(total_chars(pieces) as u32).to_le_bytes()); // ccpText
    word_document[0x1A2..0x1A6].copy_from_slice(&0u32.to_le_bytes()); // fcClx (start of 1Table)
    word_document[0x1A6..0x1AA].copy_from_slice(&(lcb_clx as u32).to_le_bytes()); // lcbClx

    write_cfb(path, &[("WordDocument", &word_document), ("1Table", &clx)]);
}

/// The synthetic Word 6.0 file writer: wIdent 0xA5DC, nFib 101 (Word 6), contiguous 8-bit
/// text from fcMin `@0x18` for `ccpText@0x34` — the layout the NIST Mac files carry.
fn build_word6(path: &Path, text: &str, fast_saved: bool) {
    let bytes = text.as_bytes(); // synthetic 6.0 text is ASCII — the 8-bit shape
    let mut word_document = vec![0u8; 0x800 + bytes.len()];
    word_document[0x00..2].copy_from_slice(&0xA5DCu16.to_le_bytes()); // wIdent
    word_document[0x02..4].copy_from_slice(&101u16.to_le_bytes()); // nFib: Word 6.0
    word_document[0x18..28].copy_from_slice(&0x800u32.to_le_bytes()); // fcMin
    word_document[0x1C..0x20].copy_from_slice(&(bytes.len() as u32).to_le_bytes()); // ccpText
    word_document[0x800..].copy_from_slice(bytes);
    if fast_saved {
        // Fast-saved: the CLX rides INSIDE WordDocument, pieces 8-bit, fc = byte offset.
        let clx = build_clx(&[(bytes, true)], 0x800, false);
        let at = 0x1000;
        word_document.extend_from_slice(&vec![0u8; at - word_document.len()]);
        let fc_clx = word_document.len();
        word_document.extend_from_slice(&clx);
        word_document[0x0A..12].copy_from_slice(&0x0004u16.to_le_bytes()); // bit 2: fast-saved
        word_document[0x160..0x164].copy_from_slice(&(fc_clx as u32).to_le_bytes()); // fcClx
        word_document[0x164..0x168].copy_from_slice(&(clx.len() as u32).to_le_bytes());
        // lcbClx
    }
    write_cfb(path, &[("WordDocument", &word_document)]);
}

/// Plcfpcd bytes + the Prc/Pcdt envelope, mapping `pieces` onto a stream from `text_at`.
/// `word97` selects the engine's conventions: a compressed Word 97 piece stores `fc` DOUBLED
/// with bit 30; a Unicode Word 97 piece stores it raw (UTF-16LE); Word 6 fast-saved pieces
/// are 8-bit with `fc` RAW (the measured shape carries no compression flag).
fn build_clx(pieces: &[(&[u8], bool)], text_at: usize, word97: bool) -> Vec<u8> {
    let mut descriptors = Vec::new();
    let mut offset = text_at;
    let mut cp = 0usize;
    for (bytes, compressed) in pieces {
        let count = if *compressed || !word97 {
            bytes.len()
        } else {
            bytes.len() / 2
        };
        let fc = if word97 && *compressed {
            (offset * 2) | 0x4000_0000
        } else {
            offset
        };
        descriptors.push((cp, fc));
        cp += count;
        offset += bytes.len();
    }

    let mut plcfpcd = Vec::new();
    for (start, _) in &descriptors {
        plcfpcd.extend_from_slice(&(*start as u32).to_le_bytes());
    }
    plcfpcd.extend_from_slice(&(cp as u32).to_le_bytes());
    for (_, fc) in &descriptors {
        plcfpcd.extend_from_slice(&[0x00, 0x00]); // descriptor
        plcfpcd.extend_from_slice(&(*fc as u32).to_le_bytes());
        plcfpcd.extend_from_slice(&[0x00, 0x00]); // propMod
    }
    assert_eq!(
        (plcfpcd.len() - 4) % 12,
        0,
        "Plcfpcd is (n+1) CPs + n 8-byte Pcds"
    );

    let mut clx = Vec::new();
    clx.push(0x01); // Prc: style variables, Word always carries one
    clx.extend_from_slice(&4u16.to_le_bytes());
    clx.extend_from_slice(&[0x00; 4]);
    clx.push(0x02); // Pcdt
    clx.extend_from_slice(&(plcfpcd.len() as u16).to_le_bytes());
    clx.extend_from_slice(&[0x00, 0x00]); // the 4-byte length word the measured readers consume
    clx.extend_from_slice(&plcfpcd);
    clx
}

fn pieces_area(pieces: &[(&[u8], bool)], _at: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for (bytes, _) in pieces {
        out.extend_from_slice(bytes);
    }
    out
}

fn total_chars(pieces: &[(&[u8], bool)]) -> usize {
    pieces
        .iter()
        .map(|(bytes, compressed)| {
            if *compressed {
                bytes.len()
            } else {
                bytes.len() / 2
            }
        })
        .sum()
}

/// The container writer: `cfb`'s own create path, so the file is a byte-true compound file.
fn write_cfb(path: &Path, streams: &[(&str, &[u8])]) {
    let mut container = cfb::create(path).unwrap();
    for (name, bytes) in streams {
        let mut stream = container.create_stream(name).unwrap();
        stream.write_all(bytes).unwrap();
    }
    container.flush().unwrap();
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[test]
fn a_synthetic_word97_file_answers_its_pieces() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.doc");

    build_word97(
        &path,
        &[
            // A Unicode piece: paragraphs end at 0x0D.
            (&utf16("First paragraph\nSecond\n"), false),
            // A compressed piece: cp1252 bytes (é = 0xE9, em-dash = 0x97).
            (b"caf\xe9 \x97 done\n", true),
            // Field begin/separator/end: the code is dropped, the RESULT survives.
            (&utf16("\u{13}CODE\u{14}RESULT\u{15}"), false),
            // A table cell mark (0x07) becomes a newline; footnote marks vanish.
            (&utf16("row\u{07}foot\u{1F}note"), false),
        ],
    );

    let got = text_of(&path).unwrap();
    assert_eq!(
        got,
        "First paragraph\nSecond\ncafé — done\nRESULTrow\nfootnote"
    );
}

#[test]
fn the_engine_opens_a_word97_file_through_the_doc_extractor() {
    // The seam: doc.rs routes ext "doc" to `word::text` — the extractor answers the text,
    // not the old decline.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("seam.doc");
    build_word97(&path, &[(&utf16("Seam text\r"), false)]);
    assert_eq!(text_of(&path).unwrap(), "Seam text\n");
}

#[test]
fn a_word6_file_answers_its_contiguous_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w6.doc");
    build_word6(&path, "Word six text\rsecond line\n", false);
    assert_eq!(text_of(&path).unwrap(), "Word six text\nsecond line\n");
}

#[test]
fn a_word6_fast_saved_file_answers_its_pieces() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w6fs.doc");
    build_word6(&path, "Fast saved\rpieces\n", true);
    assert_eq!(text_of(&path).unwrap(), "Fast saved\npieces\n");
}

#[test]
fn an_encrypted_word_file_refuses_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.doc");
    let mut fib = vec![0u8; 0x800];
    fib[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    fib[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
    fib[0x0A..12].copy_from_slice(&0x0200u16.to_le_bytes());
    write_cfb(
        &path,
        &[
            ("WordDocument", &fib),
            ("\u{6}Encrypted", &[0u8; 8]), // MS-OFFCRYPTO 2.3.4: the password stream
        ],
    );
    let why = word::text(&path).unwrap_err().to_string();
    assert_eq!(why, "password-protected");
}

#[test]
fn the_fib_password_flag_refuses_by_name_too() {
    // Word 97's own password shape: FIB flags bit 8, no Encryption stream.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flagged.doc");
    let mut fib = vec![0u8; 0x800];
    fib[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    fib[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
    fib[0x0A..12].copy_from_slice(&0x0300u16.to_le_bytes()); // 1Table + encrypted
    write_cfb(&path, &[("WordDocument", &fib), ("1Table", &[0u8; 16])]);
    let why = word::text(&path).unwrap_err().to_string();
    assert_eq!(why, "password-protected");
}

#[test]
fn a_pre_cfb_word_file_refuses_at_the_container() {
    // Word 2/5/Mac: not a compound file. The engine's refusal, measured here — and the
    // pin tests/doc.rs carries for the header-junk file quotes this exact word.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.doc");
    std::fs::write(
        &path,
        b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1WordDocumentjunkjunk",
    )
    .unwrap();
    let why = text_of(&path).unwrap_err().to_string();
    assert_eq!(why, "unreadable: not a Word (.doc) compound file");
}

#[test]
fn a_pre_word6_fib_refuses_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("word2.doc");
    let mut fib = vec![0u8; 0x800];
    fib[0..2].copy_from_slice(&0xA5DBu16.to_le_bytes());
    fib[2..4].copy_from_slice(&45u16.to_le_bytes()); // nFib 45: Word 2 for Windows
    write_cfb(&path, &[("WordDocument", &fib)]);
    let why = word::text(&path).unwrap_err().to_string();
    assert_eq!(why, "unreadable: Word files from before Word 6 aren't read");
}

#[test]
fn an_unknown_fib_version_refuses_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("odd.doc");
    let mut fib = vec![0u8; 0x800];
    fib[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    fib[2..4].copy_from_slice(&150u16.to_le_bytes()); // 46..=100 / 105..=191: unknown
    write_cfb(&path, &[("WordDocument", &fib)]);
    let why = word::text(&path).unwrap_err().to_string();
    assert_eq!(why, "unreadable: Word format 0x0096 files aren't read");
}

/// The four real NIST files (out of git: `$DIGUP_DOCFIX`): two Word 97 (nFib 0x00C1) and
/// two Mac Word 6 (nFib 0x0065). The pins quote the Rust reader's own measured answers,
/// cross-checked 2026-11-28 against antiword 0.37 (`-m UTF-8.txt`) on this box. The suite
/// SKIPS with a printed note when the env is unset — CI fetches nothing.
#[test]
fn the_nist_corpus_decodes_and_carries_its_measured_lines() {
    let Some(root) = std::env::var_os("DIGUP_DOCFIX") else {
        eprintln!("note: DIGUP_DOCFIX unset — the real .doc corpus suite skipped");
        return;
    };
    let root = Path::new(&root);
    let cases = [
        (
            "arch31.doc",
            120_000,
            &["TIPSTER Text Architecture Design"][..],
        ),
        ("cm13.doc", 40_000, &["Configuration Management Plan"]),
        (
            "con112.doc",
            60_000,
            &["TIPSTER Text Phase II Architecture Concept"],
        ),
        ("req201.doc", 8_000, &["Prepared by"]),
    ];
    for (name, min_len, lines) in cases {
        let path = root.join(name);
        if !path.exists() {
            eprintln!("note: {} missing under DIGUP_DOCFIX — case skipped", name);
            continue;
        }
        let text = text_of(&path).expect(name);
        assert!(text.len() > min_len, "{name}: {} bytes of text", text.len());
        for line in lines {
            assert!(text.contains(line), "{name}: missing {line:?}");
        }
    }
}
