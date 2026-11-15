//! Rich Text Format, read for its words (Platform/Portable/Docs+Portable.swift:114): control
//! words dropped (paragraphs and lines kept as line breaks), escapes decoded, destination groups
//! (fonts, colors, styles, info, pictures) skipped. Enough for search.

/// The engine walks Unicode SCALARS (`rtf.unicodeScalars`), which in Rust is plain `char`.
/// The `\'hh` window: Swift's `UInt8(two chars, radix: 16)` over exactly the
/// TWO characters the reader slices (`chars[(i + 1)...(i + 2)]`). Measured
/// exhaustively on swift-corelibs 6.4 (2026-11-15): a Swift oracle over all
/// 1,089 windows of the window alphabet (hex, space, '+', '-', '_', tab, NBSP,
/// 'x', 'z', quote, 'g' - `f7all.swift`) against `u8::from_str_radix`
/// (`f7all.rs`): 1,088 of 1,089 windows agree - signs ("+4"->4, "+41"->65),
/// spaces (" 4"), underscores ("4_", "_4", "1_", "+_"), '-', quote, NBSP, "0x"
/// all behave identically. The one divergence is spelled out: Swift accepts
/// "-0" (a negative zero fits UInt8) where from_str_radix refuses. A window
/// needs its two characters (Swift's `i + 2 < count` gate); the CALLER moves
/// the reader - two on success (Swift's `i += 3`), one on refusal (the window
/// stays literal text).
fn hex_window(chars: &[char], at: usize) -> Option<u8> {
    if at + 2 >= chars.len() {
        return None;
    }
    let hex: String = chars[at..at + 2].iter().collect();
    u8::from_str_radix(&hex, 16)
        .ok()
        .or(if hex == "-0" { Some(0) } else { None })
}

pub fn text(rtf: &str) -> String {
    let chars: Vec<char> = rtf.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut skip_depth = 0i32; // inside a skipped group: its nesting depth
    let mut unicode_skip: i64 = 1; // \ucN: characters after \uN to skip
    let mut pending_skip = 0i64;
    let is_letter = |c: char| c.is_ascii_alphabetic();
    let is_digit = |c: char| c.is_ascii_digit();
    while i < chars.len() {
        let c = chars[i];
        if skip_depth > 0 {
            if c == '{' {
                skip_depth += 1;
            } else if c == '}' {
                skip_depth -= 1;
            } else if c == '\\' {
                i += 1;
            }
            i += 1;
            continue;
        }
        match c {
            '{' => {
                let mut j = i + 1;
                if j < chars.len() && chars[j] == '\\' {
                    j += 1;
                    if j < chars.len() && chars[j] == '*' {
                        // {\* …}: a destination readers may ignore
                        skip_depth = 1;
                        i = j + 1;
                        continue;
                    }
                    let mut word = String::new();
                    while j < chars.len() && is_letter(chars[j]) && word.chars().count() < 32 {
                        word.push(chars[j]);
                        j += 1;
                    }
                    if SKIPPED_GROUPS.contains(&word.as_str()) {
                        skip_depth = 1;
                        i = j;
                        continue;
                    }
                }
                i += 1;
            }
            '}' => i += 1,
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    break;
                }
                let next = chars[i];
                if next == '\'' {
                    // \'hh: a byte in the document's code page (Windows-1252
                    // here), parsed by `hex_window`.
                    if let Some(byte) = hex_window(&chars, i + 1) {
                        if pending_skip > 0 {
                            pending_skip -= 1;
                        } else {
                            out.push(windows1252(byte));
                        }
                        i += 3; // Swift's success: the quote + the 2-char window
                        continue;
                    }
                    i += 1; // refused: the quote moves one, the window is text
                } else if is_letter(next) {
                    let mut word = String::new();
                    let mut number = String::new();
                    let mut j = i;
                    while j < chars.len() && is_letter(chars[j]) {
                        word.push(chars[j]);
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == '-' {
                        number.push('-');
                        j += 1;
                    }
                    while j < chars.len() && is_digit(chars[j]) {
                        number.push(chars[j]);
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == ' ' {
                        j += 1; // the space ending a control word is part of it
                    }
                    match word.as_str() {
                        "par" | "line" | "sect" | "page" | "row" => out.push('\n'),
                        "tab" | "cell" => out.push('\t'),
                        "emdash" => out.push('—'),
                        "endash" => out.push('–'),
                        "lquote" => out.push('‘'),
                        "rquote" => out.push('’'),
                        "ldblquote" => out.push('“'),
                        "rdblquote" => out.push('”'),
                        "bullet" => out.push('•'),
                        "uc" => unicode_skip = parse_int(&number).unwrap_or(1),
                        "u" => {
                            // \uN is a signed 16-bit value (negative ones wrap); anything past
                            // that is a broken file, not a trap.
                            if let Some(value) = parse_int(&number) {
                                if (-65536..65536).contains(&value) {
                                    let code = if value < 0 { value + 65536 } else { value };
                                    if let Some(scalar) = char::from_u32(code as u32) {
                                        out.push(scalar);
                                    }
                                }
                            }
                            pending_skip = unicode_skip;
                        }
                        _ => {}
                    }
                    i = j;
                } else {
                    match next {
                        '\\' | '{' | '}' => out.push(next),
                        '~' => out.push(' '),
                        '\n' | '\r' => out.push('\n'),
                        _ => {} // \-, \_, \:, \| and other symbols
                    }
                    i += 1;
                }
            }
            '\r' | '\n' => i += 1,
            _ => {
                if pending_skip > 0 {
                    pending_skip -= 1;
                } else {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    out
}

/// Swift's `Int(text)`: an optional `+`, underscores between digits, all decimal digits.
fn parse_int(text: &str) -> Option<i64> {
    let bytes: Vec<char> = text.chars().collect();
    if bytes.is_empty() {
        return None;
    }
    let signed = bytes[0] == '-';
    let mut index = if signed || bytes[0] == '+' { 1 } else { 0 };
    // Swift's Int accepts a leading '+', and underscores between digits.
    let mut value: i128 = 0;
    let mut digits = 0usize;
    let mut previous_underscore = true;
    while index < bytes.len() {
        let ch = bytes[index];
        if ch == '_' {
            if previous_underscore || index + 1 == bytes.len() {
                return None;
            }
            previous_underscore = true;
            index += 1;
            continue;
        }
        let digit = ch.to_digit(10)?;
        value = value * 10 + i128::from(digit);
        if value > i128::from(i64::MAX) + 1 {
            return None;
        }
        digits += 1;
        previous_underscore = false;
        index += 1;
    }
    if digits == 0 {
        return None;
    }
    if signed {
        i64::try_from(-value).ok()
    } else {
        i64::try_from(value).ok()
    }
}

const SKIPPED_GROUPS: [&str; 26] = [
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "header",
    "footer",
    "headerl",
    "headerr",
    "footerl",
    "footerr",
    "listtable",
    "listoverridetable",
    "rsidtbl",
    "generator",
    "themedata",
    "colorschememapping",
    "datastore",
    "xmlnstbl",
    "latentstyles",
    "pgdsctbl",
    "filetbl",
    "revtbl",
    "mmathPr",
    "fldinst",
    "object",
];

/// Windows-1252's upper half; the rest is Latin-1 — Swift's `specials` DICTIONARY
/// (Docs+Portable.swift:203-209), reproduced key-for-key: 27 entries covering ONLY
/// the mapped bytes, and the answer for everything else is
/// `specials[byte] ?? UInt32(byte)` — the RAW byte — which for the six undefined
/// slots (0x81, 0x8D, 0x90, 0x9D) is not a Unicode scalar (the scalars 0x81, 0x8D,
/// 0x90, 0x9D do not exist) and `Unicode.Scalar(..) ?? " "` answers " ". The
/// audit-base packed the table into a 32-entry array spanning 0x80…0x9F and indexed
/// `byte - 0x80` for the WHOLE 0x80…0xFF half: bytes 0xA0…0xFF (ÿ, the bullet
/// pages, every accented letter a Windows document writes) PANIC the indexer
/// (index 127 of 32 — a .rtf with \'ff is a crash, and a crash in the index job is
/// the measured interruption path), and the four defined-as-raw slots 0x81/0x8D/
/// 0x90/0x9D answer the control byte where the engine answers " ".
fn windows1252(byte: u8) -> char {
    let special: [(u8, u32); 27] = [
        (0x80, 0x20AC),
        (0x82, 0x201A),
        (0x83, 0x0192),
        (0x84, 0x201E),
        (0x85, 0x2026),
        (0x86, 0x2020),
        (0x87, 0x2021),
        (0x88, 0x02C6),
        (0x89, 0x2030),
        (0x8A, 0x0160),
        (0x8B, 0x2039),
        (0x8C, 0x0152),
        (0x8E, 0x017D),
        (0x91, 0x2018),
        (0x92, 0x2019),
        (0x93, 0x201C),
        (0x94, 0x201D),
        (0x95, 0x2022),
        (0x96, 0x2013),
        (0x97, 0x2014),
        (0x98, 0x02DC),
        (0x99, 0x2122),
        (0x9A, 0x0161),
        (0x9B, 0x203A),
        (0x9C, 0x0153),
        (0x9E, 0x017E),
        (0x9F, 0x0178),
    ];
    match special.iter().find(|(key, _)| *key == byte) {
        Some((_, value)) => char::from_u32(*value).unwrap_or(' '),
        // `?? UInt32(byte)` — `Unicode.Scalar(raw)` exists for 0xA0…0xFF (Latin-1
        // verbatim) and is NIL for the four gaps (the scalars 0x81, 0x8D, 0x90, 0x9D
        // do not exist) -> Swift's `?? " "` answers " ". Rust's `from_u32` ACCEPTS
        // those, so the gap is spelled out:
        None => match byte {
            0x81 | 0x8D | 0x90 | 0x9D => ' ',
            _ => byte as char,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::hex_window;

    fn esc(w: &str) -> Option<u8> {
        let mut chars: Vec<char> = w.chars().collect();
        chars.push('Z'); // the tail Swift's `i + 2 < count` gate needs
        hex_window(&chars, 0)
    }

    #[test]
    fn the_hex_window_is_the_measured_radix_shape() {
        // Every assertion is a row of the 1,089-window Swift oracle of 2026-11-15.
        assert_eq!(esc("41"), Some(0x41));
        assert_eq!(esc("FF"), Some(0xFF));
        assert_eq!(esc("+4"), Some(4)); // the sign eats a window slot
        assert_eq!(esc("-0"), Some(0)); // Swift's only accepted negative
        assert_eq!(esc("4_"), None); // the reader's 2-char window: refused,
        assert_eq!(esc("_4"), None); // like the spot-probe's 3-char slices said
        assert_eq!(esc("1_"), None); // NOT (that was `Int` over 3 chars)
        assert_eq!(esc("+_"), None);
        assert_eq!(esc(" +"), None);
        assert_eq!(esc(" 4"), None);
        assert_eq!(esc("-4"), None); // underflow: -4 is not a UInt8
        assert_eq!(esc("0x"), None);
        assert_eq!(esc("1."), None);
        assert_eq!(esc("5'"), None);
        assert_eq!(esc("\u{a0}4"), None); // NBSP: Swift nil, Rust InvalidDigit
                                          // The gate: a window needs its two characters (Swift's `i + 2 < count`).
        let chars: Vec<char> = "'4".chars().collect();
        assert_eq!(hex_window(&chars, 1), None);
    }
}
