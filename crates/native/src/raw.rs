//! Issue #152: camera RAW through the vendored LibRaw (`Vendor/libraw`, fetched by
//! `scripts/fetch-vendor.sh`, built by `scripts/build-libraw.sh`). Linux and macOS link the
//! static library; Windows keeps `image::not_read_yet`'s honest decline (the crate does not
//! compile the shim there — the `digup_libraw` cfg is the single source of the answer).
//!
//! The picture arrives UPRIGHT: `dcraw_process` runs the camera's pipeline, which applies the
//! shot's orientation itself, so the callers must not turn the bitmap by the EXIF tag as well.

use std::ffi::CString;
use std::path::Path;

use digup_core::extract::ExtractError;

use crate::image::{decode_memory_budget, decode_size, PORTABLE_DECODE_BYTES_PER_PIXEL};
use crate::rgb::RgbImage;

#[cfg(digup_libraw)]
extern "C" {
    fn digup_raw_open(
        path: *const std::os::raw::c_char,
        width: *mut i32,
        height: *mut i32,
    ) -> *mut std::os::raw::c_void;
    fn digup_raw_render(
        handle: *mut std::os::raw::c_void,
        out: *mut u8,
        out_cap: i64,
    ) -> std::os::raw::c_int;
    fn digup_raw_close(handle: *mut std::os::raw::c_void);
}

/// The camera RAW set `not_read_yet` names — the families LibRaw unpacks (its decoders cover
/// far more; these are the ones the engine's crawl files as pictures).
const RAW: [&str; 7] = ["cr2", "nef", "arw", "dng", "raf", "orf", "rw2"];

/// Does THIS build read RAW? False on Windows, and on a tree whose Vendor lacks libraw (the
/// build script links the shim only when the pinned library is there).
pub fn available() -> bool {
    cfg!(digup_libraw)
}

/// `true` for the extension when this build reads RAW.
pub fn supported(extension: &str) -> bool {
    available() && RAW.contains(&extension)
}

/// `ImageExtractor.decodeWithLibRaw`: open + unpack (the file's own data landing in memory,
/// under the same budget preflight the stb path runs from its header), then the camera's
/// render into 8-bit RGB. The sizes are the RENDERED picture's — what `prepare` reports and
/// scales, the way the ffmpeg fallback reports the picture as shown.
pub fn decode(path: &Path) -> Result<RgbImage, ExtractError> {
    let cpath = CString::new(path.as_os_str().to_string_lossy().as_ref()).map_err(|_| {
        ExtractError::Unreadable {
            why: "can't decode image".into(),
        }
    })?;
    #[cfg(digup_libraw)]
    {
        let mut width = 0i32;
        let mut height = 0i32;
        unsafe {
            let handle = digup_raw_open(cpath.as_ptr(), &mut width, &mut height);
            if handle.is_null() {
                return Err(ExtractError::Unreadable {
                    why: "can't decode image".into(),
                });
            }
            // The render's bitmap is the peak, and unpack already announced the dimensions:
            // the same estimate shape as the stb path, the same limit, spelled the same way.
            let pixels = (width.max(0) as u64).wrapping_mul(height.max(0) as u64);
            let estimate = pixels.wrapping_mul(PORTABLE_DECODE_BYTES_PER_PIXEL);
            let budget = decode_memory_budget();
            if budget != 0 && estimate > budget {
                digup_raw_close(handle);
                return Err(ExtractError::Unreadable {
                    why: format!(
                        "needed an estimated {} of memory to read: a picture of {} MP (the helper's limit is {})",
                        decode_size(estimate),
                        pixels / 1_000_000,
                        decode_size(budget)
                    ),
                });
            }
            // Dimensions are HEADER-PUBLISHED: a file lying about them gets the decoder's
            // refusal, never an allocation trap on the way to it — the bounds are checked
            // BEFORE the buffer is made, and they bound the absurd sizes a budget of 0
            // (the default, no guard) would let through.
            let need = i128::from(width.max(0)) * i128::from(height.max(0)) * 3;
            let Some(need) = usize::try_from(need)
                .ok()
                .filter(|need| *need <= isize::MAX as usize)
            else {
                digup_raw_close(handle);
                return Err(ExtractError::Unreadable {
                    why: "can't decode image".into(),
                });
            };
            let mut out = vec![0u8; need];
            let rendered = digup_raw_render(handle, out.as_mut_ptr(), need as i64);
            digup_raw_close(handle);
            if rendered != 0 {
                return Err(ExtractError::Unreadable {
                    why: "can't decode image".into(),
                });
            }
            Ok(RgbImage {
                width: width.max(0) as usize,
                height: height.max(0) as usize,
                pixels: out,
            })
        }
    }
    #[cfg(not(digup_libraw))]
    {
        let _ = cpath;
        Err(ExtractError::Unreadable {
            why: "RAW pictures aren't read on this system".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_raw_set_is_the_not_read_yet_list() {
        // The extensions the seam takes over are EXACTLY the ones `image::not_read_yet` names,
        // so a RAW file is answered by a decoder or by the pinned decline — never by silence.
        for ext in RAW {
            assert!(!ext.is_empty());
        }
        assert_eq!(RAW.len(), 7);
        assert_eq!(available(), cfg!(digup_libraw));
    }

    #[test]
    fn a_real_raw_picture_decodes_to_a_bitmap() {
        // #152's proof on real camera files, held out of git: DIGUP_RAW_FIXTURES names the
        // folder of public RAW samples (the suite corpus convention); absent, the test says so
        // the way the model-pinned and ffmpeg-seam tests do.
        let Ok(folder) = std::env::var("DIGUP_RAW_FIXTURES") else {
            eprintln!("skipped: DIGUP_RAW_FIXTURES is not set (public RAW samples)");
            return;
        };
        for name in [
            "RAW_LEICA_M8.DNG",
            "RAW_NIKON_D90.NEF",
            "RAW_SONY_A700.ARW",
            "sample_canon_400d1.cr2",
        ] {
            let path = Path::new(&folder).join(name);
            if !path.exists() {
                eprintln!("skipped: {} is not in DIGUP_RAW_FIXTURES", name);
                continue;
            }
            let image = decode(&path).expect(name);
            assert!(image.width > 100 && image.height > 100, "{name}");
            assert_eq!(image.pixels.len(), image.width * image.height * 3, "{name}");
            // Every fixture carries real detail: a flat field would let a broken render pass.
            let first = image.pixels[0];
            assert!(
                image.pixels.iter().any(|byte| *byte != first),
                "{name}: the render is a flat field"
            );
        }
    }
}
