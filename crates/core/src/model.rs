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

/// Foundation's `.whitespacesAndNewlines` as measured on the ⑩a probe: `.whitespaces`
/// (the Unicode space separators Zs — NBSP, ideographic space, the thin spaces — plus the
/// tab) union the line controls (LF, VT, FF, CR, NEL, LS, PS). NBSP IS in it (scenario
/// `n161`), U+200B (ZWSP) is not.
fn is_ws_newline(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\n'
                | '\u{0B}'
                | '\u{0C}'
                | '\r'
                | '\u{85}'
    )
}

/// What's typed after "name:" ("" for "name:" alone), nil for a search that isn't one of
/// names (Model.swift:124). The leading trim runs on `Character.isWhitespace` — the first
/// scalar's White_Space; the trailing one on `.whitespacesAndNewlines` — both Character by
/// Character (graphemes).
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
        // `trimmingCharacters(in:)` (swift-corelibs) drops the leading and trailing runs of
        // SCALARS in the set — graphemes do not move as units (measured, ⑩a `facts-trim`
        // f049: "  ́x" → "́x": the space strips and the combining mark stays).
        let chars: Vec<char> = body.chars().collect();
        let mut i = 0;
        let mut j = chars.len();
        while i < j && is_ws_newline(chars[i]) {
            i += 1;
        }
        while j > i && is_ws_newline(chars[j - 1]) {
            j -= 1;
        }
        Some(chars[i..j].iter().collect())
    }
}
