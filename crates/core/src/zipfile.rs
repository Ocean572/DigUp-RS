//! A zip file's entries (`ZipArchive`, Platform/Portable/Docs+Portable.swift:51) — reading
//! through miniz on the Swift side. The port reproduces miniz's acceptance rules AT THE
//! SAME POINTS: `mz_zip_reader_init` runs the archive-level checks over the End of Central
//! Directory, the zip64 records, and EVERY central-directory record, and ANY failure makes
//! the open fail with `not a zip file` (Docs+Portable.swift:64) — the audit-base validated
//! per entry, so an encrypted/masked Office file answered `can't read word/document.xml`
//! where the engine answers `not a zip file`, and an archive with an oversized local-offset
//! opened where miniz refuses it. Entry lookup is miniz's too: `mz_zip_reader_init(zip,
//! size, 0)` SORTS the central directory by lowercased filename (a heapsort,
//! miniz.c:3578), and `locate_file` (flags 0) binary-searches that order (miniz.c:4457) —
//! which fixes WHICH duplicate (case-variant) entry wins, byte-for-byte.

use flate2::read::DeflateDecoder;
use std::io::Read;

use crate::extract::ExtractError;

/// The most an entry may declare (`ZipArchive.maxEntrySize`): the indexer keeps at most 5,000
/// chunks of a document (about 9 M characters), so 64 MB of XML is far more than it can use.
pub const MAX_ENTRY_BYTES: u64 = 64 << 20;

/// `MZ_ZIP64_END_OF_CENTRAL_DIR_HEADER_SIZE` + `…_LOCATOR_SIZE` (miniz.c:3253-3254).
const EOCD64_SIZE: usize = 56;
const EOCD64_LOCATOR_SIZE: usize = 20;

fn not_a_zip() -> ExtractError {
    // Every `mz_zip_reader_init` failure lands in the same Swift refusal.
    ExtractError::Unreadable {
        why: "not a zip file".into(),
    }
}

#[derive(Debug)]
struct Entry {
    name: Vec<u8>,
    /// The record's extra field bytes — `file_stat` walks them for the zip64 sizes.
    extra: Vec<u8>,
    method: u16,
    flags: u16,
    comp_size: u64,
    uncomp_size: u64,
    offset: u64,
}

#[derive(Debug)]
pub struct ZipArchive {
    bytes: Vec<u8>,
    entries: Vec<Entry>,
    /// `m_sorted_central_dir_offsets`: central-dir indices in filename order (the heapsort
    /// `mz_zip_reader_sort_central_dir_offsets_by_filename` produces, reproduced below).
    sorted: Vec<u32>,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

fn sig_at(bytes: &[u8], at: usize) -> u32 {
    u32_at(bytes, at)
}

/// `MZ_TOLOWER` (miniz.c:3239) — ASCII A–Z only.
fn lower(byte: u8) -> u8 {
    if byte.is_ascii_uppercase() {
        byte - b'A' + b'a'
    } else {
        byte
    }
}

/// `mz_zip_reader_filename_less` (miniz.c:3545): compare the lowercased bytes of the two
/// names over their common length; equal through it → the SHORTER name is the lesser.
fn filename_less(entries: &[Entry], l: u32, r: u32) -> bool {
    let (l_name, r_name) = (&entries[l as usize].name, &entries[r as usize].name);
    let common = l_name.len().min(r_name.len());
    let mut k = 0;
    while k < common {
        let (a, b) = (lower(l_name[k]), lower(r_name[k]));
        if a != b {
            return a < b;
        }
        k += 1;
    }
    l_name.len() < r_name.len()
}

/// `mz_zip_reader_sort_central_dir_offsets_by_filename` (miniz.c:3578) — a plain heap sort,
/// reproduced line for line: the build pass from `(size - 2) >> 1` down, then the drain pass
/// swapping the root to `end`. Its result is deterministic for a fixed input order, which is
/// what makes the binary search's tie pick reproducible.
fn sort_by_filename(entries: &[Entry]) -> Vec<u32> {
    let size = entries.len();
    let mut indices: Vec<u32> = (0..size as u32).collect();
    if size <= 1 {
        return indices;
    }
    let size = size as u32;
    let mut start = (size - 2) >> 1;
    loop {
        let mut root = start;
        loop {
            let mut child = (root << 1) + 1;
            if child >= size {
                break;
            }
            child += u32::from(
                child + 1 < size
                    && filename_less(
                        entries,
                        indices[child as usize],
                        indices[(child + 1) as usize],
                    ),
            );
            if !filename_less(entries, indices[root as usize], indices[child as usize]) {
                break;
            }
            indices.swap(root as usize, child as usize);
            root = child;
        }
        if start == 0 {
            break;
        }
        start -= 1;
    }
    let mut end = size - 1;
    while end > 0 {
        indices.swap(end as usize, 0);
        let mut root = 0u32;
        loop {
            let mut child = (root << 1) + 1;
            if child >= end {
                break;
            }
            child += u32::from(
                child + 1 < end
                    && filename_less(
                        entries,
                        indices[child as usize],
                        indices[(child + 1) as usize],
                    ),
            );
            if !filename_less(entries, indices[root as usize], indices[child as usize]) {
                break;
            }
            indices.swap(root as usize, child as usize);
            root = child;
        }
        end -= 1;
    }
    indices
}

impl ZipArchive {
    /// `mz_zip_reader_init(handle, size, 0)`: the EOCD near the tail, the zip64 records when
    /// their locator sits in front of it, the central directory it points at, and miniz's
    /// sanity check of EVERY record (miniz.c:3867-3975). Any failure — no EOCD, a
    /// multidisk layout, a masked/encrypted entry, an out-of-window central directory, a
    /// local offset past the file, sizes that contradict their method — is `not a zip file`;
    /// the audit-base accepted what miniz refuses and deferred its rejections per entry.
    pub fn open(path: &std::path::Path) -> Result<Self, ExtractError> {
        let bytes = std::fs::read(path).map_err(|error| ExtractError::Unreadable {
            why: error.to_string(),
        })?;
        let len = bytes.len();
        if len < 22 {
            return Err(not_a_zip()); // MZ_ZIP_END_OF_CENTRAL_DIR_HEADER_SIZE
        }
        let eocd = Self::find_eocd(&bytes).ok_or_else(not_a_zip)?;

        let mut total = u16_at(&bytes, eocd + 10) as usize;
        let mut on_disk = u16_at(&bytes, eocd + 8) as usize;
        let mut this_disk = u16_at(&bytes, eocd + 4) as u32;
        let mut cdir_disk = u16_at(&bytes, eocd + 6) as u32;
        let mut size = u32_at(&bytes, eocd + 12) as usize;
        let mut offset = u32_at(&bytes, eocd + 16) as u64;

        // The zip64 records: the locator IMMEDIATELY IN FRONT of the EOCD marks the archive
        // (miniz.c:3722-3731); the EOCD64 is read at eocd − 76 first, then at the locator's
        // own offset — one valid signature ends the search, two failures refuse the archive.
        let locator_at = eocd.checked_sub(EOCD64_LOCATOR_SIZE);
        let zip64 = locator_at.is_some_and(|at| sig_at(&bytes, at) == 0x0706_4b50);
        if zip64 {
            if eocd < EOCD64_LOCATOR_SIZE + EOCD64_SIZE {
                return Err(not_a_zip());
            }
            let mut at = eocd - EOCD64_LOCATOR_SIZE - EOCD64_SIZE;
            if !Self::eocd64_sig(&bytes, at) {
                let at64 = u64_at(&bytes, locator_at.expect("locator present"));
                if at64 > (len - EOCD64_SIZE) as u64 {
                    return Err(not_a_zip());
                }
                at = at64 as usize;
                if !Self::eocd64_sig(&bytes, at) {
                    return Err(not_a_zip());
                }
            }
            if u64_at(&bytes, at + 4) < (EOCD64_SIZE - 12) as u64 {
                return Err(not_a_zip()); // m_size_of_center_direct
            }
            if u32_at(&bytes, locator_at.expect("locator present") + 16) != 1 {
                return Err(not_a_zip()); // m_total_number_of_disks
            }
            let total64 = u64_at(&bytes, at + 32);
            let on_disk64 = u64_at(&bytes, at + 24);
            let size64 = u64_at(&bytes, at + 40);
            if total64 > u32::MAX.into() || on_disk64 > u32::MAX.into() || size64 > u32::MAX.into()
            {
                return Err(not_a_zip()); // miniz's practical limits
            }
            total = total64 as usize;
            on_disk = on_disk64 as usize;
            size = size64 as usize;
            offset = u64_at(&bytes, at + 48);
            this_disk = u32_at(&bytes, at + 16);
            cdir_disk = u32_at(&bytes, at + 20);
        }

        // The archive-level checks (miniz.c:3808-3846).
        if total != on_disk {
            return Err(not_a_zip()); // UNSUPPORTED_MULTIDISK
        }
        if (this_disk | cdir_disk) != 0 && (this_disk != 1 || cdir_disk != 1) {
            return Err(not_a_zip()); // UNSUPPORTED_MULTIDISK
        }
        if size < total * 46 {
            return Err(not_a_zip()); // m_cdir_size < total * MZ_ZIP_CENTRAL_DIR_HEADER_SIZE
        }
        if size > len || offset > (len - size) as u64 {
            return Err(not_a_zip()); // the central directory inside the file
        }
        let offset = offset as usize;
        if eocd < offset + size {
            return Err(not_a_zip()); // the EOCD follows the central directory
        }
        if zip64 && eocd - (offset + size) < EOCD64_SIZE + EOCD64_LOCATOR_SIZE {
            return Err(not_a_zip()); // the zip64 records between the central directory and the EOCD
        }

        // miniz's per-record sanity loop (miniz.c:3867), over the central-directory WINDOW:
        // `n` counts the WINDOW's bytes the record may use — the audit-base measured records
        // against the whole file, so a record running past the central directory's end into
        // the EOCD region opened here and died in miniz.
        let mut at = offset;
        let mut remaining = size;
        let mut entries: Vec<Entry> = Vec::new();
        let mut zip64_has_extended = false; // m_zip64_has_extended_info_fields: set once, gates the scan
        for _ in 0..total {
            if remaining < 46 || sig_at(&bytes, at) != 0x0201_4b50 {
                return Err(not_a_zip());
            }
            let name_len = u16_at(&bytes, at + 28) as usize;
            let extra_len = u16_at(&bytes, at + 30) as usize;
            let comment_len = u16_at(&bytes, at + 32) as usize;
            let record = 46 + name_len + extra_len + comment_len;
            if record > remaining {
                return Err(not_a_zip()); // MZ_ZIP_CENTRAL_DIR_HEADER_SIZE + … > n
            }
            let name = bytes[at + 46..at + 46 + name_len].to_vec();
            let extra = bytes[at + 46 + name_len..at + 46 + name_len + extra_len].to_vec();
            let method = u16_at(&bytes, at + 10);
            let flags = u16_at(&bytes, at + 8);
            let comp = u32_at(&bytes, at + 20) as u64;
            let uncomp = u32_at(&bytes, at + 24) as u64;
            let local = u32_at(&bytes, at + 42) as u64;

            // The zip64-extra MARKING scan (miniz.c:3884-3945): runs for the first record
            // that declares a 32-bit-limit size, walks the extra STRICTLY (a short field
            // header or a field that overruns the extra refuses the ARCHIVE), and stops at
            // the 0x0001 field. The audit-base clamped spans and stopped at a zero-length
            // field, so it kept 0xFFFF_FFFF and answered `is 4095 MB` per entry where miniz
            // refuses the archive.
            if !zip64_has_extended
                && extra_len != 0
                && comp.max(uncomp).max(local) == u32::MAX as u64
            {
                let mut scan = extra.as_slice();
                while !scan.is_empty() {
                    if scan.len() < 4 {
                        return Err(not_a_zip()); // extra_size_remaining < sizeof(mz_uint16)*2
                    }
                    let field = u16_at(scan, 0);
                    let field_size = u16_at(scan, 2) as usize;
                    if field_size + 4 > scan.len() {
                        return Err(not_a_zip()); // the field overruns the extra
                    }
                    if field == 0x0001 {
                        zip64_has_extended = true;
                        break;
                    }
                    scan = &scan[4 + field_size..];
                }
            }

            // Sizes against the method (miniz.c:3950-3955): the `!MZ_READ_LE32(p + METHOD_OFS)`
            // test reads the method AND the time bytes as one u32 — stored with a zero time
            // must have equal sizes; nonzero size with zero storage is corrupt. Guarded by
            // both 32-bit sizes being real (a zip64 placeholder skips it).
            if comp != u32::MAX as u64 && uncomp != u32::MAX as u64 {
                let head = u32_at(&bytes, at + 10); // method (10..12) + file time (12..14)
                if (head == 0 && uncomp != comp) || (uncomp != 0 && comp == 0) {
                    return Err(not_a_zip());
                }
            }

            // The entry's disk (miniz.c:3957-3960): 0xFFFF, or a disk that is neither this
            // disk nor 1, is UNSUPPORTED_MULTIDISK.
            let disk_index = u16_at(&bytes, at + 34);
            if disk_index == u16::MAX || (disk_index as u32 != this_disk && disk_index != 1) {
                return Err(not_a_zip());
            }

            // The local header + stored data inside the file (miniz.c:3962-3966) — at INIT,
            // with the central sizes. The audit-base deferred this per entry, so one bad
            // offset answered `can't read {name}` where miniz refuses the whole archive.
            if comp != u32::MAX as u64 && local + 30 + comp > len as u64 {
                return Err(not_a_zip());
            }

            // A masked local directory (ZipCrypto/AES password, bit 13) is
            // UNSUPPORTED_ENCRYPTION AT INIT (miniz.c:3968-3970): a password-protected
            // .docx answers `not a zip file`, not `can't read word/document.xml`.
            if flags & 8192 != 0 {
                return Err(not_a_zip());
            }

            entries.push(Entry {
                name,
                extra,
                method,
                flags,
                comp_size: comp,
                uncomp_size: uncomp,
                offset: local,
            });
            at += record;
            remaining -= record;
        }

        let sorted = sort_by_filename(&entries);
        Ok(Self {
            bytes,
            entries,
            sorted,
        })
    }

    /// The EOCD signature, searched from the tail back through the comment window miniz scans
    /// (64 KB + its own 22 bytes, `locate_header_sig`'s stop rule). The RIGHTMOST signature
    /// match wins — miniz does not pre-filter candidates, it takes the first hit and its
    /// archive-level checks refuse the file when the pair is implausible, so the port must
    /// not skip to an older valid EOCD miniz never reaches.
    fn find_eocd(bytes: &[u8]) -> Option<usize> {
        let floor = bytes.len().saturating_sub(65_558);
        (floor..=bytes.len() - 22)
            .rev()
            .find(|&at| sig_at(bytes, at) == 0x0605_4b50)
    }

    fn eocd64_sig(bytes: &[u8], at: usize) -> bool {
        at + EOCD64_SIZE <= bytes.len() && sig_at(bytes, at) == 0x0606_4b50
    }

    /// `mz_zip_reader_locate_file(zip, name, nil, 0)` — flags 0, a sorted central directory
    /// and no comment: the BINARY SEARCH (miniz.c:4457). Which entry wins a case-variant tie
    /// is set by the heapsort order and the probe positions, exactly as miniz picks it.
    fn locate(&self, name: &str) -> Option<usize> {
        let wanted = name.as_bytes();
        let mut l = 0i64;
        let mut h = self.sorted.len() as i64 - 1;
        while l <= h {
            let m = l + ((h - l) >> 1);
            let index = self.sorted[m as usize] as usize;
            let found = &self.entries[index].name;
            let common = found.len().min(wanted.len());
            let mut k = 0;
            let mut comp = 0i32;
            while k < common {
                let (a, b) = (lower(found[k]), lower(wanted[k]));
                if a != b {
                    comp = a as i32 - b as i32;
                    break;
                }
                k += 1;
            }
            if comp == 0 {
                comp = found.len() as i32 - wanted.len() as i32;
            }
            if comp == 0 {
                return Some(index);
            } else if comp < 0 {
                l = m + 1;
            } else {
                h = m - 1;
            }
        }
        None
    }

    /// The entry's zip64 sizes, `mz_zip_file_stat_internal`'s walk (miniz.c:4365-4420): the
    /// 0x0001 field read STRICTLY (a short field header, an overrunning field, or a field
    /// body too short for the size being overridden fails the stat — Swift's
    /// `mz_zip_reader_file_stat` == 0 lands in `can't read {name}`), then uncomp, comp,
    /// offset in miniz's order.
    fn file_stat(&self, entry: &Entry) -> Result<(u64, u64, u64), ExtractError> {
        let mut uncomp = entry.uncomp_size;
        let mut comp = entry.comp_size;
        let mut offset = entry.offset;
        if comp.max(uncomp).max(offset) == u32::MAX as u64 {
            let mut scan = entry.extra.as_slice();
            while !scan.is_empty() {
                if scan.len() < 4 {
                    return Err(ExtractError::Unreadable {
                        why: "zip64 extra".into(), // mapped to `can't read {name}` by the caller
                    });
                }
                let field = u16_at(scan, 0);
                let field_size = u16_at(scan, 2) as usize;
                if field_size + 4 > scan.len() {
                    return Err(ExtractError::Unreadable {
                        why: "zip64 extra".into(),
                    });
                }
                if field == 0x0001 {
                    let body = &scan[4..4 + field_size];
                    let mut at = 0usize;
                    if uncomp == u32::MAX as u64 {
                        if body.len() < at + 8 {
                            return Err(ExtractError::Unreadable {
                                why: "zip64 extra".into(),
                            });
                        }
                        uncomp = u64_at(body, at);
                        at += 8;
                    }
                    if comp == u32::MAX as u64 {
                        if body.len() < at + 8 {
                            return Err(ExtractError::Unreadable {
                                why: "zip64 extra".into(),
                            });
                        }
                        comp = u64_at(body, at);
                        at += 8;
                    }
                    if offset == u32::MAX as u64 {
                        if body.len() < at + 8 {
                            return Err(ExtractError::Unreadable {
                                why: "zip64 extra".into(),
                            });
                        }
                        offset = u64_at(body, at);
                    }
                    break;
                }
                scan = &scan[4 + field_size..];
            }
        }
        Ok((comp, uncomp, offset))
    }

    /// The entry's bytes, or `None` when the zip has no such entry. Its declared size is checked
    /// first, before anything is allocated for it: a zip of a few KB can declare gigabytes.
    pub fn data(&self, name: &str) -> Result<Option<Vec<u8>>, ExtractError> {
        let cannot = || ExtractError::Unreadable {
            why: format!("can't read {name}"),
        };
        let Some(index) = self.locate(name) else {
            return Ok(None);
        };
        let entry = &self.entries[index];
        let (comp, uncomp, offset) = self.file_stat(entry).map_err(|_| cannot())?;
        if uncomp > MAX_ENTRY_BYTES {
            return Err(ExtractError::Unreadable {
                why: format!("{name} is {} MB, more than is read", uncomp >> 20),
            });
        }
        // The local header: miniz inflates from there, with the central sizes.
        let head = offset as usize;
        if head + 30 > self.bytes.len() || &self.bytes[head..head + 4] != b"PK\x03\x04" {
            return Err(cannot());
        }
        let name_len = u16_at(&self.bytes, head + 26) as usize;
        let extra_len = u16_at(&self.bytes, head + 28) as usize;
        let data = head + 30 + name_len + extra_len;
        if data > self.bytes.len() {
            return Err(cannot());
        }
        if entry.flags & 0x01 != 0 {
            return Err(cannot()); // encrypted: miniz has no key, the extract fails
        }
        let uncomp = uncomp as usize;
        match entry.method {
            0 => {
                // Stored: miniz copies `comp_size` bytes and requires they ARE the declared
                // size — the init check enforced that for real 32-bit pairs; a zip64 override
                // reaches here with 64-bit sizes.
                let comp = comp as usize;
                if comp != uncomp || self.bytes.len() < data + comp {
                    return Err(cannot());
                }
                Ok(Some(self.bytes[data..data + comp].to_vec()))
            }
            8 => {
                let comp = comp as usize;
                if self.bytes.len() < data + comp {
                    return Err(cannot()); // data_ofs + comp_size past the archive
                }
                let stream = &self.bytes[data..data + comp];
                let mut reader = DeflateDecoder::new(stream);
                let mut out: Vec<u8> = Vec::new();
                // The stream must end cleanly and yield exactly the declared size — miniz
                // inflates into a buffer of that size and fails when it overflows.
                match reader.read_to_end(&mut out) {
                    Ok(_) if out.len() == uncomp => Ok(Some(out)),
                    _ => Err(cannot()),
                }
            }
            _ => Err(cannot()), // bzip2, lzma, AES: miniz has no reader for them
        }
    }

    /// An entry the format can't do without.
    pub fn required(&self, name: &str) -> Result<Vec<u8>, ExtractError> {
        match self.data(name)? {
            Some(bytes) => Ok(bytes),
            None => Err(ExtractError::Unreadable {
                why: format!("no {name} inside"),
            }),
        }
    }

    /// `Zip.entry`: the entry's bytes as text (`String(decoding: …, as: UTF8.self)`).
    pub fn entry(&self, name: &str) -> Result<String, ExtractError> {
        Ok(crate::xmlscan::lossy_utf8(&self.required(name)?))
    }
}

#[cfg(test)]
mod tests {
    use super::ZipArchive;
    use std::path::PathBuf;

    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn u16(v: u16) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    fn u32(v: u32) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    fn u64_bytes(v: u64) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    /// A local header + stored body, as a writer puts them.
    fn local(name: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = b"PK\x03\x04".to_vec();
        out.extend(u16(20)); // version needed
        out.extend(u16(0)); // flags
        out.extend(u16(0)); // method: stored
        out.extend(u16(0)); // time
        out.extend(u16(0)); // date
        out.extend(u32(0)); // crc
        out.extend(u32(body.len() as u32)); // comp
        out.extend(u32(body.len() as u32)); // uncomp
        out.extend(u16(name.len() as u16));
        out.extend(u16(0)); // extra
        out.extend(name);
        out.extend(body);
        out
    }

    /// A central-directory record.
    fn central(
        name: &[u8],
        extra: &[u8],
        method: u16,
        flags: u16,
        comp: u32,
        uncomp: u32,
        disk: u16,
        local_ofs: u32,
    ) -> Vec<u8> {
        let mut out = b"PK\x01\x02".to_vec();
        out.extend(u16(20)); // made by
        out.extend(u16(20)); // needed
        out.extend(u16(flags));
        out.extend(u16(method));
        out.extend(u16(0)); // time
        out.extend(u16(0)); // date
        out.extend(u32(0)); // crc
        out.extend(u32(comp));
        out.extend(u32(uncomp));
        out.extend(u16(name.len() as u16));
        out.extend(u16(extra.len() as u16));
        out.extend(u16(0)); // comment
        out.extend(u16(disk));
        out.extend(u16(0)); // internal attrs
        out.extend(u32(0)); // external attrs
        out.extend(u32(local_ofs));
        out.extend(name);
        out.extend(extra);
        out
    }

    fn eocd(
        total: u16,
        on_disk: u16,
        this_disk: u16,
        cdir_disk: u16,
        size: u32,
        ofs: u32,
    ) -> Vec<u8> {
        let mut out = b"PK\x05\x06".to_vec();
        out.extend(u16(this_disk));
        out.extend(u16(cdir_disk));
        out.extend(u16(on_disk));
        out.extend(u16(total));
        out.extend(u32(size));
        out.extend(u32(ofs));
        out.extend(u16(0)); // comment
        out
    }

    /// A one-entry stored zip, exactly as the writer lays it out.
    fn one_entry_zip(name: &str, body: &[u8], extra: &[u8], flags: u16) -> Vec<u8> {
        let head = local(name.as_bytes(), body);
        let cdir_ofs = head.len() as u32;
        let cdir = central(
            name.as_bytes(),
            extra,
            0,
            flags,
            body.len() as u32,
            body.len() as u32,
            0,
            0, // the local header is at the file's start
        );
        let mut out = head;
        out.extend(&cdir);
        out.extend(eocd(1, 1, 0, 0, cdir.len() as u32, cdir_ofs));
        out
    }

    fn open_bytes(name: &str, bytes: &[u8]) -> Result<ZipArchive, crate::extract::ExtractError> {
        let path = tree(name).join("a.zip");
        std::fs::write(&path, bytes).unwrap();
        ZipArchive::open(&path)
    }

    fn why(error: &crate::extract::ExtractError) -> String {
        format!("{error}")
    }

    /// miniz marks a password-protected Office zip (bit 13 — the local directory is masked)
    /// UNSUPPORTED_ENCRYPTION **at init** (miniz.c:3968): Swift's open throws
    /// `not a zip file`. The audit-base checked the flag per entry, so the file opened and
    /// the extract answered `can't read word/document.xml` — a different wire detail for the
    /// SAME file.
    #[test]
    fn a_masked_archive_is_refused_at_open() {
        let zip = one_entry_zip("word/document.xml", b"<x/>", &[], 0x2001);
        let error = open_bytes("digup-zip-masked", &zip).expect_err("miniz refuses the archive");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// Bit 0 alone (ZipCrypto "encrypt selected files") is NOT checked at init — the entry
    /// fails at extract, per entry, exactly as the audit-base answered: `can't read a.txt`.
    #[test]
    fn a_bit_zero_entry_fails_per_entry_as_miniz_does() {
        let zip = one_entry_zip("a.txt", b"x", &[], 0x0001);
        let archive = open_bytes("digup-zip-bit0", &zip).expect("init does not check bit 0");
        let error = archive
            .data("a.txt")
            .expect_err("miniz has no key for the entry");
        assert!(why(&error).contains("can't read a.txt"), "{error}");
    }

    /// `m_total_files != cdir_entries_on_this_disk` (miniz.c:3808) — UNSUPPORTED_MULTIDISK.
    #[test]
    fn a_multidisk_eocd_is_refused() {
        let head = local(b"a.txt", b"x");
        let cdir_ofs = head.len() as u32;
        let cdir = central(b"a.txt", &[], 0, 0, 1, 1, 0, cdir_ofs);
        let mut zip = head;
        zip.extend(&cdir);
        zip.extend(eocd(2, 1, 0, 0, cdir.len() as u32, cdir_ofs)); // total 2, on disk 1
        let error = open_bytes("digup-zip-multidisk", &zip).expect_err("miniz refuses");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// `cdir_size < total * 46` and the central directory inside the file (miniz.c:3814-3818).
    #[test]
    fn a_central_directory_size_below_the_entries_is_refused() {
        let zip = one_entry_zip("a.txt", b"x", &[], 0);
        let mut zip = zip;
        // Last 22 bytes are the EOCD; rewrite its size field to 45 < 46.
        let at = zip.len() - 22 + 12;
        zip[at..at + 4].copy_from_slice(&45u32.to_le_bytes());
        let error = open_bytes("digup-zip-cdirsize", &zip).expect_err("miniz refuses");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// `local_header_ofs + 30 + comp_size > archive_size` is checked AT INIT
    /// (miniz.c:3962-3966): a corrupt local offset refuses the WHOLE archive. The audit-base
    /// deferred it per entry, so `data("a.txt")` answered `can't read a.txt` where Swift
    /// answers `not a zip file`.
    #[test]
    fn a_local_offset_past_the_file_refuses_the_archive() {
        let mut zip = one_entry_zip("a.txt", b"x", &[], 0);
        // The central record's local-header offset: record start (len − 22 − 51) + 42.
        let at = zip.len() - 22 - 46 - 5 + 42;
        zip[at..at + 4].copy_from_slice(&100_000u32.to_le_bytes());
        let error = open_bytes("digup-zip-localofs", &zip).expect_err("miniz refuses at init");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// The sizes-method consistency (miniz.c:3950-3955), with miniz's exact reads:
    /// `!MZ_READ_LE32(p + METHOD_OFS)` covers the method AND the time bytes, and the rule is
    /// `stored-with-zero-time && sizes unequal` OR `nonzero size with zero storage`.
    #[test]
    fn sizes_that_contradict_the_method_refuse_the_archive() {
        // Stored, equal sizes: fine.
        let good = one_entry_zip("a.txt", b"x", &[], 0);
        open_bytes("digup-zip-sizes-ok", &good).expect("a plain stored zip opens");
        // Stored with comp 10, uncomp 20: corrupt.
        let mut zip = one_entry_zip("a.txt", b"x", &[], 0);
        let at = zip.len() - 22 - 46; // the central record start
        zip[at + 20..at + 24].copy_from_slice(&10u32.to_le_bytes());
        zip[at + 24..at + 28].copy_from_slice(&20u32.to_le_bytes());
        let error = open_bytes("digup-zip-sizes-bad", &zip).expect_err("miniz refuses");
        assert!(why(&error).contains("not a zip file"), "{error}");
        // Zero comp with nonzero uncomp: corrupt.
        let mut zip = one_entry_zip("a.txt", b"x", &[], 0);
        zip[at + 20..at + 24].copy_from_slice(&0u32.to_le_bytes());
        zip[at + 24..at + 28].copy_from_slice(&1u32.to_le_bytes());
        let error = open_bytes("digup-zip-sizes-zero", &zip).expect_err("miniz refuses");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// `disk_index != num_this_disk && disk_index != 1` (miniz.c:3957-3960): an entry claiming
    /// disk 2 is UNSUPPORTED_MULTIDISK.
    #[test]
    fn an_entry_on_another_disk_is_refused() {
        let head = local(b"a.txt", b"x");
        let cdir_ofs = head.len() as u32;
        let cdir = central(b"a.txt", &[], 0, 0, 1, 1, 2, cdir_ofs); // disk start 2
        let mut zip = head;
        zip.extend(&cdir);
        zip.extend(eocd(1, 1, 0, 0, cdir.len() as u32, cdir_ofs));
        let error = open_bytes("digup-zip-entrydisk", &zip).expect_err("miniz refuses");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// F6 — the init zip64 MARKING scan walks the extra field strictly (miniz.c:3915-3945):
    /// a zero-span field is a valid field that it steps over, NOT a stop. A 0x0001 field
    /// after one supplies the 64-bit sizes and the entry reads. The audit-base stopped at
    /// the zero-span field, kept 0xFFFF_FFFF, and answered `is 4095 MB, more than is read`.
    #[test]
    fn the_zip64_extra_walk_steps_over_a_zero_span_field() {
        let body = b"12345";
        let extra: Vec<u8> = [
            u16(0x0009),
            u16(0),
            u16(0x0001),
            u16(24),
            u64_bytes(body.len() as u64),
            u64_bytes(body.len() as u64),
            u64_bytes(1_000_000_000), // local header offset: past 2^32
        ]
        .concat();
        // The local header sits at its normal offset; the central declares 0xFFFFFFFF so the
        // walk runs, and the 64-bit offset in the extra points at it.
        let head = local(b"f.bin", body);
        let cdir = central(b"f.bin", &extra, 0, 0, u32::MAX, u32::MAX, 0, u32::MAX);
        let cdir_ofs = head.len(); // the record begins right after the local header
        let mut zip = head;
        zip.extend(&cdir);
        zip.extend(eocd(1, 1, 0, 0, cdir.len() as u32, cdir_ofs as u32));
        // Rewrite the extra's 64-bit offset to the local header's real place: 0. The extra
        // starts at record + 46 + name; after the 4-byte 0x0009 field, the 0x0001 header
        // (4 bytes), its body is comp, uncomp, offset — the third slot is at +16.
        let extra_at = cdir_ofs + 46 + b"f.bin".len();
        zip[extra_at + 4 + 4 + 16..extra_at + 4 + 4 + 24].copy_from_slice(&0u64.to_le_bytes());
        let archive = open_bytes("digup-zip-zip64walk", &zip).expect("miniz marks zip64 and opens");
        let data = archive
            .data("f.bin")
            .expect("the entry")
            .expect("the entry is there");
        assert_eq!(
            data, body,
            "the 64-bit sizes are applied (fails-before: 4095 MB refusal)"
        );
    }

    /// The same walk REFUSES the archive when a field overruns the extra.
    #[test]
    fn an_overrunning_zip64_extra_field_refuses_the_archive() {
        let body = b"12345";
        let extra: Vec<u8> = [
            u16(0x0001),
            u16(100), // claims 100 bytes in a 4-byte extra
        ]
        .concat();
        let head = local(b"f.bin", body);
        let cdir = central(b"f.bin", &extra, 0, 0, u32::MAX, u32::MAX, 0, u32::MAX);
        let cdir_ofs = head.len() as u32;
        let mut zip = head;
        zip.extend(&cdir);
        zip.extend(eocd(1, 1, 0, 0, cdir.len() as u32, cdir_ofs));
        let error = open_bytes("digup-zip-extraoverrun", &zip).expect_err("miniz refuses");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// F5 — `locate_file` is a BINARY SEARCH over the heapsorted names (miniz.c:4457), not a
    /// scan. Two entries whose names are equal lowercased take the search's pick; the
    /// audit-base's central-order `position()` took the FIRST. With two records the heap
    /// sort swaps them (ties sort by length: not less) and the probe lands on index 0 of the
    /// sorted order — the SECOND central entry.
    #[test]
    fn the_locate_pick_is_the_heapsorts_binary_search_pick() {
        let first = b"first content";
        let second = b"SECOND content"; // same name lowercased, different bytes
        let head_a = local(b"content.xml", first);
        let head_b = local(b"content.xml", second);
        let cdir_a = central(
            b"content.xml",
            &[],
            0,
            0,
            first.len() as u32,
            first.len() as u32,
            0,
            0,
        );
        let cdir_b = central(
            b"Content.xml",
            &[],
            0,
            0,
            second.len() as u32,
            second.len() as u32,
            0,
            head_a.len() as u32,
        );
        let cdir_ofs = (head_a.len() + head_b.len()) as u32;
        let mut zip = head_a;
        zip.extend(&head_b);
        zip.extend(&cdir_a);
        zip.extend(&cdir_b);
        zip.extend(eocd(
            2,
            2,
            0,
            0,
            (cdir_a.len() + cdir_b.len()) as u32,
            cdir_ofs,
        ));
        let archive = open_bytes("digup-zip-tie", &zip).expect("the archive opens");
        let data = archive
            .data("content.xml")
            .expect("the entry")
            .expect("a match");
        assert_eq!(
            data, second,
            "the search's pick is the SECOND record (fails-before: first in central order)"
        );
    }

    /// miniz takes the RIGHTMOST EOCD signature and its checks refuse the archive when the
    /// pair is implausible (miniz.c:3814-3821). The audit-base's pre-filter skipped an
    /// implausible candidate to an older valid EOCD, opening a file miniz refuses.
    #[test]
    fn the_rightmost_eocd_signature_is_the_one_miniz_checks() {
        let mut zip = one_entry_zip("a.txt", b"x", &[], 0);
        // A trailing blob whose PK\x05\x06 (the rightmost signature) claims a 4 GB directory.
        let mut blob = b"PK\x05\x06".to_vec();
        blob.extend(&[0u8; 18]);
        blob[12..16].copy_from_slice(&u32::MAX.to_le_bytes()); // cdir size: absurd
        zip.extend(&blob);
        let error =
            open_bytes("digup-zip-rightmost", &zip).expect_err("miniz refuses the rightmost");
        assert!(why(&error).contains("not a zip file"), "{error}");
    }

    /// The empty archive (EOCD at 0, everything zero) opens as a zip with no entries —
    /// miniz's baseline (kept from the audit-base's pinned shape).
    #[test]
    fn an_empty_archive_opens_with_no_entries() {
        let archive =
            open_bytes("digup-zip-empty", &eocd(0, 0, 0, 0, 0, 0)).expect("miniz opens it");
        assert_eq!(archive.entries.len(), 0);
        assert_eq!(archive.locate("anything"), None);
    }
}
