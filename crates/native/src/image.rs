//! The picture seam of the portable platform (`ImageExtractor`, `Images+Portable.swift:7-104`):
//! which kinds of picture this system can draw at all, and why the ones it cannot says so.
//!
//! `prepare` — decode, turn, scale, write the copy the model sees — lands with the EXIF reader
//! and the ffmpeg pipe (the memory preflight of issue #182 belongs to it), in the next step.

use std::path::Path;

/// `ImageExtractor.drawsVectorPictures` on this platform: stb and ffmpeg don't draw vector
/// pictures, so an `.svg` is never read here.
pub const DRAWS_VECTOR_PICTURES: bool = false;

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
