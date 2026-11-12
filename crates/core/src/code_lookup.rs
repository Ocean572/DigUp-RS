//! Code name lookup (the `CodeLookup` enum at CodeExtractor.swift:252): the words keyword
//! search keeps from a code name, the phrase pattern that binds them, where a name is
//! *defined* in a stretch of code and how surely, and the line to open at for the query's
//! words. `Searcher.codePlace` and `Searcher.search` drive it; a query like
//! "quantize_row_q8_0_ref" becomes one FTS phrase ("quantize row q8 0 ref") and the
//! stretch that defines it, so a code search lands on the definition, not on the calls.
//!
//! Foundation semantics are the ⑩a measured ones (the cross-check at
//! `/tmp/opencode/searchcheck`): Character = grapheme cluster, its `isLetter`/`isNumber`/
//! `isUppercase`/`isLowercase` tests inspect the FIRST scalar, `trimmingCharacters` drops
//! scalar runs, `lowercased()` is context-free, splits with `whereSeparator` walk
//! graphemes, `components(separatedBy:)` splits on scalars.

use std::sync::OnceLock;

use std::collections::HashSet;
use unicode_segmentation::UnicodeSegmentation;

use crate::search_text::{
    is_alphanumeric_scalar, is_letter_character, is_lowercase_character, is_number_character,
    is_uppercase_character, simple_lowercase, trim_whitespaces, trim_whitespaces_newlines,
};

/// Words that mean a definition ("def", "func", "class", "#define"…). The 4 plain
/// binding words (let/var/const/val) are in the set for the strength test's sake and
/// excluded from `defines_something`.
const DEFINERS: [&str; 33] = [
    "def",
    "func",
    "fn",
    "fun",
    "function",
    "class",
    "struct",
    "enum",
    "protocol",
    "interface",
    "trait",
    "type",
    "typealias",
    "actor",
    "extension",
    "impl",
    "module",
    "namespace",
    "object",
    "record",
    "macro",
    "const",
    "let",
    "var",
    "val",
    "union",
    "mod",
    "sub",
    "@interface",
    "@implementation",
    "@protocol",
    "#define",
    "function*",
];

/// Words before a name that mean it's being used, not defined ("await name(…)").
const USERS: [&str; 28] = [
    "return", "await", "yield", "throw", "new", "else", "case", "print", "echo", "assert", "not",
    "and", "or", "in", "is", "do", "go", "defer", "try", "catch", "if", "while", "for", "typeof",
    "delete", "raise", "import", "from",
];

/// How comments and docstrings start.
const COMMENT_STARTS: [&str; 8] = ["//", "#", "/*", "*", "\"\"\"", "'''", "<!--", ";"];

fn definers() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| DEFINERS.iter().copied().collect())
}

fn users() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| USERS.iter().copied().collect())
}

fn name_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:(?:\.|::|->|#)[A-Za-z_$][A-Za-z0-9_$]*)*$")
            .expect("the engine's identifier regex")
    })
}

/// The first scalar of a Character — what Swift's property tests inspect (⑩a facts).
fn first_scalar(grapheme: &str) -> Option<char> {
    grapheme.chars().next()
}

/// The words keyword search keeps from a code name ("quantize_row_q8_0_ref" → quantize
/// row q8 0 ref, as one phrase), when the query is one: snake_case, camelCase, dotted,
/// or with digits (`identifier`, CodeExtractor.swift:256). Nil for words anyone would
/// say ("retry", "Renamer"). The regex is the engine's NSRE full match over the trimmed
/// name (the wsnl trim leaves no trailing line break for `$`'s ICU subtlety).
pub fn identifier(query: &str) -> Option<Vec<String>> {
    let mut name = trim_whitespaces_newlines(query).to_string();
    if name.ends_with("()") {
        name.truncate(name.len() - 2); // Swift's removeLast(2) of the "()" suffix
    }
    if !name_regex().is_match(&name) {
        return None;
    }
    let camel = name
        .graphemes(true)
        .skip(1)
        .filter_map(first_scalar)
        .any(is_uppercase_character)
        && name
            .graphemes(true)
            .filter_map(first_scalar)
            .any(is_lowercase_character);
    let marks_a_name = name
        .graphemes(true)
        .any(|g| "_.:#$".contains(g) || first_scalar(g).is_some_and(is_number_character));
    if !camel && !marks_a_name {
        return None;
    }
    Some(
        simple_lowercase(&name)
            .split(|c: char| !is_alphanumeric_scalar(c))
            .filter(|word| !word.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// The keyword search pattern for an identifier's words: one phrase, so they match only
/// side by side, as written.
pub fn phrase(words: &[String]) -> String {
    format!("\"{}\"", words.join(" "))
}

/// A word that starts a definition, minus the plain bindings (let/var/const/val —
/// "let name = …" reads as a use).
pub fn defines_something(word: &str) -> bool {
    definers().contains(word) && word != "let" && word != "var" && word != "const" && word != "val"
}

/// Where `name` is defined in `text` (a stretch whose first line is `first`): the line,
/// and how surely — 2 after a word that defines something or a C-style definition, 1 for
/// a declaration ending in ";" (CodeExtractor.swift:275). The first strength-2 sighting
/// wins immediately; otherwise the strongest. Nil when the stretch only uses it.
pub fn definition(name: &str, text: &str, first: i64) -> Option<(i64, i32)> {
    let mut best: Option<(i64, i32)> = None;
    for (offset, (start, end)) in lines(text).iter().enumerate() {
        let line = &text[*start..*end];
        for (rb, re) in occurrences(name, line) {
            let before = trim_whitespaces(&line[..rb]);
            let after = trim_whitespaces(&line[re..]);
            let strength = definition_strength(before, after, line);
            if strength > best.map(|b| b.1).unwrap_or(0) {
                best = Some((first + offset as i64, strength));
            }
            if strength == 2 {
                return best;
            }
        }
    }
    best
}

fn definition_strength(before: &str, after: &str, line: &str) -> i32 {
    let previous = split_whitespace_ws(before).last().unwrap_or("");
    if definers().contains(previous) {
        return 2;
    }
    if before.starts_with("func (") && before.ends_with(")") {
        return 2; // a Go method
    }
    if before.starts_with("- (") || before.starts_with("+ (") {
        // an Objective-C method
        return if after.starts_with(":")
            || after.is_empty()
            || after.starts_with("{")
            || after.starts_with(";")
        {
            2
        } else {
            0
        };
    }
    // C-style: "static void name(…) {", "struct gguf_context * name(…)", not a call or
    // an assignment.
    let looks_like_a_definition = after.starts_with("(")
        && !before.is_empty()
        && !users().contains(previous)
        && !before.chars().any(|c| "=(,.!?".contains(c))
        && !before.contains("->")
        && before.graphemes(true).next_back().is_some_and(|g| {
            first_scalar(g).is_some_and(|c| is_letter_character(c) || is_number_character(c))
                || "*&>_]".contains(g)
        });
    if !looks_like_a_definition {
        return 0;
    }
    if trim_whitespaces(line).ends_with(";") {
        1
    } else {
        2
    }
}

/// The line (from `first`) to open at for `words`: the first line with one of them that
/// defines something ("def retry_with_backoff"), else the first such line of code (not a
/// comment), else the first such line. Nil when none is there (CodeExtractor.swift:322).
/// A word counts as itself or as the start of a name's part (the "retry" of
/// retryWithBackoff and of retry_with_backoff).
pub fn first_line(words: &[String], text: &str, first: i64) -> Option<i64> {
    let words: Vec<String> = words
        .iter()
        .filter(|w| w.graphemes(true).count() >= 2)
        .map(|w| simple_lowercase(w))
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut first_code: Option<usize> = None;
    let mut first_any: Option<usize> = None;
    for (offset, (start, end)) in lines(text).iter().enumerate() {
        let line = &text[*start..*end];
        if !words.iter().any(|w| has_word(w, line)) {
            continue;
        }
        let trimmed = trim_whitespaces(line);
        if split_whitespace_ws(trimmed).take(4).any(defines_something) {
            return Some(first + offset as i64);
        }
        if first_code.is_none() && !COMMENT_STARTS.iter().any(|c| trimmed.starts_with(c)) {
            first_code = Some(offset);
        }
        if first_any.is_none() {
            first_any = Some(offset);
        }
    }
    (first_code.or(first_any)).map(|offset| first + offset as i64)
}

/// `text.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" || $0 ==
/// "\r\n" })` — grapheme breaks: LF is a Character, CRLF is ONE Character (UAX #29), a
/// lone CR is not a break at all. Returns the byte range of every line, empties kept.
fn lines(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (at, grapheme) in text.grapheme_indices(true) {
        if grapheme == "\n" || grapheme == "\r\n" {
            out.push((start, at));
            start = at + grapheme.len();
        }
    }
    out.push((start, text.len()));
    out
}

/// `String.split(whereSeparator: \.isWhitespace)` — grapheme-wise, empty pieces dropped;
/// `Character.isWhitespace` inspects the first scalar.
fn split_whitespace_ws(text: &str) -> impl Iterator<Item = &str> {
    split_graphemes(text, |(_, grapheme)| {
        first_scalar(grapheme).is_some_and(char::is_whitespace)
    })
    .into_iter()
    .filter(|piece| !piece.is_empty())
}

/// Splits `text` at graphemes the predicate picks; pieces are byte slices.
fn split_graphemes(text: &str, mut is_sep: impl FnMut((usize, &str)) -> bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (at, grapheme) in text.grapheme_indices(true) {
        if is_sep((at, grapheme)) {
            out.push(&text[start..at]);
            start = at + grapheme.len();
        }
    }
    out.push(&text[start..]);
    out
}

/// Whether `line` has `word` (lowercased) as a word or as the start of a name's part
/// (`has`, CodeExtractor.swift:340). The engine's `range(of:)` (swift-corelibs) searches
/// UTF-16 **scalar windows** over the line lowercased Character-by-Character — a hit
/// that starts mid-grapheme bridges its index UP to the next Character boundary — and a
/// word counts at the line's start, after something that is neither a letter nor a
/// number, or at a capital letter (camelCase). Measured (⑩b, `f299`): "কুমির" counts in
/// "কুমিরের", whose graphemes split ক|ু|ম|ি|রে — a grapheme window would never match.
fn has_word(word: &str, line: &str) -> bool {
    let line_g: Vec<&str> = line.graphemes(true).collect();
    let lower_g: Vec<String> = line_g.iter().map(|g| simple_lowercase(g)).collect();
    let needle: Vec<char> = word.chars().collect();
    if needle.is_empty() {
        return true; // Swift's range(of: "") matches at the start — the bound checks pass
    }
    let units = UnitStream::new(lower_g.iter().map(String::as_str), identity_scalar);
    let mut i = 0usize;
    while i + needle.len() <= units.len() {
        if units.window_eq(i, &needle) {
            let lb = units.bridged_lower_bound(i);
            let clear = lb == 0
                || lower_g[lb - 1]
                    .chars()
                    .next()
                    .map(|c| !is_letter_character(c) && !is_number_character(c))
                    .unwrap_or(false)
                || lb < line_g.len()
                    && line_g[lb]
                        .chars()
                        .next()
                        .is_some_and(is_uppercase_character);
            if clear {
                return true;
            }
            i = units.after_unit(i + needle.len() - 1); // the engine continues at the bridged upperBound
        } else {
            i += 1;
        }
    }
    false
}

/// The engine's scalar-unit search stream (swift-corelibs NSString): the line as a run of
/// UTF-16 units, each folded, remembering the Character (grapheme) it belongs to. A hit
/// bridging back to String.Index lands on a Character boundary, moving UP to the end of
/// the grapheme it started inside (`bridged_lower_bound`); the scan then continues at the
/// bridged upper bound (`after_unit`).
struct UnitStream {
    units: Vec<char>,
    grapheme_of: Vec<usize>,
    /// First unit index of each grapheme, with a trailing sentinel = `units.len()`.
    unit_start: Vec<usize>,
}

fn identity_scalar(c: char) -> char {
    c
}

impl UnitStream {
    fn new<'a>(pieces: impl Iterator<Item = &'a str>, fold: fn(char) -> char) -> Self {
        let mut units: Vec<char> = Vec::new();
        let mut grapheme_of: Vec<usize> = Vec::new();
        let mut unit_start: Vec<usize> = Vec::new();
        for (gi, piece) in pieces.enumerate() {
            unit_start.push(units.len());
            for c in piece.chars() {
                units.push(fold(c));
                grapheme_of.push(gi);
            }
        }
        unit_start.push(units.len());
        Self {
            units,
            grapheme_of,
            unit_start,
        }
    }

    fn len(&self) -> usize {
        self.units.len()
    }

    fn window_eq(&self, i: usize, needle: &[char]) -> bool {
        needle
            .iter()
            .enumerate()
            .all(|(k, c)| self.units[i + k] == *c)
    }

    /// The Character index the engine sees for a hit starting at unit `i`: on a grapheme
    /// boundary it stays, mid-grapheme it moves up to the next boundary.
    fn bridged_lower_bound(&self, i: usize) -> usize {
        let g = self.grapheme_of[i];
        if i == 0 || self.grapheme_of[i - 1] != g {
            g
        } else {
            g + 1
        }
    }

    /// The first unit of the grapheme after the one holding unit `i` — the bridged upper
    /// bound the scan continues at.
    fn after_unit(&self, i: usize) -> usize {
        self.unit_start[self.grapheme_of[i] + 1]
    }
}

/// Where `name` is in `line` as a whole name (not a part of a longer one), whatever its
/// case (`occurrences`, CodeExtractor.swift:361): the engine's `.caseInsensitive` search
/// is a folded scalar-unit window (an expanding fold like ß→ss never stretches the
/// window); the neighbors of a hit — at the bridged Character boundaries — must not be
/// name characters, `isLetter || isNumber || _ || $` on the first scalar.
fn occurrences(name: &str, line: &str) -> Vec<(usize, usize)> {
    let needle: Vec<char> = name.chars().map(fold_scalar).collect();
    if needle.is_empty() {
        return Vec::new(); // the engine's scan with an empty name never terminates
    }
    let graphemes: Vec<(usize, usize)> = line
        .grapheme_indices(true)
        .map(|(at, g)| (at, at + g.len()))
        .collect();
    let strings: Vec<&str> = line.graphemes(true).collect();
    let units = UnitStream::new(line.graphemes(true), fold_scalar);
    let mut found = Vec::new();
    let mut i = 0usize;
    while i + needle.len() <= units.len() {
        if units.window_eq(i, &needle) {
            let lb = units.bridged_lower_bound(i);
            let ub = units.grapheme_of[i + needle.len() - 1] + 1;
            let clear_before = lb == 0 || !is_name_character(strings[lb - 1]);
            let clear_after = ub == strings.len() || !is_name_character(strings[ub]);
            if clear_before && clear_after {
                found.push((
                    graphemes[lb].0,
                    if ub < graphemes.len() {
                        graphemes[ub].0
                    } else {
                        line.len()
                    },
                ));
            }
            i = units.after_unit(i + needle.len() - 1);
        } else {
            i += 1;
        }
    }
    found
}

fn fold_scalar(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn is_name_character(grapheme: &str) -> bool {
    first_scalar(grapheme).is_some_and(|c| is_letter_character(c) || is_number_character(c))
        || grapheme == "_"
        || grapheme == "$"
}
