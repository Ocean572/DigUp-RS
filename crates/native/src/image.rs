//! The picture seam of the portable platform (`ImageExtractor`, `Images+Portable.swift:7-104`):
//! the copy the vision encoder is fed, the camera's date, and which kinds of picture this system
//! can read at all.
//!
//! `prepare` is the engine's order, step for step: read the EXIF headers, ask stb's header for the
//! stored size, run the memory preflight of issue #182 BEFORE decoding anything, decode the whole
//! picture (stb, or ffmpeg as 8-bit RGB down a PPM pipe for what stb can't read), turn it the way
//! the camera held it, scale it so the long side is `maxPixel`, and write the copy under a UUID
//! name. The refusal texts are what `index-service` reports and what the app shows.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use digup_core::extract::ExtractError;
use digup_core::uuid::uuid_string;

use crate::exif;
use crate::ffmpeg;
use crate::rgb::RgbImage;
use crate::stb;

/// `ImageExtractor.drawsVectorPictures` on this platform: stb and ffmpeg don't draw vector
/// pictures, so an `.svg` is never read here.
pub const DRAWS_VECTOR_PICTURES: bool = false;

/// `ImageExtractor.portableDecodeBytesPerPixel`: stb decodes the whole picture (3 B/px),
/// `oriented` copies it, and the scaling copies it again. Estimated, not measured.
pub const PORTABLE_DECODE_BYTES_PER_PIXEL: u64 = 8;

/// `ImageExtractor.rawDecodeBytesPerPixel` — measured on macOS, and RAW is read on that side only.
/// The constant travels with the preflight so both sides of the port read the same note.
pub const RAW_DECODE_BYTES_PER_PIXEL: u64 = 192;

/// `ImageExtractor.layeredTIFFBytesPerPixel`: every full-resolution layer decoded with its masks.
pub const LAYERED_TIFF_BYTES_PER_PIXEL: u64 = 16;

/// `ImageExtractor.decodeMemoryBudget`: what reading one picture may cost this helper in memory,
/// set at its start from `--memory-limit`. 0 = no preflight, for the tools that don't index under
/// a watchdog. The engine's `nonisolated(unsafe) static var` is an atomic here.
static DECODE_MEMORY_BUDGET: AtomicU64 = AtomicU64::new(0);

pub fn decode_memory_budget() -> u64 {
    DECODE_MEMORY_BUDGET.load(Ordering::Relaxed)
}

pub fn set_decode_memory_budget(bytes: u64) {
    DECODE_MEMORY_BUDGET.store(bytes, Ordering::Relaxed);
}

/// `ImageExtractor.Prepared`: the copy the model reads, the size the index records (as STORED, the
/// way ImageIO reports it), and the camera's own `DateTimeOriginal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub path: String,
    pub width: i64,
    pub height: i64,
    pub taken: Option<String>,
}

/// `ImageExtractor.notReadYet(_:)`: why a RAW picture is not readable on this system, `None` for
/// every other kind. The set is the camera RAW list stb and ffmpeg do not decode.
pub fn not_read_yet(ext: &str) -> Option<String> {
    const RAW: [&str; 7] = ["cr2", "nef", "arw", "dng", "raf", "orf", "rw2"];
    RAW.contains(&ext).then(|| {
        format!(
            "RAW pictures ({}) aren't read on this system yet",
            ext.to_uppercase()
        )
    })
}

/// `ImageExtractor.decodeSize(_:)`: how the memory preflight spells a size — at or above a GiB as
/// one decimal in gigabytes (`String(format: "%.1f GB", …)`, which is C's printf rounding a binary
/// double, the same rule Rust's `{:.1}` follows), below it as whole mebibytes (`bytes >> 20`).
pub fn decode_size(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    if bytes >= GIB {
        return format!("{:.1} GB", bytes as f64 / GIB as f64);
    }
    format!("{} MB", bytes >> 20)
}

/// A path's `pathExtension` lowercased, the way `prepare` asks the question.
pub fn lower_extension(path: &Path) -> String {
    digup_core::code::extension_of(&path.to_string_lossy()).to_lowercase()
}

/// `ImageExtractor.prepare(_:maxPixel:minSide:to:png:)`.
pub fn prepare(
    path: &Path,
    max_pixel: i64,
    min_side: i64,
    folder: &Path,
    png: bool,
) -> Result<Prepared, ExtractError> {
    let meta = exif::read(path);
    let orientation = meta.as_ref().map_or(1, |exif| exif.orientation);

    let image;
    let (width, height);
    if let Some((stored_width, stored_height, _)) = stb::info(path) {
        let (stored_width, stored_height) = (i64::from(stored_width), i64::from(stored_height));
        if stored_width.max(stored_height) < min_side {
            return Err(ExtractError::TooSmall {
                width: stored_width,
                height: stored_height,
            });
        }
        // Here the whole picture is decoded, turned and scaled, so full-size pixels are the peak
        // and the header alone can tell it: a giant JPEG decodes even where ImageIO would stream
        // tiles. The engine's `UInt64 … &* …` (issue #182 review S4): stb reports the header's own
        // numbers, and a file lying about them (a 4-byte dimension of 0x7FFFFFFF) must give a huge
        // estimate, not an overflow trap inside the guard built to stop traps. `&*` is WRAPPING
        // multiplication — a 2,147,483,647² picture × 8 B is 2^65 mod 2^64, and the refusal names
        // THAT number (17179869183.4 GB), so the port wraps exactly like the engine, in both
        // multiplications. (Saturating would answer 18446744073.7 GB: the gate measures strings.)
        let pixels = (stored_width.max(0) as u64).wrapping_mul((stored_height.max(0)) as u64);
        let estimate = pixels.wrapping_mul(PORTABLE_DECODE_BYTES_PER_PIXEL);
        let budget = decode_memory_budget();
        if budget != 0 && estimate > budget {
            return Err(ExtractError::Unreadable {
                why: format!(
                    "needed an estimated {} of memory to read: a picture of {} MP (the helper's limit is {})",
                    decode_size(estimate),
                    pixels / 1_000_000,
                    decode_size(budget)
                ),
            });
        }
        let decoded = RgbImage::load(path).ok_or_else(|| ExtractError::Unreadable {
            why: "can't decode image".into(),
        })?;
        image = decoded.oriented(orientation);
        width = stored_width;
        height = stored_height;
    } else {
        if let Some(note) = not_read_yet(&lower_extension(path)) {
            return Err(ExtractError::Unreadable { why: note });
        }
        if meta.as_ref().is_some_and(|exif| exif.tiled) {
            return Err(ExtractError::Unreadable {
                why: "tiled TIFF pictures aren't read on this system yet".into(),
            });
        }
        image = decode_with_ffmpeg(path)?;
        // Its size as shown (ImageIO reports it as stored: the two differ for a picture turned 90°).
        width = image.width as i64;
        height = image.height as i64;
        if width.max(height) < min_side {
            return Err(ExtractError::TooSmall { width, height });
        }
    }
    let mut image = image;
    let longest = image.width.max(image.height);
    if longest as i64 > max_pixel {
        let scale = max_pixel as f64 / longest as f64;
        let scaled = |side: usize| (side as f64 * scale).round().max(1.0) as usize;
        image = image.resized(scaled(image.width), scaled(image.height))?;
    }
    // The copy's own name is a Foundation UUID, uppercased as `uuidString` spells it.
    let name = format!("{}.{}", uuid_string(), if png { "png" } else { "jpg" });
    let out = folder.join(name);
    image.write(&out, png)?;
    Ok(Prepared {
        path: out.to_string_lossy().into_owned(),
        width,
        height,
        taken: meta.and_then(|exif| exif.date_time_original),
    })
}

/// `ImageExtractor.decodeWithFFmpeg(_:)`: the whole picture, through ffmpeg as 8-bit RGB (PPM down
/// a pipe), under the same memory limit the stb path enforces before decoding.
pub fn decode_with_ffmpeg(path: &Path) -> Result<RgbImage, ExtractError> {
    let tool = ffmpeg::ffmpeg();
    if tool.is_none() {
        return Err(ExtractError::Unreadable {
            why: "ffmpeg isn't installed (HEIC, WebP and TIFF pictures need it)".into(),
        });
    }
    let budget = decode_memory_budget();
    let mut data: Vec<u8> = Vec::new();
    let arguments = ffmpeg::picture_arguments(path);
    let run = ffmpeg::run(tool.as_deref(), &arguments, &mut |chunk: &[u8]| {
        data.extend_from_slice(chunk);
        // The picture whole, made as PPM over the pipe (3 bytes per pixel, before any scaling),
        // lands in the helper's own memory: the helper's limit stops the reading, and `run`
        // stops ffmpeg with it (issue #182: this is where a huge TIFF/HEIC lands on this side).
        if budget != 0 && (data.len() as u64) > budget {
            return Err(ExtractError::Unreadable {
                why: format!("needed more than {} of memory to read", decode_size(budget)),
            });
        }
        Ok(())
    });
    if let Err(error) = run {
        return match error {
            // The memory cap above, not a broken picture.
            ffmpeg::RunError::Extract(error) => Err(error),
            ffmpeg::RunError::Io(_) => {
                let ext = lower_extension(path);
                if ext == "heic" || ext == "heif" {
                    Err(ExtractError::Unreadable {
                        why: "this ffmpeg can't read HEIC (8.1 can)".into(),
                    })
                } else {
                    Err(ExtractError::Unreadable {
                        why: "can't decode image".into(),
                    })
                }
            }
        };
    }
    RgbImage::load_bytes(&data).ok_or_else(|| ExtractError::Unreadable {
        why: "can't decode image".into(),
    })
}
