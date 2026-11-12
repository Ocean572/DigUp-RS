//! Keyword-search text normalization (ARCHITECTURE_MAP.md §2), ported from
//! `Sources/DigUpKit/SearchText.swift`. These run in the v1→2 (rebuild `fts` from stored text)
//! and v4→5 (drop digit-only vectors) migrations, so an index that predates v6 migrates the same
//! way the Swift engine would.

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::UnicodeNormalization;

/// NFKC + the script fixes SearchText applies (SearchText.swift:8): presentation forms and
/// ligatures plain, full-width to ASCII, invisible formatting dropped, Arabic typed as people type
/// it (no tatweel, no harakat, one alef).
pub fn normalized(text: &str) -> String {
    let compatible: String = text.nfkc().collect();
    let mut out = String::with_capacity(compatible.len());
    for scalar in compatible.chars() {
        match scalar as u32 {
            0x200B => out.push(' '), // zero-width space: a word break (Thai, Khmer)
            // joiners, direction marks, soft hyphen, BOM
            0x00AD
            | 0x061C
            | 0x200C..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x2069
            | 0xFEFF => {}
            // tatweel; harakat and the dagger alef
            0x0640 | 0x064B..=0x065F | 0x0670 => {}
            // alef with hamza/madda/wasla → alef
            0x0622 | 0x0623 | 0x0625 | 0x0671 => out.push('\u{0627}'),
            _ => out.push(scalar),
        }
    }
    out
}

fn is_letter(scalar: char) -> bool {
    matches!(
        get_general_category(scalar),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

/// True when text says something in words: at least a fifth of the non-space characters are
/// letters (marks count, such as Bengali vowel signs). A table of numbers is not, so it gets
/// keyword search only (SearchText.swift:37).
pub fn has_words(text: &str) -> bool {
    let mut letters = 0u64;
    let mut others = 0u64;
    for scalar in text.chars() {
        if scalar == '\n' || scalar.is_whitespace() {
            continue;
        }
        if is_letter(scalar) {
            letters += 1;
        } else {
            others += 1;
        }
    }
    letters > 0 && letters * 4 >= others
}

/// Enough prose for a vector: `hasWords`, and at least four words with a letter in them. A stub
/// (`DEBUG=true`, `root = true`, a two-line .gitignore) passes the letters test and then wins
/// generic queries on meaning alone, since text that short collapses toward the model's generic
/// centre; its words are still found by keyword (SearchText.swift:48).
pub fn has_prose(text: &str) -> bool {
    if !has_words(text) {
        return false;
    }
    let mut words = 0;
    for word in text.split_whitespace() {
        if word.chars().any(is_letter) {
            words += 1;
            if words >= 4 {
                return true;
            }
        }
    }
    false
}

/// What's wrong with a PDF page's text layer, when it can't be used as read (SearchText.swift:178).
#[derive(Debug, PartialEq, Eq)]
pub enum PageTextProblem {
    /// Bengali/Hindi read in glyph order: vowel signs ahead of their consonants.
    GlyphOrder,
    /// Arabic letters lost at a lam ligature (a bare alef before a "?" or ".").
    LostLetters,
}

fn arabic(value: u32) -> bool {
    (0x0600..=0x06FF).contains(&value)
}

fn is_indic_vowel_sign(value: u32) -> bool {
    matches!(
        value,
        0x09BE..=0x09C4 | 0x09C7 | 0x09C8 | 0x09CB | 0x09CC | 0x09D7 | 0x09E2 | 0x09E3
            | 0x093A | 0x093B | 0x093E..=0x094C | 0x094E | 0x094F | 0x0955..=0x0957 | 0x0962 | 0x0963
    )
}

fn is_indic_consonant_or_nukta(value: u32) -> bool {
    matches!(
        value,
        0x0995..=0x09B9 | 0x09BC | 0x09CE | 0x09DC | 0x09DD | 0x09DF | 0x09F0 | 0x09F1
            | 0x0915..=0x0939 | 0x093C | 0x0958..=0x095F | 0x0979..=0x097F
    )
}

impl PageTextProblem {
    /// Ported from SearchText.swift:188. `isAlphabetic` for the Arabic lam check is approximated by
    /// the Letter category (every alphabetic scalar in the Arabic block is a letter); the check only
    /// gates which garbled PDFs are re-queued for OCR, and only on a v1→v2 migration.
    pub fn check(text: &str) -> Option<PageTextProblem> {
        let nfc: String = text.nfc().collect();
        let mut vowel_signs = 0u64;
        let mut misplaced = 0u64;
        let mut lost_letters = 0u64;
        let mut previous = ' ';
        let mut before_previous = ' ';
        for scalar in nfc.chars() {
            let value = scalar as u32;
            if is_indic_vowel_sign(value) {
                vowel_signs += 1;
                if !is_indic_consonant_or_nukta(previous as u32) {
                    misplaced += 1;
                }
            }
            if before_previous == '\u{0627}'
                && (previous == '?' || previous == '.' || previous == '\u{FFFD}')
                && arabic(value)
                && is_letter(scalar)
            {
                lost_letters += 1;
            }
            before_previous = previous;
            previous = scalar;
        }
        if vowel_signs >= 10 && misplaced * 20 >= vowel_signs {
            return Some(PageTextProblem::GlyphOrder);
        }
        if lost_letters >= 2 {
            return Some(PageTextProblem::LostLetters);
        }
        None
    }
}
