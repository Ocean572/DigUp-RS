//! Rich Text Format, read for its words (Platform/Portable/Docs+Portable.swift:114): control
//! words dropped (paragraphs and lines kept as line breaks), escapes decoded, destination groups
//! (fonts, colors, styles, info, pictures) skipped. Enough for search.

/// The engine walks Unicode SCALARS (`rtf.unicodeScalars`), which in Rust is plain `char`.
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
                    // \'hh: a byte in the document's code page (Windows-1252 here)
                    if i + 2 < chars.len() {
                        let hex: String = chars[i + 1..i + 3].iter().collect();
                        if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                            if pending_skip > 0 {
                                pending_skip -= 1;
                            } else {
                                out.push(windows1252(byte));
                            }
                            i += 3;
                            continue;
                        }
                    }
                    i += 1;
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

/// Windows-1252's upper half; the rest is Latin-1.
fn windows1252(byte: u8) -> char {
    let special: [u32; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, // 0x80…0x87
        0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, // 0x88…0x8F
        0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, // 0x90…0x97
        0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178, // 0x98…0x9F
    ];
    if byte < 0x80 {
        return byte as char;
    }
    let value = special[usize::from(byte - 0x80)];
    char::from_u32(value).unwrap_or(' ')
}
