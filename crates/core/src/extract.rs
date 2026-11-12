//! `ExtractError` (Extractors.swift:3-20): why a file went into the index unreadable, or out of
//! it entirely. The description strings are what `index-service` reports and what the app shows.
//! And the `excerpt` helper (Extractors.swift:249): the one-line preview of a match.

use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExtractError {
    #[error("unreadable: {why}")]
    Unreadable { why: String },
    #[error("small image {width}×{height}")]
    TooSmall { width: i64, height: i64 },
    #[error("password-protected")]
    Locked,
    #[error("{why}")]
    Empty { why: String },
    /// Not for the index after all, once read (code a program made, or holding a private key).
    #[error("{why}")]
    LeftOut { why: String },
}

// One line, at most `limit` characters: for result lists (Extractors.swift:249).
// Only the start is shown, so only the start is cleaned up (a page or a chunk can be
// thousands of characters): NSRE `\s+` collapses every White_Space run to one space
// (runs at the ends included, then the `.whitespaces` trim below), and the line takes
// an "…" when it runs past `limit` graphemes — or when the `limit*8` prefix left
// source behind, whose tail the collapse might have hidden.

/// `excerpt(_:limit:)` — the engine's one-line preview of a match.
pub fn excerpt(text: &str, limit: usize) -> String {
    let take = limit * 8;
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let has_more = graphemes.len() > take;
    let start: String = graphemes.iter().take(take).copied().collect();
    let collapsed = crate::search_text::collapse_whitespace(&start);
    let line = crate::search_text::trim_whitespaces(&collapsed);
    if line.graphemes(true).count() > limit || has_more {
        let head: String = line.graphemes(true).take(limit).collect();
        head + "…"
    } else {
        line.to_string()
    }
}
