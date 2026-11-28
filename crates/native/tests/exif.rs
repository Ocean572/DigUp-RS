//! `EXIF.read` and `ImageExtractor.prepare`, pinned against the engine's own answers
//! (`Platform/Portable/EXIF.swift`, `Images+Portable.swift`).
//!
//! The wide gate is the picture cross-check (`/tmp/opencode/imagecheck`: 114 fixtures — the 65
//! Pillow picture files, 49 hand-built EXIF fixtures — read through the engine's Swift code and
//! the port side by side in three ffmpeg modes (the pinned ffmpeg 4.4.2, none, a tool that will
//! not start): four `prepare` scenarios each (PNG, JPEG, the `minSide` gate, the #182 budget),
//! with every prepared copy and pixel buffer compared by SHA-256: **114 fixtures, 0 differing**,
//! over 1000 files per side.
//!
//! These tests carry the fixtures inside the workspace: a Swift generator made each file with a
//! deterministic writer (the pixel formula, stb's own PNG/JPEG writer, hand-written TIFF/BMP/TGA
//! layouts, an EXIF TIFF block, HEIF boxes), and reported what the engine's EXIF reader and
//! `prepare` answered. The expected answers, the fixture digests, and the SHA-256 of every
//! prepared copy here are those answers.

use std::path::{Path, PathBuf};

use digup_native::{exif, image, rgb};
use sha2::{Digest, Sha256};

const EXIF_DATE: &str = "2026:11:11 12:34:56";

fn pixels(w: usize, h: usize, seed: i64) -> rgb::RgbImage {
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let p = (y * w + x) * 3;
            let (x, y) = (x as i64, y as i64);
            out[p] = ((x * 7 + y * 13 + seed) % 256) as u8;
            out[p + 1] = ((x * 3 + (y * y) / 3 + seed) % 256) as u8;
            out[p + 2] = ((x * 11 + y * 2 + seed * 7) % 256) as u8;
        }
    }
    rgb::RgbImage {
        width: w,
        height: h,
        pixels: out,
    }
}

fn le16(v: i64) -> Vec<u8> {
    ((v as u16).to_le_bytes()).to_vec()
}
fn le32(v: i64) -> Vec<u8> {
    ((v as u32).to_le_bytes()).to_vec()
}
fn be16(v: i64) -> Vec<u8> {
    ((v as u16).to_be_bytes()).to_vec()
}
fn be32(v: i64) -> Vec<u8> {
    ((v as u32).to_be_bytes()).to_vec()
}
fn s16(v: i64, little: bool) -> Vec<u8> {
    if little {
        le16(v)
    } else {
        be16(v)
    }
}
fn s32(v: i64, little: bool) -> Vec<u8> {
    if little {
        le32(v)
    } else {
        be32(v)
    }
}

/// The EXIF TIFF block: header at 0, IFD0 at 8 with 274 (orientation, an inline SHORT) and 0x8769
/// (the Exif IFD pointer) in tag order, then the Exif IFD's single 0x9003 DateTimeOriginal entry
/// (19 ASCII chars stored after it). Offsets count from the TIFF header, as `parse` resolves them.
fn exif_block(orientation: Option<i64>, date: bool, little: bool) -> Vec<u8> {
    let mut rows: Vec<(i64, i64, i64, Vec<u8>)> = Vec::new();
    if let Some(o) = orientation {
        let mut field = s16(o, little);
        field.extend([0, 0]);
        rows.push((274, 3, 1, field));
    }
    if date {
        rows.push((
            0x8769,
            4,
            1,
            s32(8 + 2 + 12 * (rows.len() as i64 + 1) + 4, little),
        ));
    }
    let exif_ifd = 8 + 2 + 12 * rows.len() as i64 + 4;
    let mut out: Vec<u8> = if little {
        vec![0x49, 0x49]
    } else {
        vec![0x4D, 0x4D]
    };
    out.extend(s16(42, little));
    out.extend(s32(8, little));
    out.extend(s16(rows.len() as i64, little));
    for (tag, kind, items, field) in rows {
        let mut f = field;
        while f.len() < 4 {
            f.push(0);
        }
        out.extend(s16(tag, little));
        out.extend(s16(kind, little));
        out.extend(s32(items, little));
        out.extend(f);
    }
    out.extend(s32(0, little)); // no more IFDs after IFD0
    if date {
        out.extend(s16(1, little));
        out.extend(s16(0x9003, little));
        out.extend(s16(2, little));
        out.extend(s32(19, little));
        out.extend(s32(exif_ifd + 2 + 12 + 4, little));
        out.extend(s32(0, little));
        out.extend(EXIF_DATE.as_bytes());
    }
    out
}

/// A full-file TIFF: the baseline strip layout, with the orientation tag, an Exif IFD with
/// DateTimeOriginal, tile tags (TileWidth is what `EXIF.tiled` flags), and the "MM" spelling.
fn tiff(
    source: &rgb::RgbImage,
    orientation: Option<i64>,
    date: bool,
    tiled: bool,
    little: bool,
) -> Vec<u8> {
    let mut rows: Vec<(i64, i64, i64, Vec<u8>)> = vec![
        (256, 3, 1, s16(source.width as i64, little)),
        (257, 3, 1, s16(source.height as i64, little)),
        (258, 3, 3, Vec::new()), // BitsPerSample: three SHORTs, outside the IFD
        (259, 3, 1, s16(if tiled { 8 } else { 1 }, little)),
        (262, 3, 1, s16(2, little)),
        (273, 4, 1, Vec::new()),
    ];
    if let Some(o) = orientation {
        let mut field = s16(o, little);
        field.extend([0, 0]);
        rows.push((274, 3, 1, field));
    }
    rows.extend([
        (277, 3, 1, s16(3, little)),
        (278, 3, 1, s16(source.height as i64, little)),
        (
            279,
            4,
            1,
            s32((source.width * source.height * 3) as i64, little),
        ),
        (284, 3, 1, s16(1, little)),
    ]);
    if tiled {
        rows.push((322, 3, 1, s16(source.width as i64, little))); // TileWidth: the flag
        rows.push((323, 3, 1, s16(source.height as i64, little)));
        rows.push((324, 4, 1, Vec::new()));
        rows.push((
            325,
            4,
            1,
            s32((source.width * source.height * 3) as i64, little),
        ));
    }
    if date {
        rows.push((0x8769, 4, 1, Vec::new()));
    }
    let strip = 8 + 2 + 12 * rows.len() as i64 + 4;
    let bits = strip + source.pixels.len() as i64;
    let exif_ifd = bits + 6;
    let mut out: Vec<u8> = if little {
        vec![0x49, 0x49]
    } else {
        vec![0x4D, 0x4D]
    };
    out.extend(s16(42, little));
    out.extend(s32(8, little));
    out.extend(s16(rows.len() as i64, little));
    for (tag, kind, items, field) in rows {
        let mut f = field;
        match tag {
            258 => f = s32(bits, little),
            273 | 324 => f = s32(strip, little),
            0x8769 => f = s32(exif_ifd, little),
            _ => {
                while f.len() < 4 {
                    f.push(0);
                }
            }
        }
        out.extend(s16(tag, little));
        out.extend(s16(kind, little));
        out.extend(s32(items, little));
        out.extend(f);
    }
    out.extend(s32(0, little)); // no next IFD
    out.extend(source.pixels.iter().copied());
    out.extend(s16(8, little));
    out.extend(s16(8, little));
    out.extend(s16(8, little));
    if date {
        out.extend(s16(1, little));
        out.extend(s16(0x9003, little));
        out.extend(s16(2, little));
        out.extend(s32(19, little));
        out.extend(s32(exif_ifd + 2 + 12 + 4, little));
        out.extend(s32(0, little));
        out.extend(EXIF_DATE.as_bytes());
    }
    out
}

/// stb's own JPEG with an APP1 spliced after FFD8 (marker, length, "Exif\\0\\0", the TIFF block).
/// `lie` makes the segment's length claim more than it holds — `EXIF.jpeg` reads it bounded by
/// the file's real end, while stb's marker walk follows the length it states.
fn jpeg_app1(
    folder: &Path,
    name: &str,
    source: &rgb::RgbImage,
    orientation: Option<i64>,
    date: bool,
    lie: i64,
) -> Vec<u8> {
    let tmp = folder.join(format!("{name}.tmp.jpg"));
    source.write(&tmp, false).expect("stb writes a JPEG");
    let raw = std::fs::read(&tmp).expect("the JPEG stb wrote");
    let _ = std::fs::remove_file(&tmp);
    let mut out: Vec<u8> = raw[..2].into(); // FFD8
    if orientation.is_some() || date {
        let block = exif_block(orientation, date, true);
        out.extend([0xFF, 0xE1]);
        out.extend(be16(2 + 6 + block.len() as i64 + lie));
        out.extend([0x45, 0x78, 0x69, 0x66, 0, 0]);
        out.extend(block);
    }
    out.extend(raw[2..].iter().copied());
    out
}

/// The huge-header JPEG: JFIF APP0 + DQT + SOF0 32767x32767 (the widest stb accepts) + a TRAILING
/// APP1 whose length lies + EOI. `stbi_info` stops at the SOF so the header answers 1073 MP; the
/// decode has no scan; the EXIF read finds the trailing segment bounded by the file.
fn huge_jpeg() -> Vec<u8> {
    let block = exif_block(Some(6), true, true);
    let mut out: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    out.extend(b"JFIF\0");
    out.extend([1, 1, 0, 0, 72, 0, 72, 0, 0]);
    out.extend([0xFF, 0xDB, 0x00, 0x43, 0x00]);
    out.extend((1u8..=64).collect::<Vec<u8>>());
    out.extend([0xFF, 0xC0, 0x00, 0x11, 0x08, 0x7F, 0xFF, 0x7F, 0xFF, 0x03]);
    out.extend([0x11, 0x11, 0x00, 0x22, 0x11, 0x01, 0x33, 0x11, 0x01]);
    out.extend([0xFF, 0xE1]);
    out.extend(be16(2 + 6 + block.len() as i64 + 40));
    out.extend([0x45, 0x78, 0x69, 0x66, 0, 0]);
    out.extend(block);
    out.extend([0xFF, 0xD9]);
    out
}

fn box_at(type_name: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = be32(8 + body.len() as i64);
    out.extend(type_name);
    out.extend(body);
    out
}

/// The hand-built HEIF/AVIF: ftyp + meta (iinf naming one Exif item, iloc placing its bytes at
/// the file's end) + the item (4-byte TIFF offset, two filler bytes, the EXIF block).
fn heif(major: &str, orientation: Option<i64>, date: bool) -> Vec<u8> {
    let block = exif_block(orientation, date, true);
    let mut item: Vec<u8> = vec![0, 0, 0, 2, 0, 0];
    item.extend(&block);
    let mut infe_body: Vec<u8> = vec![3, 0, 0, 0];
    infe_body.extend(be32(1));
    infe_body.extend(be16(0));
    infe_body.extend(b"Exif");
    let infe = box_at(b"infe", &infe_body);
    let mut iinf_body: Vec<u8> = vec![0, 0, 0, 0];
    iinf_body.extend(be16(1));
    iinf_body.extend(&infe);
    let iinf = box_at(b"iinf", &iinf_body);
    let mut ftyp_body: Vec<u8> = major.as_bytes().into();
    ftyp_body.extend(be32(0));
    ftyp_body.extend(major.as_bytes());
    ftyp_body.extend(major.as_bytes());
    let ftyp = box_at(b"ftyp", &ftyp_body);
    let mut iloc_body: Vec<u8> = vec![1, 0, 0, 0, 0x44, 0x00];
    iloc_body.extend(be16(1));
    iloc_body.extend(be16(1));
    iloc_body.extend(be16(0));
    iloc_body.extend(be16(0));
    iloc_body.extend(be16(1));
    iloc_body.extend(be32(0)); // placeholder: the extent offset is filled below
    iloc_body.extend(be32(item.len() as i64));
    let iloc_len = 8 + iloc_body.len() as i64;
    let offset = ftyp.len() as i64 + 8 + 4 + iinf.len() as i64 + iloc_len;
    let at = iloc_body.len() - 8;
    iloc_body[at..at + 4].copy_from_slice(&be32(offset));
    let iloc = box_at(b"iloc", &iloc_body);
    let mut meta_body: Vec<u8> = vec![0, 0, 0, 0];
    meta_body.extend(&iinf);
    meta_body.extend(&iloc);
    let mut out = ftyp;
    out.extend(box_at(b"meta", &meta_body));
    out.extend(item);
    out
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Foundation's `UUID().uuidString` shape (pinned by `crates/core/tests/uuid.rs`): 36 UPPERCASE
/// canonical chars, version 4, RFC 4122 variant, dashes at 8/13/18/23 — the engine names every
/// prepared copy this way.
fn is_uuid_name(name: &str) -> bool {
    let characters: Vec<char> = name.chars().collect();
    characters.len() == 36
        && characters.iter().enumerate().all(|(index, character)| {
            if [8, 13, 18, 23].contains(&index) {
                *character == '-'
            } else {
                character.is_ascii_hexdigit() && !character.is_ascii_lowercase()
            }
        })
        && characters[14] == '4'
        && "89AB".contains(characters[19])
}

fn fixture_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-exif-{name}"));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// The scenario parameters the cross-check probe runs (the Swift side reads the same table).
fn scenario_parameters(label: &str) -> (i64, u64, bool) {
    match label {
        "s1" => (40, 0, true),
        "s2" => (40, 0, false),
        "s3" => (1000, 0, true),
        "s4" => (40, 1_000_000, true),
        _ => panic!("no such scenario: {label}"),
    }
}

/// A fixture, the recipe that builds it, and everything the engine answered about it.
struct Fixture {
    name: &'static str,
    sha: &'static str,
    recipe: Recipe,
    exif: Option<(i64, Option<&'static str>, bool)>,
    scenarios: &'static [Scenario],
}

enum Recipe {
    Tiff {
        w: usize,
        h: usize,
        seed: i64,
        orientation: Option<i64>,
        date: bool,
        tiled: bool,
        little: bool,
    },
    Jpeg {
        w: usize,
        h: usize,
        seed: i64,
        orientation: Option<i64>,
        date: bool,
        lie: i64,
    },
    Heif {
        major: &'static str,
        orientation: Option<i64>,
        date: bool,
    },
    Huge,
    Raw,
    Png {
        w: usize,
        h: usize,
        seed: i64,
    },
}

/// One `prepare` scenario: the label picks the parameters (minSide / budget / png), and the
/// engine answered either a copy (its name is "U.<ext>": the UUID shape is pinned by
/// `is_uuid_name`, `crates/core/tests/uuid.rs` measures it) or a refusal text.
struct Scenario {
    label: &'static str,
    ok: bool,
    w: i64,
    h: i64,
    taken: Option<&'static str>,
    sha: &'static str,
    why: &'static str,
}

fn build_fixture(folder: &Path, fixture: &Fixture) -> Vec<u8> {
    let path = folder.join(fixture.name);
    let bytes = match &fixture.recipe {
        Recipe::Tiff {
            w,
            h,
            seed,
            orientation,
            date,
            tiled,
            little,
        } => tiff(&pixels(*w, *h, *seed), *orientation, *date, *tiled, *little),
        Recipe::Jpeg {
            w,
            h,
            seed,
            orientation,
            date,
            lie,
        } => jpeg_app1(
            folder,
            fixture.name,
            &pixels(*w, *h, *seed),
            *orientation,
            *date,
            *lie,
        ),
        Recipe::Heif {
            major,
            orientation,
            date,
        } => heif(major, *orientation, *date),
        Recipe::Huge => huge_jpeg(),
        Recipe::Raw => b"Not a RAW picture, just its name for the refusal.\n".to_vec(),
        Recipe::Png { w, h, seed } => {
            pixels(*w, *h, *seed)
                .write(&path, true)
                .expect("stb writes a PNG");
            return std::fs::read(&path).expect("the PNG stb wrote");
        }
    };
    std::fs::write(&path, &bytes).expect("write the fixture");
    bytes
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "tif-full.tif", sha: "d0a40735092ad217de56784628323f50d9a365ffab57c6ba84f2e20d800b25b5",
        recipe: Recipe::Tiff { w: 40, h: 24, seed: 0, orientation: Some(6), date: true, tiled: false, little: true },
        exif: Some((6, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[],
    },

    Fixture {
        name: "tif-mm.tif", sha: "b5e7dd65f578f5df1ff886fe2bd079ea2fb35124d076ab2ca8a80cd1e823dc35",
        recipe: Recipe::Tiff { w: 24, h: 16, seed: 44, orientation: Some(3), date: true, tiled: false, little: false },
        exif: Some((3, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[],
    },

    Fixture {
        name: "tif-plain.tif", sha: "be140331d75d8ffa4b2c4385ff55f0dcc5eb576a2478b7ffb25b4a0b8d014079",
        recipe: Recipe::Tiff { w: 40, h: 24, seed: 0, orientation: None, date: false, tiled: false, little: true },
        exif: Some((1, None, false)),
        scenarios: &[],
    },

    Fixture {
        name: "tif-tiled.tif", sha: "40930628457414cb64f757c8b9b52236d1beef722ef260b21c902733fba7c2b1",
        recipe: Recipe::Tiff { w: 20, h: 12, seed: 7, orientation: Some(6), date: false, tiled: true, little: true },
        exif: Some((6, None, true)),
        scenarios: &[Scenario { label: "s1", ok: false, w: 0, h: 0, taken: None, sha: "", why: "unreadable: tiled TIFF pictures aren't read on this system yet" }],
    },

    Fixture {
        name: "heic-exif.heic", sha: "0f8cf9d3b46637497725082d8340e311ed0fe064c6a0fe94fa6965113be77b61",
        recipe: Recipe::Heif { major: "heic", orientation: Some(6), date: true },
        exif: Some((6, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[],
    },

    Fixture {
        name: "avif-plain.avif", sha: "1d2cfff30288841c49933c05848085aa95dc3eed40c3b5f1f5a4d22c5735e8c7",
        recipe: Recipe::Heif { major: "avif", orientation: None, date: false },
        exif: Some((1, None, false)),
        scenarios: &[],
    },

    Fixture {
        name: "jpg-o6-700x500-5.jpg", sha: "4c8693222f27b1c5b120504b2c40ddb6eb99c2d9e5187b4cbe77e72dc0b53aaf",
        recipe: Recipe::Jpeg { w: 700, h: 500, seed: 5, orientation: Some(6), date: true, lie: 0 },
        exif: Some((6, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[Scenario { label: "s1", ok: true, w: 700, h: 500, taken: Some("2026:11:11 12:34:56"), sha: "4656d73bb27575bbc31b24367d80c0f0d01b09fd5fa76aa2a40400670fa2efdf", why: "" }, Scenario { label: "s2", ok: true, w: 700, h: 500, taken: Some("2026:11:11 12:34:56"), sha: "9255548bdc4282029a2d494beac3e2e0a6652a8fa6c0fa6a7a3471df498f6e4f", why: "" }, Scenario { label: "s3", ok: false, w: 0, h: 0, taken: None, sha: "", why: "small image 700×500" }, Scenario { label: "s4", ok: false, w: 0, h: 0, taken: None, sha: "", why: "unreadable: needed an estimated 2 MB of memory to read: a picture of 0 MP (the helper's limit is 0 MB)" }],
    },

    Fixture {
        name: "jpg-plain-40x24-0.jpg", sha: "9cd264e6f02fcc8726bb8f36f35657c6c43e540362547a03b5a040a896a60872",
        recipe: Recipe::Jpeg { w: 40, h: 24, seed: 0, orientation: None, date: false, lie: 0 },
        exif: None,
        scenarios: &[Scenario { label: "s1", ok: true, w: 40, h: 24, taken: None, sha: "75558b3ea29a869430968a37b11aff3d615251dca8fd9972341196c09270f607", why: "" }, Scenario { label: "s2", ok: true, w: 40, h: 24, taken: None, sha: "c217ba67687072f842a44b8bfd44408c97020461ddc1318c283b4ce4d4214680", why: "" }],
    },

    Fixture {
        name: "jpg-date-40x24-1.jpg", sha: "afe19d7c7f3f18eca05903229c3824623026ba18cba874e2548647104ecfd65b",
        recipe: Recipe::Jpeg { w: 40, h: 24, seed: 1, orientation: None, date: true, lie: 0 },
        exif: Some((1, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[Scenario { label: "s1", ok: true, w: 40, h: 24, taken: Some("2026:11:11 12:34:56"), sha: "22d48ad00b1c817231ec6afdb9356b1e8b05e162a9ac53bef65f0654cd50d80b", why: "" }, Scenario { label: "s2", ok: true, w: 40, h: 24, taken: Some("2026:11:11 12:34:56"), sha: "df4290fb1aa8b0bbd7d4c04064c1762d0ded6d893a0a9df5b02fbddfab8f8d91", why: "" }],
    },

    Fixture {
        name: "jpg-o9-40x24-2.jpg", sha: "5317e785d6f9617f52ea26363ad40e9628e75f6e9721bf3b4806a0870691466b",
        recipe: Recipe::Jpeg { w: 40, h: 24, seed: 2, orientation: Some(9), date: false, lie: 0 },
        exif: Some((9, None, false)),
        scenarios: &[Scenario { label: "s1", ok: true, w: 40, h: 24, taken: None, sha: "b04e2291afd42dca82db99d26a63e25b0aa6f05d68245884de2436b68d865e3b", why: "" }],
    },

    Fixture {
        name: "jpg-o0-40x24-3.jpg", sha: "a43c2659efd8ac1af149be8ae63df8a1ddc1ee2e7172ee3a3bde297b16fb9fc0",
        recipe: Recipe::Jpeg { w: 40, h: 24, seed: 3, orientation: Some(0), date: true, lie: 0 },
        exif: Some((0, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[Scenario { label: "s1", ok: true, w: 40, h: 24, taken: Some("2026:11:11 12:34:56"), sha: "618b09158fb21fc252b10b24f173f9e3cc4669551e72d8c1978229b80f7a84e7", why: "" }],
    },

    Fixture {
        name: "jpg-lies-40x24-5.jpg", sha: "9d9c4ceeaf4e0fc76abfcac50779eff1a74033fbbf2a3000eeedaf9bde2764a5",
        recipe: Recipe::Jpeg { w: 40, h: 24, seed: 5, orientation: Some(6), date: true, lie: 60 },
        exif: Some((6, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[Scenario { label: "s1", ok: true, w: 40, h: 24, taken: Some("2026:11:11 12:34:56"), sha: "961c8793520e8d724afd1b1e14320b740cbf2dc6e42343a561dbcad0d145c6d4", why: "" }],
    },

    Fixture {
        name: "jpg-o6-30x20-4.jpg", sha: "95c68e11be6a0026b6431a73f1085665155fcb86a88562338bcbfd229d4dc3aa",
        recipe: Recipe::Jpeg { w: 30, h: 20, seed: 4, orientation: Some(6), date: true, lie: 0 },
        exif: Some((6, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[Scenario { label: "s1", ok: false, w: 0, h: 0, taken: None, sha: "", why: "small image 30×20" }, Scenario { label: "s2", ok: false, w: 0, h: 0, taken: None, sha: "", why: "small image 30×20" }, Scenario { label: "s3", ok: false, w: 0, h: 0, taken: None, sha: "", why: "small image 30×20" }, Scenario { label: "s4", ok: false, w: 0, h: 0, taken: None, sha: "", why: "small image 30×20" }],
    },

    Fixture {
        name: "huge-32767.jpg", sha: "47fe2fb0d88cd6dcd2b553f9c0c3b526d487e7df2f34c477e978a0d6f33b6f3d",
        recipe: Recipe::Huge,
        exif: Some((6, Some("2026:11:11 12:34:56"), false)),
        scenarios: &[Scenario { label: "s1", ok: false, w: 0, h: 0, taken: None, sha: "", why: "unreadable: can't decode image" }, Scenario { label: "s4", ok: false, w: 0, h: 0, taken: None, sha: "", why: "unreadable: needed an estimated 8.0 GB of memory to read: a picture of 1073 MP (the helper's limit is 0 MB)" }],
    },

    Fixture {
        name: "raw-fake.cr2", sha: "9e82d457ad176c59830323d4c10be7d77d95a762c9bd6756e56887fa7b612c23",
        recipe: Recipe::Raw,
        exif: None,
        scenarios: &[Scenario { label: "s1", ok: false, w: 0, h: 0, taken: None, sha: "", why: "unreadable: RAW pictures (CR2) aren't read on this system yet" }],
    },

    Fixture {
        name: "small-30x20.png", sha: "703bae80f8f01789d5dc506e3b5569bab024537b8fa090179e9e5c77071de71e",
        recipe: Recipe::Png { w: 30, h: 20, seed: 3 },
        exif: None,
        scenarios: &[Scenario { label: "s1", ok: false, w: 0, h: 0, taken: None, sha: "", why: "small image 30×20" }],
    },

];

#[test]
fn every_exif_header_answers_what_the_engine_read() {
    let folder = fixture_folder("read");
    for fixture in FIXTURES {
        let bytes = build_fixture(&folder, fixture);
        let path = folder.join(fixture.name);
        assert_eq!(
            digest(&bytes),
            fixture.sha,
            "{}: the generator made the engine's bytes",
            fixture.name
        );
        let got = exif::read(&path);
        match (fixture.exif, &got) {
            (None, None) => {}
            (Some((orientation, date, tiled)), Some(meta)) => {
                assert_eq!(
                    meta.orientation, orientation,
                    "{}: orientation",
                    fixture.name
                );
                assert_eq!(
                    meta.date_time_original.as_deref(),
                    date,
                    "{}: DateTimeOriginal",
                    fixture.name
                );
                assert_eq!(meta.tiled, tiled, "{}: tiled", fixture.name);
            }
            (Some(_), None) => panic!(
                "{}: the engine read EXIF, the port found none",
                fixture.name
            ),
            (None, Some(meta)) => panic!(
                "{}: the engine found no EXIF, the port read orientation {}",
                fixture.name, meta.orientation
            ),
        }
    }
}

#[test]
fn prepare_writes_the_copy_the_engine_writes() {
    let folder = fixture_folder("prepare");
    for fixture in FIXTURES {
        build_fixture(&folder, fixture);
        let path = folder.join(fixture.name);
        for scenario in fixture.scenarios {
            let (min_side, budget, png) = scenario_parameters(scenario.label);
            image::set_decode_memory_budget(budget);
            let out = folder.join(format!("out-{}", scenario.label));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            match image::prepare(&path, 400, min_side, &out, png) {
                Ok(prepared) => {
                    assert!(
                        scenario.ok,
                        "{} {}: a refusal the engine did not see",
                        fixture.name, scenario.label
                    );
                    let name = Path::new(&prepared.path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .expect("the prepared copy has a name");
                    let (uuid, ext) = name.split_once('.').expect("uuid.ext");
                    assert!(
                        is_uuid_name(uuid),
                        "{} {}: not Foundation's uuidString: {uuid}",
                        fixture.name,
                        scenario.label
                    );
                    assert_eq!(
                        ext,
                        if png { "png" } else { "jpg" },
                        "{} {}: the extension",
                        fixture.name,
                        scenario.label
                    );
                    assert_eq!(
                        prepared.width, scenario.w,
                        "{}: the STORED width the index records",
                        fixture.name
                    );
                    assert_eq!(
                        prepared.height, scenario.h,
                        "{}: the STORED height",
                        fixture.name
                    );
                    assert_eq!(
                        prepared.taken.as_deref(),
                        scenario.taken,
                        "{}: the camera's own date",
                        fixture.name
                    );
                    let written =
                        std::fs::read(&prepared.path).expect("the copy the writer placed");
                    assert_eq!(
                        digest(&written),
                        scenario.sha,
                        "{} {}: the scaled copy, pixel for pixel and byte for byte",
                        fixture.name,
                        scenario.label
                    );
                }
                Err(why) => {
                    assert!(
                        !scenario.ok,
                        "{} {}: the engine wrote a copy, the port refused: {why}",
                        fixture.name, scenario.label
                    );
                    // #152: the pinned refusal text is the DECLINING build's answer. A build
                    // that links libraw takes the RAW file to the decoder, and the fake's
                    // refusal is the decoder's own — the status (`failed`) is the same on
                    // both fronts, which is what the cross-engine gate compares.
                    let is_raw = fixture
                        .name
                        .rsplit('.')
                        .next()
                        .is_some_and(digup_native::raw::supported);
                    let expected = if is_raw {
                        "unreadable: can't decode image"
                    } else {
                        scenario.why
                    };
                    assert_eq!(
                        why.to_string(),
                        expected,
                        "{}: the refusal text",
                        fixture.name
                    );
                }
            }
            image::set_decode_memory_budget(0);
        }
    }
}
