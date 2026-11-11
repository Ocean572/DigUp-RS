//! Packed 8-bit RGB in memory: what stb decodes to and encodes from — the port of `RGBImage`
//! (`Images+Portable.swift:107-202`), the picture the vision encoder is fed.
//!
//! The shapes that matter for parity are the ones the engine shows the model: `oriented` turns
//! the picture the way the camera held it (EXIF 1–8) exactly as ImageIO's thumbnails are on
//! macOS, `resized` scales in sRGB space with stb's own scaler, `write` encodes the downscaled
//! copy (PNG, or JPEG at quality 90), and `average_hash` is the 8×8 perceptual hash the video
//! side uses for its keyframes.

use std::path::Path;

use digup_core::extract::ExtractError;

use crate::stb;

/// `RGBImage`: packed RGB, three bytes per pixel, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl RgbImage {
    /// `RGBImage.load(_ path:)`: decoded from a file, `None` when stb can't read it.
    pub fn load(path: &Path) -> Option<Self> {
        let (width, height, pixels) = stb::load(path)?;
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// `RGBImage.load(_ data:)`: the same from a file's bytes. The engine's own ceiling is
    /// `Int(Int32.max)` bytes, because stb's length argument is an `Int32`.
    pub fn load_bytes(data: &[u8]) -> Option<Self> {
        let (width, height, pixels) = stb::load_bytes(data)?;
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// `oriented(_:)`: turned the way the camera held it (EXIF orientation 1–8); 1 (and any
    /// nonsense value) answers the picture unchanged. The source pixel that lands at (x, y) is
    /// the engine's table — `Images+Portable.swift:144-152` — with the swapped axes for the
    /// orientations that transpose (5–8).
    pub fn oriented(&self, orientation: i64) -> Self {
        if !(2..=8).contains(&orientation) {
            return self.clone();
        }
        let swaps = orientation >= 5;
        let (w, h) = if swaps {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let mut out = vec![0u8; self.pixels.len()];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = match orientation {
                    2 => (self.width - 1 - x, y),                   // mirrored
                    3 => (self.width - 1 - x, self.height - 1 - y), // upside down
                    4 => (x, self.height - 1 - y),                  // flipped
                    5 => (y, x),                                    // transposed
                    6 => (y, self.height - 1 - x),                  // 90° clockwise
                    7 => (self.width - 1 - y, self.height - 1 - x), // transverse
                    _ => (self.width - 1 - y, x),                   // 8: 90° counterclockwise
                };
                let s = (sy * self.width + sx) * 3;
                let d = (y * w + x) * 3;
                out[d] = self.pixels[s];
                out[d + 1] = self.pixels[s + 1];
                out[d + 2] = self.pixels[s + 2];
            }
        }
        Self {
            width: w,
            height: h,
            pixels: out,
        }
    }

    /// `resized(width:height:)`: scaled by stb's sRGB scaler (stride 0 = the natural one). A
    /// refusal is the engine's "can't resize image".
    pub fn resized(&self, new_width: usize, new_height: usize) -> Result<Self, ExtractError> {
        let pixels = stb::resize(&self.pixels, self.width, self.height, new_width, new_height)
            .ok_or_else(|| ExtractError::Unreadable {
                why: "can't resize image".into(),
            })?;
        Ok(Self {
            width: new_width,
            height: new_height,
            pixels,
        })
    }

    /// `write(to:png:)`: the downscaled copy, as PNG or as JPEG at quality 90. The failure names
    /// the file it tried to write, which is what `index-service` reports.
    pub fn write(&self, path: &Path, png: bool) -> Result<(), ExtractError> {
        if stb::write(path, self.width, self.height, &self.pixels, png) {
            return Ok(());
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Err(ExtractError::Unreadable {
            why: format!("can't write {name}"),
        })
    }

    /// `averageHash()`: the 8×8 average hash — each cell's mean gray, and the cells brighter than
    /// the mean of all 64 set their bit. `VideoExtractor.averageHash` on macOS, cell by cell.
    pub fn average_hash(&self) -> u64 {
        let side = 8usize;
        let mut cells = vec![0i64; side * side];
        for cy in 0..side {
            let y0 = cy * self.height / side;
            let y1 = self.height.min((y0 + 1).max((cy + 1) * self.height / side));
            for cx in 0..side {
                let x0 = cx * self.width / side;
                let x1 = self.width.min((x0 + 1).max((cx + 1) * self.width / side));
                let mut sum = 0i64;
                let mut n = 0i64;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let p = (y * self.width + x) * 3;
                        let gray = (i64::from(self.pixels[p]) * 299
                            + i64::from(self.pixels[p + 1]) * 587
                            + i64::from(self.pixels[p + 2]) * 114)
                            / 1000;
                        sum += gray;
                        n += 1;
                    }
                }
                cells[cy * side + cx] = if n > 0 { sum / n } else { 0 };
            }
        }
        let total: i64 = cells.iter().sum();
        let mean = total / (cells.len() as i64);
        let mut hash = 0u64;
        for (index, cell) in cells.iter().enumerate() {
            if *cell > mean {
                hash |= 1 << index;
            }
        }
        hash
    }
}
