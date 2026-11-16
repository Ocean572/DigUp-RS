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

/// True when text says something in words: at least a fifth of the non-space characters are
/// letters (marks count, such as Bengali vowel signs). A table of numbers is not, so it gets
/// keyword search only (SearchText.swift:37).
///
/// The set is Foundation's `CharacterSet.letters` — the Letters AND the combining Marks
/// (`is_letter_mark`, measured on the ⑩a probe's `facts-sets`), which is what the doc
/// above says and what `hasWords` counts (`CharacterSet.letters.contains(scalar)`,
/// SearchText.swift:39). Counting L* alone made a Devanagari or harakat-heavy Arabic page
/// answer "no letters worth a vector" where the engine embeds it: `has_prose` gates the
/// chunk/page/OCR embedding (`indexer.rs:663/1078/1248`) and `migrate_long_files`, so the
/// two engines embedded different files from the same corpus. `has_prose`'s per-word test
/// is the same set (`word.unicodeScalars.contains(where: CharacterSet.letters.contains)`,
/// SearchText.swift:51).
pub fn has_words(text: &str) -> bool {
    let mut letters = 0u64;
    let mut others = 0u64;
    for scalar in text.chars() {
        if scalar == '\n' || scalar.is_whitespace() {
            continue;
        }
        if is_letter_mark(scalar) {
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
        if word.chars().any(is_letter_mark) {
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

/// Foundation's `CharacterSet.letters` membership (`SearchText.hasWords`/`hasProse` count
/// letters with it, SearchText.swift:39/51): the Letters and the combining Marks — the
/// Numbers are NOT letters (measured, ⑩a `facts-sets`).
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

/// Foundation's `CharacterSet.whitespaces`, measured on BOTH Foundation families.
/// swift-corelibs (the engine's Linux shape) answers 19 scalars — tab, space, the Zs
/// separators (NBSP, Ogham, U+2000–200A, narrow NBSP, medium mathematical, ideographic)
/// and **ZWSP U+200B** (the glibc locale's `iswspace` includes it; ZWJ U+200D is NOT in,
/// measured scalar-by-scalar over the whole range, `f9full.swift` 2026-11-14 in
/// `digup-builder:fixes3`). Apple's Foundation answers the SAME 19 scalars: tab + the Zs
/// separators + ZWSP, and NOT the line controls — measured on a Mac with the pinned Xcode's
/// `xcrun swift` (`wsprobe.swift`, 2026-11-15): `"\u{200B}a\u{200B}".trimmingCharacters(in:
/// .whitespaces)` is `"a"` (ZWSP trims) and `"a\r"` keeps its CR. So there is NO platform
/// split — the earlier "Darwin excludes ZWSP, trims the CR" note was a measurement taken on a
/// box with no macOS runner, and it made the port diverge from the Swift app on the very
/// platform it named. The set is the same on Linux, macOS and Windows; ZWSP trims everywhere.
pub fn is_whitespace_set(c: char) -> bool {
    c == '\t'
        || get_general_category(c) == GeneralCategory::SpaceSeparator
        || c == '\u{200B}'
}

/// Foundation's `CharacterSet.whitespacesAndNewlines`: `.whitespaces` (ZWSP included on
/// swift-corelibs, as above) plus the line controls — measured 26 scalars on
/// swift-corelibs: `.whitespaces`' 19 plus LF, VT, FF, CR, NEL, LS, PS (`f9full.swift`:
/// added by whitespacesAndNewlines = A B C D 85 2028 2029). The trim drops a trailing CR
/// on BOTH sets ("end\r" -> "end") — the split-lines channels (`code.rs`) ride that fact.
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

/// The pinned shape of `.whitespaces` at a line's edge (the F9 channels): a trailing CR
/// SURVIVES the trim on the Linux engine (f9full: CR is not in the set) and trims on
/// the macOS engine.
///
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

/// `replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)` — NSRE's `\s`
/// is the White_Space property: every run becomes ONE space, leading and trailing runs
/// kept, nothing is trimmed (measured, ⑩a `facts-re`: `" x "` for `" x  "`).
pub fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    if space {
        out.push(' ');
    }
    out
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

/// Swift's `Script.arabic.contains(value) && scalar.properties.isAlphabetic`
/// (SearchText.swift:198) inside the block `arabic` names. Measured 2026-11-13 with Perl's
/// Unicode 16 data (`/\p{Alphabetic}/` over every scalar of U+0600-U+06FF): the block holds
/// 52 combining marks, 46 of them alphabetic (the harakat, the dagger alef, the Quranic
/// small-high marks U+06D6-U+06DC, U+06E1-U+06E4, U+06E7-U+06E8, U+06ED), and exactly six are
/// not — U+0658, U+06DF, U+06E0, U+06EA, U+06EB, U+06EC. No alphabetic scalar in the block is
/// outside the letters-and-marks set, so the letters-and-marks set minus those six IS the
/// Alphabetic property here; the plain set would count six shapes Swift does not.
fn is_arabic_alphabetic(value: u32) -> bool {
    char::from_u32(value).is_some_and(is_letter_mark)
        && !matches!(value, 0x0658 | 0x06DF | 0x06E0 | 0x06EA..=0x06EC)
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
    /// Ported from SearchText.swift:197-199: the lam-ligature shape is `Script.arabic` AND
    /// `properties.isAlphabetic`, which inside the block the port's `arabic` gate names is
    /// `is_arabic_alphabetic` — measured against Perl's Unicode 16 data, not approximated.
    /// The check only gates which garbled PDFs are re-queued for OCR, and only on a v1→v2
    /// migration.
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
                && is_arabic_alphabetic(value)
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

#[cfg(test)]
mod tests {
    use super::{
        is_whitespace_newline_set, is_whitespace_set, trim_whitespaces, trim_whitespaces_newlines,
    };

    /// F9 — the `.whitespaces` trim set, measured scalar-by-scalar over the whole
    /// Unicode range on swift-corelibs 6.4 (`f9full.swift`, 2026-11-14, the container
    /// `digup-builder:fixes3`): 19 scalars — tab, space, the Zs separators (NBSP,
    /// Ogham, U+2000–200A, narrow NBSP, medium mathematical, ideographic) and **ZWSP
    /// U+200B**; the line controls (LF/VT/FF/CR/NEL/LS/PS) and ZWJ are NOT in it.
    /// Apple's Foundation answers the SAME 19 scalars — measured on a Mac with the pinned
    /// Xcode (`wsprobe.swift`, 2026-11-15): ZWSP trims under `.whitespaces`, CR survives.
    /// There is NO platform split for ZWSP; the earlier "Darwin WITHOUT ZWSP" note came from
    /// a box with no macOS runner and made the port diverge from the Swift app on macOS.
    /// `str::trim` (Rust's White_Space) is the opposite at both edges: it TRIMS the
    /// controls and KEEPS ZWSP. The engine's trims (query names, snippet edges, the
    /// `;` test, code summaries) ride the Foundation sets, identically on all three OSes.
    #[test]
    fn the_whitespace_set_is_the_measured_foundation_set() {
        // In on every platform: tab, space, NBSP, thin, ideographic.
        for c in ['\t', ' ', '\u{A0}', '\u{2009}', '\u{3000}'] {
            assert!(is_whitespace_set(c), "{:04X}", c as u32);
        }
        // The line controls: in `.whitespacesAndNewlines`, NOT in `.whitespaces`.
        for c in [
            '\n', '\u{0B}', '\u{0C}', '\r', '\u{85}', '\u{2028}', '\u{2029}',
        ] {
            assert!(!is_whitespace_set(c), "ws must keep {:04X}", c as u32);
            assert!(
                is_whitespace_newline_set(c),
                "wn must take {:04X}",
                c as u32
            );
        }
        // Not in either: ZWJ, BOM, NUL.
        for c in ['\u{200D}', '\u{FEFF}', '\0'] {
            assert!(!is_whitespace_set(c) && !is_whitespace_newline_set(c));
        }
        // ZWSP: in `.whitespaces` on every Foundation family (corelibs and Apple both,
        // measured on a Mac), so it is in `.whitespacesAndNewlines` too. No platform split.
        assert!(is_whitespace_set('\u{200B}'), "200B IS in, on every platform");
        assert!(
            is_whitespace_newline_set('\u{200B}'),
            "wn = ws ∪ controls ⊇ 200B"
        );
    }

    /// The trim shapes those sets produce, pinned from the same probes (the engine's
    /// channels: `.whitespaces` at snippet/lookup/summary edges).
    #[test]
    fn the_trims_answer_the_measured_shapes() {
        // A trailing CR survives `.whitespaces` and goes in `.whitespacesAndNewlines`
        // on EVERY platform (the controls are in neither/both sets).
        assert_eq!(trim_whitespaces("end\r"), "end\r");
        assert_eq!(trim_whitespaces_newlines("end\r"), "end");
        assert_eq!(trim_whitespaces("a\u{2028}"), "a\u{2028}");
        assert_eq!(trim_whitespaces_newlines("a\u{2028}"), "a");
        // NBSP and thin trim on every platform (Zs).
        assert_eq!(trim_whitespaces("x \u{A0}"), "x");
        // ZWSP trims on every platform (it is in `.whitespaces` on corelibs and Apple).
        assert_eq!(trim_whitespaces("\u{200B}end"), "end");
        assert_eq!(trim_whitespaces("end\u{200B}"), "end");
    }
}
