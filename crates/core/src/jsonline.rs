//! One JSON object per line, byte for byte the way the engine writes it — `emit`
//! (`Sources/digup/IndexService.swift:519`) hands Foundation's `JSONSerialization` an object with
//! `.sortedKeys` and writes the result straight to the file descriptor. The server and the shell
//! read these lines as JSON, so the details Foundation picks (the `\/` in every path, the sorted
//! keys, how a double is spelled) are the port's own burden: the parity gate diffs two engines'
//! output as bytes.
//!
//! Measured against Swift 6.4 / Foundation on this box (`/tmp/opencode/jsonfmt`):
//! * compact, no spaces; keys sorted by Foundation's localized order (`.sortedKeys`), which is
//!   case-insensitive, numeric-aware, space before punctuation before digits before letters;
//! * `/` is escaped as `\/`; the control characters use the short forms `\b \t \n \f \r` and,
//!   for the rest of U+0000…U+001F, `\u00xx` in LOWERCASE hex (measured over all 32 of them by
//!   `/tmp/opencode/doccheck/escprobe.swift`); `/`, `"`, `\`, U+007F, U+2028/9, U+FEFF and
//!   U+FFFD up from U+0020 are written raw (as UTF-8), so non-ASCII folder names stay readable;
//! * a number is the shortest decimal that reads back as the same double, in plain notation when
//!   its leading digit sits between 10⁻⁴ and 10¹⁵ (both ends included) and as `1.23e+16`
//!   otherwise, with an exponent of at least two digits; a whole double is written without a
//!   decimal point (`100`, `-10`, `0`, and `-0.0` as `-0`).

use serde_json::{Number, Value};
use std::cmp::Ordering;
use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::UnicodeNormalization;

/// One event as one line. `None` when a number is NaN or infinite: `JSONSerialization` refuses
/// those, and `emit` writes nothing (`guard let data = try?`), so the port writes nothing either.
pub fn to_line(fields: &Value) -> Option<String> {
    let mut out = String::with_capacity(160);
    write_value(fields, &mut out)?;
    Some(out)
}

fn write_value(value: &Value, out: &mut String) -> Option<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(on) => out.push_str(if *on { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number_string(number)?),
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort_by(|a, b| key_cmp(a, b));
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(&fields[*key], out)?;
            }
            out.push('}');
        }
    }
    Some(())
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            // The other control characters, in LOWERCASE hex (measured: `\u000b`, `\u001a`).
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Integers go out exactly as they came in. A double goes out the way Foundation spells it.
fn number_string(number: &Number) -> Option<String> {
    if number.is_i64() || number.is_u64() {
        return Some(number.to_string());
    }
    swift_double(number.as_f64()?)
}

/// Foundation's spelling of a double, measured on the values at the head of each rule
/// (`0.0001` plain, `1e-05` exponent, `1234567890123456` plain, `1.2345678901234568e+16`).
///
/// The digits are the shortest that read back as the same double, rounded **half to even** — the
/// 80 exact-tie cases of the tie probe match that rule and nothing else (`-1323365521330295.25`
/// → `-1323365521330295.2`, `2215125484055601.75` → `…601.8`). Rust's `{:e}` is also the shortest
/// round-trip decimal, but on a tie it spends one digit more and rounds half away
/// (`…295.3`), so the port takes Rust's digit count as a starting point, walks it down while a
/// shorter spelling still reads back, and rounds the last digit itself.
fn swift_double(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None; // JSONSerialization throws on NaN / ±∞: emit writes nothing
    }
    if value == 0.0 {
        return Some(if value.is_sign_negative() { "-0" } else { "0" }.to_string());
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let magnitude = value.abs();
    // The exact decimal expansion: Rust spells it exactly when asked for 1074 fractional digits,
    // because every double is a binary fraction. "1.25e3" → digits "125", power 3.
    let (digits, mut power) = exact_digits(magnitude)?;
    let (rust_digits, _) = scientific_digits(magnitude)?;
    let mut count = rust_digits.chars().count();
    while count > 1 {
        let (shorter, _) = round_half_to_even(&digits, count - 1);
        if reads_back_as(&shorter, power, value) {
            count -= 1;
        } else {
            break;
        }
    }
    let (rounded, carried) = round_half_to_even(&digits, count);
    if carried {
        power += 1; // 9.99… rounded up became 10…: the leading digit moved one place up
    }
    let digits = rounded;

    if power <= -5 || power >= 16 {
        let mut out = String::from(sign);
        out.push(digits.chars().next()?);
        if digits.chars().count() > 1 {
            out.push('.');
            out.push_str(&digits.chars().skip(1).collect::<String>());
        }
        out.push('e');
        out.push(if power < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", power.abs())); // Foundation pads to at least two digits
        return Some(out);
    }
    let mut out = String::from(sign);
    if power < 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat((-power - 1) as usize));
        out.push_str(&digits);
        return Some(out);
    }
    let before_point = power as usize + 1; // digits that sit left of the decimal point
    let length = digits.chars().count();
    if length <= before_point {
        out.push_str(&digits); // a whole double: no decimal point
        out.push_str(&"0".repeat(before_point - length));
    } else {
        out.push_str(&digits[..before_point]);
        out.push('.');
        out.push_str(&digits[before_point..]);
    }
    Some(out)
}

/// The exact decimal digits of a positive finite double and the power of its leading digit.
fn exact_digits(magnitude: f64) -> Option<(String, i32)> {
    let spelled = format!("{:.1074e}", magnitude); // the exact expansion, padded with zeros
    let (mantissa, exponent) = spelled.split_once('e')?;
    let power: i32 = exponent.parse().ok()?;
    let digits = mantissa.chars().filter(|ch| *ch != '.').collect::<String>();
    let digits = digits.trim_end_matches('0').to_string();
    (!digits.is_empty()).then_some((digits, power))
}

/// Rust's own shortest form (the digit count it found, and the power of its leading digit).
fn scientific_digits(magnitude: f64) -> Option<(String, i32)> {
    let spelled = format!("{:e}", magnitude);
    let (mantissa, exponent) = spelled.split_once('e')?;
    let power: i32 = exponent.parse().ok()?;
    let digits = mantissa.chars().filter(|ch| *ch != '.').collect::<String>();
    (!digits.is_empty()).then_some((digits, power))
}

/// Round the exact digit string to `count` significant digits, half to even: the digit Rust's
/// shortest form dropped decides, and on a bare 5 the kept digit goes to the even side.
fn round_half_to_even(digits: &str, count: usize) -> (String, bool) {
    let kept: String = digits.chars().take(count).collect();
    let dropped: String = digits.chars().skip(count).collect();
    let Some(first) = dropped.chars().next() else {
        return (kept, false);
    };
    let up = first > '5'
        || (first == '5' && dropped.chars().skip(1).any(|ch| ch != '0'))
        || (first == '5'
            && dropped.chars().skip(1).all(|ch| ch == '0')
            && kept
                .chars()
                .next_back()
                .unwrap_or('0')
                .to_digit(10)
                .unwrap_or(0)
                % 2
                == 1);
    if !up {
        return (kept, false);
    }
    // Add one to the kept digits; a carry past the front ("99…" → "10…") is reported.
    let mut out: Vec<char> = kept.chars().collect();
    let mut index = out.len();
    while index > 0 {
        index -= 1;
        if out[index] == '9' {
            out[index] = '0';
        } else {
            out[index] = char::from_u32(out[index] as u32 + 1).unwrap_or('0');
            let text: String = out.into_iter().collect();
            return (text.trim_end_matches('0').to_string(), false);
        }
    }
    ("1".to_string(), true)
}

/// Does this digit string, read as `d.dd…×10^power`, parse back to exactly this double?
fn reads_back_as(digits: &str, power: i32, value: f64) -> bool {
    let mut text = String::new();
    let mut chars = digits.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    text.push(first);
    let rest: String = chars.collect();
    if !rest.is_empty() {
        text.push('.');
        text.push_str(&rest);
    }
    text.push('e');
    text.push_str(&power.to_string());
    text.parse::<f64>()
        .map(|back| back == value)
        .unwrap_or(false)
}

/// `.sortedKeys`' order. Foundation compares keys with `localizedStandardCompare` (ICU): a run of
/// digits is the number it spells, letters ignore case with the lowercase form first, diacritics
/// come last, and space, punctuation, digits and letters sort in that order.
///
/// The keys the engine puts in a JSON object are its own words — kind names, the "skipped"
/// labels ("Photos library" lands after "other type" exactly because the comparison ignores
/// case), fingerprints, state names. The rules above match Foundation for that set, and for an
/// ASCII path used as a key; a key outside it (arbitrary text, a locale other than the
/// container's) sorts by these rules, which is not full ICU collation.
pub fn key_cmp(a: &str, b: &str) -> Ordering {
    let (left, right): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (mut i, mut j) = (0usize, 0usize);
    // Filled while the primaries tie: an accent decides before case does ("A" before "ä",
    // "a" before "Ä"), and case decides last ("photos" before "Photos").
    let (mut left_marks, mut right_marks): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let (mut left_cases, mut right_cases): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    while i < left.len() && j < right.len() {
        if left[i].is_ascii_digit() && right[j].is_ascii_digit() {
            let run = |chars: &Vec<char>, at: usize| {
                let mut end = at;
                while end < chars.len() && chars[end].is_ascii_digit() {
                    end += 1;
                }
                let span: String = chars[at..end].iter().collect();
                (end, span.trim_start_matches('0').to_string(), span.len())
            };
            let (next_i, left_number, left_len) = run(&left, i);
            let (next_j, right_number, right_len) = run(&right, j);
            match left_number
                .len()
                .cmp(&right_number.len())
                .then_with(|| left_number.cmp(&right_number))
                // Same number: Foundation puts the spelling with fewer leading zeros first ("10"
                // before "010", "a1" before "a01").
                .then_with(|| left_len.cmp(&right_len))
            {
                Ordering::Equal => {
                    i = next_i;
                    j = next_j;
                }
                order => return order,
            }
            continue;
        }
        let (one, two) = (left[i], right[j]);
        match class(one)
            .cmp(&class(two))
            .then_with(|| primary(one).cmp(&primary(two)))
        {
            Ordering::Equal => {
                left_marks.push(marks(one));
                right_marks.push(marks(two));
                left_cases.push(case_rank(one));
                right_cases.push(case_rank(two));
                i += 1;
                j += 1;
            }
            order => return order,
        }
    }
    // A key whose primary weights are a prefix of the other's comes first, whatever case its
    // shared letters were in ("photo" before "Photos library", "ä" before "A 1").
    match left.len().cmp(&right.len()) {
        Ordering::Equal => {}
        order => return order,
    }
    left_marks
        .cmp(&right_marks)
        .then_with(|| left_cases.cmp(&right_cases))
        .then_with(|| a.chars().cmp(b.chars()))
}

/// The secondary weight of a character: 0 for a plain letter, 1 when it carries marks ("ä", "é",
/// a combining accent). Foundation compares accents before case.
fn marks(ch: char) -> u8 {
    let has_mark = ch.nfd().skip(1).any(|mark| {
        matches!(
            get_general_category(mark),
            GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark
        )
    });
    u8::from(has_mark)
}

/// Case rank: lowercase 0, uppercase 1, anything else (a digit, punctuation, a symbol) 2.
fn case_rank(ch: char) -> u8 {
    if ch == ch.to_lowercase().next().unwrap_or(ch) && ch != ch.to_uppercase().next().unwrap_or(ch)
    {
        0
    } else if ch == ch.to_uppercase().next().unwrap_or(ch)
        && ch != ch.to_lowercase().next().unwrap_or(ch)
    {
        1
    } else {
        2
    }
}

/// The collation class of a character: space, then `_`, then the rest of the punctuation, then
/// digits, then letters, then everything else. (Foundation: `"a b" < "a_1" < "a-1" < "a.b" <
/// "a/b" < "a0b" < "ab"`.)
fn class(ch: char) -> u8 {
    match ch {
        ' ' => 0,
        '_' => 1,
        c if (c as u32) < 0x30 && c != ' ' => 2,
        c if c.is_ascii_digit() => 3,
        c if c.is_alphabetic() => 4,
        _ => 5,
    }
}

/// The primary weight of a character: case folded, with diacritics taken off ("ä" compares as "a"
/// first, and its accent only decides a tie).
fn primary(ch: char) -> String {
    let folded = ch.to_lowercase().next().unwrap_or(ch);
    let stripped: String = folded
        .nfd()
        .filter(|mark| {
            !matches!(
                get_general_category(*mark),
                GeneralCategory::NonspacingMark
                    | GeneralCategory::EnclosingMark
                    | GeneralCategory::SpacingMark
            )
        })
        .collect();
    stripped.chars().next().unwrap_or(folded).to_string()
}
