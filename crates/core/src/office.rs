//! `OfficeText` (OfficeText.swift): spreadsheets and presentations (xlsx, pptx, ods, odp) read
//! for their words from the XML inside their zip, the same way on every system. A sheet comes
//! out as its name, then a line per row with its cells separated by tabs; a presentation as its
//! slides in order, each followed by its speaker notes. Formulas (their last value is kept),
//! styles, charts and pictures are left out. Old binary .xls and .ppt aren't read.

use std::collections::HashMap;

use crate::extract::ExtractError;
use crate::xmlscan::{Event, XMLScanner};
use crate::zipfile::ZipArchive;

/// Reading stops past what the index keeps of a document (`IndexOptions.maxChunks`, 5,000
/// chunks of ~1,800 characters), so a sheet of a million rows doesn't fill memory with text that
/// would be cut anyway.
pub const MAX_CHARACTERS: usize = 10_000_000;

pub fn text(path: &std::path::Path, ext: &str) -> Result<String, ExtractError> {
    // An Office file with a password is an OLE compound file holding the encrypted zip.
    if is_compound_file(path) {
        return Err(ExtractError::Locked);
    }
    let zip = ZipArchive::open(path)?;
    let mut out = Output::default();
    match ext {
        "xlsx" => workbook(&zip, &mut out)?,
        "pptx" => presentation(&zip, &mut out)?,
        "ods" => spreadsheet(&zip.required("content.xml")?, &mut out),
        _ => slides(&zip.required("content.xml")?, &mut out), // odp
    }
    Ok(trim_ws(&out.text))
}

fn is_compound_file(path: &std::path::Path) -> bool {
    let Ok(data) = std::fs::File::open(path).and_then(|mut file| {
        let mut head = [0u8; 8];
        use std::io::Read;
        file.read_exact(&mut head)?;
        Ok(head)
    }) else {
        return false;
    };
    data == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]
}

type Links = HashMap<String, (String, String)>;

/// Sheets in the workbook's order, cells with their shared strings or their last computed value.
fn workbook(zip: &ZipArchive, out: &mut Output) -> Result<(), ExtractError> {
    let book = main_part(zip, "xl/workbook.xml")?;
    let links = relationships(zip, &book)?;
    let mut shared: Vec<String> = Vec::new();
    if let Some((_, target)) = links
        .values()
        .find(|(kind, _)| kind.ends_with("/sharedStrings"))
    {
        if let Some(xml) = zip.data(target)? {
            shared = shared_strings(&xml);
        }
    }
    let mut scan = XMLScanner::new(zip.required(&book)?);
    let mut sheets: Vec<(String, String)> = Vec::new();
    while let Some(event) = scan.next() {
        if let Event::Open {
            name, attributes, ..
        } = &event
        {
            if local(name) == "sheet" {
                if let Some(id) = scan.attribute(":id", *attributes) {
                    if let Some((kind, target)) = links.get(&id) {
                        if kind.ends_with("/worksheet") {
                            sheets.push((
                                scan.attribute("name", *attributes).unwrap_or_default(),
                                target.clone(),
                            ));
                        }
                    }
                }
            }
        }
    }
    for (name, part) in sheets {
        if out.full() {
            break;
        }
        let Ok(Some(xml)) = zip.data(&part) else {
            continue;
        };
        out.paragraph();
        out.line(&name);
        worksheet(&xml, &shared, out);
    }
    Ok(())
}

/// The table of strings cells point at: the text of each `<si>`, its runs joined, phonetic
/// readings left out.
fn shared_strings(xml: &[u8]) -> Vec<String> {
    let mut scan = XMLScanner::new(xml.to_vec());
    let mut strings: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    let mut phonetic = 0i32;
    while let Some(event) = scan.next() {
        match event {
            Event::Open { name, empty, .. } => {
                match local(&name).as_str() {
                    "si" => current = String::new(),
                    "t" => in_text = !empty,
                    "rPh" if !empty => phonetic += 1,
                    _ => {}
                }
                if empty && local(&name) == "si" {
                    strings.push(String::new());
                }
            }
            Event::Close { name } => match local(&name).as_str() {
                "si" => strings.append(&mut vec![std::mem::take(&mut current)]),
                "t" => in_text = false,
                "rPh" => phonetic -= 1,
                _ => {}
            },
            Event::Text { range, raw } => {
                if in_text && phonetic == 0 {
                    current.push_str(&scan.string(range, raw));
                }
            }
        }
    }
    strings
}

fn worksheet(xml: &[u8], shared: &[String], out: &mut Output) {
    let mut scan = XMLScanner::new(xml.to_vec());
    let mut row: Vec<String> = Vec::new();
    let mut kind = "n".to_string();
    let mut value = String::new();
    let mut in_value = false;
    let mut inline = false;
    let mut in_text = false;
    let mut phonetic = 0i32;
    while !out.full() {
        let Some(event) = scan.next() else { break };
        match event {
            Event::Open {
                name,
                attributes,
                empty,
            } => match local(&name).as_str() {
                "row" => row.clear(),
                "c" => {
                    kind = scan
                        .attribute("t", attributes)
                        .unwrap_or_else(|| "n".into());
                    value = String::new();
                }
                "v" => in_value = !empty,
                "is" => inline = !empty,
                "t" => in_text = !empty,
                "rPh" if !empty => phonetic += 1,
                _ => {}
            },
            Event::Close { name } => match local(&name).as_str() {
                "row" => {
                    if !row.is_empty() {
                        out.line(&row.join("\t"));
                    }
                }
                "c" => {
                    let shown: String = match kind.as_str() {
                        // A shared string index; a missing one says nothing.
                        "s" => crate::xmlscan::parse_radix(&trim_ws(&value), 10)
                            .and_then(|index| shared.get(index as usize))
                            .cloned()
                            .unwrap_or_default(),
                        "b" | "e" => String::new(), // TRUE/FALSE and #N/A say nothing to search for
                        _ => value.clone(), // numbers, dates (as day counts), strings computed by formulas
                    };
                    let cell = trim_ws(&shown);
                    if !cell.is_empty() {
                        row.push(cell);
                    }
                }
                "v" => in_value = false,
                "is" => inline = false,
                "t" => in_text = false,
                "rPh" => phonetic -= 1,
                _ => {}
            },
            Event::Text { range, raw } => {
                if in_value || (inline && in_text && phonetic == 0) {
                    value.push_str(&scan.string(range, raw));
                }
            }
        }
    }
}

/// Slides in the presentation's order (not their file names'), each followed by its notes.
fn presentation(zip: &ZipArchive, out: &mut Output) -> Result<(), ExtractError> {
    let deck = main_part(zip, "ppt/presentation.xml")?;
    let links = relationships(zip, &deck)?;
    let mut scan = XMLScanner::new(zip.required(&deck)?);
    let mut slides: Vec<String> = Vec::new();
    while let Some(event) = scan.next() {
        if let Event::Open {
            name, attributes, ..
        } = &event
        {
            if local(name) == "sldId" {
                if let Some(id) = scan.attribute(":id", *attributes) {
                    if let Some((kind, target)) = links.get(&id) {
                        if kind.ends_with("/slide") {
                            slides.push(target.clone());
                        }
                    }
                }
            }
        }
    }
    for slide in slides {
        if out.full() {
            break;
        }
        let Ok(Some(xml)) = zip.data(&slide) else {
            continue;
        };
        out.paragraph();
        drawing_text(&xml, out);
        let links = relationships(zip, &slide)?;
        if let Some((_, target)) = links
            .values()
            .find(|(kind, _)| kind.ends_with("/notesSlide"))
        {
            if let Ok(Some(xml)) = zip.data(target) {
                drawing_text(&xml, out);
            }
        }
    }
    Ok(())
}

/// The text of a slide or notes page: `<a:t>` runs, a line per paragraph. Fields (the slide
/// number on a notes page, a date) are left out.
fn drawing_text(xml: &[u8], out: &mut Output) {
    let mut scan = XMLScanner::new(xml.to_vec());
    let mut paragraph = String::new();
    let mut in_text = false;
    let mut field = 0i32;
    while !out.full() {
        let Some(event) = scan.next() else { break };
        match event {
            Event::Open { name, empty, .. } => match local(&name).as_str() {
                "t" => in_text = !empty,
                "br" => paragraph.push('\n'),
                "fld" if !empty => field += 1,
                _ => {}
            },
            Event::Close { name } => match local(&name).as_str() {
                "t" => in_text = false,
                "fld" => field -= 1,
                "p" => {
                    let line = trim_ws(&paragraph);
                    if !line.is_empty() {
                        out.line(&line);
                    }
                    paragraph = String::new();
                }
                _ => {}
            },
            Event::Text { range, raw } => {
                if in_text && field == 0 {
                    paragraph.push_str(&scan.string(range, raw));
                }
            }
        }
    }
}

/// The package's main part (`xl/workbook.xml`, `ppt/presentation.xml`) as `_rels/.rels` names it.
fn main_part(zip: &ZipArchive, default: &str) -> Result<String, ExtractError> {
    let links = relationships(zip, "")?;
    Ok(links
        .values()
        .find(|(kind, _)| kind.ends_with("/officeDocument"))
        .map(|(_, target)| target.clone())
        .unwrap_or_else(|| default.to_string()))
}

/// A part's links to other parts, by id, from the `_rels` file beside it, with their targets as
/// zip entry names.
fn relationships(zip: &ZipArchive, part: &str) -> Result<Links, ExtractError> {
    let folder = part.rsplit_once('/').map(|(head, _)| head).unwrap_or("");
    let file = part.rsplit('/').next().unwrap_or("");
    let rels = if folder.is_empty() {
        String::new()
    } else {
        format!("{folder}/")
    } + "_rels/"
        + file
        + ".rels";
    let Some(xml) = zip.data(&rels)? else {
        return Ok(HashMap::new());
    };
    let mut scan = XMLScanner::new(xml);
    let mut links = HashMap::new();
    while let Some(event) = scan.next() {
        if let Event::Open {
            name, attributes, ..
        } = &event
        {
            if local(name) != "Relationship" {
                continue;
            }
            if scan.attribute("TargetMode", *attributes).as_deref() == Some("External") {
                continue;
            }
            let (Some(id), Some(target)) = (
                scan.attribute("Id", *attributes),
                scan.attribute("Target", *attributes),
            ) else {
                continue;
            };
            links.insert(
                id,
                (
                    scan.attribute("Type", *attributes).unwrap_or_default(),
                    resolve(&target, folder),
                ),
            );
        }
    }
    Ok(links)
}

/// A relationship's target as a zip entry name: absolute ones start at the package's root,
/// others at the part's folder ("../notesSlides/notesSlide1.xml" from ppt/slides).
pub fn resolve(target: &str, folder: &str) -> String {
    let mut path: Vec<String> = if target.starts_with('/') {
        Vec::new()
    } else {
        folder
            .split('/')
            .filter(|piece| !piece.is_empty())
            .map(String::from)
            .collect()
    };
    for piece in target.split('/') {
        match piece {
            "." => continue,
            ".." => {
                path.pop();
            }
            _ => path.push(piece.to_string()),
        }
    }
    path.join("/")
}

/// Each sheet's name, then its rows, cells separated by tabs. Comments' authors and dates are
/// left out.
fn spreadsheet(xml: &[u8], out: &mut Output) {
    let mut scan = XMLScanner::new(xml.to_vec());
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut paragraph = String::new();
    let mut in_cell = false;
    let mut boolean = false;
    let mut in_paragraph = 0i32;
    let mut skipped = 0i32;
    while !out.full() {
        let Some(event) = scan.next() else { break };
        match event {
            Event::Open {
                name,
                attributes,
                empty,
            } => match local(&name).as_str() {
                "table" => {
                    out.paragraph();
                    let name = scan.attribute("table:name", attributes).unwrap_or_default();
                    out.line(&name);
                }
                "table-row" => row.clear(),
                "table-cell" | "covered-table-cell" => {
                    // TRUE/FALSE say nothing to search for, as in xlsx.
                    boolean = scan.attribute("office:value-type", attributes).as_deref()
                        == Some("boolean");
                    in_cell = !empty;
                    cell = String::new();
                }
                "p" | "h" => {
                    if !empty {
                        in_paragraph += 1;
                        paragraph = String::new();
                    }
                }
                "s" | "tab" | "line-break" => {
                    if in_paragraph > 0 {
                        paragraph.push(' ');
                    }
                }
                "creator" | "date" if !empty => skipped += 1,
                _ => {}
            },
            Event::Close { name } => match local(&name).as_str() {
                "table-row" => {
                    if !row.is_empty() {
                        out.line(&row.join("\t"));
                    }
                }
                "table-cell" | "covered-table-cell" => {
                    let text = trim_ws(&cell);
                    if !text.is_empty() && !boolean {
                        row.push(text);
                    }
                    in_cell = false;
                }
                "p" | "h" => {
                    in_paragraph -= 1;
                    let text = trim_ws(&paragraph);
                    if text.is_empty() {
                        continue;
                    }
                    if in_cell {
                        cell.push_str(if cell.is_empty() { "" } else { " " });
                        cell.push_str(&text);
                    } else {
                        out.line(&text);
                    }
                }
                "creator" | "date" => skipped -= 1,
                _ => {}
            },
            Event::Text { range, raw } => {
                if in_paragraph > 0 && skipped == 0 {
                    paragraph.push_str(&scan.string(range, raw));
                }
            }
        }
    }
}

/// Each page's text, a line per paragraph, its notes at the end of it. Page numbers and dates
/// are left out.
fn slides(xml: &[u8], out: &mut Output) {
    let mut scan = XMLScanner::new(xml.to_vec());
    let mut paragraph = String::new();
    let mut in_paragraph = 0i32;
    let mut skipped = 0i32;
    while !out.full() {
        let Some(event) = scan.next() else { break };
        match event {
            Event::Open { name, empty, .. } => match local(&name).as_str() {
                "page" => out.paragraph(),
                "p" | "h" => {
                    if !empty {
                        in_paragraph += 1;
                        paragraph = String::new();
                    }
                }
                "s" | "tab" => {
                    if in_paragraph > 0 {
                        paragraph.push(' ');
                    }
                }
                "line-break" => {
                    if in_paragraph > 0 {
                        paragraph.push('\n');
                    }
                }
                "page-number" | "date-time" | "creator" | "date" if !empty => skipped += 1,
                _ => {}
            },
            Event::Close { name } => match local(&name).as_str() {
                "p" | "h" => {
                    in_paragraph -= 1;
                    let text = trim_ws(&paragraph);
                    if !text.is_empty() {
                        out.line(&text);
                    }
                    paragraph = String::new();
                }
                "page-number" | "date-time" | "creator" | "date" => skipped -= 1,
                _ => {}
            },
            Event::Text { range, raw } => {
                if in_paragraph > 0 && skipped == 0 {
                    paragraph.push_str(&scan.string(range, raw));
                }
            }
        }
    }
}

/// A tag's name without its namespace prefix ("a:t" → "t"): writers don't all use the same
/// prefixes.
fn local(name: &str) -> String {
    match name.rfind(':') {
        Some(at) => name[at + 1..].to_string(),
        None => name.to_string(),
    }
}

/// `.trimmingCharacters(in: .whitespacesAndNewlines)` over CHARACTERS (grapheme clusters):
/// a cluster trims only when all its scalars are whitespace.
pub fn trim_ws(text: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    let clusters: Vec<&str> = text.graphemes(true).collect();
    let is_ws = |cluster: &str| cluster.chars().all(char::is_whitespace);
    let mut start = 0usize;
    let mut end = clusters.len();
    while start < end && is_ws(clusters[start]) {
        start += 1;
    }
    while end > start && is_ws(clusters[end - 1]) {
        end -= 1;
    }
    clusters[start..end].concat()
}

/// The text being built, a line at a time, up to `maxCharacters`.
#[derive(Default)]
struct Output {
    text: String,
}

impl Output {
    fn full(&self) -> bool {
        self.text.len() >= MAX_CHARACTERS // utf8.count
    }

    fn line(&mut self, line: &str) {
        if self.full() {
            return;
        }
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// A blank line between sheets or slides.
    fn paragraph(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with("\n\n") {
            self.text.push('\n');
        }
    }
}
