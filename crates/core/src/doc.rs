//! `DocExtractor` (Extractors.swift:159) + `readText`/`richText`/`Zip`/`asHTML`
//! (Platform/Portable/Docs+Portable.swift): plain text from txt/md directly, HTML by stripping
//! tags, rtf/docx/odt through the file's own XML, spreadsheets and presentations from their XML
//! (`OfficeText`), and Apple Mail's `.emlx` through `MailMessage`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::extract::ExtractError;
use crate::{mail, office, rtf, zipfile};

/// Plain text from one file, as the indexer reads it.
pub fn text_of(path: &std::path::Path) -> Result<String, ExtractError> {
    let ext = crate::code::extension_of(&path.to_string_lossy()).to_lowercase();
    match ext.as_str() {
        "docx" | "doc" | "odt" | "rtf" => rich_text(path, &ext),
        "xlsx" | "pptx" | "ods" | "odp" => office::text(path, &ext),
        "html" | "htm" => Ok(plain_text_from_html(&read_text(path)?)),
        "emlx" => {
            // Apple Mail's message files: header lines + text bodies (issue #146); nil = the raw text.
            let mail_text = read_text(path)?;
            Ok(mail::text(mail_text.as_bytes()).unwrap_or(mail_text))
        }
        _ => read_text(path),
    }
}

/// `DocExtractor.readText`: Foundation's encoding guess, measured on this box (Swift 6.4 /
/// swift-corelibs): a BOM picks UTF-8 / UTF-16 / UTF-32 (UTF-32 BOMs first, so "FF FE 00 00" is
/// UTF-32LE and not UTF-16LE with a tail); without a BOM the bytes must be strict UTF-8 — NULs
/// and U+FFFE included, which a strict UTF-8 reader keeps as the characters they are — and
/// anything that fails to decode reads as ISO-Latin-1 (every byte a character).
pub fn read_text(path: &std::path::Path) -> Result<String, ExtractError> {
    let bytes = std::fs::read(path).map_err(|error| ExtractError::Unreadable {
        why: error.to_string(),
    })?;
    let head = &bytes[..bytes.len().min(4)];
    let decoded = if head.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        decode_utf32(&bytes[4..], true)
    } else if head.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        decode_utf32(&bytes[4..], false)
    } else if head.starts_with(&[0xFE, 0xFF]) {
        decode_utf16(&bytes[2..], true)
    } else if head.starts_with(&[0xFF, 0xFE]) {
        decode_utf16(&bytes[2..], false)
    } else if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(String::from_utf8_lossy(&bytes[3..]).into_owned());
    } else {
        return Ok(String::from_utf8(bytes.clone())
            .unwrap_or_else(|_| bytes.iter().map(|byte| char::from(*byte)).collect()));
    };
    // A BOM whose body does not decode falls back the way `readText` does: Latin-1 over the
    // whole file, BOM included.
    Ok(decoded.unwrap_or_else(|| fallback_latin1(&bytes)))
}

fn fallback_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16(units.as_slice()).ok()
}

fn decode_utf32(bytes: &[u8], big_endian: bool) -> Option<String> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = String::new();
    for group in bytes.chunks(4) {
        let value = if big_endian {
            u32::from_be_bytes([group[0], group[1], group[2], group[3]])
        } else {
            u32::from_le_bytes([group[0], group[1], group[2], group[3]])
        };
        out.push(char::from_u32(value)?);
    }
    Some(out)
}

/// The `<tag … />` strip patterns, compiled ONCE per tag for the process: `as_html` runs the
/// same handful of tags on every docx/odt it reads, and building them per call is per-file work
/// on the indexer's hot path. The pattern bytes are the site's own `format!`, unchanged.
static TAG_PATTERNS: LazyLock<Mutex<HashMap<String, Regex>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The compiled pattern for one tag (the same `<{tag}\\b[^>]*/?>` the site built inline), taken
/// from the cache; a tag seen for the first time compiles once and is inserted.
fn tag_pattern(tag: &str) -> Regex {
    let mut cache = TAG_PATTERNS.lock().expect("tag pattern cache");
    if let Some(pattern) = cache.get(tag) {
        return pattern.clone();
    }
    let pattern = Regex::new(&format!("<{tag}\\b[^>]*/?>")).expect("tag pattern");
    cache.insert(tag.to_string(), pattern.clone());
    pattern
}

/// Word-processing XML as the little HTML `plain_text_from_html` reads: paragraph ends become
/// `</p>`, breaks `<br>`, tabs and spaces a space; every other tag is stripped there.
fn as_html(xml: &str, paragraphs: &[&str], breaks: &[&str], spaces: &[&str]) -> String {
    let mut text = xml.to_string();
    for tag in paragraphs {
        text = text.replace(&format!("</{tag}>"), "</p>");
    }
    for &tag in breaks {
        let pattern = tag_pattern(tag);
        text = pattern.replace_all(&text, "<br>").into_owned();
    }
    for &tag in spaces {
        let pattern = tag_pattern(tag);
        text = pattern.replace_all(&text, " ").into_owned();
    }
    text
}

fn rich_text(path: &std::path::Path, ext: &str) -> Result<String, ExtractError> {
    // `richText` answers the text file's own words BEFORE it opens a zip: an RTF is not a zip,
    // and a legacy .doc refuses without reading anything.
    if ext == "rtf" {
        return Ok(rtf::text(&read_text(path)?));
    }
    if ext == "doc" {
        return Err(ExtractError::Unreadable {
            why: "legacy Word (.doc) files aren't read on this system".into(),
        });
    }
    let zip = zipfile::ZipArchive::open(path)?;
    match ext {
        "docx" => Ok(plain_text_from_html(&as_html(
            &zip.entry("word/document.xml")?,
            &["w:p"],
            &["w:br", "w:cr"],
            &["w:tab"],
        ))),
        "odt" => Ok(plain_text_from_html(&as_html(
            &zip.entry("content.xml")?,
            &["text:p", "text:h"],
            &["text:line-break"],
            &["text:tab", "text:s"],
        ))),
        _ => unreachable!("richText is asked only for docx, doc, odt and rtf"),
    }
}

static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<script\b.*?</script\s*>").unwrap());
static STYLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)<style\b.*?</style\s*>").unwrap());
static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
static HEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<head\b.*?</head\s*>").unwrap());
static BLOCK_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<(br|/p|/div|/li|/h[1-6]|/tr|/section|/article)\b[^>]*>").unwrap()
});
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
static NUMERIC_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&#(x[0-9a-fA-F]+|[0-9]+);").unwrap());
static RUNS_OF_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+").unwrap());
static AROUND_NEWLINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\n\s*").unwrap());

/// Good enough for search: drops scripts, styles and comments, turns block ends into line
/// breaks, strips the remaining tags, and decodes common entities.
pub fn plain_text_from_html(html: &str) -> String {
    let mut text = SCRIPT.replace_all(html, " ").into_owned();
    text = STYLE.replace_all(&text, " ").into_owned();
    text = COMMENT.replace_all(&text, " ").into_owned();
    text = HEAD.replace_all(&text, " ").into_owned();
    text = BLOCK_END.replace_all(&text, "\n").into_owned();
    text = TAG.replace_all(&text, " ").into_owned();
    // The engine loops these over a Swift Dictionary, whose iteration order is randomized per
    // process; the port runs them in the engine's written order. Inputs where one entity's
    // replacement creates text that matches the next entity ("&amp;lt;") answer in the engine in
    // that run's order — no other process can reproduce it, so the port is deterministic here.
    for (entity, character) in [
        ("&nbsp;", " "),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&hellip;", "…"),
    ] {
        text = text.replace(entity, character);
    }
    text = NUMERIC_ENTITY
        .replace_all(&text, |caps: &regex::Captures| {
            let code = &caps[1];
            let value = match code.strip_prefix('x') {
                Some(hex) => crate::xmlscan::parse_radix(hex, 16),
                None => crate::xmlscan::parse_radix(code, 10),
            };
            value
                .and_then(char::from_u32)
                .map(|ch| ch.to_string())
                .unwrap_or_else(|| " ".to_string())
        })
        .into_owned();
    text = RUNS_OF_SPACES.replace_all(&text, " ").into_owned();
    text = AROUND_NEWLINES.replace_all(&text, "\n").into_owned();
    office::trim_ws(&text)
}

static CHUNK_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\u{00A0}]+").unwrap());
static THREE_NEWLINES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n\s*\n+").unwrap());
static ANY_SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Splits text into ~`size`-character pieces (~450 tokens) that overlap a little, breaking at
/// whitespace. Swift counts CHARACTERS, so the port walks grapheme clusters.
pub fn chunks(text: &str, size: i64, overlap: i64, limit: i64) -> Vec<String> {
    // A plain string, so Swift puts the real tab and no-break space in the class; the regex
    // crate spells them the same.
    let clean = CHUNK_SPACES.replace_all(text, " ");
    let clean = THREE_NEWLINES.replace_all(&clean, "\n\n");
    let clean = office::trim_ws(&clean);
    let clusters: Vec<&str> =
        unicode_segmentation::UnicodeSegmentation::graphemes(clean.as_str(), true).collect();
    let is_ws = |cluster: &str| cluster.chars().all(char::is_whitespace);

    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let size = size as usize;
    let overlap = overlap as usize;
    while start < clusters.len() && (out.len() as i64) < limit {
        let mut end = (start + size).min(clusters.len());
        if end < clusters.len() {
            // Break at the last whitespace, when that keeps more than half the window.
            if let Some(space) = (start..end).rev().find(|at| is_ws(clusters[*at])) {
                if space - start > size / 2 {
                    end = space;
                }
            }
        }
        let chunk = office::trim_ws(&clusters[start..end].concat());
        if !chunk.is_empty() {
            out.push(chunk);
        }
        if end >= clusters.len() {
            break;
        }
        let mut next = if end >= overlap + start {
            end - overlap
        } else {
            end
        };
        if next <= start {
            next = end;
        }
        // Start the next chunk on a word boundary.
        while next < end && !is_ws(clusters[next]) {
            next += 1;
        }
        start = if next < end { next } else { end };
    }
    out
}

/// One line, at most `limit` characters: for result lists.
pub fn excerpt(text: &str, limit: usize) -> String {
    // Only the start is shown, so only the start is cleaned up (a page or a chunk can be
    // thousands of characters).
    let clusters: Vec<&str> =
        unicode_segmentation::UnicodeSegmentation::graphemes(text, true).collect();
    let prefix_len = (limit * 8).min(clusters.len());
    let prefix: String = clusters[..prefix_len].concat();
    let line = office::trim_ws_in(
        &ANY_SPACES.replace_all(&prefix, " "),
        crate::search_text::is_whitespace_set,
    );
    let line_clusters: Vec<&str> =
        unicode_segmentation::UnicodeSegmentation::graphemes(line.as_str(), true).collect();
    if line_clusters.len() > limit || prefix_len < clusters.len() {
        return format!(
            "{}…",
            line_clusters[..limit.min(line_clusters.len())].concat()
        );
    }
    line
}
