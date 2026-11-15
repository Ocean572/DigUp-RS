//! `excerpt(_:limit:)` (Extractors.swift:249-255): the one-line preview the search
//! answers carry, per hit.
//!
//! The reference below is the algorithm transcribed exactly as Swift spells it —
//! `prefix(limit*8)`, whitespace collapse, trim, and the "…" when the line runs past
//! `limit` graphemes **or** the prefix left source behind. The tests pin `excerpt`
//! against it on the inputs where the shape bites: grapheme clusters that must not
//! split at the cut, the exact 8×limit boundary, the trailing-source rule, and the
//! collapse's effect on the length test.

use digup_core::extract::excerpt;
use unicode_segmentation::UnicodeSegmentation;

/// Swift's own transcription, using only whole-string operations (a `String.prefix`
/// cannot split a grapheme cluster; `hasMore` is `prefix.end < text.end`).
fn reference(text: &str, limit: usize) -> String {
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let take = limit * 8;
    let start: String = graphemes.iter().take(take).copied().collect();
    let has_more = graphemes.len() > take;
    // `\s+` → " " over the prefix, then trim `.whitespaces`.
    let collapsed: String = {
        let mut out = String::new();
        let mut run = false;
        for c in start.chars() {
            if c.is_whitespace() {
                run = true;
            } else {
                if run && !out.is_empty() {
                    out.push(' ');
                }
                run = false;
                out.push(c);
            }
        }
        if run && !out.is_empty() {
            out.push(' ');
        }
        out
    };
    let line = collapsed.trim_start().trim_end().to_string();
    if line.graphemes(true).count() > limit || has_more {
        let head: String = line.graphemes(true).take(limit).collect();
        head + "…"
    } else {
        line
    }
}

#[test]
fn excerpt_matches_the_engine_transcription() {
    let zwj = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"; // family emoji: ONE grapheme
    let cases: Vec<(String, usize)> = vec![
        ("".to_string(), 240),
        ("   ".to_string(), 240),
        ("one line".to_string(), 240),
        ("a\r\nb\t c".to_string(), 240),
        (format!("{zwj} {} kids", "visit".repeat(4)), 6),
        // A combining-script run: 400 graphemes of Devanagari + matras, cut mid-cluster order.
        (
            ["कि", "ता", "बें"]
                .iter()
                .flat_map(|w| std::iter::repeat_n(*w, 3))
                .map(|w| w.to_string())
                .collect::<Vec<_>>()
                .join(" "),
            20,
        ),
        // The exact boundaries of the prefix: 8*limit and 8*limit+1 graphemes, and the
        // line that is exactly `limit` after the collapse.
        ("x".repeat(8 * 5), 5),
        ("x".repeat(8 * 5 + 1), 5),
        (["a"; 40].join(" \n "), 40),
        ("  leading and trailing  ".to_string(), 240),
    ];
    for (text, limit) in cases {
        assert_eq!(
            excerpt(&text, limit),
            reference(&text, limit),
            "excerpt must answer as `prefix(limit*8)` + collapse + trim + … does: {text:?}"
        );
    }
}

/// The two rules that put the "…" there, kept apart: the collapsed line running past
/// `limit` (truncation, no matter how long the source was), and source left behind the
/// 8×limit prefix (the engine's `start.endIndex < text.endIndex`), which adds the
/// ellipsis even when the line fits. Whitespace collapse is the way to separate them:
/// a long field of spaces collapses to nothing, so only the source rule can fire.
#[test]
fn the_ellipsis_names_the_source_left_behind_the_prefix() {
    // The line rule: 24 a's, limit 3 — the line is what runs past the limit.
    assert_eq!(excerpt(&"a".repeat(8 * 3), 3), "aaa…");
    assert_eq!(excerpt(&"a".repeat(8 * 3 + 1), 3), "aaa…");

    // The source rule alone: 25 graphemes is one past the 24-grapheme prefix; the
    // collapsed line is `a` (fits under the limit), and the ellipsis comes from the
    // left-behind tail. Cut at exactly the prefix, the same collapse answers bare.
    assert_eq!(excerpt(&format!("a{}", " ".repeat(8 * 3)), 3), "a…");
    assert_eq!(excerpt(&format!("a{}", " ".repeat(8 * 3 - 1)), 3), "a");

    // A 210-grapheme field of spaces: the whole text is inside the prefix, the line
    // collapses to empty, and nothing was left behind — empty answer, not an ellipsis.
    assert_eq!(excerpt(&" \n\t".repeat(70), 30), "");
    assert_eq!(excerpt(&" \n\t".repeat(10), 30), "");
}
