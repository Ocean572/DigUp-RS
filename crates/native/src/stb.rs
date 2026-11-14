//! The vendored stb surface (`Vendor/stb`, compiled by `build.rs`): the decoder, the sRGB scaler,
//! the PNG/JPEG writers. The declarations mirror the headers' C signatures; `STBIR_RGB` is the
//! public `stbir_pixel_layout` value 3 (the header's `#define STBIR_RGB bad_dont_use_in_…` block
//! lives inside `STB_IMAGE_RESIZE_IMPLEMENTATION`, so consumers see the enum).

use std::ffi::CString;
use std::path::Path;

use libc::{c_char, c_int, c_uchar, c_void};

// edition 2021: plain `extern "C"` blocks (the workspace's `rust-version` predates the 2024
// spelling), so each call is wrapped in `unsafe { … }` at the seam below.
extern "C" {
    fn stbi_info(filename: *const c_char, x: *mut c_int, y: *mut c_int, comp: *mut c_int) -> c_int;
    fn stbi_load(
        filename: *const c_char,
        x: *mut c_int,
        y: *mut c_int,
        comp: *mut c_int,
        req_comp: c_int,
    ) -> *mut c_uchar;
    fn stbi_load_from_memory(
        buffer: *const c_uchar,
        len: c_int,
        x: *mut c_int,
        y: *mut c_int,
        comp: *mut c_int,
        req_comp: c_int,
    ) -> *mut c_uchar;
    fn stbi_image_free(retval: *mut c_void);
    fn stbir_resize_uint8_srgb(
        input_pixels: *const c_uchar,
        input_w: c_int,
        input_h: c_int,
        input_stride_in_bytes: c_int,
        output_pixels: *mut c_uchar,
        output_w: c_int,
        output_h: c_int,
        output_stride_in_bytes: c_int,
        num_channels: c_int,
    ) -> *mut c_uchar;
    fn stbi_write_png(
        filename: *const c_char,
        w: c_int,
        h: c_int,
        comp: c_int,
        data: *const c_void,
        stride_in_bytes: c_int,
    ) -> c_int;
    fn stbi_write_jpg(
        filename: *const c_char,
        x: c_int,
        y: c_int,
        comp: c_int,
        data: *const c_void,
        quality: c_int,
    ) -> c_int;
}

/// `STBIR_RGB` from the public layout enum: 3 channels, order as given.
const STBIR_RGB: c_int = 3;

fn c_path(path: &Path) -> Option<CString> {
    #[cfg(unix)]
    let bytes: Vec<u8> = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes: Vec<u8> = path.to_string_lossy().as_bytes().to_vec();
    CString::new(bytes).ok()
}

/// `stbi_info`: the header's own numbers (width, height, channels), `None` when stb can't read
/// the file at all.
pub fn info(path: &Path) -> Option<(i32, i32, i32)> {
    let name = c_path(path)?;
    let (mut width, mut height, mut channels) = (0 as c_int, 0 as c_int, 0 as c_int);
    // SAFETY: `name` is a NUL-terminated CString that outlives the call; the three
    // `&mut c_int` are live, aligned, initialized locals — stbi_info writes ints through
    // them and reads `name` only up to the NUL. stb's public API: a failed read returns 0.
    let read = unsafe { stbi_info(name.as_ptr(), &mut width, &mut height, &mut channels) };
    (read != 0).then_some((width, height, channels))
}

/// `stbi_load(…, req_comp 3)`: the whole picture as packed RGB.
pub fn load(path: &Path) -> Option<(usize, usize, Vec<u8>)> {
    let name = c_path(path)?;
    let (mut width, mut height, mut channels) = (0 as c_int, 0 as c_int, 0 as c_int);
    // SAFETY: `name` is a NUL-terminated CString alive for the call; the `&mut c_int` are
    // live initialized locals; `take` below is the only consumer of the returned pointer
    // and frees it with stbi_image_free. stbi_load returns malloc'd pixels (req_comp 3
    // forces RGB) or NULL.
    let pixels = unsafe { stbi_load(name.as_ptr(), &mut width, &mut height, &mut channels, 3) };
    take(pixels, width, height)
}

/// `stbi_load_from_memory(…, req_comp 3)`: the same from a file's bytes (`None` past
/// `Int32.max` bytes, the ceiling the engine's own guard uses).
pub fn load_bytes(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if data.is_empty() || data.len() > i32::MAX as usize {
        return None;
    }
    let (mut width, mut height, mut channels) = (0 as c_int, 0 as c_int, 0 as c_int);
    // SAFETY: `data` is borrowed immutably for the whole call (the decode reads its bytes
    // synchronously), its length is guarded above to `1..=i32::MAX` (stb's length
    // argument is an Int32 — the engine's own ceiling), the `&mut c_int` are live
    // initialized locals, and `take` frees the returned buffer.
    let pixels = unsafe {
        stbi_load_from_memory(
            data.as_ptr(),
            data.len() as c_int,
            &mut width,
            &mut height,
            &mut channels,
            3,
        )
    };
    take(pixels, width, height)
}

/// Copy the decoded pixels out of stb's buffer and hand the buffer back.
fn take(pixels: *mut c_uchar, width: c_int, height: c_int) -> Option<(usize, usize, Vec<u8>)> {
    if pixels.is_null() || width <= 0 || height <= 0 {
        if !pixels.is_null() {
            unsafe { stbi_image_free(pixels as *mut c_void) };
        }
        return None;
    }
    let count = width as usize * height as usize * 3;
    // SAFETY: stbi_load/stbi_load_from_memory with req_comp 3 returns a buffer of
    // `width * height * 3` initialized bytes (checked positive above) owned by stb;
    // the slice borrows it only for the `to_vec()` copy, and `stbi_image_free` (the
    // library's own free, called after the copy and never a second time on this
    // pointer) returns it. `count` cannot overflow: width/height are checked `> 0` and
    // stb caps each at STBIR__MAX (well below usize::MAX/3 on every supported target).
    let out = unsafe { std::slice::from_raw_parts(pixels, count).to_vec() };
    unsafe { stbi_image_free(pixels as *mut c_void) };
    Some((width as usize, height as usize, out))
}

/// `stbir_resize_uint8_srgb`: sRGB-space scaling (`false` when stb refuses, which the engine
/// reports as "can't resize image").
pub fn resize(
    pixels: &[u8],
    width: usize,
    height: usize,
    to_width: usize,
    to_height: usize,
) -> Option<Vec<u8>> {
    let mut out = vec![0u8; to_width * to_height * 3];
    // SAFETY: the source slice is read within `width * height * 3` bytes, the destination
    // buffer is exactly `to_width * to_height * 3` initialized bytes with matching strides
    // (0 = natural), and both live for the call; stbir writes only inside the destination
    // (it fills the whole buffer when it succeeds, leaves it untouched and returns NULL
    // when it refuses). Callers pass the sizes the picture was decoded with.
    let done = unsafe {
        stbir_resize_uint8_srgb(
            pixels.as_ptr(),
            width as c_int,
            height as c_int,
            0,
            out.as_mut_ptr(),
            to_width as c_int,
            to_height as c_int,
            0,
            STBIR_RGB,
        )
    };
    (!done.is_null()).then_some(out)
}

/// `stbi_write_png` / `stbi_write_jpg` (the engine writes JPEGs at quality 90).
pub fn write(path: &Path, width: usize, height: usize, pixels: &[u8], png: bool) -> bool {
    let Some(name) = c_path(path) else {
        return false;
    };
    // SAFETY: `name` is a NUL-terminated CString alive for the call; `pixels` is read as
    // `width * height * 3` bytes (the callers pass the decoded picture's own numbers, and
    // `RgbImage` guarantees its vector is exactly that long); stb writes only to the file
    // named and returns 0/1. The engine's writer uses quality 90 for JPEG
    // (`jpeg(pixels:quality:90)`), which is the literal below.
    let written = if png {
        unsafe {
            stbi_write_png(
                name.as_ptr(),
                width as c_int,
                height as c_int,
                3,
                pixels.as_ptr() as *const c_void,
                (width * 3) as c_int,
            )
        }
    } else {
        unsafe {
            stbi_write_jpg(
                name.as_ptr(),
                width as c_int,
                height as c_int,
                3,
                pixels.as_ptr() as *const c_void,
                90,
            )
        }
    };
    written != 0
}
