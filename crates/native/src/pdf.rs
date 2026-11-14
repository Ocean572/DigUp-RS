//! PDFs without PDFKit (Linux, Windows): PDFium, Chromium's PDF library, prebuilt
//! (`Vendor/pdfium`, `scripts/fetch-vendor.sh`) and linked by `build.rs` — the same library the
//! Swift engine links through `Sources/CPDFium` (`PDF+Portable.swift`). No OCR here, so a text
//! layer that lost letters stays as it is.
//!
//! PDFium is one process-wide library and not thread-safe: every use goes through
//! `with_document`, under `LOCK`. PDFium reads through `FileReader` (any file name opens; a big
//! file is never read whole) — the engine's `FileReader.swift` for the same reason: C's `fopen`
//! and Windows' `CreateFileA` read the ANSI code page, so a Bengali or Arabic file name
//! wouldn't open. Blocks come by offset with `pread`, one at a time.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use digup_core::extract::ExtractError;
use digup_core::search_text::PageTextProblem;
use digup_core::uuid::uuid_string;

use crate::rgb::RgbImage;

// The C side, exactly the declarations in Vendor/pdfium/include (fpdfview.h, fpdf_text.h,
// fpdf_doc.h). `unsigned long` is `c_ulong` (8 bytes on Linux, 4 on Windows): the engine's
// "PDFium counts in C's unsigned long: 4 GB at most on Windows" guard falls out of that type.
#[repr(C)]
struct FpdfFileAccess {
    m_file_len: libc::c_ulong,
    m_get_block: extern "C" fn(
        param: *mut libc::c_void,
        position: libc::c_ulong,
        p_buf: *mut libc::c_uchar,
        size: libc::c_ulong,
    ) -> libc::c_int,
    m_param: *mut libc::c_void,
}

extern "C" {
    fn FPDF_InitLibrary();
    fn FPDF_LoadCustomDocument(
        file_access: *mut FpdfFileAccess,
        password: *const libc::c_char,
    ) -> *mut libc::c_void;
    fn FPDF_GetLastError() -> libc::c_ulong;
    fn FPDF_CloseDocument(document: *mut libc::c_void);
    fn FPDF_GetPageCount(document: *mut libc::c_void) -> libc::c_int;
    fn FPDF_LoadPage(document: *mut libc::c_void, index: libc::c_int) -> *mut libc::c_void;
    fn FPDF_ClosePage(page: *mut libc::c_void);
    fn FPDF_GetPageWidthF(page: *mut libc::c_void) -> f32;
    fn FPDF_GetPageHeightF(page: *mut libc::c_void) -> f32;
    fn FPDF_GetMetaText(
        document: *mut libc::c_void,
        tag: *const libc::c_char,
        buffer: *mut libc::c_void,
        buffer_len: libc::c_ulong,
    ) -> libc::c_ulong;

    fn FPDFText_LoadPage(page: *mut libc::c_void) -> *mut libc::c_void;
    fn FPDFText_ClosePage(text_page: *mut libc::c_void);
    fn FPDFText_CountChars(text_page: *mut libc::c_void) -> libc::c_int;
    fn FPDFText_GetText(
        text_page: *mut libc::c_void,
        start_index: libc::c_int,
        count: libc::c_int,
        result: *mut u16,
    ) -> libc::c_int;

    fn FPDFBitmap_Create(
        width: libc::c_int,
        height: libc::c_int,
        format: libc::c_int,
    ) -> *mut libc::c_void;
    fn FPDFBitmap_Destroy(bitmap: *mut libc::c_void);
    fn FPDFBitmap_FillRect(
        bitmap: *mut libc::c_void,
        x: libc::c_int,
        y: libc::c_int,
        width: libc::c_int,
        height: libc::c_int,
        color: libc::c_ulong,
    ) -> libc::c_int;
    fn FPDF_RenderPageBitmap(
        bitmap: *mut libc::c_void,
        page: *mut libc::c_void,
        x: libc::c_int,
        y: libc::c_int,
        width: libc::c_int,
        height: libc::c_int,
        rotate: libc::c_int,
        flags: libc::c_int,
    );
    fn FPDFBitmap_GetBuffer(bitmap: *mut libc::c_void) -> *mut libc::c_void;
    fn FPDFBitmap_GetStride(bitmap: *mut libc::c_void) -> libc::c_int;
}

/// `FPDF_ERR_PASSWORD` (fpdfview.h:615): PDFium's own word for a protected file.
const FPDF_ERR_PASSWORD: libc::c_ulong = 4;
/// `FPDF_ANNOT` (fpdfview.h:826): draw annotations, as the engine's renders do.
const FPDF_ANNOT: libc::c_int = 0x01;

/// `PDFium.lock` + the one-time `FPDF_InitLibrary()` (`PDF+Portable.swift:9-10`): PDFium is
/// process-wide and not thread-safe, so every call below holds this lock end to end.
static LOCK: Mutex<()> = Mutex::new(());

fn init_library() {
    static READY: OnceLock<()> = OnceLock::new();
    READY.get_or_init(|| unsafe { FPDF_InitLibrary() });
}

/// `FileReader` (`FileReader.swift`): size + a block read by offset. `pread` keeps the file
/// offset private, which is what makes the callback safe to call from any thread while the
/// document is open.
struct FileReader {
    handle: File,
    size: u64,
}

impl FileReader {
    fn open(path: &Path) -> std::io::Result<Self> {
        let handle = File::open(path)?;
        let size = handle.metadata()?.len();
        Ok(Self { handle, size })
    }

    /// Copies up to `count` bytes from `offset` into `buffer`; how many it copied (0 on any
    /// failure, the engine's `guard … else { return 0 }`).
    fn read(&self, offset: u64, buffer: &mut [u8]) -> usize {
        self.handle.read_at(buffer, offset).unwrap_or_default()
    }
}

/// The `m_GetBlock` trampoline: `FileReader.from(param).read(…) == size ? 1 : 0`.
// `c_ulong` is 64-bit on Linux and 32 on Windows: the widening casts are the Windows build's,
// unnecessary only on the gate's own platform.
#[allow(clippy::unnecessary_cast)]
extern "C" fn get_block(
    param: *mut libc::c_void,
    position: libc::c_ulong,
    p_buf: *mut libc::c_uchar,
    size: libc::c_ulong,
) -> libc::c_int {
    if p_buf.is_null() {
        return 0;
    }
    let reader = unsafe { &*(param as *const FileReader) };
    let buffer = unsafe { std::slice::from_raw_parts_mut(p_buf, size as usize) };
    if reader.read(position as u64, buffer) == size as usize {
        1
    } else {
        0
    }
}

/// `PDFium.withDocument`: opens through the block reader, runs `body` under the lock, closes
/// the document after (the reader outlives the document, as `withExtendedLifetime` keeps it in
/// the engine). The password guard answers `locked`; any other failure names "can't open PDF".
pub fn with_document<T>(
    path: &Path,
    body: impl FnOnce(*mut libc::c_void) -> T,
) -> Result<T, ExtractError> {
    init_library();
    let reader = FileReader::open(path).map_err(|e| ExtractError::Unreadable {
        why: format!("can't open PDF ({e})"),
    })?;
    // PDFium counts in C's unsigned long: 4 GB at most on Windows (the cast is Windows's).
    #[allow(clippy::unnecessary_cast)]
    let too_big = reader.size > libc::c_ulong::MAX as u64;
    if too_big {
        return Err(ExtractError::Unreadable {
            why: "too big".to_string(),
        });
    }
    let reader = Box::new(reader);
    let mut access = FpdfFileAccess {
        m_file_len: reader.size as libc::c_ulong,
        m_get_block: get_block,
        m_param: Box::into_raw(reader) as *mut libc::c_void,
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let document = unsafe { FPDF_LoadCustomDocument(&mut access, std::ptr::null()) };
    // Put the reader back so it drops (and closes) after the document is closed.
    let reader = unsafe { Box::from_raw(access.m_param as *mut FileReader) };
    if document.is_null() {
        if unsafe { FPDF_GetLastError() } == FPDF_ERR_PASSWORD {
            return Err(ExtractError::Locked);
        }
        return Err(ExtractError::Unreadable {
            why: "can't open PDF".to_string(),
        });
    }
    let made = body(document);
    unsafe { FPDF_CloseDocument(document) };
    drop(reader);
    Ok(made)
}

/// `PDFium.text`: the page's text in reading order, lines ending in "\n"
/// (`PDF+Portable.swift:39-49`). `FPDFText_GetText` counts the terminator in `written`, so the
/// text is `written - 1` units; lone \r and \r\n pairs become \n. UTF-16 that doesn't decode
/// takes the replacement characters, which is Foundation's `String(decoding:as:)`.
/// takes a PDFium handle — the engine's Swift API has the same shape (`PDFium.text(_ page:
/// FPDF_PAGE)`), and these are only ever called with a live handle from `with_document`,
/// which is what makes them safe; `clippy::not_unsafe_ptr_arg_deref` is allowed per function
/// with that argument.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn page_text(page: *mut libc::c_void) -> String {
    let text_page = unsafe { FPDFText_LoadPage(page) };
    if text_page.is_null() {
        return String::new();
    }
    let out = page_text_in(text_page);
    unsafe { FPDFText_ClosePage(text_page) };
    out
}

fn page_text_in(text_page: *mut libc::c_void) -> String {
    let count = unsafe { FPDFText_CountChars(text_page) };
    if count <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; count as usize + 1];
    let written = unsafe { FPDFText_GetText(text_page, 0, count, buffer.as_mut_ptr()) };
    if written <= 1 {
        return String::new();
    }
    let text = String::from_utf16_lossy(&buffer[..written as usize - 1]);
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `PDFium.render`: the page as a picture, fitted into a square of `pixels` (as PDFKit's
/// thumbnail is on macOS), annotations drawn (`PDF+Portable.swift:52-74`). PDFium's bitmap is
/// BGRx; the engine reads it through `GetStride` and packs RGB.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn render_page(page: *mut libc::c_void, pixels: usize) -> Option<RgbImage> {
    let width = unsafe { FPDF_GetPageWidthF(page) } as f64;
    let height = unsafe { FPDF_GetPageHeightF(page) } as f64;
    // `guard width > 0, height > 0`: a 0×0 mediabox page — and any NaN measure — skips, exactly
    // as a `> 0` comparison answers false for NaN.
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let scale = pixels as f64 / width.max(height);
    let w = (width * scale).round().max(1.0) as usize;
    let h = (height * scale).round().max(1.0) as usize;
    let bitmap = unsafe { FPDFBitmap_Create(w as libc::c_int, h as libc::c_int, 0) };
    if bitmap.is_null() {
        return None;
    }
    unsafe {
        FPDFBitmap_FillRect(
            bitmap,
            0,
            0,
            w as libc::c_int,
            h as libc::c_int,
            0xFFFF_FFFF,
        )
    };
    unsafe {
        FPDF_RenderPageBitmap(
            bitmap,
            page,
            0,
            0,
            w as libc::c_int,
            h as libc::c_int,
            0,
            FPDF_ANNOT,
        )
    };
    let buffer = unsafe { FPDFBitmap_GetBuffer(bitmap) };
    let out = if buffer.is_null() {
        None
    } else {
        let stride = unsafe { FPDFBitmap_GetStride(bitmap) } as usize;
        let bytes = unsafe { std::slice::from_raw_parts(buffer as *const u8, stride * h) };
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let s = y * stride + x * 4;
                let d = (y * w + x) * 3;
                rgb[d] = bytes[s + 2];
                rgb[d + 1] = bytes[s + 1];
                rgb[d + 2] = bytes[s];
            }
        }
        Some(RgbImage {
            width: w,
            height: h,
            pixels: rgb,
        })
    };
    unsafe { FPDFBitmap_Destroy(bitmap) };
    out
}

/// `PDFium.meta`: a metadata string ("Title", "Author"…), or nil when there's none
/// (`PDF+Portable.swift:77-84`). PDFium answers the size in bytes including the terminator;
/// the text runs to its first NUL.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn meta(document: *mut libc::c_void, tag: &str) -> Option<String> {
    let tag = match std::ffi::CString::new(tag) {
        Ok(tag) => tag,
        Err(_) => return None,
    };
    let size = unsafe { FPDF_GetMetaText(document, tag.as_ptr(), std::ptr::null_mut(), 0) };
    if size <= 2 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize) / 2];
    unsafe {
        FPDF_GetMetaText(
            document,
            tag.as_ptr(),
            buffer.as_mut_ptr() as *mut libc::c_void,
            size,
        )
    };
    let units: Vec<u16> = buffer.into_iter().take_while(|unit| *unit != 0).collect();
    let text = String::from_utf16_lossy(&units);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// One PDF page as the index wants it (`PDFExtractor.Page`, Extractors.swift:148-156).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// 1-based.
    pub number: usize,
    /// May be empty for scans.
    pub text: String,
    /// Set when the page had too little text and was rendered for the vision encoder.
    pub image_path: Option<String>,
    /// The text came from OCR: the text layer was garbled (`PageTextProblem::lost_letters`).
    pub ocr: bool,
    /// Text read in glyph order: too garbled to show, enough words for a meaning vector beside
    /// the page's picture.
    pub garbled: Option<String>,
}

/// `PDFExtractor.pages` (`PDF+Portable.swift:90-116`): the pages in `range` (0-based, clamped
/// to the real page count), text pages as text, scans / slides / glyph-order pages rendered
/// into `folder` under a UUID name. Returns the pages, the real total, and the "Title".
pub fn pages(
    url: &Path,
    range: std::ops::Range<usize>,
    min_text: usize,
    render_pixels: usize,
    folder: &Path,
) -> Result<(Vec<Page>, usize, Option<String>), ExtractError> {
    with_document(url, |document| {
        let total = unsafe { FPDF_GetPageCount(document) }.max(0) as usize;
        // `.clamped(to: 0..<total)`: a range past the last page reads to it, an empty one reads none.
        let from = range.start.min(total);
        let to = range.end.min(total);
        let mut out: Vec<Page> = Vec::new();
        for index in from..to {
            let page = unsafe { FPDF_LoadPage(document, index as libc::c_int) };
            if page.is_null() {
                continue;
            }
            // Swift's `try image.write` throws out of `extract`: the pages read so far go with
            // it and the file is `failed`, not `done` with pictures missing.
            let made = made_page(page, index, min_text, render_pixels, folder);
            unsafe { FPDF_ClosePage(page) };
            out.push(made?);
        }
        Ok((out, total, meta(document, "Title")))
    })?
}

/// One page's line (`PDF+Portable.swift:98-113`): text when its text layer is good enough, a
/// rendered PNG in `folder` otherwise. A page whose PICTURE won't render is the engine's
/// `guard let … else { continue }` — the page is skipped, the file goes on (the engine's
/// render is a `nil` return, not a throw). A copy that can't be WRITTEN is the engine's
/// `try image.write` (PDF+Portable.swift:111): the error propagates through `pages` and the
/// file is `failed` with the write's text in `detail`, never `done` with the picture's
/// segment missing. The `Ok(Page)` arm is text-only; the picture arms are the two below.
fn made_page(
    page: *mut libc::c_void,
    index: usize,
    min_text: usize,
    render_pixels: usize,
    folder: &Path,
) -> Result<Page, ExtractError> {
    let mut text = page_text(page).trim().to_string();
    let mut garbled: Option<String> = None;
    match PageTextProblem::check(&text) {
        Some(PageTextProblem::LostLetters) | None => {
            // `text.count` (PDF+Portable.swift:102) counts grapheme clusters: a base letter
            // with its combining marks is ONE character. The scalar count would call a page
            // of Devanagari / Bengali / Thai text twice as long as the engine says it is,
            // and keep as text the page the engine renders for the vision encoder.
            // (Measured 2026-11-14 on this box's PDFium: the seam's text extraction drops
            // combining marks — a fixture of e + U+0301 ×30 extracts 30 scalars — so on
            // this engine the two counts coincide for every text PDFium hands this seam;
            // the rule is the engine's, and a PDFium that keeps the marks keeps the parity.)
            if digup_core::xmlscan::grapheme_len(&text) >= min_text {
                return Ok(Page {
                    number: index + 1,
                    text,
                    image_path: None,
                    ocr: false,
                    garbled: None,
                });
            }
        }
        Some(PageTextProblem::GlyphOrder) => {
            garbled = Some(std::mem::take(&mut text));
        }
    }
    let image = render_page(page, render_pixels).ok_or_else(|| ExtractError::Unreadable {
        why: format!("page {} has no picture", index + 1),
    })?;
    let out = folder.join(format!("{}.png", uuid_string()));
    // `PDF+Portable.swift:111` is `try image.write(to: out, png: true)`: a copy that cannot
    // be written (the render folder is on a read-only mount, the disk is full, the folder
    // vanished mid-run) THROWS out of `extract`. The engine's file then lands `failed` with
    // the write's message in `detail`; a swallowed error lands it `done` with the page's
    // picture segment missing forever — the index would say a scanned book was read. The
    // "can't write …" shape is the port's convention for a child/encoder write failure
    // (media.rs:129-131, 307-309), shared by ffmpeg's frames and the WAV windows.
    image
        .write(&out, true)
        .map_err(|_| ExtractError::Unreadable {
            why: format!("can't write {}", out.to_string_lossy()),
        })?;
    Ok(Page {
        number: index + 1,
        text,
        image_path: Some(out.to_string_lossy().into_owned()),
        ocr: false,
        garbled,
    })
}

/// `PDFExtractor.renderPage` (`PDF+Portable.swift:121-132`): page `number` (from 1) as a JPEG
/// for a preview, fitted into a square of `pixels`.
pub fn render_preview(
    number: usize,
    url: &Path,
    pixels: usize,
    out: &Path,
) -> Result<(), ExtractError> {
    with_document(url, |document| {
        // `Int32(number - 1)`: a number below 1 asks for page -1, which PDFium refuses, and the
        // refusal names the number the caller gave.
        let index = number as i64 - 1;
        let page = unsafe { FPDF_LoadPage(document, index as libc::c_int) };
        if page.is_null() {
            return Err(ExtractError::Unreadable {
                why: format!("no page {number}"),
            });
        }
        let made = match render_page(page, pixels) {
            Some(image) => image.write(out, false).map(|_| ()),
            None => Err(ExtractError::Unreadable {
                why: format!("can't render page {number}"),
            }),
        };
        unsafe { FPDF_ClosePage(page) };
        made
    })
    .and_then(|made| made)
}

/// `Estimator.pages(of:)` (`PDF+Portable.swift:136-138`): FPDF_GetPageCount, 1 when the
/// document can't be read — the estimate's seam, installed over core's header scan at startup.
pub fn pages_of(path: &Path) -> i64 {
    with_document(path, |document| unsafe { FPDF_GetPageCount(document) })
        .map(|count| count.max(0) as i64)
        .unwrap_or(1)
}

/// The document's page count inside an open document (`FPDF_GetPageCount`, for callers that
/// already hold one — the probe's "meta" scenario reads it beside the tags).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn with_page_count(document: *mut libc::c_void) -> i64 {
    unsafe { FPDF_GetPageCount(document) }.max(0) as i64
}

/// The same behind the C-ABI pointer core's seam holds. A name C can't carry is the engine's
/// `try?` answer: 1.
unsafe extern "C" fn pages_of_c(path: *const libc::c_char) -> i64 {
    if path.is_null() {
        return 1;
    }
    let Ok(name) = unsafe { std::ffi::CStr::from_ptr(path) }.to_str() else {
        return 1;
    };
    pages_of(Path::new(name))
}

/// `crates/cli` calls this at process start: from here on the estimate counts pages with the
/// same PDFium the engine counts them with.
pub fn install_pages_hook() {
    digup_core::estimator::set_pages_hook(Some(pages_of_c));
}
