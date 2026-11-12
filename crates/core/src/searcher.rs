//! Query words and FTS5 patterns (the `Searcher.swift` statics, ⑩a): what keyword search
//! looks for, how a query becomes FTS5 patterns, the kinds a query asks for, and when a
//! query is a code/ID lookup. The word lists ride verbatim from Searcher.swift:383/437 —
//! English, Bengali and Arabic, in the spelling `terms` sees (normalized, lowercase).
//!
//! The Swift statics split words with `CharacterSet.alphanumerics` (the Letters, Numbers,
//! combining Marks and connector punctuation — the Mc of a Bengali matra and the underscore
//! of a code name survive the split, the ZWJ between letters does not).

use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

use crate::model::FileKind;
use crate::search_text::{inflects, is_alphanumeric_scalar, normalized, simple_lowercase};

fn is_number_category(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
    )
}

/// Swift's `Character.isLetter` as measured (⑩a `facts-grapheme`): the Letters, the
/// Spacing and Enclosing Marks (the Bengali matra is a letter) and the Letter Numbers
/// (`Ⅷ`) — NOT the Nd digits, the No symbols, NOT the Mn combining marks. Apply it to
/// the first scalar of a Character; `Character.isNumber` is `is_number_category`.
pub fn is_letter_character(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::LetterNumber
    )
}

fn stopwords() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().copied().collect())
}

pub const STOPWORDS: [&str; 91] = [
    "a",
    "an",
    "the",
    "of",
    "in",
    "on",
    "at",
    "to",
    "for",
    "with",
    "and",
    "or",
    "my",
    "me",
    "that",
    "this",
    "these",
    "those",
    "from",
    "about",
    "where",
    "what",
    "which",
    "who",
    "when",
    "is",
    "was",
    "are",
    "were",
    "be",
    "it",
    "its",
    "some",
    "any",
    "find",
    "show",
    "file",
    "files",
    "photo",
    "photos",
    "picture",
    "image",
    "images",
    "screenshot",
    "screenshots",
    "video",
    "videos",
    "pdf",
    "document",
    "documents",
    "recording",
    "এবং",
    "ও",
    "এই",
    "সেই",
    "একটি",
    "আমার",
    "থেকে",
    "জন্য",
    "সাথে",
    "মধ্যে",
    "কোথায়",
    "কোন",
    "কী",
    "কি",
    "যে",
    "ছবি",
    "স্ক্রিনশট",
    "ভিডিও",
    "ফাইল",
    "পিডিএফ",
    "في",
    "من",
    "على",
    "الى",
    "عن",
    "مع",
    "هذا",
    "هذه",
    "ذلك",
    "التي",
    "الذي",
    "اين",
    "ما",
    "صورة",
    "صور",
    "لقطة",
    "شاشة",
    "فيديو",
    "ملف",
    "مستند",
];

pub const KIND_WORDS: [(&str, FileKind); 43] = [
    ("screenshot", FileKind::Screenshot),
    ("screenshots", FileKind::Screenshot),
    ("photo", FileKind::Image),
    ("photos", FileKind::Image),
    ("picture", FileKind::Image),
    ("pictures", FileKind::Image),
    ("image", FileKind::Image),
    ("images", FileKind::Image),
    ("pdf", FileKind::Pdf),
    ("pdfs", FileKind::Pdf),
    ("paper", FileKind::Pdf),
    ("papers", FileKind::Pdf),
    ("document", FileKind::Doc),
    ("documents", FileKind::Doc),
    ("note", FileKind::Doc),
    ("notes", FileKind::Doc),
    ("video", FileKind::Video),
    ("videos", FileKind::Video),
    ("clip", FileKind::Video),
    ("movie", FileKind::Video),
    ("audio", FileKind::Audio),
    ("song", FileKind::Audio),
    ("podcast", FileKind::Audio),
    ("voice", FileKind::Audio),
    ("sound", FileKind::Audio),
    ("স্ক্রিনশট", FileKind::Screenshot),
    ("ছবি", FileKind::Image),
    ("পিডিএফ", FileKind::Pdf),
    ("নোট", FileKind::Doc),
    ("ভিডিও", FileKind::Video),
    ("অডিও", FileKind::Audio),
    ("গান", FileKind::Audio),
    ("রেকর্ডিং", FileKind::Audio),
    ("لقطة", FileKind::Screenshot),
    ("صورة", FileKind::Image),
    ("صور", FileKind::Image),
    ("مستند", FileKind::Doc),
    ("ملاحظات", FileKind::Doc),
    ("فيديو", FileKind::Video),
    ("مقطع", FileKind::Video),
    ("صوت", FileKind::Audio),
    ("تسجيل", FileKind::Audio),
    ("اغنية", FileKind::Audio),
];

/// The content words of a query, normalized as keyword search keeps its text
/// (Searcher.swift:398): split on non-alphanumerics, words of two scalars or more count,
/// kind words and stopwords drop out — they describe what to look for, not what's in it.
/// `names: true` (a "name:" search) keeps the stopwords: that list is a prose concept and
/// a name search's words are name fragments ("Invoices" starts with "in"). First
/// occurrence order, deduplicated, as the engine's `seen.insert(…).inserted` filter.
pub fn terms(text: &str, names: bool) -> Vec<String> {
    let folded = simple_lowercase(&normalized(text));
    let stops = stopwords();
    let mut seen: HashSet<String> = HashSet::new();
    folded
        .split(|c: char| !is_alphanumeric_scalar(c))
        .filter(|word| word.chars().count() >= 2 && (names || !stops.contains(*word)))
        .filter(|word| seen.insert((*word).to_string()))
        .map(String::from)
        .collect()
}

/// FTS5 patterns for the query's words (Searcher.swift:410): quoted, and as prefixes for
/// the last word while it's being typed and for words of languages that attach endings.
pub fn patterns(terms: &[String], prefix_last: bool) -> Vec<String> {
    terms
        .iter()
        .enumerate()
        .map(|(index, term)| {
            let prefix = (prefix_last && index == terms.len() - 1) || inflects(term);
            format!("\"{term}\"{}", if prefix { "*" } else { "" })
        })
        .collect()
}

/// The query's words as an FTS5 OR-query (Searcher.swift:425); nil when the query has no
/// words left after the stopword drop.
pub fn fts_query(text: &str, prefix_last: bool) -> Option<String> {
    let words = terms(text, false);
    if words.is_empty() {
        return None;
    }
    Some(patterns(&words, prefix_last).join(" OR "))
}

/// Kinds the query asks for ("…in a video", "screenshot of…", "ভিডিওতে"): a soft boost,
/// not a filter (Searcher.swift:443). Bengali attaches case endings, so a Bengali kind
/// word counts at the start of a longer word too (`inflects` keys of three scalars up).
/// `hasPrefix` is Swift's Character-wise test: "স্ক্রিনশটের" does NOT start with
/// "স্ক্রিনশট" — the words' graphemes split differently at the joined vowel ("্রি" is one
/// Character where the key has "্র" + "ি"), where a byte prefix would match. Measured in
/// ⑩a's cross-check (scenario `k124`).
pub fn kind_hints(text: &str) -> BTreeSet<FileKind> {
    let folded = simple_lowercase(&normalized(text));
    folded
        .split(|c: char| !is_alphanumeric_scalar(c))
        .filter_map(|word| {
            KIND_WORDS
                .iter()
                .find(|(key, _)| *key == word)
                .or_else(|| {
                    KIND_WORDS.iter().find(|(key, _)| {
                        inflects(key) && key.chars().count() >= 3 && grapheme_starts_with(word, key)
                    })
                })
                .map(|&(_, kind)| kind)
        })
        .collect()
}

fn grapheme_starts_with(word: &str, key: &str) -> bool {
    let mut word_g = word.graphemes(true);
    key.graphemes(true).all(|g| word_g.next() == Some(g))
}

/// Codes and IDs ("K7Q2LM", "TRX50") and long numbers ("4815162342") mean nothing to an
/// embedding but everything to keyword search (Searcher.swift:285): a query made of them
/// weighs words double and meaning half. Each term must carry a number, and letters or
/// four characters. `Character.isNumber` and `Character.isLetter` (measured in ⑩a's
/// cross-check, scenario `facts-grapheme`) test the **first scalar** of the Character:
/// isNumber is Nd∪Nl∪No, isLetter is the Letters, the Spacing and Enclosing Marks and the
/// Letter Numbers — plain digits, `²` and combining marks are NOT letters, so "123" is
/// not a lookup ("4815162342" is, by the four-character rule). Counts are graphemes.
pub fn is_lookup(terms: &[String]) -> bool {
    !terms.is_empty()
        && terms.iter().all(|term| {
            let firsts: Vec<char> = term
                .graphemes(true)
                .filter_map(|g| g.chars().next())
                .collect();
            let has_number = firsts.iter().any(|c| is_number_category(*c));
            if !has_number {
                return false;
            }
            let has_letter = firsts.iter().any(|&c| is_letter_character(c));
            has_letter || term.graphemes(true).count() >= 4
        })
}
