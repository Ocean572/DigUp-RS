//! A zip file's entries (`ZipArchive`, Platform/Portable/Docs+Portable.swift:51) — reading
//! through miniz on the Swift side. The port parses the same central directory and inflates the
//! same entries, keeping miniz's acceptance rules: the declared-size guard BEFORE anything is
//! allocated, the case-insensitive `locate_file` (its flags argument is 0), and a failed inflate
//! as "can't read".

use flate2::read::DeflateDecoder;
use std::io::Read;

use crate::extract::ExtractError;

/// The most an entry may declare (`ZipArchive.maxEntrySize`): the indexer keeps at most 5,000
/// chunks of a document (about 9 M characters), so 64 MB of XML is far more than it can use.
pub const MAX_ENTRY_BYTES: u64 = 64 << 20;

struct Entry {
    name: Vec<u8>,
    method: u16,
    flags: u16,
    comp_size: u64,
    uncomp_size: u64,
    offset: u64,
}

pub struct ZipArchive {
    bytes: Vec<u8>,
    entries: Vec<Entry>,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

impl ZipArchive {
    /// `mz_zip_reader_init(handle, size, 0)`: the End of Central Directory near the tail, then
    /// the central directory it points at. No EOCD, no archive.
    pub fn open(path: &std::path::Path) -> Result<Self, ExtractError> {
        let bytes = std::fs::read(path).map_err(|error| ExtractError::Unreadable {
            why: error.to_string(),
        })?;
        let eocd = Self::find_eocd(&bytes).ok_or_else(|| ExtractError::Unreadable {
            why: "not a zip file".into(),
        })?;
        let total = u16_at(&bytes, eocd + 10) as usize;
        let size = u32_at(&bytes, eocd + 12) as usize;
        let mut at = u32_at(&bytes, eocd + 16) as usize;
        let end = at + size;
        let mut entries = Vec::new();
        for _ in 0..total {
            if at + 46 > bytes.len() || &bytes[at..at + 4] != b"PK\x01\x02" {
                return Err(ExtractError::Unreadable {
                    why: "not a zip file".into(),
                });
            }
            let name_len = u16_at(&bytes, at + 28) as usize;
            let extra_len = u16_at(&bytes, at + 30) as usize;
            let comment_len = u16_at(&bytes, at + 32) as usize;
            let name_end = at + 46 + name_len;
            if name_end > bytes.len() {
                return Err(ExtractError::Unreadable {
                    why: "not a zip file".into(),
                });
            }
            let name = bytes[at + 46..name_end].to_vec();
            let extra = &bytes[name_end.min(bytes.len())..(name_end + extra_len).min(bytes.len())];
            let mut entry = Entry {
                name,
                method: u16_at(&bytes, at + 10),
                flags: u16_at(&bytes, at + 8),
                comp_size: u32_at(&bytes, at + 20).into(),
                uncomp_size: u32_at(&bytes, at + 24).into(),
                offset: u32_at(&bytes, at + 42).into(),
            };
            // The zip64 fields, when the 32-bit ones say "see the 0x0001 extra".
            if entry.comp_size == 0xFFFF_FFFF
                || entry.uncomp_size == 0xFFFF_FFFF
                || entry.offset == 0xFFFF_FFFF
            {
                let mut scan = extra;
                while scan.len() >= 4 {
                    let header = u16_at(scan, 0);
                    let span = u16_at(scan, 2) as usize;
                    let body = &scan[4..(4 + span).min(scan.len())];
                    if header == 0x0001 {
                        // comp, uncomp, offset in that order, each 8 bytes when present.
                        if body.len() >= 8 {
                            entry.comp_size = u64::from_le_bytes(body[0..8].try_into().unwrap());
                        }
                        if body.len() >= 16 {
                            entry.uncomp_size = u64::from_le_bytes(body[8..16].try_into().unwrap());
                        }
                        if body.len() >= 24 {
                            entry.offset = u64::from_le_bytes(body[16..24].try_into().unwrap());
                        }
                        break;
                    }
                    if span == 0 || 4 + span > scan.len() {
                        break;
                    }
                    scan = &scan[4 + span..];
                }
            }
            entries.push(entry);
            at += 46 + name_len + extra_len + comment_len;
        }
        let _ = end;
        Ok(Self { bytes, entries })
    }

    /// The EOCD signature, searched from the tail back through the comment window miniz scans
    /// (64 KB + its own 22 bytes). An empty archive — offset 0, size 0, EOCD at 0 — is a zip with
    /// no entries, which miniz opens fine.
    fn find_eocd(bytes: &[u8]) -> Option<usize> {
        if bytes.len() < 22 {
            return None;
        }
        let floor = bytes.len().saturating_sub(65_558);
        for at in (floor..=bytes.len() - 22).rev() {
            if &bytes[at..at + 4] != b"PK\x05\x06" {
                continue;
            }
            let size = u32_at(bytes, at + 12) as usize;
            let offset = u32_at(bytes, at + 16) as usize;
            // The central directory must lie in front of the EOCD; an implausible pair means the
            // signature was inside a stored entry — keep scanning back for the real EOCD.
            match offset.checked_add(size) {
                Some(sum) if sum <= at => return Some(at),
                _ => continue,
            }
        }
        None
    }

    /// `mz_zip_reader_locate_file(zip, name, nil, 0)` — flags 0 is case-INsensitive, first match
    /// in central-directory order.
    fn locate(&self, name: &str) -> Option<usize> {
        let wanted = name.as_bytes();
        self.entries.iter().position(|entry| {
            entry.name.len() == wanted.len()
                && entry
                    .name
                    .iter()
                    .zip(wanted.iter())
                    .all(|(mine, want)| mine.eq_ignore_ascii_case(want))
        })
    }

    /// The entry's bytes, or `None` when the zip has no such entry. Its declared size is checked
    /// first, before anything is allocated for it: a zip of a few KB can declare gigabytes.
    pub fn data(&self, name: &str) -> Result<Option<Vec<u8>>, ExtractError> {
        let Some(index) = self.locate(name) else {
            return Ok(None);
        };
        let entry = &self.entries[index];
        if entry.uncomp_size > MAX_ENTRY_BYTES {
            return Err(ExtractError::Unreadable {
                why: format!(
                    "{name} is {} MB, more than is read",
                    entry.uncomp_size >> 20
                ),
            });
        }
        let cannot = || ExtractError::Unreadable {
            why: format!("can't read {name}"),
        };
        // The local header: miniz inflates from there, with the central sizes.
        let head = entry.offset as usize;
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
        let uncomp = entry.uncomp_size as usize;
        match entry.method {
            0 => {
                // Stored: the bytes as they are; miniz reads the whole declared size.
                if self.bytes.len() < data + uncomp {
                    return Err(cannot());
                }
                Ok(Some(self.bytes[data..data + uncomp].to_vec()))
            }
            8 => {
                let comp = (entry.comp_size as usize).min(self.bytes.len() - data);
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
