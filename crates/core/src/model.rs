//! The engine's value model + the path/bucket range machinery (ARCHITECTURE_MAP.md §2).
//!
//! Reproduced from `Sources/DigUpKit/Model.swift` and `Sources/DigUpKit/IndexStore.swift`.
//! The range functions matter for correctness, not just tidiness: the scoped count queries trust
//! the one merged range whose `lo` is greatest at-or-below a path, and that is exact only when the
//! ranges are disjoint — merging is what makes them so (IndexStore.swift:110). Comparisons are
//! bytewise (SQLite's BINARY text collation), which keeps `/ab` out of `/a`'s range and a
//! decomposed name out of its precomposed folder.

use std::cmp::Ordering;
use std::path::Path;

use unicode_segmentation::UnicodeSegmentation;

use crate::search_text::trim_whitespaces_newlines;

/// What a file is, for indexing and grouping (Model.swift:4). Raw value == the `kind` column text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileKind {
    Screenshot,
    Image,
    Pdf,
    Doc,
    Audio,
    Video,
    Code,
}

impl FileKind {
    /// Kinds whose indexing asks the model for pictures or sound (issue #155): the
    /// media encoders stay unloaded in the embedder until a queue holds one of these.
    pub fn needs_media_encoders(&self) -> bool {
        matches!(self, Self::Image | Self::Screenshot | Self::Video | Self::Audio)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FileKind::Screenshot => "screenshot",
            FileKind::Image => "image",
            FileKind::Pdf => "pdf",
            FileKind::Doc => "doc",
            FileKind::Audio => "audio",
            FileKind::Video => "video",
            FileKind::Code => "code",
        }
    }
    pub fn parse(s: &str) -> Option<FileKind> {
        Some(match s {
            "screenshot" => FileKind::Screenshot,
            "image" => FileKind::Image,
            "pdf" => FileKind::Pdf,
            "doc" => FileKind::Doc,
            "audio" => FileKind::Audio,
            "video" => FileKind::Video,
            "code" => FileKind::Code,
            _ => return None,
        })
    }
    /// The first indexing pass orders by kind (Model.swift:16) — embedded verbatim into ORDER BY.
    pub const INDEXING_ORDER_SQL: &'static str = "CASE kind WHEN 'screenshot' THEN 0 WHEN 'image' THEN 1 WHEN 'pdf' THEN 2 WHEN 'doc' THEN 3 WHEN 'audio' THEN 4 WHEN 'video' THEN 5 ELSE 6 END";
}

/// One searchable piece of a file (Model.swift:23). Raw values, incl. `page_image`'s underscore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Name,
    Image,
    Ocr,
    Page,
    PageImage,
    Chunk,
    Audio,
    Frame,
    Lines,
    Cells,
}

impl SegmentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SegmentKind::Name => "name",
            SegmentKind::Image => "image",
            SegmentKind::Ocr => "ocr",
            SegmentKind::Page => "page",
            SegmentKind::PageImage => "page_image",
            SegmentKind::Chunk => "chunk",
            SegmentKind::Audio => "audio",
            SegmentKind::Frame => "frame",
            SegmentKind::Lines => "lines",
            SegmentKind::Cells => "cells",
        }
    }
    /// `SegmentKind(rawValue:)` — the caller applies the Swift default (`.name` in `search`).
    pub fn parse(s: &str) -> Option<SegmentKind> {
        Some(match s {
            "name" => SegmentKind::Name,
            "image" => SegmentKind::Image,
            "ocr" => SegmentKind::Ocr,
            "page" => SegmentKind::Page,
            "page_image" => SegmentKind::PageImage,
            "chunk" => SegmentKind::Chunk,
            "audio" => SegmentKind::Audio,
            "frame" => SegmentKind::Frame,
            "lines" => SegmentKind::Lines,
            "cells" => SegmentKind::Cells,
            _ => return None,
        })
    }
}

/// What produced a segment's vector (Model.swift:38); scores normalize per modality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modality {
    None,
    Text,
    Image,
    Audio,
}

impl Modality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Modality::None => "none",
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Audio => "audio",
        }
    }
    /// `Modality(rawValue:)` — `search`'s reload defaults an unknown string to `.none`.
    pub fn parse(s: &str) -> Option<Modality> {
        Some(match s {
            "none" => Modality::None,
            "text" => Modality::Text,
            "image" => Modality::Image,
            "audio" => Modality::Audio,
            _ => return None,
        })
    }
}

/// A helper that had to stop on a file leaves this note beside the index (`StoppedFile`,
/// Model.swift:143). The exit status that says "a file was stopped: start a new helper"
/// is EX_TEMPFAIL — 75; the watchdog writes the note and dies with it, and the next
/// helper takes the note (`take` removes it as it reads) and marks that file failed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoppedFile {
    pub path: Option<String>,
    pub why: String,
}

impl StoppedFile {
    pub const EXIT_CODE: i32 = 75;

    /// The fatal signal that says nothing about a file: SIGKILL. macOS' jetsam and Linux'
    /// OOM killer pick the process with the biggest footprint — which a helper reading a
    /// 45 MP RAW is — and SIGKILL cannot be caught, so `CrashNotes` leaves no note for it
    /// and the mark's only clue is the file the helper last reported reading. The other
    /// fatal signals arrive *inside* a file's reading and name the file at once.
    pub const UNTRACEABLE_SIGNAL: i32 = 9;

    /// Whether the helper that died by `signal` names the file it was last reading for
    /// the next helper. A catchable crash names it. SIGKILL names it only on a repeat —
    /// the same queue position killed twice, `pending` not moving between the kills; the
    /// first SIGKILL is a retry of the same file, not an accusation.
    pub fn names_file(signal: i32, repeated: bool) -> bool {
        signal != Self::UNTRACEABLE_SIGNAL || repeated
    }

    pub fn location(folder: &std::path::Path) -> std::path::PathBuf {
        folder.join("stopped-file.json")
    }

    /// The JSON bytes as the engine writes them (`JSONEncoder` over the Codable's field
    /// order): `{"path":…,"why":"…"}`, `path` null when no file is known.
    pub fn bytes(&self) -> Vec<u8> {
        #[derive(serde::Serialize)]
        struct Wire<'a> {
            path: Option<&'a str>,
            why: &'a str,
        }
        // serde_json sorts map keys; a struct keeps the declared order, as Swift does.
        serde_json::to_vec(&Wire {
            path: self.path.as_deref(),
            why: &self.why,
        })
        .unwrap_or_default()
    }

    pub fn write_to(&self, folder: &std::path::Path) {
        // `writeReplacing`: a failure leaves no half-written note behind.
        let path = Self::location(folder);
        if let Ok(temp) = Self::temp_sibling(&path) {
            use std::io::Write;
            if std::fs::File::create(&temp)
                .and_then(|mut f| f.write_all(&self.bytes()))
                .is_ok()
            {
                let _ = std::fs::rename(&temp, &path);
            } else {
                let _ = std::fs::remove_file(&temp);
            }
        }
    }

    fn temp_sibling(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        Ok(path.with_extension(format!("json.tmp{:x}", hasher.finish())))
    }

    /// The note an earlier helper left, removed as it's read.
    pub fn take(folder: &std::path::Path) -> Option<Self> {
        let path = Self::location(folder);
        let data = std::fs::read(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        serde_json::from_slice(&data).ok()
    }
}

/// A file as a walk reported it — the unit of `insert` (IndexStore.swift:621).
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: String,
    pub kind: FileKind,
    pub size: i64,
    pub mtime: f64,
    pub inode: u64,
    pub device: u64,
}

/// A file as the index knows it (`FileRecord`, IndexStore.swift:4).
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: i64,
    pub path: String,
    pub kind: FileKind,
    pub size: i64,
    pub mtime: f64,
    pub inode: u64,
    pub device: u64,
    pub state: String,
    pub detail: Option<String>,
}

/// Bytewise less-than, the order SQLite keeps TEXT in (BINARY) (IndexStore.swift:116).
pub fn byte_less(a: &str, b: &str) -> bool {
    let x = a.as_bytes();
    let y = b.as_bytes();
    for (i, byte) in x.iter().enumerate() {
        if i >= y.len() {
            return false; // b ran out: a is longer → a is after b
        }
        if byte != &y[i] {
            return byte < &y[i];
        }
    }
    x.len() < y.len() // a is a strict prefix → a is before b
}

/// The folder a file's name sits in: the path up to the last `/`, the slash dropped
/// (IndexStore.swift:141). Scanned bytewise: a 0x2F byte is always a real separator (every UTF-8
/// continuation byte is ≥ 0x80), so cutting at a slash byte yields valid UTF-8. Root-level files
/// ("/x") → `""`; a Windows drive-root file ("C:/x") → `"C:"`.
pub fn bucket_of(path: &str) -> String {
    match path.as_bytes().iter().rposition(|&b| b == b'/') {
        Some(last) => path[..last].to_string(),
        None => String::new(),
    }
}

/// A folder's real path (symlinks resolved), when it's spelled differently than given
/// (`standardizedFileURL` says /var where the walk says /private/var; IndexStore.swift:100).
fn real_path(folder: &str) -> Option<String> {
    std::fs::canonicalize(Path::new(folder))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// The ranges of the UNIQUE path index that hold the files under `folder` (IndexStore.swift:100):
/// a range for the folder as given, plus one for its real path when that's spelled differently.
pub fn path_ranges(under: &str) -> Vec<(String, String)> {
    let mut spellings = vec![under.to_string()];
    if let Some(real) = real_path(under) {
        if real.as_bytes() != under.as_bytes() {
            spellings.push(real);
        }
    }
    spellings
        .into_iter()
        .map(|path| {
            let prefix = if path.ends_with('/') {
                path
            } else {
                format!("{path}/")
            };
            let to = format!("{}{}", prefix, char::MAX);
            (prefix, to)
        })
        .collect()
}

/// The range list sorted and folded into disjoint spans (IndexStore.swift:115): nested ranges,
/// repeats, and touching ones all merge.
pub fn merged_ranges(ranges: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut sorted: Vec<(String, String)> = ranges
        .into_iter()
        .filter(|r| byte_less(&r.0, &r.1))
        .collect();
    sorted.sort_by(|a, b| {
        if byte_less(&a.0, &b.0) {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    });
    for r in sorted {
        // r.from <= last.to (touching or overlapping): fold into the last span.
        let merged = match out.last() {
            Some(last) if !byte_less(&last.1, &r.0) => {
                let mut last = last.clone();
                if byte_less(&last.1, &r.1) {
                    last.1 = r.1.clone();
                }
                Some(last)
            }
            _ => None,
        };
        match merged {
            Some(m) => {
                out.pop();
                out.push(m);
            }
            None => out.push(r),
        }
    }
    out
}

/// The ranges of the counters table's bucket key that hold the files under `folder`
/// (IndexStore.swift:167) — the path ranges mapped into the two shapes buckets come in. `own` is
/// the folder name without its trailing slash; the files sitting in the folder itself have bucket
/// exactly `own`, and a one-code-unit-past range spans that one bucket and nothing else.
pub fn bucket_ranges(under: &str) -> Vec<(String, String)> {
    path_ranges(under)
        .into_iter()
        .flat_map(|range| {
            let own = range
                .0
                .chars()
                .take(range.0.chars().count() - 1)
                .collect::<String>();
            [
                (own.clone(), format!("{own}\u{1}")),
                (range.0.clone(), range.1.clone()),
            ]
        })
        .collect()
}

/// What's typed after "name:" ("" for "name:" alone), nil for a search that isn't one of
/// names (Model.swift:124). The leading trim runs on `Character.isWhitespace` — the first
/// scalar's White_Space, dropped Character by Character (graphemes); the trailing one is
/// `trimmingCharacters(in: .whitespacesAndNewlines)` (scalar runs — `SearchText`'s trim).
pub enum NameQuery {}

impl NameQuery {
    pub const PREFIX: &str = "name:";

    pub fn text_of(query: &str) -> Option<String> {
        let mut rest = query;
        loop {
            match rest.graphemes(true).next() {
                Some(g) if g.chars().next().is_some_and(char::is_whitespace) => {
                    rest = &rest[g.len()..]
                }
                _ => break,
            }
        }
        let head: String = rest
            .graphemes(true)
            .take(Self::PREFIX.graphemes(true).count())
            .map(|g| g.to_lowercase())
            .collect();
        if head != Self::PREFIX {
            return None;
        }
        let body: String = rest.graphemes(true).skip(5).collect();
        Some(trim_whitespaces_newlines(&body).to_string())
    }
}
