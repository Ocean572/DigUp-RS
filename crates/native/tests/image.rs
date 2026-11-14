//! `RGBImage` / the stb seam / the picture refusals, pinned against the engine's own answers.
//!
//! The wide gate is the fixture cross-check (`/tmp/opencode/imagecheck`: 57 files — Pillow's JPEG
//! chroma and quality variants, PNG with palette + tRNS / RGBA / interlacing / 16-bit samples, BMP,
//! animated GIF, TGA, PNM, TIFF, WebP, the size shapes, the degenerate 1×1 / 3×3 / 8×8 pictures, a
//! JPEG cut mid-scan — where the engine's stb and the port's stb (the SAME `Vendor/stb/stb.c`)
//! wrote byte-identical JSON lines and byte-identical pictures: 57 fixtures, 0 differing, over 650
//! raw and encoded files compared by SHA-256).
//!
//! These tests carry the fixtures inside the workspace: a Swift generator (`/tmp/opencode/imagecheck
//! /gen/mk.swift`) made each file with a deterministic writer — the pixel formula below, stb's own
//! PNG/JPEG writer, and hand-written BMP/TGA/PNM layouts — and reported what the engine's picture
//! code answered. The expected dimensions, average hashes, scaled copies and pixel digests here are
//! those answers.

use std::path::{Path, PathBuf};

use digup_native::{image, rgb, stb};
use sha2::{Digest, Sha256};

/// The pixels: three crossed gradients, integer arithmetic, no randomness. `mk.swift` runs the same
/// formula, so a fixture's pixels are the same bytes on both sides.
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

fn le16(value: i64) -> [u8; 2] {
    [(value & 0xFF) as u8, ((value >> 8) & 0xFF) as u8]
}

fn le32(value: i64) -> [u8; 4] {
    [
        (value & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        ((value >> 16) & 0xFF) as u8,
        ((value >> 24) & 0xFF) as u8,
    ]
}

/// 24-bit BMP: BITMAPFILEHEADER + BITMAPINFOHEADER (40), rows bottom-up, BGR, rows padded to 4.
fn bmp(image: &rgb::RgbImage) -> Vec<u8> {
    let row = image.width * 3;
    let pad = (4 - row % 4) % 4;
    let size = 54 + (row + pad) * image.height;
    let mut out: Vec<u8> = vec![0x42, 0x4D];
    out.extend(le32(size as i64));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le32(54));
    out.extend(le32(40));
    out.extend(le32(image.width as i64));
    out.extend(le32(image.height as i64));
    out.extend(le16(1));
    out.extend(le16(24));
    out.extend(le32(0));
    out.extend(le32(((row + pad) * image.height) as i64));
    out.extend(le32(2835));
    out.extend(le32(2835));
    out.extend(le32(0));
    out.extend(le32(0));
    for y in (0..image.height).rev() {
        for x in 0..image.width {
            let p = (y * image.width + x) * 3;
            out.push(image.pixels[p + 2]);
            out.push(image.pixels[p + 1]);
            out.push(image.pixels[p]);
        }
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out
}

/// The 8-color palette of the quantized fixture, and its quantizer: nearest by squared distance,
/// ties to the lowest index.
const PALETTE: [(i64, i64, i64); 8] = [
    (0, 0, 0),
    (255, 255, 255),
    (255, 0, 0),
    (0, 255, 0),
    (0, 0, 255),
    (255, 255, 0),
    (255, 0, 255),
    (0, 255, 255),
];

fn quantize(image: &rgb::RgbImage) -> Vec<usize> {
    image
        .pixels
        .chunks(3)
        .map(|pixel| {
            let (r, g, b) = (
                i64::from(pixel[0]),
                i64::from(pixel[1]),
                i64::from(pixel[2]),
            );
            let mut best = 0usize;
            let mut best_distance = i64::MAX;
            for (index, color) in PALETTE.iter().enumerate() {
                let (dr, dg, db) = (r - color.0, g - color.1, b - color.2);
                let distance = dr * dr + dg * dg + db * db;
                if distance < best_distance {
                    best_distance = distance;
                    best = index;
                }
            }
            best
        })
        .collect()
}

/// 8-bit BMP with a palette: stb derives the palette's size from the pixel-data offset, so a file
/// that says the pixels start at 54 says it has no palette, and stb refuses it.
fn bmp8(image: &rgb::RgbImage) -> Vec<u8> {
    let indexes = quantize(image);
    let row = image.width;
    let pad = (4 - row % 4) % 4;
    let offset = 54 + 4 * PALETTE.len();
    let size = offset + (row + pad) * image.height;
    let mut out: Vec<u8> = vec![0x42, 0x4D];
    out.extend(le32(size as i64));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le32(offset as i64));
    out.extend(le32(40));
    out.extend(le32(image.width as i64));
    out.extend(le32(image.height as i64));
    out.extend(le16(1));
    out.extend(le16(8));
    out.extend(le32(0));
    out.extend(le32(((row + pad) * image.height) as i64));
    out.extend(le32(2835));
    out.extend(le32(2835));
    out.extend(le32(PALETTE.len() as i64));
    out.extend(le32(PALETTE.len() as i64));
    for color in PALETTE.iter() {
        out.push(color.2 as u8);
        out.push(color.1 as u8);
        out.push(color.0 as u8);
        out.push(0);
    }
    for y in (0..image.height).rev() {
        for x in 0..image.width {
            out.push(indexes[y * image.width + x] as u8);
        }
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    out
}

/// TGA type 2 (uncompressed), bottom-up, BGR: 18 header bytes — id length, color-map type, image
/// type, the 5 color-map bytes, the origin, the size, the depth, the descriptor.
fn tga(image: &rgb::RgbImage) -> Vec<u8> {
    let mut out: Vec<u8> = vec![0, 0, 2];
    out.extend(std::iter::repeat_n(0u8, 5));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le16(image.width as i64));
    out.extend(le16(image.height as i64));
    out.push(24);
    out.push(0);
    for y in (0..image.height).rev() {
        for x in 0..image.width {
            let p = (y * image.width + x) * 3;
            out.push(image.pixels[p + 2]);
            out.push(image.pixels[p + 1]);
            out.push(image.pixels[p]);
        }
    }
    out
}

/// TGA type 10 (RLE): packets of 1…128 pixels — a run when 3 or more pixels repeat, otherwise
/// literals grown while each new pixel differs from every pixel already in the packet.
fn tga_rle(image: &rgb::RgbImage) -> Vec<u8> {
    let mut out: Vec<u8> = vec![0, 0, 10];
    out.extend(std::iter::repeat_n(0u8, 5));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le16(image.width as i64));
    out.extend(le16(image.height as i64));
    out.push(24);
    out.push(0);
    let mut flat: Vec<u8> = Vec::with_capacity(image.pixels.len());
    for y in (0..image.height).rev() {
        for x in 0..image.width {
            let p = (y * image.width + x) * 3;
            flat.push(image.pixels[p + 2]);
            flat.push(image.pixels[p + 1]);
            flat.push(image.pixels[p]);
        }
    }
    let mut at = 0usize;
    while at < flat.len() {
        let mut run = 1usize;
        while run < 128
            && at + run * 3 + 2 < flat.len()
            && flat[at + run * 3] == flat[at]
            && flat[at + run * 3 + 1] == flat[at + 1]
            && flat[at + run * 3 + 2] == flat[at + 2]
        {
            run += 1;
        }
        if run >= 3 {
            out.push(0x80 | (run as u8 - 1));
            out.extend(flat[at..at + 3].iter().copied());
            at += run * 3;
            continue;
        }
        let mut literal = 1usize;
        while literal < 128 && at + literal * 3 + 2 < flat.len() {
            // The packet grows while the newest pixel differs from every pixel already in it.
            let base = at + (literal - 1) * 3;
            let mut distinct = true;
            for prior in 0..(literal - 1) {
                if flat[base] == flat[at + prior * 3]
                    && flat[base + 1] == flat[at + prior * 3 + 1]
                    && flat[base + 2] == flat[at + prior * 3 + 2]
                {
                    distinct = false;
                    break;
                }
            }
            if !distinct {
                break;
            }
            literal += 1;
        }
        out.push(literal as u8 - 1);
        out.extend(flat[at..at + literal * 3].iter().copied());
        at += literal * 3;
    }
    out
}

fn gray(image: &rgb::RgbImage) -> Vec<u8> {
    image
        .pixels
        .chunks(3)
        .map(|pixel| {
            ((i64::from(pixel[0]) * 299 + i64::from(pixel[1]) * 587 + i64::from(pixel[2]) * 114)
                / 1000) as u8
        })
        .collect()
}

fn ppm(image: &rgb::RgbImage) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", image.width, image.height).into_bytes();
    out.extend(image.pixels.iter().copied());
    out
}

fn pgm(image: &rgb::RgbImage) -> Vec<u8> {
    let mut out = format!("P5\n{} {}\n255\n", image.width, image.height).into_bytes();
    out.extend(gray(image).iter().copied());
    out
}

fn write_fixture(folder: &Path, answer: &Answer, source: &rgb::RgbImage) {
    let path = folder.join(answer.name);
    let bytes = match answer.kind {
        "png" => {
            source.write(&path, true).expect("stb writes a PNG");
            return;
        }
        "jpg" => {
            source.write(&path, false).expect("stb writes a JPEG");
            return;
        }
        "bmp" => bmp(source),
        "pal" => bmp8(source),
        "tga" => tga(source),
        "tgarle" => tga_rle(source),
        "ppm" => ppm(source),
        _ => pgm(source),
    };
    std::fs::write(&path, bytes).expect("write the fixture");
}

fn fixture_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-image-{name}"));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// `prepare`'s scale rule: the long side to `maxPixel`, each side rounded half away from zero,
/// never below 1 pixel.
fn target(width: usize, height: usize) -> (usize, usize) {
    let longest = width.max(height);
    if longest <= MAX_PIXEL {
        return (width, height);
    }
    let scale = MAX_PIXEL as f64 / longest as f64;
    let scaled = |side: usize| (side as f64 * scale).round().max(1.0) as usize;
    (scaled(width), scaled(height))
}

const MAX_PIXEL: usize = 400;

/// A fixture and everything the engine answered about it.
struct Answer {
    name: &'static str,
    kind: &'static str,
    w: usize,
    h: usize,
    seed: i64,
    dec: (usize, usize),
    hash: u64,
    orient: [(i64, usize, usize); 8],
    rs: (usize, usize),
    dec_sha: &'static str,
    o6_sha: &'static str,
    rs_sha: &'static str,
    rs_png_sha: &'static str,
    rs_jpg_sha: &'static str,
}

const ANSWERS: &[Answer] = &[
    Answer {
        name: "png-40x24-0.png",
        kind: "png",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9222520889437335616,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        o6_sha: "dadf73e0983caec7ee167be363beff0662658af88a7d9be2f718473ff470292b",
        rs_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        rs_png_sha: "dc1831cef95374b744ff217e572c6a74899d6419c9df6507bdb1fa8d43ff86ad",
        rs_jpg_sha: "9cd264e6f02fcc8726bb8f36f35657c6c43e540362547a03b5a040a896a60872",
    },
    Answer {
        name: "png-1x1-9.png",
        kind: "png",
        w: 1,
        h: 1,
        seed: 9,
        dec: (1, 1),
        hash: 0,
        orient: [
            (1, 1, 1),
            (2, 1, 1),
            (3, 1, 1),
            (4, 1, 1),
            (5, 1, 1),
            (6, 1, 1),
            (7, 1, 1),
            (8, 1, 1),
        ],
        rs: (1, 1),
        dec_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        o6_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        rs_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        rs_png_sha: "3a048a74d6937c4e406d2c78c9ce1da8df619ded1f54c2559f2fa1c03ae41d09",
        rs_jpg_sha: "fe4a6106eed0dbd78f89a3a71e125e05255f27d91d88bb181f4618846b3f0ae7",
    },
    Answer {
        name: "png-3x3-10.png",
        kind: "png",
        w: 3,
        h: 3,
        seed: 10,
        dec: (3, 3),
        hash: 18446736346941800640,
        orient: [
            (1, 3, 3),
            (2, 3, 3),
            (3, 3, 3),
            (4, 3, 3),
            (5, 3, 3),
            (6, 3, 3),
            (7, 3, 3),
            (8, 3, 3),
        ],
        rs: (3, 3),
        dec_sha: "2b30ecdd1bb0e52058518b108fdc2a4daf4295c6a61cc3579e0fff55128d3d6b",
        o6_sha: "09d7f6ae0cf95ee9d42a34da14b40c8d8f51d9e1b4bcd398e70cfb31d8654234",
        rs_sha: "2b30ecdd1bb0e52058518b108fdc2a4daf4295c6a61cc3579e0fff55128d3d6b",
        rs_png_sha: "ff3c35c6b1e99246fea3578c9079bd870049aeccc552ea14550f491b095661c0",
        rs_jpg_sha: "8c80bcea853f6f528df709d1d347cf2d5c35deadf82b6e4fb4fcf928b074c7b9",
    },
    Answer {
        name: "png-8x8-8.png",
        kind: "png",
        w: 8,
        h: 8,
        seed: 8,
        dec: (8, 8),
        hash: 18446454837202681984,
        orient: [
            (1, 8, 8),
            (2, 8, 8),
            (3, 8, 8),
            (4, 8, 8),
            (5, 8, 8),
            (6, 8, 8),
            (7, 8, 8),
            (8, 8, 8),
        ],
        rs: (8, 8),
        dec_sha: "f0f57c81e432a2869cdbd3856fca6264fc8dcda07a799e4a1487b874bb178178",
        o6_sha: "f7d155bffcd575bd0d0aaca0db17bb501f7c1ea9d2198e364dbf0e2e830363bf",
        rs_sha: "f0f57c81e432a2869cdbd3856fca6264fc8dcda07a799e4a1487b874bb178178",
        rs_png_sha: "73982b9c7bdf7d22971161af7fdf71ab85a035f350e1c4c6b97d62dbff263ef0",
        rs_jpg_sha: "f73341a8093b2686758a83f94f728a9054688a1eac2fff4628cfcc4f40a53771",
    },
    Answer {
        name: "png-5x7-11.png",
        kind: "png",
        w: 5,
        h: 7,
        seed: 11,
        dec: (5, 7),
        hash: 18446740710221676544,
        orient: [
            (1, 5, 7),
            (2, 5, 7),
            (3, 5, 7),
            (4, 5, 7),
            (5, 7, 5),
            (6, 7, 5),
            (7, 7, 5),
            (8, 7, 5),
        ],
        rs: (5, 7),
        dec_sha: "54718b00e67af1edcdc711a6cf48f897a814a9122d4752d8efa90a57992b6dce",
        o6_sha: "c4ec782ae2778c4fd4825c6f228507bc583bb9e0baa868cd5aeb99a0ea1ccfd2",
        rs_sha: "54718b00e67af1edcdc711a6cf48f897a814a9122d4752d8efa90a57992b6dce",
        rs_png_sha: "36a6d9e172c4b2220fda702f1aa3045218afeb56a7be48a7cc7b021cb11a2425",
        rs_jpg_sha: "a719bb7667c003ed278fcb903e60c36435490c07b579a678d3b3f79789a61dda",
    },
    Answer {
        name: "png-9x4-12.png",
        kind: "png",
        w: 9,
        h: 4,
        seed: 12,
        dec: (9, 4),
        hash: 17940354032986022112,
        orient: [
            (1, 9, 4),
            (2, 9, 4),
            (3, 9, 4),
            (4, 9, 4),
            (5, 4, 9),
            (6, 4, 9),
            (7, 4, 9),
            (8, 4, 9),
        ],
        rs: (9, 4),
        dec_sha: "8d34786ee3b55db513624760d270fc332c068de7d93c9405361d16453e794e86",
        o6_sha: "328566107ee3c1166179a0a66395382a8402aa3a5e4fd3313a07b08181f73541",
        rs_sha: "8d34786ee3b55db513624760d270fc332c068de7d93c9405361d16453e794e86",
        rs_png_sha: "1c09f63a97177de3c1e3c7cf7b870c6b3648252d97fd87b0023fde246514c5e4",
        rs_jpg_sha: "a02af685cb11c936dfb823f8e2d4aafc63ec960324f0cc3025de458974f33737",
    },
    Answer {
        name: "png-30x20-3.png",
        kind: "png",
        w: 30,
        h: 20,
        seed: 3,
        dec: (30, 20),
        hash: 18446689769089986720,
        orient: [
            (1, 30, 20),
            (2, 30, 20),
            (3, 30, 20),
            (4, 30, 20),
            (5, 20, 30),
            (6, 20, 30),
            (7, 20, 30),
            (8, 20, 30),
        ],
        rs: (30, 20),
        dec_sha: "d5ac5cb63fb9c7414c45e176bf76695e116c0db2c63eae98345b4c44ed35821a",
        o6_sha: "dc105a6500de9556009d2d99bd1dd6ea7d5150f46021d371d9a0854779499d06",
        rs_sha: "d5ac5cb63fb9c7414c45e176bf76695e116c0db2c63eae98345b4c44ed35821a",
        rs_png_sha: "703bae80f8f01789d5dc506e3b5569bab024537b8fa090179e9e5c77071de71e",
        rs_jpg_sha: "c92b58bbe9c97d85e189efdf6d5e33254ba52bfae97a01d5bd028a6873f98401",
    },
    Answer {
        name: "png-200x20-6.png",
        kind: "png",
        w: 200,
        h: 20,
        seed: 6,
        dec: (200, 20),
        hash: 10642769214192247908,
        orient: [
            (1, 200, 20),
            (2, 200, 20),
            (3, 200, 20),
            (4, 200, 20),
            (5, 20, 200),
            (6, 20, 200),
            (7, 20, 200),
            (8, 20, 200),
        ],
        rs: (200, 20),
        dec_sha: "a5ee077509271f9dd6cfc3000903322275a8a69e222dd8c196bd0388f49d669d",
        o6_sha: "88a93ebfffc90b4cd88f930e28faa1ee85e754292517a5c17783fcab5746cc30",
        rs_sha: "a5ee077509271f9dd6cfc3000903322275a8a69e222dd8c196bd0388f49d669d",
        rs_png_sha: "b9095ce1789febb45860d404c0472bed55ccb8c55d682859a70bb23d320980a3",
        rs_jpg_sha: "2a3c7d733c3f067a0d871a315707a08405adfa499d14a72ad1a76c734b3d6cbe",
    },
    Answer {
        name: "png-640x12-2.png",
        kind: "png",
        w: 640,
        h: 12,
        seed: 2,
        dec: (640, 12),
        hash: 14106333729278957784,
        orient: [
            (1, 640, 12),
            (2, 640, 12),
            (3, 640, 12),
            (4, 640, 12),
            (5, 12, 640),
            (6, 12, 640),
            (7, 12, 640),
            (8, 12, 640),
        ],
        rs: (400, 8),
        dec_sha: "5adf72f0e72180a5e68362869ff0105c39778825497454e42b16fa2a9a6b1f04",
        o6_sha: "641643e86907b8f6736fa60a6219fa4a6769dce370880d4e58eb669bd96ed1bf",
        rs_sha: "edf04a912e78cfb07fc7133e91a7e0111a525c8261c565fe24c3f78156c65391",
        rs_png_sha: "404b3184d14c9389951fb5d40de3d90ecf10206142f37cd567dba163f977f0b3",
        rs_jpg_sha: "e5237c763217cd510e20f2fb1e028c526ec4b81371dedaec9854d1bd8a604768",
    },
    Answer {
        name: "png-800x2-1.png",
        kind: "png",
        w: 800,
        h: 2,
        seed: 1,
        dec: (800, 2),
        hash: 4340410370284600380,
        orient: [
            (1, 800, 2),
            (2, 800, 2),
            (3, 800, 2),
            (4, 800, 2),
            (5, 2, 800),
            (6, 2, 800),
            (7, 2, 800),
            (8, 2, 800),
        ],
        rs: (400, 1),
        dec_sha: "8d3c70f274c206fb5f7b7591d06c41731c1a17f441a8b9840a7a4bdcd42ee382",
        o6_sha: "977f1ea230c2db30b8d107cea76f925b5dd0ccff713eeafda3c7ade3217e23b3",
        rs_sha: "900afc38e969f4ff3f53237bb1c8bbc4bfed9dadc9f2cafe9edf0e1130fa8c9d",
        rs_png_sha: "8a2e48db5f12a9e39007cdc74cd35618a35b3824776a937947daabbeb485c879",
        rs_jpg_sha: "a85258d659131a36be8e8a0d673fe80e422e632f9615990084a8feef0b6c01c9",
    },
    Answer {
        name: "png-2x800-4.png",
        kind: "png",
        w: 2,
        h: 800,
        seed: 4,
        dec: (2, 800),
        hash: 72041097085255680,
        orient: [
            (1, 2, 800),
            (2, 2, 800),
            (3, 2, 800),
            (4, 2, 800),
            (5, 800, 2),
            (6, 800, 2),
            (7, 800, 2),
            (8, 800, 2),
        ],
        rs: (1, 400),
        dec_sha: "dcb0173cb078f7d504b0184ec5e610913d761559ef50e8fd1e3aebcd4192a484",
        o6_sha: "6390dea9c71e3c3705f618d5baba538d88940282ad672bcf9d40f28fbe95bf29",
        rs_sha: "bfb1416ee8ae52898fce4119a592909b757a0261a22102cf7530b5b585a1608b",
        rs_png_sha: "137f16ca04f62c5152b3b123790c8222820d57e37f1834edbafa4439970bf8d4",
        rs_jpg_sha: "d42a1ea64b151236ad9d8717ac63c58ada7202017b6e842bd8d04dcf31e3dc06",
    },
    Answer {
        name: "png-700x500-5.png",
        kind: "png",
        w: 700,
        h: 500,
        seed: 5,
        dec: (700, 500),
        hash: 7354631705767530000,
        orient: [
            (1, 700, 500),
            (2, 700, 500),
            (3, 700, 500),
            (4, 700, 500),
            (5, 500, 700),
            (6, 500, 700),
            (7, 500, 700),
            (8, 500, 700),
        ],
        rs: (400, 286),
        dec_sha: "54df25ef8f6d7dad01516480e45e74ca5a5ad4c2f42cc3bf1f9243736eb84f4a",
        o6_sha: "230046a6f746d1f2c9cc4b67a91fa1385ecb624ee2510b3a1b6aa5304703f8ff",
        rs_sha: "4dd212352f2cec8356e7002ee6adb29582f8bf386d9d2092d0153ab947b036ac",
        rs_png_sha: "efee9154af41c5a6be250db807e361e9a9a23be5424b737999aff83c0fa82864",
        rs_jpg_sha: "be34ae0ef72bf1361c02095207f2f22ca2ad2e49f65f5dcac5d1328508ce7a69",
    },
    Answer {
        name: "jpg-40x24-0.jpg",
        kind: "jpg",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9222520906617204800,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "238e21b09ff28a9ef2eb5504040876389c1ba623afea3538d8e76ea8dd81b902",
        o6_sha: "4c786cfca049e9cd1d4adaa36d863e447368a6540dcc17276a73b9faf1eed0e3",
        rs_sha: "238e21b09ff28a9ef2eb5504040876389c1ba623afea3538d8e76ea8dd81b902",
        rs_png_sha: "75558b3ea29a869430968a37b11aff3d615251dca8fd9972341196c09270f607",
        rs_jpg_sha: "c217ba67687072f842a44b8bfd44408c97020461ddc1318c283b4ce4d4214680",
    },
    Answer {
        name: "jpg-64x40-1.jpg",
        kind: "jpg",
        w: 64,
        h: 40,
        seed: 1,
        dec: (64, 40),
        hash: 9329981299486679232,
        orient: [
            (1, 64, 40),
            (2, 64, 40),
            (3, 64, 40),
            (4, 64, 40),
            (5, 40, 64),
            (6, 40, 64),
            (7, 40, 64),
            (8, 40, 64),
        ],
        rs: (64, 40),
        dec_sha: "d27a5db53ba2fc0c2935c2973452833c576a6625553912bf2ecd1aef71f4dd2e",
        o6_sha: "ad9ff18ef11fb89c4dfdd7747f64620a66a6cbae49ab4c7dda3bea8a4877f728",
        rs_sha: "d27a5db53ba2fc0c2935c2973452833c576a6625553912bf2ecd1aef71f4dd2e",
        rs_png_sha: "df48279e2a8454ec1bf8da6351853b27b62173be8e8dd8e41e47ba1fb217c235",
        rs_jpg_sha: "1511b77263c7b447ebb8db6059424c3ce1a6855fcea07e0ff87ad9eaa413902d",
    },
    Answer {
        name: "jpg-700x500-5.jpg",
        kind: "jpg",
        w: 700,
        h: 500,
        seed: 5,
        dec: (700, 500),
        hash: 7354594322372183568,
        orient: [
            (1, 700, 500),
            (2, 700, 500),
            (3, 700, 500),
            (4, 700, 500),
            (5, 500, 700),
            (6, 500, 700),
            (7, 500, 700),
            (8, 500, 700),
        ],
        rs: (400, 286),
        dec_sha: "8c4ff0dc270a0317c3ff8459eb9952ac945db07c8472c897d7899e0190db02e9",
        o6_sha: "cf0a49e4633ad337bd510f69245346ff0ab70ec3167a13e34e6ee5867e9e2984",
        rs_sha: "5706d88a6712f68ce9105b0254dbd001f027813eb82633b8e5e6744e89b3ecc2",
        rs_png_sha: "1ed3eba88b631263138dcf399b136bb27192f68140303dba0e4f2d00afd0d263",
        rs_jpg_sha: "04bcf59a1036479dc55ce082609e4c03f4f11e70cda07a428b0956315f08a80f",
    },
    Answer {
        name: "jpg-640x12-2.jpg",
        kind: "jpg",
        w: 640,
        h: 12,
        seed: 2,
        dec: (640, 12),
        hash: 14106333729278957784,
        orient: [
            (1, 640, 12),
            (2, 640, 12),
            (3, 640, 12),
            (4, 640, 12),
            (5, 12, 640),
            (6, 12, 640),
            (7, 12, 640),
            (8, 12, 640),
        ],
        rs: (400, 8),
        dec_sha: "567b88634b78ba926f3ba9ea177d6b6f373a4e5d71a15226e97d1ffec54dfaa4",
        o6_sha: "c342de6311160ca55d27ed81e32d3685ba5cebda5639e5e875f5ac2c53ca2c00",
        rs_sha: "2c841b7f71a5bc4fee7ca291fea3d0e5a7edcac1a559fc104f09c5222eb72df9",
        rs_png_sha: "66dbe1b0b5430f560846636cddb6ae33a577e0873fa799d83c054f9de206aeac",
        rs_jpg_sha: "07b87ba81fc41fc85f22e1f9cd62d0cdb756192916d1b75c6ec0e7482d14d659",
    },
    Answer {
        name: "jpg-2x800-4.jpg",
        kind: "jpg",
        w: 2,
        h: 800,
        seed: 4,
        dec: (2, 800),
        hash: 72041097085255920,
        orient: [
            (1, 2, 800),
            (2, 2, 800),
            (3, 2, 800),
            (4, 2, 800),
            (5, 800, 2),
            (6, 800, 2),
            (7, 800, 2),
            (8, 800, 2),
        ],
        rs: (1, 400),
        dec_sha: "6cf2d79c80691a69d787c44b707151f85666533b0ff7a571bdd412b5edbf3641",
        o6_sha: "9250ad68988ff18522e1a12570618ccbeac245ab537114e50bd66e8437680b7a",
        rs_sha: "650f8a38238a3394fddbeff1ed6bd6052a5687a9f69ced537a49cefbe431e4d0",
        rs_png_sha: "4f88a3ecc473c5c49d35c16a7de21449e00b3f5ee99b8a53a271a5ec0d1d9a70",
        rs_jpg_sha: "8abf5df88363813bfda0e03893b9316a61d286b22e327cda129d64b325be9fc7",
    },
    Answer {
        name: "jpg-1x1-9.jpg",
        kind: "jpg",
        w: 1,
        h: 1,
        seed: 9,
        dec: (1, 1),
        hash: 0,
        orient: [
            (1, 1, 1),
            (2, 1, 1),
            (3, 1, 1),
            (4, 1, 1),
            (5, 1, 1),
            (6, 1, 1),
            (7, 1, 1),
            (8, 1, 1),
        ],
        rs: (1, 1),
        dec_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        o6_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        rs_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        rs_png_sha: "3a048a74d6937c4e406d2c78c9ce1da8df619ded1f54c2559f2fa1c03ae41d09",
        rs_jpg_sha: "fe4a6106eed0dbd78f89a3a71e125e05255f27d91d88bb181f4618846b3f0ae7",
    },
    Answer {
        name: "bmp-40x24-0.bmp",
        kind: "bmp",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9222520889437335616,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        o6_sha: "dadf73e0983caec7ee167be363beff0662658af88a7d9be2f718473ff470292b",
        rs_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        rs_png_sha: "dc1831cef95374b744ff217e572c6a74899d6419c9df6507bdb1fa8d43ff86ad",
        rs_jpg_sha: "9cd264e6f02fcc8726bb8f36f35657c6c43e540362547a03b5a040a896a60872",
    },
    Answer {
        name: "bmp-7x5-13.bmp",
        kind: "bmp",
        w: 7,
        h: 5,
        seed: 13,
        dec: (7, 5),
        hash: 18228593205230747840,
        orient: [
            (1, 7, 5),
            (2, 7, 5),
            (3, 7, 5),
            (4, 7, 5),
            (5, 5, 7),
            (6, 5, 7),
            (7, 5, 7),
            (8, 5, 7),
        ],
        rs: (7, 5),
        dec_sha: "22db3f06c8a252464af7cb0fcc9fc77793a84ef264fec46b7a0cfd390958c937",
        o6_sha: "2321dafb8cc079c351a6c554b7788aafb6720bcbece7f712e2b552411d7e97ed",
        rs_sha: "22db3f06c8a252464af7cb0fcc9fc77793a84ef264fec46b7a0cfd390958c937",
        rs_png_sha: "e33c78859e532092e87705b7aa46bf82b2f91f77f63073a181ddddaaa4f088dc",
        rs_jpg_sha: "248460583c218e56f1a33185513e7baa8413f86144b6e889344176ca1efe7d55",
    },
    Answer {
        name: "bmp-700x500-5.bmp",
        kind: "bmp",
        w: 700,
        h: 500,
        seed: 5,
        dec: (700, 500),
        hash: 7354631705767530000,
        orient: [
            (1, 700, 500),
            (2, 700, 500),
            (3, 700, 500),
            (4, 700, 500),
            (5, 500, 700),
            (6, 500, 700),
            (7, 500, 700),
            (8, 500, 700),
        ],
        rs: (400, 286),
        dec_sha: "54df25ef8f6d7dad01516480e45e74ca5a5ad4c2f42cc3bf1f9243736eb84f4a",
        o6_sha: "230046a6f746d1f2c9cc4b67a91fa1385ecb624ee2510b3a1b6aa5304703f8ff",
        rs_sha: "4dd212352f2cec8356e7002ee6adb29582f8bf386d9d2092d0153ab947b036ac",
        rs_png_sha: "efee9154af41c5a6be250db807e361e9a9a23be5424b737999aff83c0fa82864",
        rs_jpg_sha: "be34ae0ef72bf1361c02095207f2f22ca2ad2e49f65f5dcac5d1328508ce7a69",
    },
    Answer {
        name: "bmp-30x20-3.bmp",
        kind: "bmp",
        w: 30,
        h: 20,
        seed: 3,
        dec: (30, 20),
        hash: 18446689769089986720,
        orient: [
            (1, 30, 20),
            (2, 30, 20),
            (3, 30, 20),
            (4, 30, 20),
            (5, 20, 30),
            (6, 20, 30),
            (7, 20, 30),
            (8, 20, 30),
        ],
        rs: (30, 20),
        dec_sha: "d5ac5cb63fb9c7414c45e176bf76695e116c0db2c63eae98345b4c44ed35821a",
        o6_sha: "dc105a6500de9556009d2d99bd1dd6ea7d5150f46021d371d9a0854779499d06",
        rs_sha: "d5ac5cb63fb9c7414c45e176bf76695e116c0db2c63eae98345b4c44ed35821a",
        rs_png_sha: "703bae80f8f01789d5dc506e3b5569bab024537b8fa090179e9e5c77071de71e",
        rs_jpg_sha: "c92b58bbe9c97d85e189efdf6d5e33254ba52bfae97a01d5bd028a6873f98401",
    },
    Answer {
        name: "pal-40x24-0.bmp",
        kind: "pal",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9223082730005200896,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "76ba6e6ea41653a9034fc802c35a6c4796c810950535d64ade150bab4e0cb998",
        o6_sha: "dffdc9db000d908ba0c34200a630e491672d6abc7fe19861dbec752ca96f7b11",
        rs_sha: "76ba6e6ea41653a9034fc802c35a6c4796c810950535d64ade150bab4e0cb998",
        rs_png_sha: "ae883ec90fef0b166f6c11f0f352cde72ecb0150b2f8264a104b89eacd9f9f34",
        rs_jpg_sha: "fd2b8fe4f0f8f6861b1fedfafdf60e98875f69f85a9cfcd4ca75fedbbe6073a0",
    },
    Answer {
        name: "pal-17x9-21.bmp",
        kind: "pal",
        w: 17,
        h: 9,
        seed: 21,
        dec: (17, 9),
        hash: 18373834293211332608,
        orient: [
            (1, 17, 9),
            (2, 17, 9),
            (3, 17, 9),
            (4, 17, 9),
            (5, 9, 17),
            (6, 9, 17),
            (7, 9, 17),
            (8, 9, 17),
        ],
        rs: (17, 9),
        dec_sha: "826b29c6ba9e94ec1b60e84c85b695a244b52106c33d3d1e49ff1b0e94e4d4dc",
        o6_sha: "7799d1a6dda9e8f22912762d0a4a42cc35384e5fe4751728673319000e836aef",
        rs_sha: "826b29c6ba9e94ec1b60e84c85b695a244b52106c33d3d1e49ff1b0e94e4d4dc",
        rs_png_sha: "1afb46320c8babe5b3b7a3952f989065ed8fc6eee5b7a846186f67b9edfc3796",
        rs_jpg_sha: "925a4cb043ccdcfb54713cae15f4470e42af1f01e54cfc0e948554c145d5c3bf",
    },
    Answer {
        name: "tga-40x24-0.tga",
        kind: "tga",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9222520889437335616,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        o6_sha: "dadf73e0983caec7ee167be363beff0662658af88a7d9be2f718473ff470292b",
        rs_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        rs_png_sha: "dc1831cef95374b744ff217e572c6a74899d6419c9df6507bdb1fa8d43ff86ad",
        rs_jpg_sha: "9cd264e6f02fcc8726bb8f36f35657c6c43e540362547a03b5a040a896a60872",
    },
    Answer {
        name: "tga-700x500-5.tga",
        kind: "tga",
        w: 700,
        h: 500,
        seed: 5,
        dec: (700, 500),
        hash: 7354631705767530000,
        orient: [
            (1, 700, 500),
            (2, 700, 500),
            (3, 700, 500),
            (4, 700, 500),
            (5, 500, 700),
            (6, 500, 700),
            (7, 500, 700),
            (8, 500, 700),
        ],
        rs: (400, 286),
        dec_sha: "54df25ef8f6d7dad01516480e45e74ca5a5ad4c2f42cc3bf1f9243736eb84f4a",
        o6_sha: "230046a6f746d1f2c9cc4b67a91fa1385ecb624ee2510b3a1b6aa5304703f8ff",
        rs_sha: "4dd212352f2cec8356e7002ee6adb29582f8bf386d9d2092d0153ab947b036ac",
        rs_png_sha: "efee9154af41c5a6be250db807e361e9a9a23be5424b737999aff83c0fa82864",
        rs_jpg_sha: "be34ae0ef72bf1361c02095207f2f22ca2ad2e49f65f5dcac5d1328508ce7a69",
    },
    Answer {
        name: "tga-6x6-31.tga",
        kind: "tga",
        w: 6,
        h: 6,
        seed: 31,
        dec: (6, 6),
        hash: 14956189445629947952,
        orient: [
            (1, 6, 6),
            (2, 6, 6),
            (3, 6, 6),
            (4, 6, 6),
            (5, 6, 6),
            (6, 6, 6),
            (7, 6, 6),
            (8, 6, 6),
        ],
        rs: (6, 6),
        dec_sha: "9c65ddcc69427027570925f7aed2c7e1ced99573ab62e87c6a223698c6723b1c",
        o6_sha: "28117a3c36372c5087965731c4ca075a21471bc96c8c8dbc52e2eb8b34659bce",
        rs_sha: "9c65ddcc69427027570925f7aed2c7e1ced99573ab62e87c6a223698c6723b1c",
        rs_png_sha: "a80e7cec203b74fefdc0c69c281fdd3cb7842685629bb8e41dee0abe92259eb8",
        rs_jpg_sha: "5a345af59922bbf5abc2a5a238ce5edcc92db046a1cb2033cb1376eeb5924038",
    },
    Answer {
        name: "tgarle-40x24-0.tga",
        kind: "tgarle",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9222520889437335616,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        o6_sha: "dadf73e0983caec7ee167be363beff0662658af88a7d9be2f718473ff470292b",
        rs_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        rs_png_sha: "dc1831cef95374b744ff217e572c6a74899d6419c9df6507bdb1fa8d43ff86ad",
        rs_jpg_sha: "9cd264e6f02fcc8726bb8f36f35657c6c43e540362547a03b5a040a896a60872",
    },
    Answer {
        name: "tgarle-12x3-32.tga",
        kind: "tgarle",
        w: 12,
        h: 3,
        seed: 32,
        dec: (12, 3),
        hash: 17578643234654052576,
        orient: [
            (1, 12, 3),
            (2, 12, 3),
            (3, 12, 3),
            (4, 12, 3),
            (5, 3, 12),
            (6, 3, 12),
            (7, 3, 12),
            (8, 3, 12),
        ],
        rs: (12, 3),
        dec_sha: "7e5297696ca729908f05fcf0ef0d5418e132a3d27f0e855d945b28027fff2220",
        o6_sha: "6752dde6aa6bd3ac880d353b71125cebea6721bbcb7534457275dd8345be7785",
        rs_sha: "7e5297696ca729908f05fcf0ef0d5418e132a3d27f0e855d945b28027fff2220",
        rs_png_sha: "0c4860bc2f303c6956789c25c0bb84945cf1331bb0622c624ec7e41c5d12f7d0",
        rs_jpg_sha: "e3397bede212e2c26714406f5c2054516b8b71436d808f6e03ba0fde24b2b97b",
    },
    Answer {
        name: "tgarle-1x1-9.tga",
        kind: "tgarle",
        w: 1,
        h: 1,
        seed: 9,
        dec: (1, 1),
        hash: 0,
        orient: [
            (1, 1, 1),
            (2, 1, 1),
            (3, 1, 1),
            (4, 1, 1),
            (5, 1, 1),
            (6, 1, 1),
            (7, 1, 1),
            (8, 1, 1),
        ],
        rs: (1, 1),
        dec_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        o6_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        rs_sha: "6ba448ad059f32f7d602eadc87f8074501bbd87633a7a26385bfb3bd78fe98c5",
        rs_png_sha: "3a048a74d6937c4e406d2c78c9ce1da8df619ded1f54c2559f2fa1c03ae41d09",
        rs_jpg_sha: "fe4a6106eed0dbd78f89a3a71e125e05255f27d91d88bb181f4618846b3f0ae7",
    },
    Answer {
        name: "ppm-40x24-0.ppm",
        kind: "ppm",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9222520889437335616,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        o6_sha: "dadf73e0983caec7ee167be363beff0662658af88a7d9be2f718473ff470292b",
        rs_sha: "d13f031b56f5643215719755a4bcdcecb9871fa91c71804bb34a871a44add41d",
        rs_png_sha: "dc1831cef95374b744ff217e572c6a74899d6419c9df6507bdb1fa8d43ff86ad",
        rs_jpg_sha: "9cd264e6f02fcc8726bb8f36f35657c6c43e540362547a03b5a040a896a60872",
    },
    Answer {
        name: "ppm-3x3-10.ppm",
        kind: "ppm",
        w: 3,
        h: 3,
        seed: 10,
        dec: (3, 3),
        hash: 18446736346941800640,
        orient: [
            (1, 3, 3),
            (2, 3, 3),
            (3, 3, 3),
            (4, 3, 3),
            (5, 3, 3),
            (6, 3, 3),
            (7, 3, 3),
            (8, 3, 3),
        ],
        rs: (3, 3),
        dec_sha: "2b30ecdd1bb0e52058518b108fdc2a4daf4295c6a61cc3579e0fff55128d3d6b",
        o6_sha: "09d7f6ae0cf95ee9d42a34da14b40c8d8f51d9e1b4bcd398e70cfb31d8654234",
        rs_sha: "2b30ecdd1bb0e52058518b108fdc2a4daf4295c6a61cc3579e0fff55128d3d6b",
        rs_png_sha: "ff3c35c6b1e99246fea3578c9079bd870049aeccc552ea14550f491b095661c0",
        rs_jpg_sha: "8c80bcea853f6f528df709d1d347cf2d5c35deadf82b6e4fb4fcf928b074c7b9",
    },
    Answer {
        name: "pgm-40x24-0.pgm",
        kind: "pgm",
        w: 40,
        h: 24,
        seed: 0,
        dec: (40, 24),
        hash: 9222520889437335616,
        orient: [
            (1, 40, 24),
            (2, 40, 24),
            (3, 40, 24),
            (4, 40, 24),
            (5, 24, 40),
            (6, 24, 40),
            (7, 24, 40),
            (8, 24, 40),
        ],
        rs: (40, 24),
        dec_sha: "798658351f3e66598b09a0397811e0bddaf45e0eeb1290b882b9692ad7d21ba2",
        o6_sha: "bf6c544d607bb9bb7fba4e9b5b4007830b554533d16f4b01738fb8b9547f8ceb",
        rs_sha: "798658351f3e66598b09a0397811e0bddaf45e0eeb1290b882b9692ad7d21ba2",
        rs_png_sha: "153963f1ec65c8de70f31c651196826da67eb552022e7d107ee418e8120934da",
        rs_jpg_sha: "78dbe683ced83214415c386cfdf9f120fb7f58f1820982de8846a31aeb6e8a90",
    },
];

const NOT_READ_YET: [(&str, Option<&str>); 11] = [
    (
        "cr2",
        Some("RAW pictures (CR2) aren't read on this system yet"),
    ),
    (
        "nef",
        Some("RAW pictures (NEF) aren't read on this system yet"),
    ),
    (
        "arw",
        Some("RAW pictures (ARW) aren't read on this system yet"),
    ),
    (
        "dng",
        Some("RAW pictures (DNG) aren't read on this system yet"),
    ),
    (
        "raf",
        Some("RAW pictures (RAF) aren't read on this system yet"),
    ),
    (
        "orf",
        Some("RAW pictures (ORF) aren't read on this system yet"),
    ),
    (
        "rw2",
        Some("RAW pictures (RW2) aren't read on this system yet"),
    ),
    (
        "CR2",
        Some("RAW pictures (CR2) aren't read on this system yet"),
    ),
    ("webp", None),
    ("svg", None),
    ("", None),
];

const SIZES: [(&u64, &str); 8] = [
    (&0, "0 MB"),
    (&1048576, "1 MB"),
    (&1048575, "0 MB"),
    (&1500000, "1 MB"),
    (&1073741823, "1023 MB"),
    (&1073741824, "1.0 GB"),
    (&1500000000, "1.4 GB"),
    (&5000000000, "4.7 GB"),
];

const WRITE_WHY: &str = "unreadable: can't write x.png";

#[test]
fn every_picture_the_portable_path_reads_decodes_to_the_engines_pixels() {
    let folder = fixture_folder("decode");
    for answer in ANSWERS {
        let source = pixels(answer.w, answer.h, answer.seed);
        write_fixture(&folder, answer, &source);
        let path = folder.join(answer.name);
        let decoded = rgb::RgbImage::load(&path)
            .unwrap_or_else(|| panic!("{}: the engine decoded this file", answer.name));
        assert_eq!(
            (decoded.width, decoded.height),
            answer.dec,
            "{}: dims",
            answer.name
        );
        assert_eq!(
            (decoded.width * decoded.height * 3),
            decoded.pixels.len(),
            "{}: packed RGB",
            answer.name
        );
        assert_eq!(
            digest(&decoded.pixels),
            answer.dec_sha,
            "{}: the decoded pixels",
            answer.name
        );
        // Decoding the file's bytes is the path `decodeWithFFmpeg`'s PPM goes through.
        let bytes = std::fs::read(&path).unwrap();
        let from_bytes = rgb::RgbImage::load_bytes(&bytes).expect("the same file's bytes");
        assert_eq!(
            digest(&from_bytes.pixels),
            answer.dec_sha,
            "{}: the same bytes decode the same",
            answer.name
        );
        assert_eq!(
            decoded.average_hash(),
            answer.hash,
            "{}: 8x8 average hash",
            answer.name
        );
        // stb's own header numbers, which `prepare` measures the `minSide` gate on.
        let (width, height, _) = stb::info(&path).unwrap();
        assert_eq!(
            (width as usize, height as usize),
            answer.dec,
            "{}: stbi_info",
            answer.name
        );
    }
}

#[test]
fn the_eight_camera_turns_are_the_engines() {
    let folder = fixture_folder("turns");
    for answer in ANSWERS {
        write_fixture(&folder, answer, &pixels(answer.w, answer.h, answer.seed));
        let image = rgb::RgbImage::load(&folder.join(answer.name)).expect("decoded");
        let turned: Vec<(i64, usize, usize)> = (1..=8i64)
            .map(|orientation| {
                let out = image.clone().oriented(orientation);
                (orientation, out.width, out.height)
            })
            .collect();
        assert_eq!(
            turned.to_vec(),
            answer.orient,
            "{}: the 8 turns",
            answer.name
        );
        assert_eq!(
            digest(&image.clone().oriented(6).pixels),
            answer.o6_sha,
            "{}: the pixels after the 90° clockwise turn",
            answer.name
        );
        // A turn is its own picture: the byte count is kept, and turning twice 90° is 180°.
        let twice = image.clone().oriented(6).oriented(6);
        assert_eq!(
            (twice.width, twice.height),
            (answer.dec.0, answer.dec.1),
            "{}: 180°",
            answer.name
        );
        assert_eq!(
            image.clone().oriented(0),
            image,
            "{}: an orientation outside 1…8 changes nothing",
            answer.name
        );
    }
}

#[test]
fn the_scaled_copy_and_its_encodings_are_the_engines() {
    let folder = fixture_folder("scaled");
    for answer in ANSWERS {
        write_fixture(&folder, answer, &pixels(answer.w, answer.h, answer.seed));
        let image = rgb::RgbImage::load(&folder.join(answer.name)).expect("decoded");
        let (width, height) = target(image.width, image.height);
        assert_eq!(
            (width, height),
            answer.rs,
            "{}: the scaled dims",
            answer.name
        );
        let small = image.resized(width, height).expect("stb scales it");
        assert_eq!(
            digest(&small.pixels),
            answer.rs_sha,
            "{}: the scaled pixels",
            answer.name
        );
        // The two copies the model is fed: PNG (lossless through the scale) and JPEG at quality 90.
        let png = folder.join(format!("{}.png", answer.name));
        let jpg = folder.join(format!("{}.jpg", answer.name));
        small.write(&png, true).expect("write the PNG");
        small.write(&jpg, false).expect("write the JPEG");
        assert_eq!(
            digest(&std::fs::read(&png).unwrap()),
            answer.rs_png_sha,
            "{}: the PNG stb wrote",
            answer.name
        );
        assert_eq!(
            digest(&std::fs::read(&jpg).unwrap()),
            answer.rs_jpg_sha,
            "{}: the JPEG stb wrote",
            answer.name
        );
        // A copy at the engine's own ceiling is left alone, and a 1-pixel side stays 1 pixel.
        let whole = image.resized(image.width, image.height).expect("same size");
        assert_eq!(whole.pixels.len(), image.pixels.len());
        let one = image.resized(1, 1).expect("1×1");
        assert_eq!((one.width, one.height, one.pixels.len()), (1, 1, 3));
    }
}

#[test]
// The platform facts the portable path is built on are constants here on purpose: the pin is that
// they stay this way when the port grows.
#[allow(clippy::assertions_on_constants)]
fn the_refusal_notes_and_sizes_are_the_engines() {
    // `prepare` asks with the file's `pathExtension` lowercased, so an uppercase name is the same
    // kind of picture ("CR2" is "cr2"), and the note names it in capitals.
    for (ext, want) in NOT_READ_YET {
        assert_eq!(
            image::not_read_yet(&ext.to_lowercase()).as_deref(),
            want,
            "{ext}: the note the engine gives"
        );
    }
    for (bytes, want) in SIZES {
        assert_eq!(
            image::decode_size(*bytes),
            *want,
            "{bytes}: how the memory preflight spells it"
        );
    }
    // A copy the writer cannot place names the file it tried to write, the way index-service reports.
    let missing = std::env::temp_dir()
        .join("digup-image-missing-folder-does-not-exist")
        .join("x.png");
    let why = pixels(2, 2, 0)
        .write(&missing, true)
        .expect_err("no such folder")
        .to_string();
    assert_eq!(why, WRITE_WHY);
    assert!(
        !image::DRAWS_VECTOR_PICTURES,
        "stb and ffmpeg draw no SVG here"
    );
}
