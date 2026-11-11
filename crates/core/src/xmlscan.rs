//! `XMLScanner` (OfficeText.swift:340): a small XML reader for the parts Office files carry.
//! Tags with their attributes, text with entities decoded, no DTDs; namespace prefixes stay in
//! the names ("a:t"). It runs on BYTES exactly where the Swift one does, so the port reads the
//! same tag boundaries from the same bytes.

/// Swift's `UInt32(text, radix:)` parse, the form the entity decoding uses: an optional leading
/// `+`, digits of that radix, `_` separators between digits (Swift accepts them; Rust's
/// `from_str_radix` does not), nothing else, and it must fit a u32.
pub fn parse_radix(text: &str, radix: u32) -> Option<u32> {
    let bytes: Vec<char> = text.chars().collect();
    let (mut index, mut value) = (0usize, 0u64);
    if !bytes.is_empty() && bytes[0] == '+' {
        index += 1;
    }
    let mut digits = 0usize;
    let mut previous_underscore = true; // a leading `_` is not between digits
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
        let digit = ch.to_digit(radix)?;
        value = value
            .checked_mul(u64::from(radix))?
            .checked_add(u64::from(digit))?;
        if value > u64::from(u32::MAX) {
            return None;
        }
        digits += 1;
        previous_underscore = false;
        index += 1;
    }
    (digits > 0).then_some(value as u32)
}

/// A scalar as Swift spells it: `Unicode.Scalar(value)` answers nil for surrogates and past
/// U+10FFFF, and `String(Character(scalar))` is that one Character.
fn scalar_text(value: u32) -> Option<String> {
    char::from_u32(value).map(|ch| ch.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A start tag, its attributes as a byte range to read with `attribute`, and whether it
    /// closes itself (`<a/>`).
    Open {
        name: String,
        attributes: (usize, usize),
        empty: bool,
    },
    Close {
        name: String,
    },
    /// Text between tags, read with `string`; `raw` for CDATA, whose `&` is a plain `&`.
    Text {
        range: (usize, usize),
        raw: bool,
    },
}

pub struct XMLScanner {
    bytes: Vec<u8>,
    position: usize,
}

impl XMLScanner {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes, position: 0 }
    }

    /// The next event, mirroring Swift's `scan.next()` — deliberately a method, not
    /// `Iterator::next` (an XML stream is not an iterator of owned values).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Event> {
        while self.position < self.bytes.len() {
            let start = self.position;
            if self.bytes[start] != b'<' {
                while self.position < self.bytes.len() && self.bytes[self.position] != b'<' {
                    self.position += 1;
                }
                return Some(Event::Text {
                    range: (start, self.position),
                    raw: false,
                });
            }
            if self.starts_with(b"<![CDATA[", start) {
                let end = self.find(b"]]>", start + 9).unwrap_or(self.bytes.len());
                self.position = self.bytes.len().min(end + 3);
                return Some(Event::Text {
                    range: (start + 9, end),
                    raw: true,
                });
            }
            if self.starts_with(b"<!--", start) {
                self.position = self
                    .bytes
                    .len()
                    .min(self.find(b"-->", start + 4).unwrap_or(self.bytes.len()) + 3);
                continue;
            }
            // The tag's end: the first `>` outside quotes.
            let mut end = start + 1;
            let mut quote: u8 = 0;
            while end < self.bytes.len() {
                let byte = self.bytes[end];
                if quote != 0 {
                    if byte == quote {
                        quote = 0;
                    }
                } else if byte == b'"' || byte == b'\'' {
                    quote = byte;
                } else if byte == b'>' {
                    break;
                }
                end += 1;
            }
            self.position = self.bytes.len().min(end + 1);
            if start + 1 >= end {
                continue;
            }
            let first = self.bytes[start + 1];
            if first == b'?' || first == b'!' {
                continue; // declarations, DOCTYPE
            }
            let closing = first == b'/';
            let mut name_end = start + if closing { 2 } else { 1 };
            let name_start = name_end;
            while name_end < end && !is_space(self.bytes[name_end]) && self.bytes[name_end] != b'/'
            {
                name_end += 1;
            }
            let name = lossy_utf8(&self.bytes[name_start..name_end]);
            if closing {
                return Some(Event::Close { name });
            }
            let empty = self.bytes[end - 1] == b'/';
            return Some(Event::Open {
                name,
                attributes: (name_end, if empty { end - 1 } else { end }),
                empty,
            });
        }
        None
    }

    /// The value of attribute `name` among a tag's `attributes`. A name starting with ":" matches
    /// any prefix (":id" finds "r:id" but not "id").
    pub fn attribute(&self, name: &str, attributes: (usize, usize)) -> Option<String> {
        let wanted = name.as_bytes();
        let any_prefix = name.starts_with(':');
        let mut i = attributes.0;
        while i < attributes.1 {
            while i < attributes.1 && is_space(self.bytes[i]) {
                i += 1;
            }
            let key_start = i;
            while i < attributes.1 && self.bytes[i] != b'=' && !is_space(self.bytes[i]) {
                i += 1;
            }
            let key = &self.bytes[key_start..i];
            while i < attributes.1 && (is_space(self.bytes[i]) || self.bytes[i] == b'=') {
                i += 1;
            }
            if i >= attributes.1 {
                return None;
            }
            let quote = self.bytes[i];
            if quote != b'"' && quote != b'\'' {
                return None;
            }
            let value_start = i + 1;
            i = value_start;
            while i < attributes.1 && self.bytes[i] != quote {
                i += 1;
            }
            let matches = if any_prefix {
                key.len() > wanted.len() && key[key.len() - wanted.len()..] == wanted[..]
            } else {
                key == wanted
            };
            if matches {
                return Some(self.string((value_start, i.min(attributes.1)), false));
            }
            i += 1;
        }
        None
    }

    /// Text or an attribute's value, with `&amp;`, `&#233;` and the like decoded.
    pub fn string(&self, range: (usize, usize), raw: bool) -> String {
        let text = lossy_utf8(&self.bytes[range.0..range.1]);
        if raw || !text.contains('&') {
            return text;
        }
        use unicode_segmentation::UnicodeSegmentation;
        let graphemes: Vec<&str> = text.graphemes(true).collect();
        let mut out = String::new();
        let mut rest = 0usize;
        while let Some(at) = graphemes[rest..].iter().position(|g| *g == "&") {
            let amp = rest + at;
            out.extend(graphemes[rest..amp].iter().copied());
            // The semicolon within the next 12 CHARACTERS (Foundation reads the prefix as chars).
            let semicolon = graphemes[amp..]
                .iter()
                .take(12)
                .position(|g| *g == ";")
                .map(|at| amp + at);
            let Some(semicolon) = semicolon else {
                out.push('&');
                rest = amp + 1;
                continue;
            };
            let entity: String = graphemes[amp + 1..semicolon].concat();
            let decoded: Option<String> = match entity.as_str() {
                "amp" => Some("&".to_string()),
                "lt" => Some("<".to_string()),
                "gt" => Some(">".to_string()),
                "quot" => Some("\"".to_string()),
                "apos" => Some("'".to_string()),
                _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                    parse_radix(&entity[2..], 16).and_then(scalar_text)
                }
                _ if entity.starts_with('#') => parse_radix(&entity[1..], 10).and_then(scalar_text),
                _ => None,
            };
            if let Some(decoded) = decoded {
                out.push_str(&decoded);
                rest = semicolon + 1;
            } else {
                out.push('&');
                rest = amp + 1;
            }
        }
        out.extend(graphemes[rest..].iter().copied());
        out
    }

    fn starts_with(&self, prefix: &[u8], index: usize) -> bool {
        index + prefix.len() <= self.bytes.len()
            && self.bytes[index..index + prefix.len()] == prefix[..]
    }

    fn find(&self, needle: &[u8], from: usize) -> Option<usize> {
        let mut i = from;
        while i + needle.len() <= self.bytes.len() {
            if self.bytes[i] == needle[0] && self.bytes[i..i + needle.len()] == needle[..] {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

fn is_space(byte: u8) -> bool {
    byte == 0x20 || byte == 0x09 || byte == 0x0A || byte == 0x0D
}

/// The CHARACTERS (grapheme clusters) in a string — what Swift's `String.count` is.
pub fn grapheme_len(text: &str) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    text.graphemes(true).count()
}

/// `String(decoding: bytes, as: UTF8.self)` — lossy UTF-8: invalid sequences become U+FFFD.
pub fn lossy_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
