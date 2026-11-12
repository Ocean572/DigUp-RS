//! ⑩a+⑩b pinned suite: the query-side statics — `SearchText.matches`/`snippet`, the
//! `Folded` origins, `Searcher.terms`/`patterns`/`ftsQuery`/`kindHints`/`isLookup`,
//! `NameQuery.text` and the `CodeLookup` family (`identifier`/`phrase`/`definesSomething`/
//! `definition`/`firstLine`) — replayed against the Swift engine's own answers
//! (`fixtures/search-expected.jsonl`), measured host-to-host on the ⑩a cross-check probe
//! (`/tmp/opencode/searchcheck`, the same swift-6.4.0 toolchain the container runs) over
//! `fixtures/search-scenarios.jsonl` (313 scenarios: Bengali, Arabic, Greek, Turkish,
//! ligatures, ZWJ-joined graphemes, variation selectors, combining marks, the 500-match
//! cap, codes and IDs, NBSP/ideographic/thin spaces, line controls, and the code shapes
//! `CodeLookup` reads: C/Go/ObjC/Python/Swift definition lines, comments, camel words).
//!
//! The `facts-*` scenarios pin the Foundation semantics the port's predicates model —
//! measured on corelibs 6.4 (Linux), they are the engine's ground truth for:
//! `CharacterSet.alphanumerics` = Letters ∪ Marks ∪ Numbers (underscore and ZWJ NOT in —
//! a code name splits at `_`), `CharacterSet.letters` = Letters ∪ Marks (Nd/Nl/No NOT),
//! `Character.isLetter`/`isNumber`/`isWhitespace` = FIRST-scalar tests on L*∪Mc∪Me∪Nl /
//! Nd∪Nl∪No / White_Space, `lowercased()` context-free (no Greek final sigma: "ΣΣ"→"σσ"),
//! `hasPrefix` Character-wise ("স্ক্রিনশটের" does not start with "স্ক্রিনশট"),
//! `trimmingCharacters` dropping SCALAR runs (a leading combining mark survives:
//! "  ́x"→"́x"), and `.whitespacesAndNewlines` = the Zs separators + tab + the line
//! controls (NBSP in, ZWSP not).
//!
//! The ⑩b scenarios pin the code lookup and its search's unit semantics: swift-corelibs
//! `String.range(of:)` (plain and `.caseInsensitive`) searches UTF-16 SCALAR windows and
//! bridges a hit to Character (grapheme) boundaries moving UP to the end of the
//! containing grapheme — measured on "কুমির" in "কুমিরের" (whose graphemes split
//! ক|ু|ম|ি|রে): the scalar window matches where a grapheme window would not.
//!
//! Numbers compare by value (Swift's JSONSerialization writes whole Doubles as integers,
//! serde_json keeps `.0` — every number reads as an f64 before comparing, the equality
//! the measured gate used). The gate is 0 differing lines out of 313.

use std::collections::BTreeSet;

use digup_core::code_lookup;
use digup_core::model::{FileKind, NameQuery};
use digup_core::search_text::{self as st, is_alphanumeric_scalar, is_letter_character};
use digup_core::searcher;
use serde_json::{json, Value};
use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect()
}

/// Swift's `CharacterSet.letters`: the Letters plus the combining Marks (facts-sets).
fn is_foundation_letter(c: char) -> bool {
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

/// Swift's `Character.isNumber` set and `CharacterSet.whitespaces` (Zs ∪ tab).
fn is_number_category(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
    )
}

fn is_ws_set(c: char) -> bool {
    c == '\t' || get_general_category(c) == GeneralCategory::SpaceSeparator
}

fn is_wsnl(c: char) -> bool {
    is_ws_set(c)
        || matches!(
            c,
            '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
}

fn utf16_of(text: &str, byte: usize) -> usize {
    text[..byte].chars().map(char::len_utf16).sum()
}

/// NSRE `\s+` → " " replaced over the whole string: White_Space runs collapse to one
/// space, leading and trailing runs kept (no trimming — facts-re).
fn replace_runs(text: &str) -> String {
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

/// Swift's `trimmingCharacters(in:)`: the leading and trailing runs of SCALARS in the
/// set drop — graphemes do not move as units (facts-trim: "  ́x"→"́x").
fn trim_scalars(text: &str, in_set: fn(char) -> bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut j = chars.len();
    while i < j && in_set(chars[i]) {
        i += 1;
    }
    while j > i && in_set(chars[j - 1]) {
        j -= 1;
    }
    chars[i..j].iter().collect()
}

fn string_of(sc: &Value, key: &str) -> String {
    sc[key].as_str().expect("a string field").to_string()
}

fn strings_of(sc: &Value, key: &str) -> Vec<String> {
    sc[key]
        .as_array()
        .expect("an array field")
        .iter()
        .map(|v| v.as_str().expect("a string").to_string())
        .collect()
}

fn answer(sc: &Value) -> Value {
    match sc["fn"].as_str().expect("an fn") {
        "matches" => {
            let (word, text) = (string_of(sc, "word"), string_of(sc, "text"));
            json!({"ranges": st::matches(&word, &text)
                .into_iter()
                .map(|(s, e)| json!({"s": utf16_of(&text, s), "e": utf16_of(&text, e)}))
                .collect::<Vec<Value>>()})
        }
        "snippet" => {
            let text = string_of(sc, "text");
            let terms = strings_of(sc, "terms");
            let words = sc.get("words").and_then(Value::as_u64).unwrap_or(12) as usize;
            json!({"out": st::snippet(&text, &terms, words)})
        }
        "terms" => json!({"terms": searcher::terms(
            &string_of(sc, "text"),
            sc.get("names").and_then(Value::as_bool).unwrap_or(false),
        )}),
        "ftsQuery" => json!({"out": searcher::fts_query(
            &string_of(sc, "text"),
            sc.get("prefixLast").and_then(Value::as_bool).unwrap_or(false),
        )}),
        "kindHints" => {
            let kinds: BTreeSet<FileKind> = searcher::kind_hints(&string_of(sc, "text"));
            let mut names: Vec<&str> = kinds.iter().map(FileKind::as_str).collect();
            names.sort_unstable();
            json!({"kinds": names})
        }
        "isLookup" => json!({"out": searcher::is_lookup(&strings_of(sc, "terms"))}),
        "nameQuery" => json!({"out": NameQuery::text_of(&string_of(sc, "query"))}),
        "identifier" => json!({"words": code_lookup::identifier(&string_of(sc, "query"))}),
        "phrase" => json!({"out": code_lookup::phrase(&strings_of(sc, "words"))}),
        "definesSomething" => {
            json!({"out": code_lookup::defines_something(&string_of(sc, "word"))})
        }
        "definition" => match code_lookup::definition(
            &string_of(sc, "name"),
            &string_of(sc, "text"),
            sc["first"].as_i64().expect("a first line"),
        ) {
            Some((line, strength)) => json!({"out": {"line": line, "strength": strength}}),
            None => json!({"out": null}),
        },
        "firstLine" => json!({
            "out": code_lookup::first_line(
                &strings_of(sc, "words"),
                &string_of(sc, "text"),
                sc["first"].as_i64().expect("a first line"),
            ),
        }),
        "facts-sets" => {
            let chars: Vec<char> = sc["scalars"]
                .as_array()
                .expect("scalars")
                .iter()
                .map(|v| {
                    char::from_u32(v.as_u64().expect("a scalar") as u32).expect("a scalar char")
                })
                .collect();
            json!({"scalars": sc["scalars"], "sets": {
                "alphanumerics": chars.iter().map(|c| is_alphanumeric_scalar(*c)).collect::<Vec<bool>>(),
                "letters": chars.iter().map(|c| is_foundation_letter(*c)).collect::<Vec<bool>>(),
                "whitespaces": chars.iter().map(|c| is_ws_set(*c)).collect::<Vec<bool>>(),
                "whitespacesAndNewlines": chars.iter().map(|c| is_wsnl(*c)).collect::<Vec<bool>>(),
            }})
        }
        "facts-lower" => {
            let lowered = st::simple_lowercase(&string_of(sc, "s"));
            json!({"lower": lowered,
                   "scalars": lowered.chars().map(|c| c as u32).collect::<Vec<u32>>()})
        }
        "facts-grapheme" => {
            let s = string_of(sc, "s");
            let firsts: Vec<char> = s.graphemes(true).filter_map(|g| g.chars().next()).collect();
            json!({
                "ws": firsts.iter().map(|c| c.is_whitespace()).collect::<Vec<bool>>(),
                "number": firsts.iter().map(|c| is_number_category(*c)).collect::<Vec<bool>>(),
                "letter": firsts.iter().map(|c| is_letter_character(*c)).collect::<Vec<bool>>(),
                "count": s.graphemes(true).count(),
                "graphemes": s.graphemes(true)
                    .map(|g| g.chars().map(|c| c as u32).collect::<Vec<u32>>())
                    .collect::<Vec<Vec<u32>>>(),
            })
        }
        "facts-re" => json!({"out": replace_runs(&string_of(sc, "s"))}),
        "facts-trim" => {
            let s = string_of(sc, "s");
            json!({"wsnl": trim_scalars(&s, is_wsnl), "ws": trim_scalars(&s, is_ws_set)})
        }
        other => panic!("unknown fn {other}"),
    }
}

/// Numbers compare by value: Swift writes whole Doubles as integers, serde_json keeps
/// `.0` (the gate's normalization, as `index.rs` uses).
fn canon(v: &Value) -> Value {
    match v {
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            Value::Number(serde_json::Number::from_f64(f).expect("a finite number"))
        }
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, x)| (k.clone(), canon(x))).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canon).collect()),
        other => other.clone(),
    }
}

#[test]
fn search_statics_match_the_engine() {
    let scenarios = fixture("search-scenarios.jsonl");
    let expected = fixture("search-expected.jsonl");
    assert_eq!(
        scenarios.len(),
        expected.len(),
        "scenario and expected files must line up"
    );
    let mut fails = 0;
    for (sc, want) in scenarios.iter().zip(&expected) {
        let id = sc["id"].as_str().expect("an id").to_string();
        assert_eq!(id, want["id"].as_str().expect("an expected id"));
        let mut envelope = serde_json::Map::new();
        envelope.insert("id".into(), json!(id));
        envelope.insert("fn".into(), sc["fn"].clone());
        envelope.insert("ok".into(), json!(true));
        if let Value::Object(fields) = answer(sc) {
            for (key, value) in fields {
                envelope.insert(key, value);
            }
        }
        let (got, want) = (canon(&Value::Object(envelope)), canon(want));
        if got != want {
            fails += 1;
            let got_map = got.as_object().expect("a computed object");
            for (key, w) in want.as_object().expect("an object") {
                let g = got_map.get(key).unwrap_or(&Value::Null);
                if g != w {
                    eprintln!("{id} {}: [{key}] swift={w} rust={g}", sc["fn"]);
                }
            }
        }
    }
    assert_eq!(
        fails,
        0,
        "{} of {} scenarios differ",
        fails,
        scenarios.len()
    );
}
