//! Media handles and file serving (`api.py`'s media section): the server's own
//! bookkeeping and discipline, not engine fields.
//!
//! * `HitCache` — server-issued handles for the hits it published: the media
//!   endpoints accept ONLY these ids (README §6: "ids from the index, never
//!   paths from the client"). A bounded LRU where `get()` refreshes: a browsing
//!   session (40 handles per search) must not evict its own on-screen
//!   thumbnails (the measured 404s-with-no-recovery when eviction was
//!   insertion-order).
//! * Magic-byte typing — the type a browser is told must match what the file
//!   *is*, and code/text is always text/plain: serving a source-named
//!   .html/.svg as markup would run script from the user's own files on the
//!   server's origin. Unknown types are application/octet-stream.
//! * Ranges honored (206 + Content-Range) with a hard 8 MiB span cap —
//!   `bytes=0-` is what every media element sends first, and an uncapped open
//!   range reads a whole 20 GB clip into RAM.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use regex::Regex;

/// `HitCache(capacity=1024)`: media_id -> the hit's serving fields.
pub struct HitCache {
    capacity: usize,
    inner: Mutex<CacheInner>,
}

#[derive(Default)]
struct CacheInner {
    order: Vec<String>,
    by_id: BTreeMap<String, HitEntry>,
}

#[derive(Clone, Debug)]
pub struct HitEntry {
    pub path: String,
    pub kind: String,
    pub segment_id: i64,
    pub loc: Option<f64>,
    pub code: bool,
}

impl HitCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            inner: Mutex::new(CacheInner::default()),
        }
    }

    /// `put`: `uuid4().hex[:12]` — twelve hex chars. The engine produced the
    /// path this handle names; the handle is the server's bookkeeping.
    pub fn put(&self, entry: HitEntry) -> String {
        let media_id = random_hex12();
        let mut inner = self.inner.lock().expect("the hit cache");
        inner.by_id.insert(media_id.clone(), entry);
        inner.order.push(media_id.clone());
        while inner.order.len() > self.capacity {
            // Evict the least-recently-USED (order refreshes on get).
            if let Some(oldest) = inner.order.first().cloned() {
                inner.order.remove(0);
                inner.by_id.remove(&oldest);
            }
        }
        media_id
    }

    /// `get` — a true LRU: the lookup refreshes the entry's position.
    pub fn get(&self, media_id: &str) -> Option<HitEntry> {
        let mut inner = self.inner.lock().expect("the hit cache");
        let entry = inner.by_id.get(media_id).cloned()?;
        if let Some(at) = inner.order.iter().position(|id| id == media_id) {
            inner.order.remove(at);
        }
        inner.order.push(media_id.to_string());
        Some(entry)
    }
}

/// 12 hex chars from the OS's random source (the uuid4 shape's entropy: the
/// first 12 hex chars of a v4 uuid are 48 random bits).
pub fn random_hex12() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 6];
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)) {
        Ok(()) => {}
        Err(_) => {
            // No urandom (odd platform): a per-process-seeded mix, never a counter
            // an attacker can walk.
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
                ^ (std::process::id() as u64) << 32;
            bytes.copy_from_slice(&seed.to_le_bytes()[..6]);
        }
    }
    let mut out = String::with_capacity(12);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `_MAGIC` (api.py:158): the prefix table, in the file's order.
const MAGIC: &[(&[u8], &str)] = &[
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
    (b"%PDF", "application/pdf"),
    (b"ID3", "audio/mpeg"),
    (b"\x00\x00\x01\xb3", "audio/mpeg"), // MPEG audio frame sync (mp1/mp2)
    (b"\x1a\x45\xdf\xa3", "video/x-matroska"),
    (b"OggS", "audio/ogg"),
    (b"fLaC", "audio/flac"),
    (b"RIFF", "audio/wav"), // WAVE; AVI (RIFF AVI ) becomes octet-stream
    (b"BM", "image/bmp"),
];

/// `sniff_type`: the type from the bytes. ISO-BMFF gets its brand check (the
/// brand at 8-12 is what it IS); an unknown ftyp is octet-stream — not a guess.
pub fn sniff_type(path: &Path) -> String {
    let Ok(mut handle) = std::fs::File::open(path) else {
        return "application/octet-stream".to_string();
    };
    use std::io::Read;
    let mut head = vec![0u8; 16];
    let read = handle.read(&mut head).unwrap_or(0);
    let head = &head[..read];
    if head.get(4..8) == Some(b"ftyp") {
        match &head[8..12.min(head.len())] {
            b"M4A " | b"M4B " | b"M4P " => return "audio/mp4".to_string(),
            b"isom" | b"iso2" | b"iso4" | b"iso6" | b"avc1" | b"mp41" | b"mp42" | b"qt  " => {
                return "video/mp4".to_string()
            }
            _ => return "application/octet-stream".to_string(),
        }
    }
    for (magic, media_type) in MAGIC {
        if head.starts_with(magic) {
            if media_type.starts_with("audio/") || media_type.starts_with("video/") {
                if *media_type == "audio/wav" && head.get(8..12) != Some(b"WAVE") {
                    return "application/octet-stream".to_string();
                }
                if *media_type == "video/avi" && head.get(8..12) != Some(b"AVI ") {
                    return "application/octet-stream".to_string();
                }
            }
            return media_type.to_string();
        }
    }
    "application/octet-stream".to_string()
}

/// `_INLINE_KINDS`: pixels/sound/playable arrive inline so the browser opens
/// them; doc/code/etc. arrive as an attachment.
pub const INLINE_KINDS: [&str; 5] = ["image", "screenshot", "video", "audio", "pdf"];

/// `MAX_RANGE_SPAN`: 8 MiB — seeking needs short spans; the cap keeps a
/// `bytes=0-` of a huge clip from buffering the whole file.
pub const MAX_RANGE_SPAN: i64 = 8 << 20;

/// `parse_range`: single range → (start, end) clamped; ("unsatisfiable", size)
/// for out-of-range; None when nothing to honor (no header, multi-range,
/// unknown unit → plain 200). `If-Range` is deliberately not honored.
pub enum Span {
    None,
    Unsatisfiable,
    Range(i64, i64),
}

pub fn parse_range(header: Option<&str>, size: i64) -> Span {
    let Some(header) = header else {
        return Span::None;
    };
    let caps = Regex::new(r"^bytes=(\d*)-(\d*)$")
        .expect("range regex")
        .captures(header.trim());
    let Some(caps) = caps else {
        return Span::None; // a multi-range or an unknown unit: plain 200
    };
    let start_s = &caps[1];
    let end_s = &caps[2];
    if start_s.is_empty() && end_s.is_empty() {
        return Span::None;
    }
    if start_s.is_empty() {
        // Suffix range: last N bytes, capped.
        let length: i64 = end_s.parse().unwrap_or(i64::MAX).min(MAX_RANGE_SPAN);
        if length == 0 {
            return Span::Unsatisfiable;
        }
        return Span::Range((size - length).max(0), size - 1);
    }
    let Ok(start) = start_s.parse::<i64>() else {
        return Span::None;
    };
    let end = match end_s.parse::<i64>() {
        Ok(end) if !end_s.is_empty() => end,
        _ => size - 1,
    };
    let end = end.min(size - 1).min(start + MAX_RANGE_SPAN - 1); // open + huge ranges capped
    if start >= size || start > end {
        return Span::Unsatisfiable;
    }
    Span::Range(start, end)
}

/// `urllib.parse.quote(name)` + the RFC 5987 `filename*=UTF-8''…` shape: the
/// safe set is Python's default (letters, digits, `_.-~`, `/`) — everything
/// else percent-encoded from the UTF-8 bytes.
pub fn quote_filename(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        let safe = ch.is_ascii_alphanumeric()
            || matches!(ch, '_' | '.' | '-' | '~' | '/');
        if safe {
            out.push(ch);
        } else {
            for byte in ch.encode_utf8(&mut [0u8; 4]).as_bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_clamp_and_refuse() {
        // The measured shapes of parse_range at size 1000.
        assert!(matches!(parse_range(None, 1000), Span::None));
        assert!(matches!(
            parse_range(Some("bytes=0-99"), 1000),
            Span::Range(0, 99)
        ));
        assert!(matches!(
            parse_range(Some("bytes=900-"), 1000),
            Span::Range(900, 999)
        ));
        assert!(matches!(
            parse_range(Some("bytes=-500"), 1000),
            Span::Range(500, 999)
        ));
        assert!(matches!(
            parse_range(Some("bytes=-2000"), 1000), // suffix capped at 8 MiB: 1000 fits
            Span::Range(0, 999)
        ));
        assert!(matches!(
            parse_range(Some("bytes=-0"), 1000),
            Span::Unsatisfiable
        ));
        assert!(matches!(
            parse_range(Some("bytes=1000-1200"), 1000),
            Span::Unsatisfiable
        ));
        assert!(matches!(
            parse_range(Some("bytes=500-400"), 1000),
            Span::Unsatisfiable
        ));
        assert!(matches!(
            parse_range(Some("bytes=0-1,2-3"), 1000), // multi-range: plain 200
            Span::None
        ));
        assert!(matches!(
            parse_range(Some("bytes=0-5000000000"), 1000),
            Span::Range(0, 999)
        ));
    }

    #[test]
    fn names_quote_like_python() {
        assert_eq!(quote_filename("plain-1.pdf"), "plain-1.pdf");
        assert_eq!(quote_filename("a b.png"), "a%20b.png");
        assert_eq!(quote_filename("照片.jpg"), "%E7%85%A7%E7%89%87.jpg");
    }

    #[test]
    fn hit_cache_is_a_true_lru() {
        let cache = HitCache::new(2);
        let a = cache.put(HitEntry {
            path: "/a".into(),
            kind: "image".into(),
            segment_id: 1,
            loc: None,
            code: false,
        });
        let b = cache.put(HitEntry {
            path: "/b".into(),
            kind: "image".into(),
            segment_id: 2,
            loc: None,
            code: false,
        });
        assert!(cache.get(&a).is_some()); // a refreshes: b is the oldest now
        let c = cache.put(HitEntry {
            path: "/c".into(),
            kind: "image".into(),
            segment_id: 3,
            loc: None,
            code: false,
        });
        assert!(cache.get(&a).is_some(), "the refreshed entry survived");
        assert!(cache.get(&b).is_none(), "the oldest-by-use was evicted");
        assert!(cache.get(&c).is_some());
        assert_eq!(a.len(), 12, "the handle is 12 hex chars");
    }
}
