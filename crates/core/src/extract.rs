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
    // Swift's `text.prefix(limit * 8)` (Extractors.swift:251) walks only the graphemes it
    // shows, and `start.endIndex < text.endIndex` needs one more to know there was more.
    // `graphemes(true).collect()` walked and boxed the WHOLE text per call — document
    // chunks run to 4,000 characters, and this runs per hit on every search answer.
    let mut graphemes = text.graphemes(true);
    let start: String = graphemes.by_ref().take(take).collect();
    let has_more = graphemes.next().is_some();
    let collapsed = crate::search_text::collapse_whitespace(&start);
    let line = crate::search_text::trim_whitespaces(&collapsed);
    if line.graphemes(true).count() > limit || has_more {
        let head: String = line.graphemes(true).take(limit).collect();
        head + "…"
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::ExtractError;

    /// The refusal texts are contract bytes: `ExtractError.description`
    /// (Extractors.swift:11-17) is what lands in `files.detail`, shows in the UI, and
    /// the ⑩ cross-check scenarios compare byte for byte. The multiplication sign is
    /// U+00D7 — an ASCII "x" in `small image 30×20` passes the Rust suite alone and
    /// fails the cross-check, so it is pinned here at the source.
    #[test]
    fn refusal_texts_match_the_engines_description() {
        assert_eq!(
            ExtractError::Unreadable {
                why: "can't decode image".into()
            }
            .to_string(),
            "unreadable: can't decode image"
        );
        assert_eq!(
            ExtractError::TooSmall {
                width: 0,
                height: 0
            }
            .to_string(),
            "small image 0\u{d7}0"
        );
        assert_eq!(
            ExtractError::TooSmall {
                width: 30,
                height: 20
            }
            .to_string(),
            "small image 30\u{d7}20",
            "the sign is × (U+00D7), as Extractors.swift:14 spells it"
        );
        assert_eq!(ExtractError::Locked.to_string(), "password-protected");
        assert_eq!(
            ExtractError::Empty {
                why: "no pages".into()
            }
            .to_string(),
            "no pages"
        );
        assert_eq!(
            ExtractError::LeftOut {
                why: "generated code".into()
            }
            .to_string(),
            "generated code"
        );
    }
}
