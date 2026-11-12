//! Keyword-search text normalization (ARCHITECTURE_MAP.md §2), ported from
//! `Sources/DigUpKit/SearchText.swift`. These run in the v1→2 (rebuild `fts` from stored text)
//! and v4→5 (drop digit-only vectors) migrations, so an index that predates v6 migrates the same
//! way the Swift engine would.

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

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

/// True for words of scripts that attach endings to words (Bengali, Hindi, Arabic): keyword
/// search matches them as prefixes, so "কুমির" finds "কুমিরও" and "مدرس" finds "مدرسة"
/// (SearchText.swift:29).
pub fn inflects(word: &str) -> bool {
    word.chars()
        .any(|c| matches!(c as u32, 0x0900..=0x0DFF | 0x0600..=0x06FF))
}

fn is_mark(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

/// Foundation's `CharacterSet.alphanumerics` membership — the set `isWordCharacter` (and
/// the term split in `Searcher.terms`) runs on. Measured on the ⑩a cross-check probe
/// (`/tmp/opencode/searchcheck`, scenario `facts-sets`): it is the Letters (L*), the
/// combining Marks (Mn Mc Me) and the Numbers (Nd Nl No) — the Mc of a Bengali matra and
/// the Mn of a generic combining accent count, `²` counts (No), the underscore does NOT
/// (a code name splits at `_`), and ZWJ (Cf) between letters does NOT.
pub fn is_alphanumeric_scalar(c: char) -> bool {
    is_letter_mark_number(c)
}

pub(crate) fn is_letter_mark_number(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
    )
}

/// Foundation's `CharacterSet.letters` membership (`SearchText.hasWords` counts letters):
/// the Letters and the combining Marks — the Numbers are NOT letters (measured, ⑩a
/// `facts-sets`).
pub fn is_letter_mark(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

/// Swift's `Character.isNumber` on a Character's **first scalar** (measured, ⑩a
/// `facts-grapheme`): Nd ∪ Nl ∪ No.
pub fn is_number_character(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
    )
}

/// Swift's `Character.isLetter` on a Character's **first scalar** (measured, ⑩a
/// `facts-grapheme`): the Letters, the Spacing and Enclosing Marks (a Bengali matra is a
/// letter) and the Letter Numbers (`Ⅷ`) — NOT the Nd digits, the No symbols, NOT the Mn
/// combining marks.
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

/// Swift's `Character.isUppercase` / `isLowercase` on the first scalar.
pub fn is_uppercase_character(c: char) -> bool {
    get_general_category(c) == GeneralCategory::UppercaseLetter
}

pub fn is_lowercase_character(c: char) -> bool {
    get_general_category(c) == GeneralCategory::LowercaseLetter
}

/// Foundation's `CharacterSet.whitespaces` (measured, ⑩a): the space separators (Zs —
/// NBSP and the thin/ideographic spaces are in) plus the tab. Newlines are NOT in it.
pub fn is_whitespace_set(c: char) -> bool {
    c == '\t' || get_general_category(c) == GeneralCategory::SpaceSeparator
}

/// Foundation's `CharacterSet.whitespacesAndNewlines`: `.whitespaces` plus the line
/// controls (LF, VT, FF, CR, NEL, LS, PS) (measured, ⑩a: NBSP IS in, ZWSP is NOT).
pub fn is_whitespace_newline_set(c: char) -> bool {
    is_whitespace_set(c)
        || matches!(
            c,
            '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
}

/// Swift's `trimmingCharacters(in:)` (swift-corelibs): the leading and trailing runs of
/// SCALARS the set contains drop — graphemes do not move as units, a leading combining
/// mark survives the space that preceded it (measured, ⑩a `facts-trim`).
fn trim_by(text: &str, in_set: fn(char) -> bool) -> &str {
    let start = text
        .char_indices()
        .find(|(_, c)| !in_set(*c))
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let end = text
        .char_indices()
        .rev()
        .find(|(_, c)| !in_set(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(start);
    &text[start..end]
}

/// `trimmingCharacters(in: .whitespaces)`.
pub fn trim_whitespaces(text: &str) -> &str {
    trim_by(text, is_whitespace_set)
}

/// `trimmingCharacters(in: .whitespacesAndNewlines)`.
pub fn trim_whitespaces_newlines(text: &str) -> &str {
    trim_by(text, is_whitespace_newline_set)
}

/// `String.lowercased()` as Foundation spells it: the per-character lowercase mappings,
/// context-free — no Greek final-sigma rule (measured: `"ΣΣ".lowercased() == "σσ"`, where
/// Rust's `str::to_lowercase` writes `"σς"`). The full mappings ride (ß, ﬁ→fi, İ→i + U+0307).
pub fn simple_lowercase(text: &str) -> String {
    text.chars().flat_map(|c| c.to_lowercase()).collect()
}

/// `text` normalized one Character at a time, remembering where each piece came from
/// (SearchText.swift:125). Swift's Character is an extended grapheme cluster — a Bengali
/// conjunct ("ক্ষ", one Character), a letter joined to its neighbor by ZWJ, an emoji with
/// its variation selector — and the fold, the origins and every word-boundary test run on
/// those, so the fold iterates graphemes too.
pub struct Folded {
    /// For each UTF-16 unit of the folded text, the byte range of the source Character
    /// that produced it (Swift's NSString search runs on UTF-16 units).
    origin: Vec<(usize, usize)>,
    /// The base units of a diacritic-insensitive scan: canonical decomposition, marks
    /// dropped (what Foundation's compare ignores), each with the raw folded-unit index it
    /// stands for. The scan consumes base units in order and maps a hit back through the
    /// raw indices, so the marks between bases ride in the returned range — a match never
    /// splits a letter from its vowel sign.
    bases: Vec<(u16, usize)>,
    graphemes: Vec<(usize, usize)>,
    source: String,
}

impl Folded {
    pub fn new(source: &str) -> Self {
        let graphemes: Vec<(usize, usize)> = source
            .grapheme_indices(true)
            .map(|(at, g)| (at, at + g.len()))
            .collect();
        let mut origin: Vec<(usize, usize)> = Vec::with_capacity(source.len());
        let mut bases: Vec<(u16, usize)> = Vec::with_capacity(source.len());
        for &(start, end) in &graphemes {
            let piece = simple_lowercase(&normalized(&source[start..end]));
            for _ in piece.encode_utf16() {
                origin.push((start, end));
            }
            for base in piece.chars().flat_map(|c| c.nfd()).filter(|c| !is_mark(*c)) {
                let mut buf = [0u16; 2];
                for unit in base.encode_utf16(&mut buf) {
                    bases.push((*unit, origin.len() - 1));
                }
            }
        }
        Self {
            origin,
            bases,
            graphemes,
            source: source.to_string(),
        }
    }

    /// The source Characters the raw-unit range came from, as byte offsets, ending after
    /// the full Character of the last unit (Swift's `source.index(after: last)`). An empty
    /// range is nil, as `Folded.original` returns nil.
    pub fn original(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if end <= start || end > self.origin.len() {
            return None;
        }
        Some((self.origin[start].0, self.origin[end - 1].1))
    }

    /// The Character starting at the grapheme boundary `at` (Swift's `text[at]`).
    pub fn grapheme_at(&self, at: usize) -> Option<&str> {
        let i = self
            .graphemes
            .binary_search_by_key(&at, |(start, _)| *start)
            .ok()?;
        let (start, end) = self.graphemes[i];
        Some(&self.source[start..end])
    }

    /// The Character ending at the grapheme boundary `at` (Swift's
    /// `text[text.index(before: at)]`).
    pub fn grapheme_before(&self, at: usize) -> Option<&str> {
        let i = self
            .graphemes
            .binary_search_by_key(&at, |(start, _)| *start)
            .ok()?;
        let (start, end) = *self.graphemes.get(i.checked_sub(1)?)?;
        Some(&self.source[start..end])
    }
}

fn is_word_character(grapheme: &str) -> bool {
    // SearchText.swift:117 — *any* scalar of the Character in CharacterSet.alphanumerics.
    grapheme.chars().any(is_alphanumeric_scalar)
}

/// Where `word` is in `text` as keyword search would find it (normalized, ignoring case and
/// accents), as byte ranges of the text as it's written: whole words, and for a word of a
/// language that attaches endings, the whole word it starts (SearchText.swift:62). The 500-
/// match cap of the engine's scan is kept; ranges are returned in scan order.
pub fn matches(word: &str, text: &str) -> Vec<(usize, usize)> {
    let needle = simple_lowercase(&normalized(word));
    if needle.is_empty() {
        return Vec::new();
    }
    let folded = Folded::new(text);
    let prefix = inflects(word);
    let needle_units: Vec<u16> = needle
        .chars()
        .flat_map(|c| c.nfd())
        .filter(|c| !is_mark(*c))
        .flat_map(|c| c.to_string().encode_utf16().collect::<Vec<u16>>())
        .collect();
    let mut found: Vec<(usize, usize)> = Vec::new();
    if needle_units.is_empty() {
        return found; // Swift's scan hits the empty-needle guard and appends nothing (500 times)
    }
    let bases = folded.bases.as_slice();
    let mut base = 0usize;
    while found.len() < 500 && base + needle_units.len() <= bases.len() {
        if bases[base..base + needle_units.len()]
            .iter()
            .map(|(unit, _)| *unit)
            .eq(needle_units.iter().copied())
        {
            let raw_start = bases[base].1;
            let raw_end = bases[base + needle_units.len() - 1].1 + 1;
            base += needle_units.len();
            let Some(range) = folded.original(raw_start, raw_end) else {
                continue;
            };
            let (start, mut end) = range;
            if start > 0 && folded.grapheme_before(start).is_some_and(is_word_character) {
                continue;
            }
            if end < text.len() && folded.grapheme_at(end).is_some_and(is_word_character) {
                if !prefix {
                    continue;
                }
                // A word of a language that attaches endings marks all of the longer
                // word it starts.
                let mut tail = end;
                while tail < text.len() {
                    match folded.grapheme_at(tail) {
                        Some(g) if is_word_character(g) => tail += g.len(),
                        _ => break,
                    }
                }
                end = tail;
            }
            if let Some(last) = found.last() {
                if last.1 > start {
                    continue; // Range::overlaps
                }
            }
            found.push((start, end));
        } else {
            base += 1;
        }
    }
    found
}

/// One line from `text` around the first of `terms` it has, the terms marked « » as SQLite's
/// snippet() marks them: the excerpt of a keyword match, from the text as it's written
/// (SearchText.swift:92). None when none of the terms is in it.
pub fn snippet(text: &str, terms: &[String], words: usize) -> Option<String> {
    let mut marks: Vec<(usize, usize)> =
        terms.iter().flat_map(|term| matches(term, text)).collect();
    marks.sort_by_key(|mark| mark.0); // stable, as Swift's `sorted { lowerBound < … }`
    let first = *marks.first()?;
    let folded = Folded::new(text);
    let grapheme_len_at = |index: usize| folded.grapheme_at(index).map(str::len).unwrap_or(0);
    // Swift's `text[i].isWhitespace` is a Character test on the *first* scalar (measured:
    // " + U+0301" is whitespace, NBSP is whitespace, ZWSP is not).
    let is_ws_at = |index: usize| {
        folded
            .grapheme_at(index)
            .and_then(|g| g.chars().next())
            .is_some_and(char::is_whitespace)
    };
    let prev_boundary = |index: usize| -> usize {
        folded
            .grapheme_before(index)
            .map(|g| index - g.len())
            .unwrap_or(0)
    };
    // About `words` words, starting a few before the first match.
    let mut start = first.0;
    let mut before = 0usize;
    while start > 0 && before < words / 3 {
        start = prev_boundary(start);
        if is_ws_at(start) && start < first.0 && !is_ws_at(start + grapheme_len_at(start)) {
            before += 1;
        }
    }
    if start > 0 || is_ws_at(start) {
        start += grapheme_len_at(start);
    }
    let mut end = first.1;
    let mut after = 0usize;
    while end < text.len() && after < words - before {
        if is_ws_at(end) {
            after += 1;
        }
        end += grapheme_len_at(end);
    }
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let mut cursor = start;
    for mark in &marks {
        if mark.0 >= start && mark.1 <= end && mark.0 >= cursor {
            out.push_str(&text[cursor..mark.0]);
            out.push('«');
            out.push_str(&text[mark.0..mark.1]);
            out.push('»');
            cursor = mark.1;
        }
    }
    out.push_str(&text[cursor..end]);
    if end < text.len() {
        out.push('…');
    }
    // `\s+` → " ", then trim `.whitespaces` (space and tab): NSRegularExpression's `\s`
    // runs on the White_Space property — what `char::is_whitespace` reports.
    let mut collapsed = String::with_capacity(out.len());
    let mut in_space = false;
    for c in out.chars() {
        if c.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        in_space = false;
        collapsed.push(c);
    }
    let collapsed = collapsed.trim_matches(|c: char| c == ' ' || c == '\t');
    Some(collapsed.to_string())
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
