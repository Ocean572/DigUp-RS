//! Legacy Word `.doc` text (F-D): the OLE compound container opens with `cfb` (MIT, pure
//! Rust — the user's 2026-11-28 law keeps this reader free of C vendors), and the text comes
//! from the Word structures read directly: the FIB header, the CLX piece table, the contiguous
//! Word 6 layout. The offsets are MEASURED (2026-11-28) against the four real NIST files in
//! `/tmp/opencode/docfix` (out of git), cross-checked word-for-word against antiword 0.37:
//!
//! - FibBase: `wIdent@0`, `nFib@2`, `chse@0x14`, status flags `@0x0A`
//!   (bit 2 fast-saved, bit 8 encrypted, bit 9 = 1Table).
//! - Word 6.0/95 (`nFib` 101/102 = Word 6; 103/104 = Win95, or Mac Word 6 when `chse == 256`):
//!   contiguous 8-bit text at `fcMin@0x18` for `ccpText@0x1C` (measured on the NIST Mac
//!   files; antiword's 0x34 window there carries the total, not the main text); when
//!   fast-saved, a CLX at `fcClx@0x160/lcbClx@0x164` INSIDE WordDocument, pieces 8-bit.
//! - Word 97–2003 (`nFib >= 192`, the measured files carry 0x00C1): CLX at
//!   `fcClx@0x1A2/lcbClx@0x1A6` in the table stream, `ccpText@0x4C`; pieces UTF-16LE, or
//!   8-bit Windows-1252 when the piece's `fc` carries bit 30 (`fCompressed`, fc stored 2×).
//!
//! Named declines, measured against antiword's own refusals: pre-CFB and pre-Word-6 files
//! (`nFib` 0/28/33/35/45 — "before Word 6"); unknown `nFib`; Far-East `wIdent` (DBCS, out of
//! scope); encrypted files answer the engine's `Locked` word, `password-protected` (FIB bit 8
//! and the MS-OFFCRYPTO 2.3.4 `Encryption` stream shape a password-protected docx leaves in
//! the same container). WordPad writes RTF, which reads through the RTF reader, not here.

use std::path::Path;

use crate::extract::ExtractError;

/// The engine's cap on what one document's text stream may be, the same shape as the docx
/// entry cap: a huge stream refuses by size before it is copied.
const MAX_STREAM_BYTES: u64 = 64 * 1024 * 1024;
/// A piece table bigger than this is not a piece table (a corrupt fcClx pointer).
const MAX_CLX_BYTES: u64 = 8 * 1024 * 1024;

pub fn text(path: &Path) -> Result<String, ExtractError> {
    let mut container =
        cfb::open(path).map_err(|_| unreadable("not a Word (.doc) compound file"))?;

    let streams: Vec<String> = container
        .walk()
        .filter(|entry| entry.is_stream())
        .map(|entry| entry.name().to_string())
        .collect();

    // MS-OFFCRYPTO 2.3.4: a password-protected Word file keeps its body encrypted and adds
    // an `Encryption` stream (`Encrypted` in older files). The engine's answer is Locked —
    // the same word the zip path answers for a password-protected docx.
    if streams
        .iter()
        .any(|name| name.contains("Encrypted") || name.contains("Encryption"))
    {
        return Err(ExtractError::Locked);
    }

    if !streams.iter().any(|name| name == "WordDocument") {
        return Err(unreadable("no WordDocument stream inside"));
    }
    let word_document_len = container
        .entry("WordDocument")
        .map(|entry| entry.len())
        .unwrap_or(0);
    if word_document_len > MAX_STREAM_BYTES {
        return Err(unreadable(format!(
            "WordDocument is {} MB, more than is read",
            word_document_len / (1024 * 1024)
        )));
    }
    let word_document = read_stream(&mut container, "WordDocument")?;

    let fib = Fib::parse(&word_document)?;

    // FIB bit 8: the Word-era password flag.
    if fib.encrypted {
        return Err(ExtractError::Locked);
    }

    let text = match fib.version {
        8 => {
            // Word 97–2003: the piece table lives in the table stream the flag picked.
            let table_name = if fib.one_table { "1Table" } else { "0Table" };
            if !streams.iter().any(|name| name == table_name) {
                return Err(unreadable("no piece table in the table stream"));
            }
            let table = read_stream(&mut container, table_name)?;
            let pieces = clx_pieces(&table, fib.fc_clx, fib.lcb_clx)?;
            let mut out = String::new();
            for (cp_start, cp_end, fc, unicode) in pieces {
                if cp_start >= fib.ccp_text {
                    break;
                }
                let count = fib.ccp_text.min(cp_end) - cp_start;
                if unicode {
                    let bytes = word_document.get(fc..fc + count * 2).unwrap_or(&[]);
                    let units: Vec<u16> = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u16::from_le_bytes(*pair))
                        .collect();
                    for decoded in char::decode_utf16(units) {
                        out.push(decoded.unwrap_or(char::REPLACEMENT_CHARACTER));
                    }
                } else {
                    // fCompressed: fc is stored doubled, the bytes are Windows-1252.
                    let offset = fc / 2;
                    for &byte in word_document.get(offset..offset + count).unwrap_or(&[]) {
                        out.push(cp1252(byte));
                    }
                }
            }
            out
        }
        6 | 7 => {
            if fib.fat_saved {
                // Word 6/95 fast-saved: the CLX rides INSIDE WordDocument, pieces 8-bit.
                let pieces = clx_pieces(&word_document, fib.fc_clx, fib.lcb_clx)?;
                let mut out = String::new();
                for (cp_start, cp_end, fc, _unicode) in pieces {
                    if cp_start >= fib.ccp_text {
                        break;
                    }
                    let count = fib.ccp_text.min(cp_end) - cp_start;
                    for &byte in word_document.get(fc..fc + count).unwrap_or(&[]) {
                        out.push(cp1252(byte));
                    }
                }
                out
            } else {
                // The common Word 6/95 shape: contiguous 8-bit text from fcMin.
                let bytes = word_document
                    .get(fib.fc_min..fib.fc_min + fib.ccp_text)
                    .unwrap_or(&[]);
                bytes.iter().map(|byte| cp1252(*byte)).collect()
            }
        }
        _ => unreachable!("Fib::parse answers only 6, 7 and 8"),
    };

    Ok(normalize(&text))
}

struct Fib {
    /// 6, 7 (Word 6.0/95) or 8 (Word 97–2003) — the measured version numbers.
    version: u8,
    fat_saved: bool,
    encrypted: bool,
    one_table: bool,
    fc_min: usize,
    ccp_text: usize,
    fc_clx: usize,
    lcb_clx: usize,
}

fn unreadable(why: impl Into<String>) -> ExtractError {
    ExtractError::Unreadable { why: why.into() }
}

impl Fib {
    fn parse(word_document: &[u8]) -> Result<Fib, ExtractError> {
        if word_document.len() < 0x1A8 {
            return Err(unreadable("not a Word (.doc) compound file"));
        }
        let w_ident = u16_at(word_document, 0x00);
        let mut n_fib = u16_at(word_document, 0x02);
        if n_fib >= 0x1000 {
            // Big-endian Mac header (measured antiword rule): the version word is swapped.
            n_fib = n_fib.swap_bytes();
        }
        let chse = u16_at(word_document, 0x14);
        let flags = u16_at(word_document, 0x0A);

        // The measured version table (antiword iGetVersionNumber on this corpus):
        // 101/102 → Word 6.0; 103/104 → Win95 (chse 0) or Mac Word 6 (chse 256);
        // >= 192 → Word 97/98/2000. Everything lower is a pre-Word-6 file.
        let version = match n_fib {
            0 | 28 | 33 | 35 | 45 => {
                return Err(unreadable("Word files from before Word 6 aren't read"));
            }
            101 | 102 => 6,
            103 | 104 => {
                if chse == 256 {
                    6
                } else {
                    7
                }
            }
            // Far-East containers (DBCS, antiword needs its DBCS plugin for them):
            // out of this reader's scope, declined by name.
            _ if matches!(w_ident, 0x8098 | 0x8099 | 0xA697 | 0xA699) => {
                return Err(unreadable("Far-East Word files aren't read"));
            }
            192..=0x01FF => 8,
            other => {
                return Err(unreadable(format!(
                    "Word format 0x{other:04X} files aren't read"
                )));
            }
        };

        let (ccp_text, fc_clx, lcb_clx);
        match version {
            8 => {
                ccp_text = u32_at(word_document, 0x4C) as usize;
                fc_clx = u32_at(word_document, 0x1A2) as usize;
                lcb_clx = u32_at(word_document, 0x1A6) as usize;
            }
            _ => {
                // Measured 2026-11-28 on the NIST Mac Word 6 files (nFib 101): the pair
                // (fcMin, ccpText) sits at 0x18/0x1C — antiword's 0x34 window on these
                // files carries the TOTAL, not the main text.
                ccp_text = u32_at(word_document, 0x1C) as usize;
                fc_clx = u32_at(word_document, 0x160) as usize;
                lcb_clx = u32_at(word_document, 0x164) as usize;
            }
        }

        Ok(Fib {
            version,
            fat_saved: flags & 0x0004 != 0,
            encrypted: flags & 0x0100 != 0,
            one_table: flags & 0x0200 != 0,
            fc_min: u32_at(word_document, 0x18) as usize,
            ccp_text,
            fc_clx,
            lcb_clx,
        })
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_stream(
    container: &mut cfb::CompoundFile<std::fs::File>,
    name: &str,
) -> Result<Vec<u8>, ExtractError> {
    let mut stream = container
        .open_stream(name)
        .map_err(|_| unreadable(format!("no {name} stream inside")))?;
    let mut bytes = Vec::new();
    use std::io::Read;
    stream
        .read_to_end(&mut bytes)
        .map_err(|e| unreadable(e.to_string()))?;
    Ok(bytes)
}

/// CLX → the piece table (MS-DOC 2.4.2, layout measured in antiword findtext.c): a run of
/// `Prc` blocks (type 1: u16 length + grpprl), then the `Pcdt` (type 2: a length word,
/// 4 bytes consumed) holding Plcfpcd — (n+1) CP u32s then n Pcds (u16 descriptor, u32 fc,
/// u16 propMod). The fc bit 30 is `fCompressed`; the returned tuples are
/// `(cp_start, cp_end, fc, unicode)`.
fn clx_pieces(
    stream: &[u8],
    fc_clx: usize,
    lcb_clx: usize,
) -> Result<Vec<(usize, usize, usize, bool)>, ExtractError> {
    if lcb_clx as u64 > MAX_CLX_BYTES {
        return Err(unreadable("the piece table is too large to read"));
    }
    let clx = stream
        .get(fc_clx..fc_clx + lcb_clx)
        .ok_or_else(|| unreadable("the piece table is outside the stream"))?;

    let mut pos = 0;
    let plcfpcd = loop {
        let kind = *clx.get(pos).ok_or_else(corrupt_piece_table)?;
        match kind {
            0 => pos += 2,
            1 => {
                // Prc: style variables, not text.
                let len = u16_at(&clx[pos + 1..], 0) as usize;
                pos += 3 + len;
            }
            2 => {
                // Pcdt: type byte + a length word (measured: antiword consumes FOUR bytes
                // at the type byte — u16 lcb + 2 pad — and the Plcfpcd starts after them).
                let len = u16_at(&clx[pos + 1..], 0) as usize;
                let data = clx
                    .get(pos + 5..pos + 5 + len)
                    .ok_or_else(corrupt_piece_table)?;
                break data;
            }
            _ => return Err(corrupt_piece_table()),
        }
    };

    let len = plcfpcd.len();
    if len < 4 || (len - 4) % 12 != 0 {
        return Err(corrupt_piece_table());
    }
    let count = (len - 4) / 12;
    let mut pieces = Vec::with_capacity(count);
    for index in 0..count {
        let start = u32_at(plcfpcd, index * 4) as usize;
        let end = u32_at(plcfpcd, (index + 1) * 4) as usize;
        let fc = u32_at(plcfpcd, (count + 1) * 4 + index * 8 + 2) as usize;
        let unicode = fc & 0x4000_0000 == 0;
        let fc = fc & 0x3FFF_FFFF;
        if start >= end {
            return Err(corrupt_piece_table());
        }
        pieces.push((start, end, fc, unicode));
    }
    Ok(pieces)
}

fn corrupt_piece_table() -> ExtractError {
    unreadable("the piece table is not readable")
}

/// The document text as the engine indexes it: paragraph marks (0x0D), line/form/page
/// breaks (0x0B/0x0C) and the table cell/row marks (0x07) become newlines; field codes
/// (0x13 begin … 0x14 separator … 0x15 end) keep their RESULT and drop their code; footnote
/// marks, embedded-object marks and the layout control characters drop; tabs survive.
/// 0x20 stays a SPACE: MS-DOC 2.6 also lists it as the annotation anchor, but every real
/// file's 0x20 is the space — the synthetic gate measured a space-dropping strip range
/// 2026-11-28, and antiword reads it the same way.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut field_depth = 0usize;
    let mut in_code = false;
    for ch in text.chars() {
        match ch {
            '\u{13}' => {
                field_depth += 1;
                in_code = true;
            }
            '\u{14}' if in_code => in_code = false,
            '\u{15}' if field_depth > 0 => field_depth -= 1,
            _ if in_code => {}
            '\u{0D}' | '\u{07}' | '\u{0B}' | '\u{0C}' => out.push('\n'),
            '\u{01}' | '\u{02}' => out.push(' '),
            '\u{05}' | '\u{08}' | '\u{0E}'..='\u{12}' | '\u{16}'..='\u{1F}' => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Windows-1252 — the code page the 8-bit pieces carry (every measured file's code page;
/// Mac files read their ASCII body identically). The 0x80–0x9F slots are the 32 entries the
/// page defines beyond Latin-1; everything else equals Latin-1.
fn cp1252(byte: u8) -> char {
    match byte {
        0x80 => '\u{20AC}', // €
        0x82 => '\u{201A}', // ‚
        0x83 => '\u{0192}', // ƒ
        0x84 => '\u{201E}', // „
        0x85 => '\u{2026}', // …
        0x86 => '\u{2020}', // †
        0x87 => '\u{2021}', // ‡
        0x88 => '\u{02C6}', // ˆ
        0x89 => '\u{2030}', // ‰
        0x8A => '\u{0160}', // Š
        0x8B => '\u{2039}', // ‹
        0x8C => '\u{0152}', // Œ
        0x8E => '\u{017D}', // Ž
        0x91 => '\u{2018}', // '
        0x92 => '\u{2019}', // '
        0x93 => '\u{201C}', // “
        0x94 => '\u{201D}', // ”
        0x95 => '\u{2022}', // •
        0x96 => '\u{2013}', // –
        0x97 => '\u{2014}', // —
        0x98 => '\u{02DC}', // ˜
        0x99 => '\u{2122}', // ™
        0x9A => '\u{0161}', // š
        0x9B => '\u{203A}', // ›
        0x9C => '\u{0153}', // œ
        0x9E => '\u{017E}', // ž
        0x9F => '\u{0178}', // Ÿ
        _ => char::from(byte),
    }
}
