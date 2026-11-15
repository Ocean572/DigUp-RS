//! PDF reader parity suite — the engine's answers, pinned (⑧d).
//!
//! Every fixture here is assembled byte-for-byte as the cross-check's fixture factory builds it
//! (`make-fixtures.py`: hand-assembled xref/trailer, RC4-40 for `locked.pdf`, the
//! Type0/Identity-H chain reading the vendored TTFs), SHA-256 pinned, and every scenario answers
//! exactly what the Swift `PDF+Portable.swift` seam answered over the same bytes through the same
//! `libpdfium.so`. The expectations ride beside this file:
//! `fixtures/pdf-answers.jsonl` (13 fixtures x 10 scenarios — pages/total/title, refusal texts,
//! the meta tags, `Estimator.pages` counts, the SHA-256 of every rendered page PNG and preview
//! JPEG) and `fixtures/pdf-sweep-answers.jsonl` (the `PageTextProblem::check` sweep over
//! `fixtures/sweep-strings.json`). `/tmp/opencode/pdfcheck/xcheck.py` measures both engines
//! live; this suite replays the measured answers against the Rust seam with nothing outside the
//! repo (the same TTFs the fixtures embed).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use digup_core::extract::ExtractError;
use digup_core::search_text::PageTextProblem;
use digup_native::pdf;

const ANSWERS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pdf-answers.jsonl"
));
const SWEEP: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/pdf-sweep-answers.jsonl"
));
const SWEEP_STRINGS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/sweep-strings.json"
));
const DEJAVU: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/DejaVuSans.ttf"
));
const NOTO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/NotoSansBengali-Regular.ttf"
));

// ---------------------------------------------------------------------------
// The fixture factory, mirrored from make-fixtures.py: every token emitted the same way.

/// One TJ-array member: a literal string (WinAnsi bytes) or a hexed UTF-16BE string.
struct Part {
    payload: Payload,
    kern: i32,
}

enum Payload {
    Lit(&'static [u8]),
    Hex(String),
}

fn lit(text: &'static [u8], kern: i32) -> Part {
    Part {
        payload: Payload::Lit(text),
        kern,
    }
}

fn hex(text: &str, kern: i32) -> Part {
    Part {
        payload: Payload::Hex(text.to_string()),
        kern,
    }
}

/// The RC4-40 pad (PDF spec's default), the locked fixture's constant.
fn pad() -> Vec<u8> {
    vec![
        0x28, 0xBF, 0x4E, 0x5E, 0x94, 0x46, 0x56, 0xDD, 0x12, 0x08, 0x9E, 0x52, 0x49, 0x62, 0xAC,
        0x8C, 0x22, 0x2C, 0x3B, 0x73,
    ]
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: usize = 0;
    for i in 0..256 {
        j = (j + s[i] as usize + key[i % key.len()] as usize) & 0xFF;
        s.swap(i, j);
    }
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0usize, 0usize);
    for byte in data {
        i = (i + 1) & 0xFF;
        j = (j + s[i] as usize) & 0xFF;
        s.swap(i, j);
        out.push(byte ^ s[(s[i] as usize + s[j] as usize) & 0xFF]);
    }
    out
}

/// RFC 1321 MD5 — the RC4-40 key derivation the locked fixture needs; `sha2` 0.11 dropped
/// MD5, and the fixture's final SHA-256 pins validate this end to end (plus the RFC's own
/// `md5("abc")` self-test below). `K[i] = floor(abs(sin(i + 1)) × 2^32)`, the reference table.
fn md5(data: &[u8]) -> [u8; 16] {
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend(bit_len.to_le_bytes());

    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];

    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    for block in message.chunks(64) {
        let mut m = [0u32; 16];
        for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
            m[index] = u32::from_le_bytes(*word);
        }
        let (mut a, mut b, mut c, mut d) = (state[0], state[1], state[2], state[3]);
        for round in 0..64 {
            let (mut f, g) = match round / 16 {
                0 => ((b & c) | (!b & d), round),
                1 => ((d & b) | (!d & c), (5 * round + 1) % 16),
                2 => (b ^ c ^ d, (3 * round + 5) % 16),
                _ => (c ^ (b | !d), (7 * round) % 16),
            };
            f = f.wrapping_add(a).wrapping_add(m[g]).wrapping_add(K[round]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[round]));
        }
        for (index, word) in [a, b, c, d].iter().enumerate() {
            state[index] = state[index].wrapping_add(*word);
        }
    }
    let mut out = [0u8; 16];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// R2 owner entry: MD5 of the padded owner truncated to 56 bytes, RC4'd with the digest's head.
fn owner_key(owner: &[u8]) -> Vec<u8> {
    let mut seed = owner.to_vec();
    seed.extend(pad());
    seed.truncate(56);
    let digest = md5(&seed);
    rc4(&digest[..5], &pad())
}

/// R2 file key: MD5 over the pad + the ID's last two bytes + the permissions' four bytes.
fn encryption_key(id_bytes: &[u8], permissions: i32, padding: &[u8]) -> Vec<u8> {
    let mut seed = padding.to_vec();
    seed.extend_from_slice(&id_bytes[6..8]);
    for shift in [0, 8, 16, 24] {
        seed.push(((permissions >> shift) & 0xFF) as u8);
    }
    let digest = md5(&seed);
    digest[..5].to_vec()
}

/// A PDF literal string: parens and the backslash escaped, every byte the factory's octal form.
fn literal(item: &[u8]) -> Vec<u8> {
    let mut out = vec![b'('];
    for byte in item {
        match byte {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(*byte);
            }
            _ if *byte < 32 || *byte > 126 => out.extend(format!("\\{byte:03o}").as_bytes()),
            _ => out.push(*byte),
        }
    }
    out.push(b')');
    out
}

/// A PDF hex string: lowercase UTF-16BE hex, `<...>`.
fn hexed(text: &str) -> Vec<u8> {
    let mut out = vec![b'<'];
    for unit in text.encode_utf16() {
        out.extend(format!("{unit:04x}").as_bytes());
    }
    out.push(b'>');
    out
}

/// One content stream: a text object per line at 12-point positions, TJ arrays with a kern.
fn content(texts: &[Vec<Part>]) -> Vec<u8> {
    let mut stream = b"BT\n/F1 12 Tf\n".to_vec();
    let mut y = 700i32;
    for parts in texts {
        stream.extend(format!("1 0 0 1 72 {y} Tm\n[").as_bytes());
        for part in parts {
            match &part.payload {
                Payload::Lit(bytes) => stream.extend(literal(bytes)),
                Payload::Hex(text) => stream.extend(hexed(text)),
            }
            stream.extend(format!(" {}] TJ\n", part.kern).as_bytes());
        }
        y -= 16;
    }
    stream.extend(b"ET\n");
    stream
}
/// The whole file: header, objects in list order, classic xref, trailer, startxref, EOF.
/// `count` is the highest object number + 1; holes get free entries, so a page naming a
/// missing object (brokenpage) reads exactly like the factory's file.
fn pdf(objects: &[Object], root: u32, info: Option<u32>, extra: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    for (number, body) in objects {
        offsets.insert(*number, out.len());
        out.extend(format!("{number} 0 obj\n").as_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let count = objects.iter().map(|(number, _)| *number).max().unwrap() + 1;
    let xref = out.len();
    out.extend(format!("xref\n0 {count}\n").as_bytes());
    out.extend(b"0000000000 65535 f \n");
    for number in 1..count {
        match offsets.get(&number) {
            Some(entry) => out.extend(format!("{entry:010} 00000 n \n").as_bytes()),
            None => out.extend(b"0000000000 65535 f \n"),
        }
    }
    let mut trailer = format!("trailer\n<<\n/Size {count}\n").into_bytes();
    trailer.extend(format!("/Root {root} 0 R\n").as_bytes());
    if let Some(info) = info {
        trailer.extend(format!("/Info {info} 0 R\n").as_bytes());
    }
    trailer
        .extend(b"/ID [<6162636465666768696a6b6c6d6e6f70> <6162636465666768696a6b6c6d6e6f70>]\n");
    trailer.extend(extra);
    trailer.extend(b">>\n");
    out.extend(trailer);
    out.extend(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn page_object(number: u32, contents: u32, mediabox: &str, font: &str) -> Object {
    (
        number,
        format!(
            "<<\n/Type /Page\n/Parent 2 0 R\n/MediaBox [{mediabox}]\n\
             /Resources << /Font << /F1 {font} 0 R >> >>\n/Contents {contents} 0 R\n>>"
        )
        .into_bytes(),
    )
}

fn stream(body: &[u8]) -> Vec<u8> {
    let mut out = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    out.extend(body);
    out.extend(b"\nendstream");
    out
}

/// Pages 10, 12, … with contents 11, 13, …; the factory's numbering. The `/F1 9 0 R` a
/// WinAnsi page names is a free xref slot — PDFium's built-in metrics, measured in the sweep.
fn pages_text(texts_per_page: &[PageLines]) -> (Vec<Object>, Vec<Object>) {
    let mut pages = Vec::new();
    let mut contents_streams = Vec::new();
    for (index, texts) in texts_per_page.iter().enumerate() {
        let page_ref = 10 + 2 * index as u32;
        let contents_ref = 11 + 2 * index as u32;
        pages.push(page_object(page_ref, contents_ref, "0 0 612 792", "9"));
        contents_streams.push((contents_ref, stream(&content(texts))));
    }
    (pages, contents_streams)
}

fn doc(
    pages: &[Object],
    fonts: &[Object],
    info: Option<Object>,
    encrypt: Option<Object>,
) -> Vec<u8> {
    let kids: Vec<String> = pages
        .iter()
        .map(|(number, _)| format!("{number} 0 R"))
        .collect();
    let mut objects = vec![
        (1u32, b"<<\n/Type /Catalog\n/Pages 2 0 R>>".to_vec()),
        (
            2,
            format!(
                "<<\n/Type /Pages\n/Kids [{}]\n/Count {}\n>>",
                kids.join(" "),
                pages.len()
            )
            .into_bytes(),
        ),
    ];
    objects.extend(pages.iter().cloned());
    objects.extend(fonts.iter().cloned());
    if let Some(info) = &info {
        objects.push(info.clone());
    }
    if let Some(encrypt) = encrypt {
        objects.push(encrypt);
    }
    let extra: Vec<u8> = if objects_last_is_encrypt(&objects) {
        b"/Encrypt 101 0 R\n".to_vec()
    } else {
        Vec::new()
    };
    pdf(&objects, 1, info.map(|(number, _)| number), &extra)
}

fn objects_last_is_encrypt(objects: &[Object]) -> bool {
    objects
        .last()
        .map(|(number, _)| *number == 101)
        .unwrap_or(false)
}

fn replace(hay: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if i + needle.len() <= hay.len() && &hay[i..i + needle.len()] == needle {
            out.extend(with);
            i += needle.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The Type0/Identity-H chain. The measured font facts ride pinned (glyph ids the vendored
// TTF's cmap answers, hmtx widths, hhea/OS-2 metrics, the exact /Length1 the TTF's table
// directory sums to) — `make-fixtures.py` reads them with fontTools, and the factory's bytes
// pin the whole file below.

type Object = (u32, Vec<u8>);
type PageLines = Vec<Vec<Part>>;
type Fixtures = std::collections::BTreeMap<String, Vec<u8>>;

struct FontSpec {
    font_bytes: usize,
    length1: u32,
    ascent: i32,
    descent: i32,
    cap: i32,
    /// (unicode scalar, glyph id = CID, /W width) for every distinct scalar of the fixture's
    /// line — the factory's `getBestCmap` + `hmtx` reading, `max(width, 100)` applied.
    glyphs: &'static [(u32, u32, u32)],
}

/// glyph.pdf: the factory's line (120 characters), and the font facts read out
/// of noto: the glyph ids its cmap answers, the hmtx widths (`max(w, 100)`),
/// hhea / OS-2 metrics, the exact /Length1 the table directory sums to.
const GLYPH_LINE: &str = "\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}\u{99A}\u{9C1}\u{995}\u{9CD}\u{9A4}\u{9BF}\u{9AA}\u{9A4}\u{9CD}\u{9B0}";
const GLYPH_FONT: FontSpec = FontSpec {
    font_bytes: 201964,
    length1: 201955,
    ascent: 917,
    descent: -408,
    cap: 622,
    glyphs: &[
        (2453, 20, 807),
        (2458, 25, 567),
        (2468, 35, 707),
        (2474, 40, 716),
        (2480, 46, 596),
        (2495, 55, 266),
        (2497, 57, 100),
        (2509, 65, 100),
    ],
};

/// letters.pdf: the factory's line (39 characters), and the font facts read out
/// of dejavu: the glyph ids its cmap answers, the hmtx widths (`max(w, 100)`),
/// hhea / OS-2 metrics, the exact /Length1 the table directory sums to.
const LETTERS_LINE: &str = "\u{627}?\u{631}\u{62F}\u{646} \u{627}.\u{628}\u{64A}\u{639}\u{627}\u{62A}\u{627}?\u{631}\u{62F}\u{646} \u{627}.\u{628}\u{64A}\u{639}\u{627}\u{62A}\u{627}?\u{631}\u{62F}\u{646} \u{627}.\u{628}\u{64A}\u{639}\u{627}\u{62A}";
const LETTERS_FONT: FontSpec = FontSpec {
    font_bytes: 759720,
    length1: 759703,
    ascent: 1901,
    descent: -483,
    cap: 1901,
    glyphs: &[
        (32, 3, 651),
        (46, 17, 651),
        (63, 34, 1087),
        (1575, 1365, 569),
        (1576, 1366, 1928),
        (1578, 1368, 1928),
        (1583, 1373, 912),
        (1585, 1375, 989),
        (1593, 1383, 1222),
        (1606, 1391, 1504),
        (1610, 1395, 1603),
    ],
};

/// letterslong.pdf: the factory's line (375 characters), and the font facts read out
/// of dejavu: the glyph ids its cmap answers, the hmtx widths (`max(w, 100)`),
/// hhea / OS-2 metrics, the exact /Length1 the table directory sums to.
const LETTERSLONG_LINE: &str = "\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{648}\u{627}\u{644}\u{645}\u{628}\u{64A}\u{639}\u{627}\u{62A} \u{628}\u{64A}\u{639}\u{62A} \u{628}\u{627}\u{644}\u{623}\u{631}\u{62F}\u{646} \u{627}?\u{631}\u{62F}\u{646} \u{627}.\u{628}\u{64A}\u{639}\u{627}\u{62A}\u{627}?\u{631}\u{62F}\u{646} \u{627}.\u{628}\u{64A}\u{639}\u{627}\u{62A}";
const LETTERSLONG_FONT: FontSpec = FontSpec {
    font_bytes: 759720,
    length1: 759703,
    ascent: 1901,
    descent: -483,
    cap: 1901,
    glyphs: &[
        (32, 3, 651),
        (46, 17, 651),
        (63, 34, 1087),
        (1571, 1361, 569),
        (1575, 1365, 569),
        (1576, 1366, 1928),
        (1578, 1368, 1928),
        (1583, 1373, 912),
        (1585, 1375, 989),
        (1593, 1383, 1222),
        (1604, 1389, 1488),
        (1605, 1390, 1268),
        (1606, 1391, 1504),
        (1608, 1393, 989),
        (1610, 1395, 1603),
    ],
};

/// /Length1 = header + directory + tables, summed EXACT (the factory's measured rule: a
/// rounded-up value past the last table fails the font, and the page with it).
fn length1(data: &[u8]) -> u32 {
    let num_tables = u16::from_be_bytes([data[4], data[5]]) as u32;
    let mut sum = 12 + 16 * num_tables;
    for index in 0..num_tables {
        let entry = 12 + 16 * index + 12; // tag, checksum, offset, LENGTH
        sum += u32::from_be_bytes([
            data[entry as usize],
            data[entry as usize + 1],
            data[entry as usize + 2],
            data[entry as usize + 3],
        ]);
    }
    sum
}

fn cid_of(spec: &FontSpec, scalar: char) -> u32 {
    spec.glyphs
        .iter()
        .find(|(unicode, _, _)| *unicode == scalar as u32)
        .map(|(_, cid, _)| *cid)
        .unwrap()
}

fn width_of(spec: &FontSpec, cid: u32) -> u32 {
    spec.glyphs
        .iter()
        .find(|(_, c, _)| *c == cid)
        .map(|(_, _, width)| *width)
        .unwrap()
}

/// The Type0 chain (objects 9 cmap / 13 FontFile2 / 14 CIDFont / 16 descriptor / 12 Type0) +
/// one page spelling `line` char by char — the reading order PDFium answers at.
fn type0_objects(line: &str, spec: &FontSpec, ttf: &[u8]) -> (Vec<Object>, Vec<Object>) {
    let mut chars: Vec<char> = line.chars().collect();
    chars.sort_unstable();
    chars.dedup();
    let mut cids: Vec<u32> = chars.iter().map(|scalar| cid_of(spec, *scalar)).collect();
    cids.sort_unstable();
    cids.dedup();

    let mut cmap = b"begincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >>\n\
                     /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\n\
                     endcodespacerange\n"
        .to_vec();
    cmap.extend(format!("{} beginbfchar\n", chars.len()).as_bytes());
    for scalar in &chars {
        cmap.extend(
            format!("<{:04X}> <{:04X}>\n", cid_of(spec, *scalar), *scalar as u32).as_bytes(),
        );
    }
    cmap.extend(b"endbfchar\nendcmap\n");

    let w_cells: Vec<String> = cids
        .iter()
        .map(|cid| format!("{cid} [{}]", width_of(spec, *cid)))
        .collect();

    let mut body = b"BT\n/F1 12 Tf\n1 0 0 1 72 700 Tm\n".to_vec();
    for scalar in line.chars() {
        body.extend(format!("[<{:04X}> 0] TJ\n", cid_of(spec, scalar)).as_bytes());
    }
    body.extend(b"ET\n");

    let pages = vec![page_object(10, 11, "0 0 612 792", "12")];
    let fonts = vec![
        (11, stream(&body)),
        (9, stream(&cmap)),
        (
            13,
            format!(
                "<< /Length {} /Length1 {} >>\nstream\n",
                ttf.len(),
                spec.length1
            )
            .into_bytes()
            .into_iter()
            .chain(ttf.iter().copied())
            .chain(b"\nendstream".iter().copied())
            .collect(),
        ),
        (
            14,
            format!(
                "<<\n/Type /Font\n/Subtype /CIDFontType2\n/BaseFont /BMG-FONT\n\
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >>\n\
                 /CIDToGIDMap /Identity\n/DW 500\n/W [{}]\n>>",
                w_cells.join(" ")
            )
            .into_bytes(),
        ),
        (
            16,
            format!(
                "<<\n/Type /FontDescriptor\n/FontName /BMG-FONT\n/Flags 4\n/FontFile2 13 0 R\n\
                 /Ascent {}\n/Descent {}\n/CapHeight {}\n/StemV 80\n/ItalicAngle 0\n>>",
                spec.ascent, spec.descent, spec.cap
            )
            .into_bytes(),
        ),
        (
            12,
            b"<<\n/Type /Font\n/Subtype /Type0\n/BaseFont /BMG-FONT\n/Encoding /Identity-H\n\
               /DescendantFonts [14 0 R]\n/ToUnicode 9 0 R\n>>"
                .to_vec(),
        ),
    ];
    (pages, fonts)
}
// ---------------------------------------------------------------------------
// The thirteen fixtures, built the factory builds them.

fn fixtures_map() -> Fixtures {
    let mut made: Fixtures = std::collections::BTreeMap::new();

    // text.pdf: three pages, the first with a TJ kern; /Title set.
    let (pages, fonts) = pages_text(&[
        vec![
            vec![lit(b"DigUp cross-check page one", 0)],
            vec![lit(b"kern", -20), lit(b"ing text line", 0)],
        ],
        vec![vec![lit(
            b"Second page: a longer line of text to see reading order.",
            0,
        )]],
        vec![vec![lit(b"Third page text.", 0)]],
    ]);
    let info = (
        100u32,
        b"<<\n/Title (DigUp Cross Check)\n/Author (Fixture Builder)\n>>".to_vec(),
    );
    made.insert("text.pdf".into(), doc(&pages, &fonts, Some(info), None));

    // blank.pdf: text, a page with no text, a page with an empty mediabox.
    let (mut pages, fonts) = pages_text(&[vec![vec![lit(b"A page with a line of text.", 0)]]]);
    pages.push((
        40,
        b"<<\n/Type /Page\n/Parent 2 0 R\n/MediaBox [0 0 612 792]\n>>".to_vec(),
    ));
    pages.push((
        41,
        b"<<\n/Type /Page\n/Parent 2 0 R\n/MediaBox [0 0 0 0]\n>>".to_vec(),
    ));
    made.insert("blank.pdf".into(), doc(&pages, &fonts, None, None));

    // longrun.pdf: 120 pages of one short line (the factory writes these lines UTF-16BE).
    let texts: Vec<Vec<Vec<Part>>> = (0..120)
        .map(|line| vec![vec![hex(&format!("page {line} line"), 0)]])
        .collect();
    let (pages, fonts) = pages_text(&texts);
    made.insert("longrun.pdf".into(), doc(&pages, &fonts, None, None));

    // glyph / letters / letterslong: the scripts, via the embedded Type0 font.
    for (name, line, spec, ttf) in [
        ("glyph.pdf", GLYPH_LINE, &GLYPH_FONT, NOTO),
        ("letters.pdf", LETTERS_LINE, &LETTERS_FONT, DEJAVU),
        (
            "letterslong.pdf",
            LETTERSLONG_LINE,
            &LETTERSLONG_FONT,
            DEJAVU,
        ),
    ] {
        assert_eq!(
            ttf.len(),
            spec.font_bytes,
            "{name}'s font is the vendored file"
        );
        assert_eq!(
            length1(ttf),
            spec.length1,
            "{name}'s /Length1 sums the table directory"
        );
        let (pages, fonts) = type0_objects(line, spec, ttf);
        made.insert(name.into(), doc(&pages, &fonts, None, None));
    }

    // brokenpage.pdf: page 1's content names a missing object (PDFium's LoadPage skip).
    let (mut pages, fonts) = pages_text(&[
        vec![vec![lit(b"page one", 0)]],
        vec![vec![lit(b"page two", 0)]],
        vec![vec![lit(b"page three", 0)]],
    ]);
    pages[0] = (
        pages[0].0,
        replace(&pages[0].1, b"/Contents 11 0 R", b"/Contents 99 0 R"),
    );
    made.insert("brokenpage.pdf".into(), doc(&pages, &fonts, None, None));

    // plain.pdf: one page, no /Info at all.
    let (pages, fonts) = pages_text(&[vec![vec![lit(b"a plain page", 0)]]]);
    made.insert("plain.pdf".into(), doc(&pages, &fonts, None, None));

    // locked.pdf: RC4-40 (V1 R2), user password empty, the fixture's constant document ID.
    let id_bytes = *b"abcdefghijklmnop";
    let key = encryption_key(&id_bytes, -1, &pad());
    let mut obj = b"<<\n/Filter /Standard\n/V 1\n/R 2\n/Length 5\n/P -1\n/O ".to_vec();
    obj.extend(literal(&owner_key(b"")));
    obj.extend(b"\n/U ");
    obj.extend(literal(&rc4(&key, &pad())));
    obj.extend(b"\n>>");
    let (pages, fonts) = pages_text(&[vec![vec![lit(b"a secret page of text", 0)]]]);
    made.insert(
        "locked.pdf".into(),
        doc(&pages, &fonts, None, Some((101, obj))),
    );

    // truncated.pdf: the xref promises a block past the end of the file.
    let whole = made.get("text.pdf").expect("text.pdf built first").clone();
    let mut data = whole[..whole.len() - 24].to_vec();
    data.extend(b"0000099999 00000 n \n");
    made.insert("truncated.pdf".into(), data);

    // brokenfont.pdf: page 2's font resource names a dictionary (PDFium's LoadPage fails:
    // the pages() skip branch).
    let (pages, mut fonts) = pages_text(&[
        vec![vec![lit(b"first", 0)]],
        vec![vec![lit(b"second", 0)]],
        vec![vec![lit(b"third", 0)]],
    ]);
    let broken_page = (
        42u32,
        "<<\n/Type /Page\n/Parent 2 0 R\n/MediaBox [0 0 612 792]\n\
         /Resources << /Font << /F1 43 0 R >> >>\n/Contents 13 0 R\n>>"
            .as_bytes()
            .to_vec(),
    );
    fonts.push((43, b"<<\n/Not really a font\n>>".to_vec()));
    let picked = vec![pages[0].clone(), broken_page, pages[2].clone()];
    made.insert("brokenfont.pdf".into(), doc(&picked, &fonts, None, None));

    made.insert("empty.pdf".into(), Vec::new());
    made.insert(
        "junk.pdf".into(),
        b"%PDF-1.7\ngarbage with no xref at all\n".to_vec(),
    );
    made
}
// ---------------------------------------------------------------------------
// The scenario runner: the same ten reads the cross-check's Swift probe ran per fixture,
// answering in the probe's JSON shape so the pinned lines compare field for field.

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_of(path: &Path) -> String {
    hex_of(&Sha256::digest(
        std::fs::read(path).expect("the picture the seam just wrote"),
    ))
}

fn write_fixtures(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("digup-pdf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    for (name, data) in fixtures_map() {
        std::fs::write(dir.join(name), data).expect("write the fixture");
    }
    dir
}

fn row(fixture: &str, scenario: &str, ok: bool) -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("fixture".into(), json!(fixture));
    row.insert("scenario".into(), json!(scenario));
    row.insert("ok".into(), json!(ok));
    row
}

fn refused(fixture: &str, scenario: &str, error: &str) -> Value {
    let mut row = row(fixture, scenario, false);
    row.insert("why".into(), json!(error));
    Value::Object(row)
}

/// One page's answer line: the engine's fields, plus the SHA-256 of the PNG the seam wrote
/// (the driver hashed the files off disk; hashing them here measures the same bytes).
fn page_row(page: &pdf::Page) -> Value {
    let mut row = Map::new();
    row.insert("garbled".into(), json!(page.garbled));
    row.insert("number".into(), json!(page.number));
    row.insert("ocr".into(), json!(page.ocr));
    row.insert("text".into(), json!(page.text));
    if let Some(image) = &page.image_path {
        row.insert("sha".into(), json!(sha256_of(Path::new(image))));
    }
    Value::Object(row)
}

fn run_scenario(fixtures: &Path, out: &Path, fixture: &str, scenario: &str) -> Value {
    let path = fixtures.join(fixture);
    let folder = out.join(fixture);
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).expect("scenario folder");

    match scenario {
        "all" | "all1" | "mid" | "none" => {
            // The engine's parameters: minText 200 with the full range, a minText-1 sweep,
            // 2..<4, and an empty range; ranges past the last page exercise the clamp.
            let (range, min_text) = match scenario {
                "all" => (0..usize::MAX, 200usize),
                "all1" => (0..usize::MAX, 1),
                "mid" => (2..4, 1),
                _ => (0..0, 1),
            };
            match pdf::pages(&path, range, min_text, 1400, &folder) {
                Ok((pages, total, title)) => {
                    let mut row = row(fixture, scenario, true);
                    row.insert(
                        "pages".into(),
                        Value::Array(pages.iter().map(page_row).collect()),
                    );
                    row.insert("total".into(), json!(total));
                    row.insert("title".into(), json!(title));
                    Value::Object(row)
                }
                Err(error) => refused(fixture, scenario, &error.to_string()),
            }
        }
        "render1" | "render2" | "render0" | "render9" => {
            let number: usize = scenario[6..].parse().unwrap();
            let preview = folder.join(format!("{scenario}.jpg"));
            match pdf::render_preview(number, &path, 480, &preview) {
                Ok(()) => {
                    let mut row = row(fixture, scenario, true);
                    row.insert("sha".into(), json!(sha256_of(&preview)));
                    Value::Object(row)
                }
                Err(error) => refused(fixture, scenario, &error.to_string()),
            }
        }
        "pages_of" => {
            let mut row = row(fixture, scenario, true);
            row.insert("count".into(), json!(pdf::pages_of(&path)));
            Value::Object(row)
        }
        "meta" => {
            let made = pdf::with_document(&path, |document| {
                let mut row = row(fixture, scenario, true);
                for tag in ["Title", "Author", "Subject"] {
                    row.insert(tag.into(), json!(pdf::meta(document, tag)));
                }
                row.insert("count".into(), json!(pdf::with_page_count(document)));
                Value::Object(row)
            });
            match made {
                Ok(value) => value,
                Err(error) => refused(fixture, scenario, &error.to_string()),
            }
        }
        other => panic!("no such scenario: {other}"),
    }
}
// ---------------------------------------------------------------------------
// The pinned suite.

/// The Rust builders must produce the factory's bytes — the digests the cross-check measured.
#[test]
fn fixture_bytes_match_the_factory() {
    let pins: &[(&str, &str)] = &[
        (
            "blank.pdf",
            "71d0e60e50caa5a476aef3a52ac977d363c719024f7bb8eda42746f1d98ef38d",
        ),
        (
            "brokenfont.pdf",
            "ba51eba1614ae8a7f4697e00f83915fa57c7ddc3106625e2721a5dc4ab22eefe",
        ),
        (
            "brokenpage.pdf",
            "19b0f755a4af1d8976910fbcd56347906b19111cb637679f448fa87fdf4f4a58",
        ),
        (
            "empty.pdf",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            "glyph.pdf",
            "a5653fe0de33485aed98b8c498b425d3fe0d1ce071b389518361fe37fac83af4",
        ),
        (
            "junk.pdf",
            "d168b02d442775914fe08d54dd092e9c1ebbab3d616984d33becb70b9eed3e0b",
        ),
        (
            "letters.pdf",
            "20b41fc83960a263ce8476315e08c11f3195bc558730565689e9933e7591ada8",
        ),
        (
            "letterslong.pdf",
            "ca5ca1e364e3372e0313290fee78cb28fa7985d6b8dfa68b43da442b3724eaac",
        ),
        (
            "locked.pdf",
            "806e173aafacc5cdd831ece99af06615bceef145e91ec7d4e9caa29cb7bcc6f7",
        ),
        (
            "longrun.pdf",
            "791235129631aca17dba2994f1e0b0f990783b9c3af9ada7d23b92787bd9177f",
        ),
        (
            "plain.pdf",
            "a889190a53364006638d1610617b4407a7f1830ac53e83f708cae01d601ba37c",
        ),
        (
            "text.pdf",
            "22027b46f54d66603fbbae489d081c99f8bd53b7709898cf8c2646fd06e214ee",
        ),
        (
            "truncated.pdf",
            "5cca4ab03653bf1164560c7e726840c77e05eb8f84731dded5aa3254b1219d95",
        ),
    ];
    let built = fixtures_map();
    assert_eq!(built.len(), pins.len(), "all thirteen fixtures");
    for (name, want) in pins {
        let data = built.get(*name).expect(name);
        assert_eq!(&hex_of(&Sha256::digest(data)), want, "the bytes of {name}");
    }
}

/// The engine's answers, every (fixture, scenario): pages/total/title, refusal texts, the meta
/// tags, `Estimator.pages` counts, and the SHA-256 of every rendered page PNG / preview JPEG.
///
/// The replay runs in a FRESH CHILD PROCESS (`--ignored --exact`), serially, exactly as the
/// cross-check driver measured the table: a live PDFium keeps a PROCESS-GLOBAL fallback-font
/// cache, and the page whose font resource is broken renders by it — measured on this box
/// (2026-11-14, `brokenfont.pdf` page 2 under scenario "all"): the FIRST render in a fresh
/// process answers `945be28e…` (the pinned bytes); any render after the cache has warmed
/// answers different pixels (`86271d6c…` when warmed by the same fixture's own first render,
/// `06f9a565…` when warmed by other fixtures' renders in a parallel-suite process). The Swift
/// driver measured every scenario once, in one serial process, starting cold — so the replay
/// must start cold too. Running the table inside the suite's own process made the pixel pins
/// depend on which sibling tests had already rendered (the suite grew pixel-rendering pins
/// over the audit; the replay silently inherited their warm state).
#[test]
fn engine_answers_match() {
    let exe = std::env::current_exe().expect("the test binary");
    let status = std::process::Command::new(exe)
        .arg("--ignored")
        .arg("--exact")
        .arg("engine_answers_match_core")
        .arg("--test-threads=1")
        .status()
        .expect("the replay child runs");
    assert!(
        status.success(),
        "the cold-process replay of the pinned answers exited nonzero (child output above)"
    );
}

/// The replay itself — runs ONLY in the driver's child process (`--ignored`).
#[test]
#[ignore = "driven by engine_answers_match in a fresh cold process"]
fn engine_answers_match_core() {
    let fixtures = write_fixtures("answers");
    let out = fixtures.join("out");
    std::fs::create_dir_all(&out).expect("out dir");
    let mut measured = 0;
    for line in ANSWERS.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let want: Value = serde_json::from_str(line).expect("an answer line");
        let fixture = want["fixture"].as_str().expect("fixture");
        let scenario = want["scenario"].as_str().expect("scenario");
        let got = run_scenario(&fixtures, &out, fixture, scenario);
        assert_eq!(got, want, "{fixture} / {scenario}");
        measured += 1;
    }
    assert_eq!(measured, 130, "13 fixtures x 10 scenarios");
}

/// `PageTextProblem::check` over the sweep's shared strings: the glyph-order thresholds
/// (vowel signs >= 10, misplaced * 20 >= vowel signs) and the lost-letters table.
#[test]
fn page_text_problem_sweep() {
    let strings: Vec<Value> = serde_json::from_str(SWEEP_STRINGS).expect("the sweep strings");
    let mut measured = 0;
    for line in SWEEP.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let want: Value = serde_json::from_str(line).expect("a sweep answer");
        let id = want["id"].as_str().expect("id");
        let text = strings
            .iter()
            .find(|entry| entry["id"] == json!(id))
            .expect(id)["text"]
            .as_str()
            .expect("text");
        let problem = match PageTextProblem::check(text) {
            None => "none",
            Some(PageTextProblem::GlyphOrder) => "glyphOrder",
            Some(PageTextProblem::LostLetters) => "lostLetters",
        };
        assert_eq!(
            problem,
            want["problem"].as_str().expect("problem"),
            "sweep {id}"
        );
        measured += 1;
    }
    assert_eq!(measured, 25, "the sweep's cases");
}

/// The suite's own primitives: the RFC 1321 MD5 test vectors the locked fixture's key needs.
#[test]
fn md5_matches_the_rfc() {
    assert_eq!(hex_of(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(hex_of(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
}

/// The seam's process-wide lock serializes concurrent readers: five threads read page counts,
/// page text, and the locked refusal at once.
#[test]
fn lock_serializes_concurrent_reads() {
    let fixtures = std::sync::Arc::new(write_fixtures("threads"));
    let out = fixtures.join("out");
    std::fs::create_dir_all(&out).expect("out dir");
    let mut workers = Vec::new();
    for worker in 0..5 {
        let fixtures = Arc::clone(&fixtures);
        let out = out.join(format!("w{worker}"));
        workers.push(std::thread::spawn(move || {
            assert_eq!(
                pdf::pages_of(&fixtures.join("longrun.pdf")),
                120,
                "worker {worker}"
            );
            std::fs::create_dir_all(&out).expect("worker folder");
            let (pages, total, title) =
                pdf::pages(&fixtures.join("text.pdf"), 2..3, 1, 1400, &out).expect("worker read");
            assert_eq!(
                (total, title.as_deref()),
                (3, Some("DigUp Cross Check")),
                "worker {worker}"
            );
            assert_eq!(pages[0].text, "Third page text.", "worker {worker}");
            let locked = pdf::pages(&fixtures.join("locked.pdf"), 0..1, 1, 1400, &out);
            assert!(
                matches!(locked, Err(ExtractError::Locked)),
                "worker {worker} stays refused"
            );
        }));
    }
    for worker in workers {
        worker.join().expect("the worker finished");
    }
}

/// The engine's `extract` is `throws`: `PDF+Portable.swift:111` runs
/// `try image.write(to: out, png: true)`, so a page whose rendered copy cannot be written
/// throws out of the extraction — the file lands `failed` with the write's message, not
/// `done` with the page's picture segment missing forever (an index that says a scanned
/// book was read, with the pictures gone). The port swallowed the write error as `.ok()?`
/// (a silent page skip). Pin: a render folder this process cannot write into — the
/// bind-mounted render folder of a container, the read-only davfs2 mount of the Nextcloud
/// pilot — must answer the refusal, in the port's "can't write …" convention
/// (media.rs:129-131, 307-309).
#[cfg(unix)]
#[test]
fn a_page_copy_that_cannot_be_written_fails_the_extraction() {
    use std::os::unix::fs::PermissionsExt;

    let dir = write_fixtures("writefail");
    let scenario = dir.join("nofolder");
    std::fs::create_dir(&scenario).expect("scenario folder");
    std::fs::set_permissions(&scenario, std::fs::Permissions::from_mode(0o500))
        .expect("the render folder becomes unwritable");

    let answer = pdf::pages(&dir.join("blank.pdf"), 0..usize::MAX, 200, 1400, &scenario);

    std::fs::set_permissions(&scenario, std::fs::Permissions::from_mode(0o700))
        .expect("the scenario folder is deletable again");
    let pages = match answer {
        Ok(pages) => pages,
        Err(digup_core::extract::ExtractError::Unreadable { why }) => {
            assert!(
                why.starts_with("can't write ") && why.ends_with(".png"),
                "the refusal names the unwritable page copy, in the port's write-failure \
                 convention: {why}"
            );
            return;
        }
        Err(other) => panic!("the refusal is the write's, in the port's convention: {other}"),
    };
    // Fails-before (the audit-base `.ok()?`): the extraction answers `Ok`, its pages carry
    // no picture paths, and not one PNG landed in the unwritable folder — the file indexes
    // `done` with the rendered segments missing, where Swift's file is `failed`.
    let drawn: Vec<&pdf::Page> = pages
        .0
        .iter()
        .filter(|page| page.image_path.is_some())
        .collect();
    let landed = std::fs::read_dir(&scenario)
        .map(|entries| entries.filter_map(|entry| entry.ok()).count())
        .unwrap_or(0);
    panic!(
        "the seam swallowed the write error: {} pages with {} pictures, {landed} files on \
         disk — Swift's extract THREW at PDF+Portable.swift:111 and the file is 'failed'",
        pages.0.len(),
        drawn.len()
    );
}

/// `minText` is a CHARACTER test (`text.count >= minText`, PDF+Portable.swift:102):
/// Foundation's `String.count` counts **grapheme clusters**, so a base letter with its
/// combining marks counts ONCE. The port tested `text.chars().count()` — scalars — and a
/// page of Devanagari/Bengali/Thai text (base + matra pairs) reads TWICE as long as the
/// engine says it is: the page the engine renders as a picture for the vision encoder
/// (its text layer "too thin"), the port keeps as a text page — no picture, no page_image
/// segment, no vision vector. The seam is real on the scripts the vendored fonts carry.
///
/// The fixture is built here (not in the pinned 13): 30 base+combining pairs = 30
/// graphemes, 60 scalars, DejaVu's own cmap answers the glyph ids (e = 72, U+0301 = 690).
/// `min_text = 31` sits exactly between the two counts:
///   graphemes 30 < 31 -> Swift RENDERS the page (picture + text);
///   scalars 60 >= 31  -> the scalar test answers a bare text page.
/// The pin is Swift's shape: `image_path` Some, text kept, `ocr` false.
#[test]
fn the_min_text_test_counts_graphemes_not_scalars() {
    let line: String = "e\u{301}".repeat(30);
    assert_eq!(line.chars().count(), 60);
    assert_eq!(digup_core::xmlscan::grapheme_len(&line), 30);
    let spec = FontSpec {
        font_bytes: 759720,
        length1: 759703,
        ascent: 1901,
        descent: -483,
        cap: 1901,
        glyphs: &[(101, 72, 1260), (769, 690, 100)],
    };
    let (pages, fonts) = type0_objects(&line, &spec, DEJAVU);
    let dir = std::env::temp_dir().join(format!("digup-pdf-combining-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("combining.pdf");
    std::fs::write(&path, doc(&pages, &fonts, None, None)).expect("write the fixture");
    let out = dir.join("renders");
    std::fs::create_dir_all(&out).expect("render folder");

    let (pages, _total, _title) = pdf::pages(&path, 0..usize::MAX, 31, 1400, &out).expect("read");
    assert_eq!(pages.len(), 1, "one page");
    let page = &pages[0];
    assert_eq!(
        digup_core::xmlscan::grapheme_len(&page.text),
        30,
        "the seam read the fixture it wrote: 30 graphemes (60 scalars)"
    );
    assert!(
        page.image_path.is_some(),
        "the engine counts CHARACTERS: 30 graphemes < the min_text 31, so the page's text \
         layer is too thin and it renders a picture for the vision encoder (the scalar test \
         answers 60 >= 31 and keeps a bare text page: no picture, no page_image segment, \
         no vision vector)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
