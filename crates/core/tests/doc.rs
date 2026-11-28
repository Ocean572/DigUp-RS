//! `DocExtractor` / `OfficeText` / `RTF` / `MailMessage` / the zip reader, pinned against the
//! engine's own answers.
//!
//! The wide gate is the fixture cross-check (`/tmp/opencode/doccheck`: 47 files — the encodings
//! Foundation guesses, the HTML strip's shapes, an RTF with destinations, signed `uN` scalars and
//! `'hh` bytes in Windows-1252, docx / odt / xlsx / pptx / ods / odp packages, the zip refusals (no
//! part, not a zip, an empty archive, an entry declaring 65 MB, a damaged entry stream), chunk
//! shapes, nine `.emlx` messages and the plain files — whose JSON lines the engine and the port
//! wrote identically: 47 files, 0 differing). These tests carry the same fixtures and the engine's
//! measured answers inside the workspace.

// The whole measured answer set travels with the fixtures, not only the fields the pins compare.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};

use digup_core::{doc, rtf, zipfile};
use flate2::write::DeflateEncoder;
use flate2::Compression;
use unicode_segmentation::UnicodeSegmentation;

const BOM_UTF8_BYTES: &[u8] = b"\xef\xbb\xbfbom utf8 \xc3\xa9\n";
const BOM_UTF8_TEXT: &str = "bom utf8 é\n";
const UTF16LE_BOM_BYTES: &[u8] = b"\xff\xfel\x00e\x00 \x00\xe9\x00\n\x00";
const UTF16LE_BOM_TEXT: &str = "le é\n";
const UTF16BE_BOM_BYTES: &[u8] = b"\xfe\xff\x00l\x00e\x00 \x00\xe9\x00\n";
const UTF16BE_BOM_TEXT: &str = "le é\n";
const UTF32LE_BOM_BYTES: &[u8] = b"\xff\xfe\x00\x00u\x00\x00\x00t\x00\x00\x00\n\x00\x00\x00";
const UTF32LE_BOM_TEXT: &str = "ut\n";
const NUL_NO_BOM_BYTES: &[u8] = b"u\x00t\x00f\x00\n\x00";
const NUL_NO_BOM_TEXT: &str = "u\u{00}t\u{00}f\u{00}\n\u{00}";
const FFFE_BYTES: &[u8] = b"text \xef\xbf\xbe marker\n";
const FFFE_TEXT: &str = "text ￾ marker\n";
const HIGH_ACCENTS_BYTES: &[u8] = b"\xc3\xa9 \xc3\xa8\n";
const HIGH_ACCENTS_TEXT: &str = "é è\n";
const INVALID_PAIR_BYTES: &[u8] = b"\xe9\xe9\n";
const INVALID_PAIR_TEXT: &str = "éé\n";
const CP1252_BYTES: &[u8] = b"euro \x80 1252 quotes \x93\x94\n";
const CP1252_TEXT: &str = "euro  1252 quotes \n";
const CP1252_C50: &[&str] = &["euro  1252 quotes "];
const CP1252_C12: &[&str] = &["euro  1252", "quotes "];
const CP1252_C1800: &[&str] = &["euro  1252 quotes "];
const CP1252_EXCERPT: &str = "euro  1252 quotes ";
const LATIN1_BYTES: &[u8] = b"na\xefve caf\xe9 latin1\ntail\ntail\ntail\n";
const LATIN1_TEXT: &str = "naïve café latin1\ntail\ntail\ntail\n";
const LATIN1_C50: &[&str] = &["naïve café latin1\ntail\ntail\ntail"];
const LATIN1_C12: &[&str] = &["naïve café", "latin1"];
const LATIN1_C1800: &[&str] = &["naïve café latin1\ntail\ntail\ntail"];
const LATIN1_EXCERPT: &str = "naïve café latin1 tail tail tail";
const PLAIN_BYTES: &[u8] = b"plain ascii text\nsecond line\n";
const PLAIN_TEXT: &str = "plain ascii text\nsecond line\n";
const PLAIN_C50: &[&str] = &["plain ascii text\nsecond line"];
const PLAIN_C12: &[&str] = &["plain ascii", "text\nsecond"];
const PLAIN_C1800: &[&str] = &["plain ascii text\nsecond line"];
const PLAIN_EXCERPT: &str = "plain ascii text second line";
const ONLY_NEWLINES_BYTES: &[u8] = b"\n\n\n\n";
const ONLY_NEWLINES_TEXT: &str = "\n\n\n\n";
const ONLY_NEWLINES_C50: &[&str] = &[];
const ONLY_NEWLINES_C12: &[&str] = &[];
const ONLY_NEWLINES_C1800: &[&str] = &[];
const ONLY_NEWLINES_EXCERPT: &str = "";
const NOTES_MD_BYTES: &[u8] = b"# Markdown\n\n*Text* written as markdown.\nlong long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long \n";
const NOTES_MD_TEXT: &str = "# Markdown\n\n*Text* written as markdown.\nlong long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long \n";
const NOTES_MD_C50: &[&str] = &[
    "# Markdown\n\n*Text* written as markdown.\nlong long",
    "long long long long long long long long long",
    "long long long long long long long long long",
    "long long long long long long long long long",
    "long long long long long long long long long",
    "long long long long long long long long long",
    "long long long long long long long long long",
    "long long long long long long long long long",
    "long long long long long long long long long",
];
const NOTES_MD_C12: &[&str] = &["# Markdown", "*Text*"];
const NOTES_MD_C1800: &[&str] = &["# Markdown\n\n*Text* written as markdown.\nlong long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long", "long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long", "long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long"];
const NOTES_MD_EXCERPT: &str = "# Markdown *Text* written as markdown. long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long long l…";
const PAGE_HTML_BYTES: &[u8] = b"<!DOCTYPE html>\n<html><head><title>Titles are gone</title>\n<script>var a = \"<b>invisible</b>\";</script>\n<style> p { color: red } </style></head>\n<body  class=\"doc\">\n  <h1>Heading</h1>\n  <p>First &amp; entity test: &lt;b&gt; &nbsp; &quot;quoted&quot; &#39;&#39; &mdash; &ndash; &hellip;</p>\n  <p>numeric &#233; &#x2764; &#xD800; and &#xZZ;</p>\n  <ul><li>one\tone</li><li>two</li></ul>\n  <!-- a comment <p>hidden</p> -->\n  <table><tr><td>cell one</td><td>cell two</td></tr></table>\n  <div>div text</div>\n  <section>section text</section>\n  <br/>\n  trailing\t\tspaces\n</body></html>\n";
const PAGE_HTML_TEXT: &str = "Heading\nFirst & entity test: <b> \"quoted\" '' — – …\nnumeric é ❤ and &#xZZ;\none one\ntwo\ncell one cell two\ndiv text\nsection text\ntrailing spaces";
const PAGE_HTML_C50: &[&str] = &[
    "Heading\nFirst & entity test: <b> \"quoted\" '' — –",
    "'' — – …\nnumeric é ❤ and &#xZZ;\none one\ntwo\ncell",
    "two\ncell one cell two\ndiv text\nsection text",
    "text\ntrailing spaces",
];
const PAGE_HTML_C12: &[&str] = &["Heading", "First &"];
const PAGE_HTML_C1800: &[&str] = &["Heading\nFirst & entity test: <b> \"quoted\" '' — – …\nnumeric é ❤ and &#xZZ;\none one\ntwo\ncell one cell two\ndiv text\nsection text\ntrailing spaces"];
const PAGE_HTML_EXCERPT: &str = "Heading First & entity test: <b> \"quoted\" '' — – … numeric é ❤ and &#xZZ; one one two cell one cell two div text section text trailing spaces";
const NOTES_RTF_BYTES: &[u8] = b"{\\rtf1\\ansi\\ansicpg1252\\deff0\n{\\fonttbl{\\f0 Times New Roman;}}\n{\\colortbl;\\red0\\green0\\blue0;}\n{\\info{\\title A title is skipped}\\author also skipped}\n{\\generator made by word}\n\\pard\\par\nIntroduction paragraph.\\par\nSecond line with a\\tab tab and an\\emdash emdash, a\\endash endash\\line\nand a lquote\\lquote l, rquote\\rquote r, ldblquote\\ldblquote l, rdblquote\\rdblquote r, bullet\\bullet b\\par\n\\u8212 unicode dash \\u-9232 neg unicode\\par\n\\\xe2\x80\x99 escape \xe2\x80\x99 brace \\{ brace \\} slash \\\\ \\~ nbsp \\* star\\par\n\\\'84 cp1252 euro\\\'92quote\\\'97dash\\\'80 euro\\par\n{\\*\\expanded \\uc2\\u233 combine skip \xc3\xa9 tail\\par}\n{\\pard nested visible text\\par}\n\\sectsect page\\page row\\row sect\\sect\\par\n{\\field{\\*\\fldinst{PAGE}}{\\fldrslt field result visible\\par}}\n{\\pict invisible}\\pard last\\par\n}\n";
const NOTES_RTF_TEXT: &str = "\nIntroduction paragraph.\nSecond line with a\ttab and an—emdash, a–endash\nand a lquote‘l, rquote’r, ldblquote“l, rdblquote”r, bullet•b\n—nicode dash eg unicode\n escape ’ brace { brace } slash \\   nbsp  star\n„ cp1252 euro’quote—dash€ euro\nnested visible text\npage\nrow\nsect\n\nfield result visible\nlast\n";
const NOTES_RTF_C50: &[&str] = &[
    "Introduction paragraph.\nSecond line with a tab",
    "a tab and an—emdash, a–endash\nand a lquote‘l,",
    "lquote‘l, rquote’r, ldblquote“l, rdblquote”r,",
    "bullet•b\n—nicode dash eg unicode\n escape ’ brace",
    "’ brace { brace } slash \\ nbsp star\n„ cp1252",
    "„ cp1252 euro’quote—dash€ euro\nnested visible",
    "visible text\npage\nrow\nsect\n\nfield result visible",
    "visible\nlast",
];
const NOTES_RTF_C12: &[&str] = &["Introduction", "paragraph."];
const NOTES_RTF_C1800: &[&str] = &["Introduction paragraph.\nSecond line with a tab and an—emdash, a–endash\nand a lquote‘l, rquote’r, ldblquote“l, rdblquote”r, bullet•b\n—nicode dash eg unicode\n escape ’ brace { brace } slash \\ nbsp star\n„ cp1252 euro’quote—dash€ euro\nnested visible text\npage\nrow\nsect\n\nfield result visible\nlast"];
const NOTES_RTF_EXCERPT: &str = "Introduction paragraph. Second line with a tab and an—emdash, a–endash and a lquote‘l, rquote’r, ldblquote“l, rdblquote”r, bullet•b —nicode dash eg unicode escape ’ brace { brace } slash \\ nbsp star „ cp1252 euro’quote—dash€ euro nested vis…";
const SOLID_BYTES: &[u8] = b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
const SOLID_TEXT: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
const SOLID_C50: &[&str] = &[
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
];
const SOLID_C12: &[&str] = &["xxxxxxxxxxxx", "xxxxxxxxxxxx"];
const SOLID_C1800: &[&str] = &["xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"];
const SOLID_EXCERPT: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx…";
const SPACES_BYTES: &[u8] = b"a\xc2\xa0\t b\n\n\nc\nplain tail\n";
const SPACES_TEXT: &str = "a \t b\n\n\nc\nplain tail\n";
const SPACES_C50: &[&str] = &["a b\n\nc\nplain tail"];
const SPACES_C12: &[&str] = &["a b\n\nc\nplain", "tail"];
const SPACES_C1800: &[&str] = &["a b\n\nc\nplain tail"];
const SPACES_EXCERPT: &str = "a b c plain tail";
const FAMILY_BYTES: &[u8] = b"\xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7 \xf0\x9f\x91\xa8\xe2\x80\x8d\xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x91\xa7\n";
const FAMILY_TEXT: &str = "👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧\n";
const FAMILY_C50: &[&str] = &["👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧"];
const FAMILY_C12: &[&str] = &["👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧", "👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧"];
const FAMILY_C1800: &[&str] = &["👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧"];
const FAMILY_EXCERPT: &str = "👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧 👨‍👩‍👧";
const EMPTY_TXT_BYTES: &[u8] = b"";
const EMPTY_TXT_TEXT: &str = "";
const EMPTY_TXT_C50: &[&str] = &[];
const EMPTY_TXT_C12: &[&str] = &[];
const EMPTY_TXT_C1800: &[&str] = &[];
const EMPTY_TXT_EXCERPT: &str = "";
const RTF_UC_BYTES: &[u8] = b"{\\rtf1\\ansi\\deff0\\uc2\\u233 \\u-9232 x\\par}";
const RTF_UC_TEXT: &str = "é\n";
const RTF_UC_C50: &[&str] = &["é"];
const RTF_UC_C12: &[&str] = &["é"];
const RTF_UC_C1800: &[&str] = &["é"];
const RTF_UC_EXCERPT: &str = "é";
const RTF_TAB_BYTES: &[u8] = b"a\\tab b\\par c\\tab d";
const RTF_TAB_TEXT: &str = "a\tb\nc\td";
const RTF_TAB_C50: &[&str] = &["a b\nc d"];
const RTF_TAB_C12: &[&str] = &["a b\nc d"];
const RTF_TAB_C1800: &[&str] = &["a b\nc d"];
const RTF_TAB_EXCERPT: &str = "a b c d";
const RTF_STAR_BYTES: &[u8] = b"{\\*\\expanded hidden}visible";
const RTF_STAR_TEXT: &str = "visible";
const RTF_STAR_C50: &[&str] = &["visible"];
const RTF_STAR_C12: &[&str] = &["visible"];
const RTF_STAR_C1800: &[&str] = &["visible"];
const RTF_STAR_EXCERPT: &str = "visible";
const RTF_ESC_BYTES: &[u8] = b"keep \\{brace\\} slash \\\\ tilde \\~ star \\* dash \\-";
const RTF_ESC_TEXT: &str = "keep {brace} slash \\ tilde   star  dash ";
const RTF_ESC_C50: &[&str] = &["keep {brace} slash \\ tilde star dash"];
const RTF_ESC_C12: &[&str] = &["keep {brace}", "slash \\"];
const RTF_ESC_C1800: &[&str] = &["keep {brace} slash \\ tilde star dash"];
const RTF_ESC_EXCERPT: &str = "keep {brace} slash \\ tilde star dash";
const RTF_EMPTY_BYTES: &[u8] = b"{\\rtf1\\ansi}";
const RTF_EMPTY_TEXT: &str = "";
const RTF_EMPTY_C50: &[&str] = &[];
const RTF_EMPTY_C12: &[&str] = &[];
const RTF_EMPTY_C1800: &[&str] = &[];
const RTF_EMPTY_EXCERPT: &str = "";
const LEGACY_DOC_BYTES: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1WordDocumentjunkjunk";
// F-D (2026-11-28): the legacy .doc reader answers this file's shape by name — a CFB magic
// that does not open as a compound file (the pre-CFB Word 2/5/Mac refusal, measured in
// tests/word.rs `a_pre_cfb_word_file_refuses_at_the_container`).
const LEGACY_DOC_WHY: &str = "unreadable: not a Word (.doc) compound file";
const LOCKED_XLSX_BYTES: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
const LOCKED_XLSX_WHY: &str = "password-protected";
const NOTZIP_XLSX_BYTES: &[u8] = b"PK\x03\x04not a real zip at all";
const NOTZIP_XLSX_WHY: &str = "unreadable: not a zip file";
const EMPTY_XLSX_BYTES: &[u8] =
    b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
const EMPTY_XLSX_WHY: &str = "unreadable: no xl/workbook.xml inside";
const CAP_DOCX_BYTES: &[u8] = b"PK\x03\x04\x14\x00\x00\x00\x08\x00Y\x7fk]6W\xde\xdc\xa2\x00\x00\x00\x18\x01\x00\x00\x0b\x00\x00\x00_rels/.rels\x8d\xcf;\x0e\xc20\x0c\x06\xe0\xabD\xde\xa9\x0b\x03B\xa8i\x17\x84\xd4\x15\x95\x03D\x89\x9bF4\x0f%\xe1u{20P\xc4\xc0h\xfb\xf7g\xb9\xe9\x1evf7\x8a\xc9x\xc7a]\xd5\xc0\xc8I\xaf\x8c\xd3\x1c\xce\xc3q\xb5\x83\xaemN4\x8b\\\x12i2!\xb1\xb2\xe2\x12\x87)\xe7\xb0GLr\"+R\xe5\x03\xb92\x19}\xb4\"\x972j\x0cB^\x84&\xdc\xd4\xf5\x16\xe3\xa7\x01K\x93\xf5\x8aC\xec\xd5\x1a\xd8\xf0\x0c\xf4\x8f\xed\xc7\xd1H:xy\xb5\xe4\xf2\x8f\x13_\x89\"\x8b\xa8)s\xb8\xfb\xa8P\xbd\xdbUa\x01\xdb\x06\x17/\xb6/PK\x03\x04\x14\x00\x00\x00\x08\x00Y\x7fk]\xfa\xdd\xff\x14(\x01\x00\x00\x0e\x02\x00\x00\x11\x00\x00\x00word/document.xml\x85\x91\xcdN\x021\x10\x80_\xa5\xe9\x81\x9b\x14\xd0\x18\xb3\xb0K<\x887cD\x1f`v;,\x95\xedO\xa6\x85\xd5\xe7\xf0\xe0\x85x\xf3b|*\x0f\xc4\xc7\xb0\x8d\x12\xd4h\xb8L\xd3\xe9\xd7o&3\xa3\xf1\x9dn\xd8\n\xc9+kr\xde\xef\xf68CSY\xa9L\x9d\xf3\x9b\xeb\xc9\xc1\t\x1f\x17\xa36\x93\xb6Zj4\x81E\xde\xf8\xac\xcd\xf9<\x04\x97\t\xe1\xab9j\xf0]\xeb\xd0\xc4\xb7\x99%\r!^\xa9\x16\xad%\xe9\xc8V\xe8}\xd4\xe9F\x0cz\xbdc\xa1A\x19\x9e\x94\xa5\x95\xf7\xe9t)P\n\xa1\x98(\xf2\x819 \xa8\t\xdc|$R2E\xdaAP\x8aO\x18f\x01\x89\x01\x8b\x99\x1f\xa0\xf8r~\x17O\xb1\xb2F~a\xb16\xfdr\x94\x84\xb0\xd8k\xb9Z\x1a\xffgOi,\x99wPa\xce\x1d\xa1GZ!/\xd8\xadU\x06%\xeb\x80vC\xe6]\xa3\xc2\xde\x12\x17K]\xc6u\xb0\xfe\xe0\xf0\x88\x81\x89\x9f\x9b0\xd4@\x0b\xdf\xa9\xc3\x90\xbd\xad\x1f\xd8\xe6\xf1y\xb3~}\x7fy\xfa_6k\xe4Ti\xd7 k3e|\xa0\x9c_\x9e\x9e\x9fq\xb1\xe5\xc4v\xfeb\xb7\xdb\xe2\x03PK\x01\x02\x14\x03\x14\x00\x00\x00\x08\x00Y\x7fk]6W\xde\xdc\xa2\x00\x00\x00\x18\x01\x00\x00\x0b\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x01\x00\x00\x00\x00_rels/.relsPK\x01\x02\x14\x03\x14\x00\x00\x00\x08\x00Y\x7fk]\xfa\xdd\xff\x14(\x01\x00\x00\x01\x00\x10\x04\x11\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x01\xcb\x00\x00\x00word/document.xmlPK\x05\x06\x00\x00\x00\x00\x02\x00\x02\x00x\x00\x00\x00\"\x02\x00\x00\x00\x00";
const CAP_DOCX_WHY: &str = "unreadable: word/document.xml is 65 MB, more than is read";
const BOMB_DOCX_BYTES: &[u8] = b"PK\x03\x04\x14\x00\x00\x00\x08\x00Y\x7fk]6W\xde\xdc\xa2\x00\x00\x00\x18\x01\x00\x00\x0b\x00\x00\x00_rels/.rels\x8d\xcf;\x0e\xc20\x0c\x06\xe0\xabD\xde\xa9\x0b\x03B\xa8i\x17\x84\xd4\x15\x95\x03D\x89\x9bF4\x0f%\xe1u{20P\xc4\xc0h\xfb\xf7g\xb9\xe9\x1evf7\x8a\xc9x\xc7a]\xd5\xc0\xc8I\xaf\x8c\xd3\x1c\xce\xc3q\xb5\x83\xaemN4\x8b\\\x12i2!\xb1\xb2\xe2\x12\x87)\xe7\xb0GLr\"+R\xe5\x03\xb92\x19}\xb4\"\x972j\x0cB^\x84&\xdc\xd4\xf5\x16\xe3\xa7\x01K\x93\xf5\x8aC\xec\xd5\x1a\xd8\xf0\x0c\xf4\x8f\xed\xc7\xd1H:xy\xb5\xe4\xf2\x8f\x13_\x89\"\x8b\xa8)s\xb8\xfb\xa8P\xbd\xdbUa\x01\xdb\x06\x17/\xb6/PK\x03\x04\x14\x00\x00\x00\x08\x00Y\x7fk]\xfa\xdd\xff\x14(\x01\x00\x00\x0e\x02\x00\x00\x11\x00\x00\x00word/document.xml\x85\x91\xcd\x01\x00\x10\x04\x80_\xa5\xe9\x81\x9b\x14\xd0\x18\xb3\xb0K<\x887cD\x1f`v;,\x95\xedO\xa6\x85\xd5\xe7\xf0\xe0\x85x\xf3b|*\x0f\xc4\xc7\xb0\x8d\x12\xd4h\xb8L\xd3\xe9\xd7o&3\xa3\xf1\x9dn\xd8\n\xc9+kr\xde\xef\xf68CSY\xa9L\x9d\xf3\x9b\xeb\xc9\xc1\t\x1f\x17\xa36\x93\xb6Zj4\x81E\xde\xf8\xac\xcd\xf9<\x04\x97\t\xe1\xab9j\xf0]\xeb\xd0\xc4\xb7\x99%\r!^\xa9\x16\xad%\xe9\xc8V\xe8}\xd4\xe9F\x0cz\xbdc\xa1A\x19\x9e\x94\xa5\x95\xf7\xe9t)P\n\xa1\x98(\xf2\x819 \xa8\t\xdc|$R2E\xdaAP\x8aO\x18f\x01\x89\x01\x8b\x99\x1f\xa0\xf8r~\x17O\xb1\xb2F~a\xb16\xfdr\x94\x84\xb0\xd8k\xb9Z\x1a\xffgOi,\x99wPa\xce\x1d\xa1GZ!/\xd8\xadU\x06%\xeb\x80vC\xe6]\xa3\xc2\xde\x12\x17K]\xc6u\xb0\xfe\xe0\xf0\x88\x81\x89\x9f\x9b0\xd4@\x0b\xdf\xa9\xc3\x90\xbd\xad\x1f\xd8\xe6\xf1y\xb3~}\x7fy\xfa_6k\xe4Ti\xd7 k3e|\xa0\x9c_\x9e\x9e\x9fq\xb1\xe5\xc4v\xfeb\xb7\xdb\xe2\x03PK\x01\x02\x14\x03\x14\x00\x00\x00\x08\x00Y\x7fk]6W\xde\xdc\xa2\x00\x00\x00\x18\x01\x00\x00\x0b\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x01\x00\x00\x00\x00_rels/.relsPK\x01\x02\x14\x03\x14\x00\x00\x00\x08\x00Y\x7fk]\xfa\xdd\xff\x14(\x01\x00\x00\x0e\x02\x00\x00\x11\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x80\x01\xcb\x00\x00\x00word/document.xmlPK\x05\x06\x00\x00\x00\x00\x02\x00\x02\x00x\x00\x00\x00\"\x02\x00\x00\x00\x00";
const BOMB_DOCX_WHY: &str = "unreadable: can't read word/document.xml";
const NOTES_DOCX_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("_rels/.rels", b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>", true),
    ("word/document.xml", b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>First paragraph</w:t></w:r><w:r><w:tab/><w:t>after a tab</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t><w:br/><w:t>after a break</w:t></w:r></w:p><w:p><w:r><w:t>Runs</w:t></w:r><w:r><w:t xml:space=\"preserve\"> joined &amp; split</w:t></w:r></w:p><w:p><w:r><w:t>Numbers 1234 and &lt;marks&gt; \xe2\x9c\x93 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e</w:t></w:r></w:p><w:p><w:fldSimple w:instr=\"PAGE\"/></w:p></w:body></w:document>", true),
    ("word/styles.xml", b"<styles><style>invisible styles text</style></styles>", true)
];
const NOTES_DOCX_TEXT: &str = "First paragraph after a tab\nSecond\nafter a break\nRuns joined & split\nNumbers 1234 and <marks> ✓ 日本語";
const NOTES_DOCX_C50: &[&str] = &[
    "First paragraph after a tab\nSecond\nafter a break",
    "a break\nRuns joined & split\nNumbers 1234 and",
    "1234 and <marks> ✓ 日本語",
];
const NOTES_DOCX_C12: &[&str] = &["First paragr", "aph after a"];
const NOTES_DOCX_EXCERPT: &str = "First paragraph after a tab Second after a break Runs joined & split Numbers 1234 and <marks> ✓ 日本語";
const STORED_DOCX_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("word/document.xml", b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>Stored entry</w:t></w:r><w:r><w:tab/><w:t>after a tab</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t><w:br/><w:t>after a break</w:t></w:r></w:p><w:p><w:r><w:t>Runs</w:t></w:r><w:r><w:t xml:space=\"preserve\"> joined &amp; split</w:t></w:r></w:p><w:p><w:r><w:t>Numbers 1234 and &lt;marks&gt; \xe2\x9c\x93 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e</w:t></w:r></w:p><w:p><w:fldSimple w:instr=\"PAGE\"/></w:p></w:body></w:document>", false),
    ("_rels/.rels", b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>", false)
];
const STORED_DOCX_TEXT: &str = "Stored entry after a tab\nSecond\nafter a break\nRuns joined & split\nNumbers 1234 and <marks> ✓ 日本語";
const STORED_DOCX_C50: &[&str] = &[
    "Stored entry after a tab\nSecond\nafter a break",
    "a break\nRuns joined & split\nNumbers 1234 and",
    "1234 and <marks> ✓ 日本語",
];
const STORED_DOCX_C12: &[&str] = &["Stored entry", "after a"];
const STORED_DOCX_EXCERPT: &str = "Stored entry after a tab Second after a break Runs joined & split Numbers 1234 and <marks> ✓ 日本語";
const NOTES_ODT_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("mimetype", b"application/vnd.oasis.opendocument.text", true),
    ("content.xml", b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\"><office:body><office:text><text:h>Heading</text:h><text:p>First paragraph<text:tab/>with a tab</text:p><text:p>Line<text:line-break/>break and<text:s/>\x08space</text:p><text:p text:style-name=\"X\">Styled &amp; marks</text:p></office:text></office:body></office:document-content>", true)
];
const NOTES_ODT_TEXT: &str =
    "Heading\nFirst paragraph with a tab\nLine\nbreak and \u{08}space\nStyled & marks";
const NOTES_ODT_C50: &[&str] = &[
    "Heading\nFirst paragraph with a tab\nLine\nbreak and",
    "break and \u{08}space\nStyled & marks",
];
const NOTES_ODT_C12: &[&str] = &["Heading", "First parag"];
const NOTES_ODT_EXCERPT: &str =
    "Heading First paragraph with a tab Line break and \u{08}space Styled & marks";
const GRID_XLSX_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("_rels/.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>", true),
    ("xl/workbook.xml", b"<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Numbers\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"Notes\" sheetId=\"2\" r:id=\"rId2\"/></sheets></workbook>", true),
    ("xl/_rels/workbook.xml.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet2.xml\"/><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings\" Target=\"../sharedStrings.xml\"/></Relationships>", true),
    ("xl/sharedStrings.xml", b"<?xml version=\"1.0\"?><sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"3\" uniqueCount=\"3\"><si><t>Shared one</t></si><si><t xml:space=\"preserve\">run<t> piece</t> joined</t><rPh><t>reading skipped</t></rPh></si><si><r><t>part</t></r><t> tail</t></si></sst>", true),
    ("xl/worksheets/sheet1.xml", b"<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\" t=\"s\"><v>1</v></c><c r=\"C1\" t=\"s\"><v>2</v></c><c r=\"D1\" t=\"s\"><v>99</v></c></row><row r=\"2\"><c r=\"A2\"><v>1234.5</v></c><c r=\"B2\" t=\"b\"><v>1</v></c><c r=\"C2\" t=\"e\"><v>#N/A</v></c></row><row r=\"3\"><c r=\"A3\" t=\"inlineStr\"><is><t>inline text</t><rPh><t>skip</t></rPh></is></c><c r=\"B3\"><f>=A2*2</f><v>2469</v></c><c r=\"D3\" t=\"str\"><v>formula string</v></c></row><row r=\"4\"><c r=\"A4\" t=\"s\"><v>2</v></c></row></sheetData></worksheet>", true),
    ("xl/worksheets/sheet2.xml", b"<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row><c t=\"s\"><v>1</v></c></row></sheetData></worksheet>", true)
];
const GRID_XLSX_TEXT: &str = "Numbers\n1234.5\ninline text\t2469\tformula string\n\nNotes";
const GRID_XLSX_C50: &[&str] = &[
    "Numbers\n1234.5\ninline text 2469 formula string",
    "string\n\nNotes",
];
const GRID_XLSX_C12: &[&str] = &["Numbers", "1234.5"];
const GRID_XLSX_EXCERPT: &str = "Numbers 1234.5 inline text 2469 formula string Notes";
const SPARSE_XLSX_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("_rels/.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>", true),
    ("xl/workbook.xml", b"<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Notes\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>", true),
    ("xl/_rels/workbook.xml.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet3.xml\"/></Relationships>", true),
    ("xl/sharedStrings.xml", b"<?xml version=\"1.0\"?><sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"3\" uniqueCount=\"3\"><si><t>Shared one</t></si><si><t xml:space=\"preserve\">run<t> piece</t> joined</t><rPh><t>reading skipped</t></rPh></si><si><r><t>part</t></r><t> tail</t></si></sst>", true),
    ("xl/worksheets/sheet3.xml", b"<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row><c t=\"s\"><v>0</v></c><c t=\"b\"><v>1</v></c><c t=\"e\"><v>#N/A</v></c><c><f>=1+1</f><v>2</v></c></row><row/><row><c t=\"inlineStr\"><is><t>inline</t><rPh><t>skip</t></rPh></is></c></row></sheetData></worksheet>", true)
];
const SPARSE_XLSX_TEXT: &str = "Notes\n2\ninline";
const SPARSE_XLSX_C50: &[&str] = &["Notes\n2\ninline"];
const SPARSE_XLSX_C12: &[&str] = &["Notes\n2", "2\ninline"];
const SPARSE_XLSX_EXCERPT: &str = "Notes 2 inline";
const DECK_PPTX_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("_rels/.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"ppt/presentation.xml\"/></Relationships>", true),
    ("ppt/presentation.xml", b"<?xml version=\"1.0\"?><p:presentation xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId2\"/><p:sldId id=\"257\" r:id=\"rId1\"/></p:sldIdLst></p:presentation>", true),
    ("ppt/_rels/presentation.xml.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"slides/slide1.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"slides/slide2.xml\"/></Relationships>", true),
    ("ppt/slides/slide1.xml", b"<?xml version=\"1.0\"?><p:sld xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Slide one title</a:t></a:r><a:br/><a:r><a:t>second run</a:t></a:r></a:p><a:p><a:r><a:t>Second paragraph &amp; marks</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:txBody><p:ph type=\"title\"/><a:p><a:fld id=\"{GUID}\" type=\"slidenum\"><a:t>4</a:t></a:fld><a:t> after field</a:t></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>", true),
    ("ppt/slides/_rels/slide1.xml.rels", b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId9\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide\" Target=\"../notesSlides/notesSlide1.xml\"/></Relationships>", true),
    ("ppt/notesSlides/notesSlide1.xml", b"<?xml version=\"1.0\"?><p:notesSlide><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:t>Speaker notes for slide one</a:t></a:p><a:p><a:fld id=\"x\" type=\"datetime\"><a:t>date skipped</a:t></a:fld></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notesSlide>", true),
    ("ppt/slides/slide2.xml", b"<?xml version=\"1.0\"?><p:sld xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:t>Slide two &lt;text&gt;</a:t></a:p></p:txBody></p:sp><p:sp><p:txBody><p:ph type=\"title\"/><a:p><a:fld id=\"{GUID}\" type=\"slidenum\"><a:t>4</a:t></a:fld><a:t> after field</a:t></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>", true)
];
const DECK_PPTX_TEXT: &str = "Slide two <text>\nafter field\n\nSlide one title\nsecond run\nSecond paragraph & marks\nafter field\nSpeaker notes for slide one";
const DECK_PPTX_C50: &[&str] = &[
    "Slide two <text>\nafter field\n\nSlide one title",
    "one title\nsecond run\nSecond paragraph & marks",
    "& marks\nafter field\nSpeaker notes for slide one",
];
const DECK_PPTX_C12: &[&str] = &["Slide two", "two <text>"];
const DECK_PPTX_EXCERPT: &str = "Slide two <text> after field Slide one title second run Second paragraph & marks after field Speaker notes for slide one";
const GRID_ODS_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("mimetype", b"application/vnd.oasis.opendocument.spreadsheet", true),
    ("content.xml", b"<?xml version=\"1.0\"?><office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\"><office:body><office:spreadsheet><table:table table:name=\"Budget\"><table:table-column/><table:table-column/><table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell office:value-type=\"float\"><text:p>Amount</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell office:value-type=\"string\"><text:p>Rent<text:tab/>monthly</text:p><text:p>second paragraph</text:p></table:table-cell><table:table-cell office:value-type=\"boolean\" office:boolean-value=\"true\"><text:p>TRUE</text:p></table:table-cell></table:table-row><table:table-row><table:covered-table-cell/><table:table-cell><text:p>empty above, text here</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell office:value-type=\"string\"><text:p>dated</text:p><text:annotation><text:p>note author</text:p></text:annotation></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>", true)
];
const GRID_ODS_TEXT: &str = "Budget\nName\tAmount\nRent monthly second paragraph\nempty above, text here\ndated note author";
const GRID_ODS_C50: &[&str] = &[
    "Budget\nName Amount\nRent monthly second paragraph",
    "paragraph\nempty above, text here\ndated note",
    "note author",
];
const GRID_ODS_C12: &[&str] = &["Budget\nName", "Amount"];
const GRID_ODS_EXCERPT: &str =
    "Budget Name Amount Rent monthly second paragraph empty above, text here dated note author";
const DECK_ODP_ENTRIES: &[(&str, &[u8], bool)] = &[
    ("mimetype", b"application/vnd.oasis.opendocument.presentation", true),
    ("content.xml", b"<?xml version=\"1.0\"?><office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:draw=\"urn:oasis:names:tc:opendocument:xmlns:drawing:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" xmlns:presentation=\"urn:oasis:names:tc:opendocument:xmlns:presentation:1.0\"><office:body><office:presentation><draw:page draw:name=\"page1\"><draw:frame><draw:text-box><text:h>Title</text:h><text:p>Body<text:line-break/>text<text:tab/>tabbed</text:p></draw:text-box></draw:frame><presentation:notes><draw:frame><draw:text-box><text:p>Notes text</text:p></draw:text-box></draw:frame></presentation:notes></draw:page><draw:page draw:name=\"page2\"><draw:frame><draw:text-box><text:p>Second page</text:p></draw:text-box></draw:frame></draw:page></office:presentation></office:body></office:document-content>", true)
];
const DECK_ODP_TEXT: &str = "Title\nBody\ntext tabbed\nNotes text\n\nSecond page";
const DECK_ODP_C50: &[&str] = &["Title\nBody\ntext tabbed\nNotes text\n\nSecond page"];
const DECK_ODP_C12: &[&str] = &["Title\nBody", "text tabbed"];
const DECK_ODP_EXCERPT: &str = "Title Body text tabbed Notes text Second page";
const MISSING_ODT_ENTRIES: &[(&str, &[u8], bool)] = &[(
    "mimetype",
    b"application/vnd.oasis.opendocument.text",
    false,
)];
const MISSING_ODT_WHY: &str = "unreadable: no content.xml inside";
const MAIL_CASES: &[(&str, &[u8], &str)] = &[
    ("b64.emlx", b"Subject: base64 body\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nYmFzZTY0IGJvZHkgw6kgd2l0aCBhY2NlbnRz\r\n", "Subject: base64 body\n\nbase64 body é with accents"),
    ("cp1252.emlx", b"Subject: =?windows-1252?Q?Euro=80_quote=93?=\r\nContent-Type: text/plain; charset=windows-1252\r\nContent-Transfer-Encoding: 8bit\r\n\r\n8bit body: \x80\x93\x94\x97 and \xe9 as bytes.\r\n", "Subject: Euro€ quote“\n\n8bit body: Â€Â“Â”Â— and Ã© as bytes.\r\n"),
    ("deep.emlx", b"Subject: deep nesting\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\ninnermost text\r\n", "Subject: deep nesting"),
    ("empty.emlx", b"Subject: only headers\r\nFrom: a@example.com\r\n\r\n", "From: a@example.com\nSubject: only headers"),
    ("html.emlx", b"Subject: html part\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<html><head><style>p{}</style></head><body><p>HTML body &amp; text</p><div>block two</div><script>hidden()</script></body></html>\r\n", "Subject: html part\n\nHTML body & text\nblock two"),
    ("multi.emlx", b"Subject: Multipart =?utf-8?Q?mail?=\r\nFrom: a@example.com\r\nContent-Type: multipart/mixed; boundary=\"b=1;2\"\r\n\r\n--b=1;2\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nFirst text part.\r\n--b=1;2\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nPHA+IEhUTUwgcGFydCAmYW1wOyBtb3JlPC9wPg==\r\n--b=1;2\r\nContent-Type: application/pdf; name=\"chart.pdf\"\r\nContent-Disposition: attachment; filename=chart.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERw==\r\n--b=1;2--\r\n", "From: a@example.com\nSubject: Multipart mail\n\nFirst text part.\r\n\n\nHTML part & more"),
    ("plain.emlx", b"X-Mailer: Apple Mail (2.1234)\r\nFrom: =?utf-8?Q=Alice=20R=C3=A9sum=C3=A9?= <alice@example.com>\r\nTo: Bob <bob@example.com>\r\nSubject: Re: =?utf-8?B?Q29waWVkIFBhdGg=?=\r\n =?utf-8?Q?_tail?=\r\nDate: Tue, 4 Nov 2025 09:00:00 +0100\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 7bit\r\n\r\nBody line one with caf\xc3\xa9 in utf-8.\r\nBody line two.\r\n", "From: =utf-8?Q=Alice=20R=C3=A9sum=C3=A9?= <alice@example.com>\nTo: Bob <bob@example.com>\nSubject: Re: Copied Path  tail\nDate: Tue, 4 Nov 2025 09:00:00 +0100\n\nBody line one with café in utf-8.\r\nBody line two.\r\n"),
    ("quoted.emlx", b"Subject: =?utf-8?Q?Quoted=20printable?=\r\nContent-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nThis is a long line broken=3Dwith an equals and =\r\nthen =E9 as latin-1, =C3=A9 as the utf-8 pair, =3Ddone, and a stray = here.\r\n", "Subject: Quoted printable\n\nThis is a long line broken=with an equals and then é as latin-1, Ã© as the utf-8 pair, =done, and a stray = here.\r\n"),
    // F11: the charset name is the FIRST NON-EMPTY piece of the parameter (Swift's
    // split omits empty pieces). The leading-colon form `charset=:iso-8859-1` names
    // Latin-1 to the engine; the audit-base named it "" and read the body as UTF-8
    // (the base64-decoded byte 0xE9 becomes U+FFFD there). Fails-before: this fixture
    // answered "caf\u{FFFD}" on the audit-base. The canonical `iso-8859-1:1987`
    // (year suffix, first piece non-empty) is audit-base-EQUIVALENT: the unit test
    // pins the equivalence, the pipeline pins the divergence.
    ("colon.emlx", b"Subject: =?iso-8859-1:1987?Q?caf=E9?=\r\nContent-Type: text/plain; charset=:iso-8859-1\r\nContent-Transfer-Encoding: base64\r\n\r\nY2Fm6Q==\r\n", "Subject: caf\u{e9}\n\ncaf\u{e9}"),
    ("stranger.emlx", b"\x00\x01\x02binary shape no header at all\x00\x02\r\nsecond\r\n", "\u{00}\u{01}\u{02}binary shape no header at all\u{00}\u{02}\r\nsecond\r\n")
];

fn temp_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-doc-{name}"));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn write(folder: &Path, name: &str, body: &[u8]) -> PathBuf {
    let path = folder.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
    path
}

fn write_zip(folder: &Path, name: &str, entries: &[(&str, &[u8], bool)]) -> PathBuf {
    write(folder, name, &zip_bytes(entries))
}

/// A zip file written by hand — local headers, the central directory, the EOCD: the layout the
/// engine's miniz reader parses, with the CRC-32 miniz verifies at extract. `deflate` is method 8
/// (a raw RFC 1951 stream, what the port inflates), else method 0 (stored).
fn zip_bytes(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data, deflate) in entries {
        let method = if *deflate { 8u8 } else { 0u8 };
        let body = if *deflate {
            deflate_bytes(data)
        } else {
            data.to_vec()
        };
        let crc = crc32(data);
        let offset = out.len() as u32;
        out.extend(b"PK\x03\x04");
        out.extend([20, 0, 0, 0, method, 0, 0, 0, 0, 0]); // version, flags, method, time, date (10)
        out.extend(crc.to_le_bytes());
        out.extend((body.len() as u32).to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend((name.len() as u16).to_le_bytes());
        out.extend(0u16.to_le_bytes()); // no extra field
        out.extend(name.as_bytes());
        out.extend(&body);
        central.extend(b"PK\x01\x02");
        central.extend([0, 0, 20, 0, 0, 0, method, 0, 0, 0, 0, 0]);
        central.extend(crc.to_le_bytes());
        central.extend((body.len() as u32).to_le_bytes());
        central.extend((data.len() as u32).to_le_bytes());
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // extra, comment, disk, attributes
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend(&central);
    out.extend(b"PK\x05\x06");
    out.extend([0, 0, 0, 0]); // disk, and the disk the central directory starts on
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // no comment
    out
}

fn deflate_bytes(data: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// The checksum zip stores: `mz_zip_reader_extract_to_heap` verifies it, so a package with a
/// wrong one is refused on both sides.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// What the indexer makes of a file: its text, the chunks at the engine's probe sizes, and the
/// one-line excerpt for result lists.
fn answers(path: &Path) -> (String, Vec<String>, Vec<String>, String) {
    let text = doc::text_of(path).expect("the engine read this file");
    (
        text.clone(),
        doc::chunks(&text, 50, 10, 9),
        doc::chunks(&text, 12, 4, 2),
        doc::excerpt(&text, 240),
    )
}

fn why(path: &Path) -> String {
    match doc::text_of(path) {
        Err(error) => error.to_string(),
        Ok(text) => panic!("the engine refused this file; the port read: {text:?}"),
    }
}

#[test]
fn read_text_sniffs_the_bom_then_falls_back_to_latin1() {
    // Measured case by case against Foundation's `String(contentsOf:usedEncoding:)` on this box.
    let folder = temp_folder("encodings");
    let read = |name: &str, body: &[u8]| doc::read_text(&write(&folder, name, body)).unwrap();
    assert_eq!(read("bom.txt", BOM_UTF8_BYTES), BOM_UTF8_TEXT); // the BOM is dropped
    assert_eq!(read("le.txt", UTF16LE_BOM_BYTES), UTF16LE_BOM_TEXT);
    assert_eq!(read("be.txt", UTF16BE_BOM_BYTES), UTF16BE_BOM_TEXT);
    // The four-byte BOMs are checked first, so "FF FE 00 00" is UTF-32LE, not UTF-16LE with a tail.
    assert_eq!(read("u32.txt", UTF32LE_BOM_BYTES), UTF32LE_BOM_TEXT);
    // Without a BOM the bytes must be strict UTF-8, NULs included: UTF-16 text with no BOM reads
    // as a string full of U+0000 characters, and a U+FFFE in the file is kept as itself.
    assert_eq!(read("nul.txt", NUL_NO_BOM_BYTES), NUL_NO_BOM_TEXT);
    assert_eq!(read("fffe.txt", FFFE_BYTES), FFFE_TEXT);
    assert_eq!(read("accents.txt", HIGH_ACCENTS_BYTES), HIGH_ACCENTS_TEXT);
    // What is not UTF-8 is Latin-1 — every byte a character, Windows-1252's specials included,
    // which is why the euro and the quotes arrive as the C1 controls that they are.
    assert_eq!(read("pair.txt", INVALID_PAIR_BYTES), INVALID_PAIR_TEXT);
    assert_eq!(read("cp.txt", CP1252_BYTES), CP1252_TEXT);
    assert_eq!(read("latin.txt", LATIN1_BYTES), LATIN1_TEXT);
    assert_eq!(
        read("newlines.txt", ONLY_NEWLINES_BYTES),
        ONLY_NEWLINES_TEXT
    );
    let (text, chunks, _, excerpt) = answers(&write(&folder, "empty.txt", EMPTY_TXT_BYTES));
    assert_eq!((text.as_str(), excerpt.as_str()), ("", ""));
    assert!(chunks.is_empty());
}

#[test]
fn plain_files_answer_their_own_text() {
    let folder = temp_folder("plain");
    let (text, chunks, small, excerpt) = answers(&write(&folder, "plain.txt", PLAIN_BYTES));
    assert_eq!(text, PLAIN_TEXT);
    assert_eq!(chunks, PLAIN_C50.to_vec());
    assert_eq!(small, PLAIN_C12.to_vec());
    assert_eq!(excerpt, PLAIN_EXCERPT);
    // A markdown file is plain text (the extension is not one of the readers') and its long tail
    // is chunked at the engine's own settings.
    let (text, chunks, _, excerpt) = answers(&write(&folder, "notes.md", NOTES_MD_BYTES));
    assert_eq!(text, NOTES_MD_TEXT);
    assert_eq!(chunks, NOTES_MD_C50.to_vec());
    assert_eq!(excerpt, NOTES_MD_EXCERPT);
    assert_eq!(
        doc::chunks(&text, 1800, 200, 5_001),
        NOTES_MD_C1800.to_vec()
    );
    assert_eq!(
        doc::text_of(&write(&folder, "latin1.txt", LATIN1_BYTES)).unwrap(),
        LATIN1_TEXT
    );
}

#[test]
fn html_loses_scripts_styles_comments_and_tags() {
    let folder = temp_folder("html");
    let (text, chunks, small, excerpt) = answers(&write(&folder, "page.html", PAGE_HTML_BYTES));
    assert_eq!(text, PAGE_HTML_TEXT, "\n{text}");
    assert_eq!(chunks, PAGE_HTML_C50.to_vec());
    assert_eq!(small, PAGE_HTML_C12.to_vec());
    assert_eq!(excerpt, PAGE_HTML_EXCERPT);
    // Scripts, styles, comments and the head are gone; block ends become line breaks; the tags
    // left are spaces; `&amp;`, `&nbsp;`, `&mdash;` and the rest decode, `&#233;` and `&#x2764;`
    // too, and `&#xD800;` (a lone surrogate) and `&#xZZ;` become a space.
    assert_eq!(
        doc::text_of(&write(&folder, "page.HTM", PAGE_HTML_BYTES)).unwrap(), // lowercased extension
        PAGE_HTML_TEXT
    );
}

#[test]
fn rtf_drops_control_words_and_keeps_the_words() {
    let folder = temp_folder("rtf");
    let (text, chunks, _, excerpt) = answers(&write(&folder, "notes.rtf", NOTES_RTF_BYTES));
    assert_eq!(text, NOTES_RTF_TEXT, "\n{text}");
    assert_eq!(chunks, NOTES_RTF_C50.to_vec());
    assert_eq!(excerpt, NOTES_RTF_EXCERPT);
    // The reader's shapes, one file each: `ucN` gates the characters after a `uN` scalar and the
    // negative values wrap (a lone surrogate is not a scalar, so it drops); `par` and `tab` are
    // breaks; a `*` destination is skipped; the symbol escapes keep their character and the rest
    // answer nothing.
    let cases: [(&str, &[u8], &str, &[&str]); 5] = [
        ("rtf-uc.rtf", RTF_UC_BYTES, RTF_UC_TEXT, RTF_UC_C12),
        ("rtf-tab.rtf", RTF_TAB_BYTES, RTF_TAB_TEXT, RTF_TAB_C12),
        ("rtf-star.rtf", RTF_STAR_BYTES, RTF_STAR_TEXT, RTF_STAR_C12),
        ("rtf-esc.rtf", RTF_ESC_BYTES, RTF_ESC_TEXT, RTF_ESC_C12),
        (
            "rtf-empty.rtf",
            RTF_EMPTY_BYTES,
            RTF_EMPTY_TEXT,
            RTF_EMPTY_C12,
        ),
    ];
    for (name, body, want, small) in cases {
        let (text, _, small_got, _) = answers(&write(&folder, name, body));
        assert_eq!(text, want, "{name}");
        assert_eq!(small_got, small.to_vec(), "{name}");
    }
}

#[test]
fn docx_and_odt_read_their_own_xml() {
    let folder = temp_folder("word");
    // Paragraph ends become line breaks, a break tag a line break, a tab or space tag a space,
    // and the rest of the tags go through the same HTML strip as a web page.
    let (text, chunks, small, excerpt) =
        answers(&write_zip(&folder, "notes.docx", NOTES_DOCX_ENTRIES));
    assert_eq!(text, NOTES_DOCX_TEXT, "\n{text}");
    assert_eq!(chunks, NOTES_DOCX_C50.to_vec());
    assert_eq!(small, NOTES_DOCX_C12.to_vec());
    assert_eq!(excerpt, NOTES_DOCX_EXCERPT);
    assert_eq!(
        doc::text_of(&write_zip(&folder, "stored.docx", STORED_DOCX_ENTRIES)).unwrap(),
        STORED_DOCX_TEXT
    ); // a stored entry reads as well as a deflated one
    assert_eq!(
        doc::text_of(&write_zip(&folder, "notes.odt", NOTES_ODT_ENTRIES)).unwrap(),
        NOTES_ODT_TEXT
    );
}

#[test]
fn xlsx_names_each_sheet_and_lists_its_rows() {
    let folder = temp_folder("xlsx");
    let (text, chunks, _, excerpt) = answers(&write_zip(&folder, "grid.xlsx", GRID_XLSX_ENTRIES));
    assert_eq!(text, GRID_XLSX_TEXT, "\n{text}");
    assert_eq!(chunks, GRID_XLSX_C50.to_vec());
    assert_eq!(excerpt, GRID_XLSX_EXCERPT);
    // Sheets in the workbook's order (by `r:id` through the rels, not by file name); an empty row
    // is passed over; booleans and errors say nothing; a formula keeps its last value; inline
    // strings are read and their phonetic readings skipped; a shared-string index with no table
    // beside it says nothing.
    assert_eq!(
        doc::text_of(&write_zip(&folder, "sparse.xlsx", SPARSE_XLSX_ENTRIES)).unwrap(),
        SPARSE_XLSX_TEXT
    );
}

#[test]
fn pptx_orders_the_slides_and_adds_their_notes() {
    let folder = temp_folder("pptx");
    // The slide order is the deck's `sldId` list, each slide is followed by its notes page, and
    // fields (a slide number, a date) are left out.
    let (text, chunks, small, excerpt) =
        answers(&write_zip(&folder, "deck.pptx", DECK_PPTX_ENTRIES));
    assert_eq!(text, DECK_PPTX_TEXT, "\n{text}");
    assert_eq!(chunks, DECK_PPTX_C50.to_vec());
    assert_eq!(small, DECK_PPTX_C12.to_vec());
    assert_eq!(excerpt, DECK_PPTX_EXCERPT);
}

#[test]
fn ods_and_odp_read_their_content_xml() {
    let folder = temp_folder("odf");
    // Each sheet's name, then a line per row with its cells separated by tabs; boolean cells and
    // a comment's author and date left out; covered cells keep the row's alignment.
    let (text, chunks, _, excerpt) = answers(&write_zip(&folder, "grid.ods", GRID_ODS_ENTRIES));
    assert_eq!(text, GRID_ODS_TEXT, "\n{text}");
    assert_eq!(chunks, GRID_ODS_C50.to_vec());
    assert_eq!(excerpt, GRID_ODS_EXCERPT);
    // A presentation: each page's paragraphs (page numbers and dates skipped), its notes after it.
    assert_eq!(
        doc::text_of(&write_zip(&folder, "deck.odp", DECK_ODP_ENTRIES)).unwrap(),
        DECK_ODP_TEXT
    );
}

#[test]
fn the_office_refusals_say_what_the_engine_says() {
    let folder = temp_folder("refusals");
    assert_eq!(
        why(&write(&folder, "legacy.doc", LEGACY_DOC_BYTES)),
        LEGACY_DOC_WHY
    );
    assert_eq!(
        why(&write(&folder, "locked.xlsx", LOCKED_XLSX_BYTES)),
        LOCKED_XLSX_WHY
    ); // an OLE compound file: the zip inside it is encrypted
    assert_eq!(
        why(&write(&folder, "notzip.xlsx", NOTZIP_XLSX_BYTES)),
        NOTZIP_XLSX_WHY
    );
    // An archive whose central directory is empty is a zip: it simply has no part to read.
    assert_eq!(
        why(&write(&folder, "empty.xlsx", EMPTY_XLSX_BYTES)),
        EMPTY_XLSX_WHY
    );
    assert_eq!(
        why(&write_zip(&folder, "missing.odt", MISSING_ODT_ENTRIES)),
        MISSING_ODT_WHY
    );
    // An entry declaring more than maxEntrySize (64 MB) is refused before anything is allocated:
    // a zip of a few KB can declare gigabytes.
    assert_eq!(
        why(&write(&folder, "cap.docx", CAP_DOCX_BYTES)),
        CAP_DOCX_WHY
    );
    // A damaged entry stream fails the inflate exactly as miniz's extract fails.
    assert_eq!(
        why(&write(&folder, "bomb.docx", BOMB_DOCX_BYTES)),
        BOMB_DOCX_WHY
    );
}

#[test]
fn zip_entries_are_found_without_regard_to_case() {
    // miniz's `mz_zip_reader_locate_file(zip, name, nil, 0)` — flags 0 is case-insensitive, the
    // first match in central-directory order. Office writers have spelled the parts both ways.
    let folder = temp_folder("zipcase");
    let path = write_zip(&folder, "notes.docx", NOTES_DOCX_ENTRIES);
    let zip = zipfile::ZipArchive::open(&path).unwrap();
    let xml = zip.entry("word/document.xml").unwrap();
    assert_eq!(zip.entry("WORD/DOCUMENT.XML").unwrap(), xml);
    assert_eq!(zip.entry("Word/Document.xml").unwrap(), xml);
    assert!(zip.data("word/missing.xml").unwrap().is_none()); // no entry: nil, not an error
    assert_eq!(
        zip.required("word/styles.xml").unwrap(),
        zip.required("WORD/STYLES.XML").unwrap()
    );
}

#[test]
fn chunks_break_at_whitespace_and_overlap_a_little() {
    let folder = temp_folder("chunks");
    let (solid, _, _, _) = answers(&write(&folder, "solid.txt", SOLID_BYTES));
    // A line longer than the size is a chunk of its own, so a solid run is cut at the size, and
    // the limit stops the walk.
    assert_eq!(doc::chunks(&solid, 12, 4, 2), SOLID_C12.to_vec());
    assert_eq!(doc::chunks(&solid, 1800, 200, 5_001), SOLID_C1800.to_vec());
    assert_eq!(doc::chunks(&solid, 1800, 200, 2).len(), 2);
    // Runs of spaces (tab, no-break space) collapse to one; three newlines become two.
    let (spaces, chunks, small, _) = answers(&write(&folder, "spaces.txt", SPACES_BYTES));
    assert_eq!(spaces, SPACES_TEXT);
    assert_eq!(chunks, SPACES_C50.to_vec());
    assert_eq!(small, SPACES_C12.to_vec());
    // The window counts CHARACTERS, not bytes: a ZWJ family is one Character, so a window of 12
    // takes six of them, and the overlap starts the next chunk on a word boundary.
    let (family, chunks, small, _) = answers(&write(&folder, "family.txt", FAMILY_BYTES));
    assert_eq!(chunks, FAMILY_C50.to_vec());
    assert_eq!(small, FAMILY_C12.to_vec());
    assert_eq!(
        UnicodeSegmentation::graphemes(family.as_str(), true).count(),
        20
    );
    let pieces = doc::chunks(&family, 12, 4, 2);
    // Six families and the spaces between them, trimmed at the end: 11 Characters, 35 scalars.
    assert_eq!(
        UnicodeSegmentation::graphemes(pieces[0].as_str(), true).count(),
        11
    );
    assert!(
        pieces[0].chars().count() > 12,
        "the window is not counting scalars"
    );
    // An empty text (and a blank one) makes no chunks at all, and a limit of none answers none.
    assert_eq!(doc::chunks("", 50, 10, 9), Vec::<String>::new());
    assert_eq!(doc::chunks("  \n ", 50, 10, 9), Vec::<String>::new());
    assert_eq!(doc::chunks("a b", 50, 10, 0), Vec::<String>::new());
}

#[test]
fn excerpt_is_one_line_of_at_most_240_characters() {
    let folder = temp_folder("excerpt");
    let (_, _, _, line) = answers(&write(&folder, "notes.rtf", NOTES_RTF_BYTES));
    assert_eq!(line, NOTES_RTF_EXCERPT);
    // Whitespace runs become one space, the ends are trimmed, and a text longer than the shown
    // prefix answers its first 240 Characters and the ellipsis.
    assert!(line.ends_with('\u{2026}'));
    assert_eq!(
        UnicodeSegmentation::graphemes(line.trim_end_matches('\u{2026}'), true).count(),
        240
    );
    let (_, _, _, line) = answers(&write(&folder, "spaces.txt", SPACES_BYTES));
    assert_eq!(line, SPACES_EXCERPT);
    let (_, _, _, line) = answers(&write(&folder, "family.txt", FAMILY_BYTES));
    assert_eq!(line, FAMILY_EXCERPT); // each family counts as one Character
    let (_, _, _, line) = answers(&write(&folder, "empty.txt", EMPTY_TXT_BYTES));
    assert_eq!(line, "");
}

#[test]
fn mail_reads_its_headers_and_text_parts() {
    // Issue #146's file-reading slice: the From / To / Cc / Subject / Date header lines first
    // (continuation lines joined, RFC 2047 encoded words decoded), then the `text/*` parts — HTML
    // stripped, quoted-printable and base64 decoded, the declared charset honoured, Windows-1252
    // and Latin-1 read byte by byte — attachments left out, nested multiparts eight levels deep at
    // most, and a file whose shape is not a message at all keeps its raw text, which is what the
    // index held before this reader existed.
    let folder = temp_folder("mail");
    for &(name, body, want) in MAIL_CASES {
        let got = doc::text_of(&write(&folder, name, body)).unwrap();
        assert_eq!(got, *want, "{name}");
    }
}

#[test]
fn rtf_codepage_undefined_bytes() {
    // Swift's `specials` is a DICTIONARY of the 26 mapped bytes; everything else
    // takes `?? UInt32(byte)` (Docs+Portable.swift:203-212): the six undefined slots
    // answer " " (0x81, 0x8D, 0x90, 0x9D are not Unicode scalars at all), the rest
    // of the upper half is Latin-1 verbatim (0xA0 NBSP … 0xFF ÿ). The audit-base's
    // 32-entry array indexed by `byte - 0x80` PANICS on 0xA0…0xFF (index 127 of 32
    // — `\'ff` is an indexer crash) and answers the control byte for 0x81/0x8D/0x90/
    // 0x9D where the engine answers " ". Fails-before: the panic line (this fixture
    // ran on the audit-base map first — recorded in the commit note).
    let out =
        rtf::text("{\\rtf1\\ansi\\ansicpg1252 \\'81\\'8D\\'90\\'9D\\'a0\\'e9\\'ff\\'80\\'92\\par}");
    assert_eq!(
        out,
        concat!(" ", " ", " ", " ", "\u{a0}", "\u{e9}", "\u{ff}", "\u{20ac}", "\u{2019}", "\n"),
        "{out}"
    );
}

#[test]
fn rtf_hex_escapes_follow_swift_radix_parsing() {
    // F7's `\'hh` window is Swift's `UInt8(two chars, radix: 16)` over the TWO
    // scalars the reader slices (`RTF.text`, Docs+Portable.swift). Measured
    // exhaustively (swift-corelibs 6.4, 2026-11-15): a Swift oracle over ALL 1,089
    // windows of the window alphabet (hex, space, '+', '-', '_', tab, NBSP, 'x',
    // 'z', quote, 'g' - f7all.swift) against `u8::from_str_radix` (f7all.rs):
    // 1,088 of 1,089 windows AGREE. Accepted on both sides: the sign ("+4"->4,
    // "+41"->65) and the hex digits; refused on both sides: spaces (" 4"),
    // underscores ("4_", "_4", "1_", "+_"), '-', quote, NBSP, "0x". The ONE
    // divergence: Swift answers 0 for "-0" - a negative zero fits UInt8 - where
    // from_str_radix refuses. Fails-before (measured at 881d0c2, the raw
    // from_str_radix shape): `\'-0A` answers the literal "-0A" where the engine
    // answers NUL + "A". (An earlier spot-probe over-read `Int(_:radix:)` over
    // THREE-character slices - "4_"->4, "_41"->65 are 3-char facts; the reader's
    // window is TWO characters and the engine refuses both shapes. The ledger
    // records the refutation.)
    //
    // A refused window is skipped by ONE character (the quote) and its two
    // characters stay literal text; an accepted window eats two (Swift's i += 3).
    // `\'5\'\'` refuses TWICE (window "5\", then the tail): a window never
    // spans a quote into the NEXT escape - the second backslash starts one. The
    // fixture uses `\'5''` to put a real quote inside a window, and the space
    // after \ansicpg1252 is the control word's own (consumed, not text).
    let out = rtf::text("{\\rtf1\\ansi\\ansicpg1252 \\'41\\' +41A\\'5''\\' +_4\\'1_234\\'4_ \\' 4\\'ff\\'1234\\'_4\\'-4\\'0x4\\'1.2\\' \u{a0}4\\'-0A}");
    // Traced window by window against the oracle:
    // "41"->0x41 "A" | " +" refused -> " +41A" (plain tail) | "5'" refused (the
    // quote is not a hex digit) -> "5''" | " +" refused -> " +_4" | "1_" refused
    // -> "1_234" | "4_" refused -> "4_ " | " 4" refused -> " 4" |
    // "ff"->0xFF Latin-1 y-umlaut | "12"->0x12 + "34" | "_4" refused |
    // "-4" refused (underflow: -4 is not a UInt8) | "0x" refused -> "0x4" |
    // "1." refused -> "1.2" | " \u{a0}" refused -> " \u{a0}4" | "-0"->0 + "A".
    let want = concat!(
        "A", " +41A", "5''", " +_4", "1_234", "4_ ", " 4", "\u{ff}", "\u{12}", "34", "_4", "-4",
        "0x4", "1.2", " \u{a0}4", "\u{0}", "A",
    );
    assert_eq!(out, want, "{out}");
}
