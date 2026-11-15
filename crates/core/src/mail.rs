//! `MailMessage` (MailMessage.swift): Apple Mail's messages (`.emlx`) read for search — the
//! sender, recipient, subject and date header lines first, then the body: `text/*` parts as they
//! are, `text/html` stripped, quoted-printable and base64 transfer encodings decoded, attachments
//! left out. A file this reader can't make sense of answers `None` and `DocExtractor` returns the
//! raw text. The walk is over BYTES, so charsets arrive intact; UTF-8 is the default and the
//! fallback.

use crate::doc::plain_text_from_html;

/// Search text for one message's bytes, `None` when its shape is not understood.
pub fn text(data: &[u8]) -> Option<String> {
    if data.len() > 8_000_000 {
        return None; // a base64 vault isn't a mail to read
    }
    one(data, 0)
}

/// One MIME block: its header block, then a body that is text, or a multipart of more blocks.
fn one(raw: &[u8], depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let split = find(raw, b"\r\n\r\n").or_else(|| find(raw, b"\n\n"))?;
    let header = header_text(&raw[..split]);
    let body = &raw[split + if raw[split] == 0x0D { 4 } else { 2 }..];
    let mut lines: Vec<String> = Vec::new();
    for field in ["From", "To", "Cc", "Subject", "Date"] {
        for value in header_field(field, &header) {
            lines.push(format!("{field}: {value}"));
        }
    }
    let content = header_field("Content-Type", &header)
        .first()
        .map(|value| value.to_lowercase())
        .unwrap_or_else(|| "text/plain".to_string());
    let transfer = header_field("Content-Transfer-Encoding", &header)
        .first()
        .cloned()
        .unwrap_or_else(|| "7bit".to_string());
    let mut out: Vec<String> = vec![lines.join("\n")];
    if content.contains("multipart") {
        let boundary = parameter("boundary", Some(&content))?;
        for chunk in parts_of(body, &format!("--{boundary}")) {
            if let Some(part) = one(chunk, depth + 1) {
                out.push(part);
            }
        }
    } else if content.starts_with("text/") {
        out.push(decode(body, &transfer, &content));
    } // an attachment's bytes: the header lines above are all it offers to a text index
    let text: String = out
        .iter()
        .filter(|line| !crate::office::trim_ws(line).is_empty())
        .cloned()
        .collect::<Vec<String>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The pieces of a body between `--boundary` markers. The scan is over bytes by hand, as the
/// engine's is (Foundation's `range(of:)` has no window argument to search in).
fn parts_of<'a>(body: &'a [u8], marker: &str) -> Vec<&'a [u8]> {
    let needle = marker.as_bytes();
    if needle.is_empty() {
        return vec![body];
    }
    let mut out: Vec<&[u8]> = Vec::new();
    let mut start = 0usize;
    while let Some(found) = find(&body[start..], needle) {
        out.push(&body[start..start + found]);
        start += found + needle.len();
    }
    out.push(&body[start..]);
    out
}

/// The header block as text: UTF-8 when its bytes are valid UTF-8 (the shape modern mail writes
/// raw), Latin-1 when they are not (the shape mail before ~2008 wrote).
fn header_text(block: &[u8]) -> String {
    String::from_utf8(block.to_vec())
        .unwrap_or_else(|_| block.iter().map(|byte| char::from(*byte)).collect())
}

// MARK: Headers

/// Values of a header field: continuation lines (the ones starting with a space) joined, RFC 2047
/// encoded-words decoded — the shape mail has used since 1997.
pub fn header_field(name: &str, header: &str) -> Vec<String> {
    let normalized = header.replace("\r\n", "\n").replace('\r', "\n");
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut collecting = false;
    for line in normalized.split('\n') {
        if line.is_empty() {
            continue;
        }
        let first = unicode_segmentation::UnicodeSegmentation::graphemes(line, true)
            .next()
            .unwrap_or(" ");
        if first == " " || first == "\t" {
            if collecting {
                current.push(' ');
                current.push_str(&crate::office::trim_ws_in(
                    line,
                    crate::search_text::is_whitespace_set,
                ));
            }
            continue;
        }
        if collecting && !current.is_empty() {
            out.push(decoded_header(&current));
        }
        collecting = false;
        let Some(colon) = line.find(':') else {
            continue;
        };
        if crate::office::trim_ws_in(&line[..colon], crate::search_text::is_whitespace_set)
            .to_lowercase()
            != name.to_lowercase()
        {
            continue;
        }
        collecting = true;
        current =
            crate::office::trim_ws_in(&line[colon + 1..], crate::search_text::is_whitespace_set);
    }
    if collecting && !current.is_empty() {
        out.push(decoded_header(&current));
    }
    out
}

/// "=?utf-8?Q?Copied=5FPath?=" → "Copied_Path"-style words, `B` base64 and `Q` quoted-printable
/// (where `_` is the space). An undecodable word stays as written: mail readers are generous,
/// and a literal is better search text than dropping the line.
pub fn decoded_header(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("=?") {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let Some(end) = tail.find("?=") else {
            // No closing marker: the rest stays as written.
            out.push_str(&rest[start..]);
            return out;
        };
        let words = &tail[..end];
        rest = &tail[end + 2..];
        let parts: Vec<&str> = words.split('?').collect();
        let decoded = if parts.len() == 3 {
            decoded_header_word(parts[0], parts[1], parts[2])
        } else {
            None
        };
        match decoded {
            Some(decoded) => out.push_str(&decoded),
            None => {
                out.push('=');
                out.push_str(words);
                out.push_str("?=");
            }
        }
    }
    out.push_str(rest);
    out
}

/// One `=?charset?encoding?text?=` word. Its own header names the charset, so the bytes the
/// `Q`/`B` encoding gives are read in it.
fn decoded_header_word(charset: &str, encoding: &str, word: &str) -> Option<String> {
    match encoding.to_uppercase().as_str() {
        "Q" => {
            let mut bytes: Vec<u8> = Vec::new();
            let mut rest = word;
            // The earlier of the next `_` (mail's space) and `=` (hex escape) — an `=`-escape
            // before a later `_` must not stop at the `_` with the raw `=C3=A9` left in the text.
            loop {
                let at = match (rest.find('_'), rest.find('=')) {
                    (Some(a), Some(b)) => a.min(b),
                    (Some(a), None) => a,
                    (None, Some(b)) => b,
                    (None, None) => break,
                };
                bytes.extend(rest[..at].bytes());
                let head = unicode_segmentation::UnicodeSegmentation::graphemes(&rest[at..], true)
                    .next()
                    .unwrap_or("");
                if head == "_" {
                    bytes.push(0x20);
                    rest = &rest[at + 1..];
                    continue;
                }
                let after = &rest[at + 1..];
                // Swift's prefix(2) takes two CHARACTERS; an undecodable pair refuses the word.
                let hex: String = unicode_segmentation::UnicodeSegmentation::graphemes(after, true)
                    .take(2)
                    .collect();
                if crate::xmlscan::grapheme_len(after) < 2 {
                    return None;
                }
                bytes.push(u8::try_from(crate::xmlscan::parse_radix(&hex, 16)?).ok()?);
                rest = advance_chars(after, 2);
            }
            bytes.extend(rest.bytes());
            Some(decoded(&bytes, Some(charset)))
        }
        "B" => {
            let mut word = String::from(word);
            while crate::xmlscan::grapheme_len(&word) % 4 != 0 {
                word.push('='); // RFC 2047 sends 7-bit words unpadded
            }
            let data = base64_loose(word.as_bytes())?;
            Some(decoded(&data, Some(charset)))
        }
        _ => None,
    }
}

/// The quoted string or bare token of a MIME parameter: `boundary="x;1"` → `x;1`. The
/// case-insensitive search runs on the value itself, character by character — indices from a
/// lowercased copy do not align when lowering changes a string's length (`İ` is 2 bytes, its
/// lower case 3), and mail headers are untrusted input.
pub fn parameter(name: &str, value: Option<&str>) -> Option<String> {
    let value = value?;
    let clusters: Vec<&str> =
        unicode_segmentation::UnicodeSegmentation::graphemes(value, true).collect();
    let needle: Vec<char> = format!("{name}=").chars().collect();
    let lower_of = |cluster: &str| cluster.to_lowercase();
    let mut at = 0usize; // grapheme index where the search resumes
    'outer: while at + needle.len() <= clusters.len() {
        for (offset, want) in needle.iter().enumerate() {
            // The needle is an ASCII field name ("boundary=", "charset="), which is what mail
            // writes: folding the cluster and comparing its first character is Foundation's
            // `.caseInsensitive` find for every needle of that shape.
            let folded = lower_of(clusters[at + offset]);
            if !folded.starts_with(*want) {
                at += 1;
                continue 'outer;
            }
        }
        let mut tail_index = at + needle.len();
        let tail: String = clusters[tail_index..].concat();
        if tail.starts_with('"') {
            tail_index += 1;
            let tail: String = clusters[tail_index..].concat();
            return match tail.find('"') {
                Some(close) => Some(tail[..close].to_string()),
                None => Some(tail),
            };
        }
        let mut head = String::new();
        for cluster in clusters[tail_index..].iter().copied() {
            // Swift's test is on CHARACTERS: a cluster stops the token when all its scalars are
            // whitespace, or when it is exactly ";" or "," (a combining mark on the punctuation
            // makes a different Character, which mail keeps).
            let stops =
                cluster.chars().all(char::is_whitespace) || cluster == ";" || cluster == ",";
            if stops {
                break;
            }
            head.push_str(cluster);
        }
        return Some(head);
    }
    None
}

// MARK: Bodies

/// The bytes of a decoded body (or of an RFC 2047 encoded-word) read in the charset its header
/// declares. UTF-8 is the default and the fallback; the two families mail before ~2008 sends are
/// named here because reading their high bytes as UTF-8 gives the replacement character for every
/// accented letter, and an accented word is a word keyword search cannot find. Any other
/// declared charset reads as UTF-8.
pub fn decoded(bytes: &[u8], charset: Option<&str>) -> String {
    match charset_name(charset).as_str() {
        "iso88591" | "latin1" => bytes.iter().map(|byte| char::from(*byte)).collect(),
        "windows1252" | "cp1252" | "1252" => bytes
            .iter()
            .map(|byte| {
                if *byte < 0x80 {
                    char::from(*byte)
                } else if *byte < 0xA0 {
                    char::from_u32(WINDOWS1252_HIGH[usize::from(*byte - 0x80)])
                        .unwrap_or('\u{FFFD}')
                } else {
                    char::from(*byte) // 0xA0…0xFF is Latin-1's own
                }
            })
            .collect(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// `ISO-8859-1:1987` and `iso_8859-1` name the same charset as `iso-8859-1`: the name's first
/// piece, letters and digits only, is what the switch above compares.
fn charset_name(charset: Option<&str>) -> String {
    let Some(charset) = charset else {
        return String::new();
    };
    // Swift's `split(separator: ":")` OMITS empty pieces (its default): the
    // canonical registered form `charset=iso-8859-1:1987` (and a leading-colon
    // `:iso-8859-1`) names "iso-8859-1" -> Latin-1. Rust's `split(':')` KEEPS
    // the empty head: the audit-base named the colon-form "" and read the body
    // as UTF-8 - the issue-#146 replacement-characters shape on the colon-form
    // name the standards register writes. A string of only separators yields
    // no pieces on both sides ("" -> UTF-8 fallback), so the first NON-empty
    // piece is the relay.
    let Some(first) = charset.split(':').find(|piece| !piece.is_empty()) else {
        return String::new();
    };
    // The filter is per CHARACTER (grapheme cluster): Foundation drops the
    // cluster WHOLE when it is neither letter nor number - a cluster's
    // separators leave no residue. Real names are ASCII, where the cluster
    // walk and a scalar walk answer the same; the shape is pinned.
    unicode_segmentation::UnicodeSegmentation::graphemes(first.to_lowercase().as_str(), true)
        .filter(|cluster| {
            cluster
                .chars()
                .all(|ch| ch.is_alphabetic() || ch.is_numeric())
        })
        .collect()
}

/// `windows-1252`'s high half, 0x80…0x9F as code points. The five slots the specification leaves
/// undefined (0x81, 0x8D, 0x8F, 0x90, 0x9D) read as the replacement character.
const WINDOWS1252_HIGH: [u32; 32] = [
    0x20AC, 0xFFFD, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, // 0x80…0x87
    0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0xFFFD, 0x017D, 0xFFFD, // 0x88…0x8F
    0xFFFD, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, // 0x90…0x97
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0xFFFD, 0x017E, 0x0178, // 0x98…0x9F
];

fn decode(body: &[u8], transfer: &str, content_type: &str) -> String {
    let charset = parameter("charset", Some(content_type));
    // 7bit, 8bit, binary — the bytes as they are, in the declared charset. An `8bit` body of an
    // older mail is the shape this fixes: the high bytes reach here intact.
    let mut text = decoded(body, charset.as_deref());
    match transfer.to_lowercase().as_str() {
        "quoted-printable" | "quoted_printable" => {
            text = quoted_printable(body, charset.as_deref())
        }
        "base64" => {
            let clean: Vec<u8> = body
                .iter()
                .copied()
                .filter(|byte| *byte != 0x0A && *byte != 0x0D)
                .collect(); // mail wraps it
            let mut clean = clean;
            while clean.len() % 4 != 0 {
                clean.push(b'='); // RFC 2047 sends 7-bit words unpadded
            }
            // A body that isn't base64 at all stays what its bytes spell — better search text
            // than the base64 letters, which is what the fallback of old handed to the index.
            if let Some(data) = base64_loose(&clean) {
                text = decoded(&data, charset.as_deref());
            }
        }
        _ => {}
    }
    if content_type.contains("html") {
        text = plain_text_from_html(&text);
    }
    text
}

/// `=3D` hex escapes and `=\r\n` soft line breaks (RFC 2045) — collected as bytes and read back
/// in the declared charset at the end, so `=C3=A9` lands as "é" and `=E9` under
/// `charset=iso-8859-1` lands as "é" too. One linear walk over the bytes.
pub fn quoted_printable(raw: &[u8], charset: Option<&str>) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(raw.len());
    let mut at = 0usize;
    while at < raw.len() {
        let byte = raw[at];
        if byte != 0x3D {
            bytes.push(byte);
            at += 1;
            continue;
        }
        if at + 1 >= raw.len() {
            bytes.push(0x3D); // `=` ends the body
            at += 1;
            continue;
        }
        let next = raw[at + 1];
        if next == 0x0A {
            at += 2; // soft break: the line ended at the `=`
            continue;
        }
        if next == 0x0D {
            // a CRLF pair is one break, a lone CR breaks on its own
            at += if at + 2 < raw.len() && raw[at + 2] == 0x0A {
                3
            } else {
                2
            };
            continue;
        }
        if at + 2 < raw.len() {
            if let Some(hex) = hex_byte(next, raw[at + 2]) {
                bytes.push(hex);
                at += 3;
                continue;
            }
        }
        bytes.push(0x3D); // a stray "=" is mail being generous: keep it
        at += 1;
    }
    decoded(&bytes, charset)
}

/// RFC 2045's two hex digits as one byte. Mail writes them uppercase; it has been generous about
/// case since then, so both spellings decode.
fn hex_byte(high: u8, low: u8) -> Option<u8> {
    let digit = |byte: u8| -> Option<u8> {
        match byte {
            0x30..=0x39 => Some(byte - 0x30),
            0x41..=0x46 => Some(byte - 0x41 + 10),
            0x61..=0x66 => Some(byte - 0x61 + 10),
            _ => None,
        }
    };
    Some(digit(high)? * 16 + digit(low)?)
}

/// Base64 read the way Foundation's `Data(base64Encoded:options: .ignoreUnknownCharacters)`
/// reads it: letters outside the alphabet are ignored, unpadded words decode, and a final
/// one-character group makes the whole thing nil — which is what lets a non-base64 body stay
/// the text its bytes spell.
pub fn base64_loose(bytes: &[u8]) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out: Vec<u8> = Vec::new();
    let mut bits: Vec<u8> = Vec::with_capacity(4);
    for &byte in bytes {
        if byte == b'=' {
            // Padding: it ends the group in progress. Two letters mean one byte, three mean two;
            // a group with no letters (the second `=` of `QQ==`) is padding of a finished group,
            // which Foundation accepts, and a lone leftover letter refuses the whole run.
            match bits.len() {
                0 => {}
                2 => {
                    out.push((bits[0] << 2) | (bits[1] >> 4));
                    bits.clear();
                }
                3 => {
                    out.push((bits[0] << 2) | (bits[1] >> 4));
                    out.push(((bits[1] & 0x0F) << 4) | (bits[2] >> 2));
                    bits.clear();
                }
                _ => return None,
            }
            continue;
        }
        let Some(six) = value(byte) else {
            continue; // ignoreUnknownCharacters
        };
        bits.push(six);
        if bits.len() == 4 {
            out.push((bits[0] << 2) | (bits[1] >> 4));
            out.push(((bits[1] & 0x0F) << 4) | (bits[2] >> 2));
            out.push(((bits[2] & 0x03) << 6) | bits[3]);
            bits.clear();
        }
    }
    match bits.len() {
        0 => Some(out),
        1 => None, // a lone leftover letter is not base64
        2 => {
            out.push((bits[0] << 2) | (bits[1] >> 4));
            Some(out)
        }
        _ => {
            out.push((bits[0] << 2) | (bits[1] >> 4));
            out.push(((bits[1] & 0x0F) << 4) | (bits[2] >> 2));
            Some(out)
        }
    }
}

/// Advance over `count` CHARACTERS (grapheme clusters) of a string.
fn advance_chars(text: &str, count: usize) -> &str {
    use unicode_segmentation::UnicodeSegmentation;
    let mut rest = text;
    for cluster in text.graphemes(true).take(count) {
        rest = &rest[cluster.len()..];
    }
    rest
}

#[cfg(test)]
mod tests {
    use super::header_field;

    #[test]
    fn the_charset_name_is_the_first_non_empty_piece_letters_digits_only() {
        // F11, against MailMessage.swift:224-227. Swift's `split(separator: ":")`
        // OMITS empty pieces: the canonical registered form `iso-8859-1:1987`
        // and the leading-colon `:iso-8859-1` name "iso-8859-1" -> Latin-1.
        // Rust's split(':') KEEPS the empty head: the audit-base named the
        // colon-form "" and read the bytes as UTF-8 - issue #146's replacement
        // characters, back on the form the standards register writes.
        use super::{charset_name, decoded, decoded_header};
        assert_eq!(charset_name(Some("iso-8859-1:1987")), "iso88591");
        assert_eq!(charset_name(Some(":iso-8859-1")), "iso88591"); // fails before: ""
        assert_eq!(charset_name(Some("ISO-8859-1:1987")), "iso88591");
        assert_eq!(charset_name(Some("iso_8859-1")), "iso88591");
        assert_eq!(charset_name(Some("windows-1252")), "windows1252");
        assert_eq!(charset_name(Some("::")), ""); // no pieces on both sides
        assert_eq!(charset_name(None), "");
        // The whole decode chain rides it: 8-bit bytes and an encoded word.
        assert_eq!(decoded(b"caf\xe9", Some("iso-8859-1:1987")), "caf\u{e9}");
        assert_eq!(decoded_header("=?iso-8859-1:1987?Q?caf=E9?="), "caf\u{e9}");
    }

    #[test]
    fn header_trims_ride_the_measured_whitespace_sets() {
        // F10, pinned against the F9-measured Foundation sets (f9full.swift,
        // swift-corelibs 6.4): `.whitespaces` = 19 scalars WITH ZWSP U+200B (the
        // Linux engine) and WITHOUT the line controls; `.whitespacesAndNewlines`
        // adds LF VT FF CR NEL LS PS. The header sites (MailMessage.swift:105,111,115)
        // ride `.whitespaces`, so:
        // ZWSP edges TRIM (it is in the set):
        assert_eq!(
            header_field("Subject", "\u{200b}Subject\u{200b}: \u{200b}cafe\u{200b}"),
            vec!["cafe"]
        );
        // NEL (U+0085) SURVIVES the value trim (not in `.whitespaces`) -
        // char::is_whitespace, the audit-base shape, dropped it;
        assert_eq!(
            header_field("Subject", "Subject: cafe\u{85}"),
            vec!["cafe\u{85}"]
        );
        // and NEL on the NAME is a name mismatch (the header is not collected):
        assert_eq!(
            header_field("Subject", "Subject\u{85}: x"),
            Vec::<String>::new()
        );
        // a continuation line joins with one space, ZWSP edges trimmed:
        assert_eq!(
            header_field("Subject", "Subject: a\r\n \u{200b}b"),
            vec!["a b"]
        );
        // CRLF normalization runs before the split (Swift MailMessage.swift:96), so
        // a CR never reaches these trims - the `.whitespaces` CR-survival of F9 is
        // unreachable HERE; it lives at the code.rs line channels.
        // The shared `.whitespacesAndNewlines` trim (office/doc sites, the mail out
        // filter) gains ZWSP too (F9 set):
        assert_eq!(crate::office::trim_ws("\u{200b}a\u{200b}"), "a");
        // and the two sets differ on the line controls (the site split):
        assert_eq!(
            crate::office::trim_ws_in("x\u{2028}", crate::search_text::is_whitespace_set),
            "x\u{2028}"
        );
        assert_eq!(crate::office::trim_ws("x\u{2028}"), "x");
    }
}
