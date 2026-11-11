//! The two EXIF facts the indexer wants where ImageIO isn't there — the way the camera held the
//! picture (orientation) and when it took it (DateTimeOriginal, as written) — plus the flag saying a
//! TIFF keeps its pixels in tiles (`TileWidth` in IFD0), which ffmpeg decodes as black. The port of
//! `Platform/Portable/EXIF.swift`.
//!
//! The file is read where asked, 64 KB at a time, 32 blocks at most: a TIFF's tags often sit after
//! its pixels, the file can be 2 GB, and a header lying about its offsets must give "not this
//! file's shape", not a trap. Every position the headers name is checked against the file's real
//! length before it is used.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// The EXIF `read` answers: orientation 1…8 (1 when the file says nothing), the camera's own
/// `DateTimeOriginal` string, and the tiled flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exif {
    pub orientation: i64,
    pub date_time_original: Option<String>,
    pub tiled: bool,
}

impl Default for Exif {
    fn default() -> Self {
        Self {
            orientation: 1,
            date_time_original: None,
            tiled: false,
        }
    }
}

const BLOCK_SIZE: usize = 1 << 16;
/// 2 MB of headers: past that, this is not a picture this reader knows.
const MAX_BLOCKS: usize = 32;

/// `FileBytes`: a file read where asked, block by block.
struct FileBytes {
    size: i64,
    file: File,
    blocks: HashMap<usize, Vec<u8>>,
}

impl FileBytes {
    fn open(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        if size > i64::MAX as u64 {
            return None;
        }
        Some(Self {
            size: size as i64,
            file,
            blocks: HashMap::new(),
        })
    }

    fn byte(&mut self, at: i64) -> Option<u8> {
        if at < 0 || at >= self.size {
            return None;
        }
        let index = (at as usize) / BLOCK_SIZE;
        if !self.blocks.contains_key(&index) {
            let block = if self.blocks.len() >= MAX_BLOCKS {
                Vec::new() // past the 2 MB of headers this reader will spend
            } else {
                let start = (index * BLOCK_SIZE) as u64;
                let want = BLOCK_SIZE.min((self.size - start as i64) as usize);
                let mut buffer = vec![0u8; want];
                match self
                    .file
                    .seek(SeekFrom::Start(start))
                    .and_then(|_| self.file.read_exact(&mut buffer))
                {
                    Ok(()) => buffer,
                    Err(_) => Vec::new(), // past the end, a closed file
                }
            };
            self.blocks.insert(index, block);
        }
        let block = self.blocks.get(&index)?;
        let offset = at as usize - index * BLOCK_SIZE;
        block.get(offset).copied()
    }

    fn bytes(&mut self, at: i64, count: i64) -> Option<Vec<u8>> {
        self.offset(at, count)?;
        let mut out = Vec::with_capacity(count as usize);
        for i in at..(at + count) {
            out.push(self.byte(i)?);
        }
        Some(out)
    }

    /// Add a file-derived position and length only when the resulting position is inside the file.
    fn offset(&self, at: i64, by: i64) -> Option<i64> {
        if at < 0 || at > self.size || by < 0 || by > self.size - at {
            return None;
        }
        Some(at + by)
    }

    /// An unsigned big-endian number of `width` bytes (0, 1, 2, 4 or 8; 0 reads as 0, as iloc means
    /// it). More than `Int.max` is not a position this reader can name.
    fn uint(&mut self, at: i64, width: i64) -> Option<i64> {
        if width <= 0 {
            return Some(0);
        }
        if width > 8 {
            return None;
        }
        let raw = self.bytes(at, width)?;
        let mut value: u64 = 0;
        for byte in raw {
            value = value << 8 | u64::from(byte);
        }
        (value <= i64::MAX as u64).then_some(value as i64)
    }

    fn u16(&mut self, at: i64, little: bool) -> Option<i64> {
        let b = self.bytes(at, 2)?;
        Some(if little {
            i64::from(b[0]) | i64::from(b[1]) << 8
        } else {
            i64::from(b[0]) << 8 | i64::from(b[1])
        })
    }

    fn u32(&mut self, at: i64, little: bool) -> Option<i64> {
        let b = self.bytes(at, 4)?;
        Some(if little {
            i64::from(b[0]) | i64::from(b[1]) << 8 | i64::from(b[2]) << 16 | i64::from(b[3]) << 24
        } else {
            i64::from(b[0]) << 24 | i64::from(b[1]) << 16 | i64::from(b[2]) << 8 | i64::from(b[3])
        })
    }
}

const EXIF_MAGIC: &[u8] = b"Exif\0\0";

/// `EXIF.read(_:)`: the file's headers, not the file. `None` when it has no EXIF block this reader
/// knows.
pub fn read(path: &Path) -> Option<Exif> {
    let mut file = FileBytes::open(path)?;
    let start = file.bytes(0, 12)?;
    if start[0] == 0xFF && start[1] == 0xD8 {
        return jpeg(&mut file);
    }
    if matches!(
        &start[..],
        [0x49, 0x49, 0x2A, 0x00, ..] | [0x4D, 0x4D, 0x00, 0x2A, ..]
    ) {
        let size = file.size;
        return parse(&mut file, 0, size);
    }
    if start[0..4] == *b"RIFF" && start[8..12] == *b"WEBP" {
        return webp(&mut file);
    }
    if start[4..8] == *b"ftyp" {
        return heif(&mut file);
    }
    None
}

/// The APP1 segment that starts with "Exif\0\0", among the headers before the image data.
fn jpeg(file: &mut FileBytes) -> Option<Exif> {
    let mut i: i64 = 2;
    while i < 1 << 20 && file.byte(i) == Some(0xFF) {
        let marker = file.byte(i + 1)?;
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            // markers without a length
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break; // the image data: no more headers
        }
        let length = file.u16(i + 2, false)?;
        if length < 2 {
            return None;
        }
        if marker == 0xE1
            && file
                .bytes(i + 4, 6)
                .is_some_and(|bytes| bytes == EXIF_MAGIC)
        {
            let end = file.size.min(i + 2 + length);
            return parse(file, i + 10, end);
        }
        i += 2 + length;
    }
    None
}

/// The RIFF chunk "EXIF" (after the image data, as WebP files order them); some writers start it
/// with "Exif\0\0".
fn webp(file: &mut FileBytes) -> Option<Exif> {
    let mut i: i64 = 12;
    loop {
        let name = file.bytes(i, 4)?;
        let size = file.u32(i + 4, true)?;
        let start = file.offset(i, 8)?;
        let end = file.offset(start, size)?;
        let next = file.offset(end, size & 1)?;
        if name == *b"EXIF" {
            let tiff = if file
                .bytes(start, 6)
                .is_some_and(|bytes| bytes == EXIF_MAGIC)
            {
                file.offset(start, 6)?
            } else {
                start
            };
            if tiff > end {
                return None;
            }
            return parse(file, tiff, end);
        }
        i = next;
    }
}

/// HEIF and AVIF: the meta box lists the file's items (`iinf`) and where their bytes are (`iloc`).
/// The Exif item's bytes start with the offset of the TIFF header after them (ISO/IEC 23008-12,
/// annex A).
fn heif(file: &mut FileBytes) -> Option<Exif> {
    let meta = box_at(file, b"meta", 0, file.size)?;
    let first_child = file.offset(meta.0, 4)?;
    if first_child > meta.1 {
        return None;
    }
    // meta is a full box: version and flags come first, so the children start after them.
    let children = (first_child, meta.1);
    let iinf = box_at(file, b"iinf", children.0, children.1)?;
    let iloc = box_at(file, b"iloc", children.0, children.1)?;
    let exif_item = item_id(file, b"Exif", iinf.0, iinf.1)?;
    let bytes = extent(file, exif_item, iloc.0, iloc.1)?;
    if bytes.1 < 4 {
        return None;
    }
    let after_offset = file.offset(bytes.0, 4)?;
    let skip = file.uint(bytes.0, 4)?;
    let tiff = file.offset(after_offset, skip)?;
    let end = file.offset(bytes.0, bytes.1)?;
    if tiff > end {
        return None;
    }
    parse(file, tiff, end)
}

/// The body (after size and type) of the first box named `type` among the boxes filling the range.
fn box_at(file: &mut FileBytes, type_name: &[u8], lower: i64, upper: i64) -> Option<(i64, i64)> {
    let mut i = lower;
    loop {
        let body = file.offset(i, 8)?;
        if body > upper {
            return None;
        }
        let declared = file.u32(i, false)?;
        let name = file.bytes(i + 4, 4)?;
        let mut header = 8i64;
        let mut size = declared;
        if declared == 1 {
            // a 64-bit size follows
            let large = file.uint(body, 8)?;
            header = 16;
            size = large;
        } else if declared == 0 {
            // the box runs to the end of the range
            size = upper - i;
        }
        if size < header {
            return None;
        }
        let end = file.offset(i, size)?;
        if end > upper {
            return None;
        }
        if name == type_name {
            return Some((i + header, end));
        }
        i = end;
    }
}

/// The id of the first item of `type` in an `iinf` box (its `infe` entries, version 2 or 3).
fn item_id(file: &mut FileBytes, type_name: &[u8], lower: i64, upper: i64) -> Option<i64> {
    let version = i64::from(file.byte(lower)?);
    let mut i = file.offset(lower, if version == 0 { 6 } else { 8 })?;
    if i > upper {
        return None;
    }
    loop {
        let body = file.offset(i, 8)?;
        if body > upper {
            return None;
        }
        let size = file.u32(i, false)?;
        if size < 8 {
            return None;
        }
        if file.bytes(i + 4, 4).is_some_and(|name| name == *b"infe") {
            if let Some(raw) = file.byte(body) {
                let infe_version = i64::from(raw);
                if infe_version >= 2 {
                    let id_size = if infe_version == 2 { 2 } else { 4 };
                    if file
                        .bytes(body + 4 + id_size + 2, 4)
                        .is_some_and(|name| name == type_name)
                    {
                        return file.uint(body + 4, id_size);
                    }
                }
            }
        }
        let end = file.offset(i, size)?;
        if end > upper {
            return None;
        }
        i = end;
    }
}

/// Where an item's bytes start in the file, and how many there are (its first extent), from an
/// `iloc` box. `None` for bytes kept anywhere but the file itself.
fn extent(file: &mut FileBytes, wanted: i64, lower: i64, upper: i64) -> Option<(i64, i64)> {
    let version = i64::from(file.byte(lower)?);
    let sizes = i64::from(file.byte(lower + 4)?);
    let more = i64::from(file.byte(lower + 5)?);
    let (offset_size, length_size, base_size) = (sizes >> 4, sizes & 15, more >> 4);
    let index_size = if version == 1 || version == 2 {
        more & 15
    } else {
        0
    };
    let id_width = if version < 2 { 2 } else { 4 };
    let mut i = lower + 6;
    let count = file.uint(i, id_width)?;
    i += id_width;
    for _ in 0..count.min(100_000) {
        let id = file.uint(i, id_width)?;
        i += id_width;
        let mut method = 0;
        if version == 1 || version == 2 {
            method = file.uint(i, 2)? & 15;
            i += 2;
        }
        i += 2; // data_reference_index
        let base = file.uint(i, base_size)?;
        let extents = file.uint(i + base_size, 2)?;
        i += base_size + 2;
        let mut first: Option<(i64, i64)> = None;
        for _ in 0..extents {
            i += index_size;
            let offset = file.uint(i, offset_size)?;
            let length = file.uint(i + offset_size, length_size)?;
            i += offset_size + length_size;
            if first.is_none() {
                let start = file.offset(base, offset)?;
                file.offset(start, length)?;
                first = Some((start, length));
            }
        }
        if id == wanted {
            return if method == 0 { first } else { None };
        }
        if i >= upper {
            return None;
        }
    }
    None
}

/// A TIFF header at `tiff` and the IFDs it points to, within `end`.
fn parse(file: &mut FileBytes, tiff: i64, end: i64) -> Option<Exif> {
    let header_end = file.offset(tiff, 8)?;
    if header_end > end {
        return None;
    }
    let order = file.bytes(tiff, 2)?;
    let little = match &order[..] {
        [0x49, 0x49] => true,
        [0x4D, 0x4D] => false,
        _ => return None,
    };
    let mut exif = Exif::default();
    let mut exif_ifd: Option<i64> = None;
    let ifd0 = u32_at(file, tiff + 4, tiff, end, little)?;
    let ifd0_start = file.offset(tiff, ifd0)?;
    if ifd0_start > end {
        return None;
    }
    scan(
        file,
        ifd0_start,
        tiff,
        end,
        little,
        &mut |file, tag, kind, _count, at| {
            if tag == 0x0112 && kind == 3 {
                if let Some(value) = u16_at(file, at, tiff, end, little) {
                    exif.orientation = value;
                }
            }
            if tag == 0x8769 && kind == 4 {
                if let Some(offset) = u32_at(file, at, tiff, end, little) {
                    if let Some(start) = file.offset(tiff, offset) {
                        if start <= end {
                            exif_ifd = Some(start);
                        }
                    }
                }
            }
            // TileWidth, in IFD0 of a TIFF read as itself (a JPEG's APP1 block has its own scope).
            if tag == 0x0142 && tiff == 0 {
                exif.tiled = true;
            }
        },
    );
    if let Some(start) = exif_ifd {
        scan(
            file,
            start,
            tiff,
            end,
            little,
            &mut |file, tag, kind, count, at| {
                if !(tag == 0x9003 && kind == 2 && count >= 19) {
                    return;
                }
                let Some(offset) = u32_at(file, at, tiff, end, little) else {
                    return;
                };
                let Some(text_start) = file.offset(tiff, offset) else {
                    return;
                };
                let Some(after) = file.offset(text_start, 19) else {
                    return;
                };
                if after > end {
                    return;
                }
                let Some(text) = file.bytes(text_start, 19) else {
                    return;
                };
                exif.date_time_original = Some(String::from_utf8_lossy(&text).into_owned());
            },
        );
    }
    Some(exif)
}

fn u16_at(file: &mut FileBytes, at: i64, tiff: i64, end: i64, little: bool) -> Option<i64> {
    if at < tiff {
        return None;
    }
    let after = file.offset(at, 2)?;
    if after > end {
        return None;
    }
    file.u16(at, little)
}

fn u32_at(file: &mut FileBytes, at: i64, tiff: i64, end: i64, little: bool) -> Option<i64> {
    if at < tiff {
        return None;
    }
    let after = file.offset(at, 4)?;
    if after > end {
        return None;
    }
    file.u32(at, little)
}

/// Calls `visit(tag, type, count, valueAt)` for each entry of the IFD at `at`.
fn scan(
    file: &mut FileBytes,
    at: i64,
    tiff: i64,
    end: i64,
    little: bool,
    visit: &mut dyn FnMut(&mut FileBytes, i64, i64, i64, i64),
) {
    let Some(count) = u16_at(file, at, tiff, end, little) else {
        return;
    };
    for n in 0..count.min(500) {
        let Some(entry) = file.offset(at, 2 + n * 12) else {
            return;
        };
        let tag = u16_at(file, entry, tiff, end, little);
        let kind = u16_at(file, entry + 2, tiff, end, little);
        let number = u32_at(file, entry + 4, tiff, end, little);
        let (Some(tag), Some(kind), Some(number)) = (tag, kind, number) else {
            return;
        };
        visit(file, tag, kind, number, entry + 8);
    }
}
