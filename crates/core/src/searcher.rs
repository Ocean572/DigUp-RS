//! Query words and FTS5 patterns (the `Searcher.swift` statics, ⑩a): what keyword search
//! looks for, how a query becomes FTS5 patterns, the kinds a query asks for, and when a
//! query is a code/ID lookup. The word lists ride verbatim from Searcher.swift:383/437 —
//! English, Bengali and Arabic, in the spelling `terms` sees (normalized, lowercase).
//!
//! The Swift statics split words with `CharacterSet.alphanumerics` (the Letters, Numbers,
//! combining Marks and connector punctuation — the Mc of a Bengali matra and the underscore
//! of a code name survive the split, the ZWJ between letters does not).

use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

use unicode_segmentation::UnicodeSegmentation;

use crate::model::FileKind;
use crate::search_text::{
    inflects, is_alphanumeric_scalar, is_letter_character, is_number_character, normalized,
    simple_lowercase,
};

fn stopwords() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().copied().collect())
}

pub const STOPWORDS: [&str; 91] = [
    "a",
    "an",
    "the",
    "of",
    "in",
    "on",
    "at",
    "to",
    "for",
    "with",
    "and",
    "or",
    "my",
    "me",
    "that",
    "this",
    "these",
    "those",
    "from",
    "about",
    "where",
    "what",
    "which",
    "who",
    "when",
    "is",
    "was",
    "are",
    "were",
    "be",
    "it",
    "its",
    "some",
    "any",
    "find",
    "show",
    "file",
    "files",
    "photo",
    "photos",
    "picture",
    "image",
    "images",
    "screenshot",
    "screenshots",
    "video",
    "videos",
    "pdf",
    "document",
    "documents",
    "recording",
    "এবং",
    "ও",
    "এই",
    "সেই",
    "একটি",
    "আমার",
    "থেকে",
    "জন্য",
    "সাথে",
    "মধ্যে",
    "কোথায়",
    "কোন",
    "কী",
    "কি",
    "যে",
    "ছবি",
    "স্ক্রিনশট",
    "ভিডিও",
    "ফাইল",
    "পিডিএফ",
    "في",
    "من",
    "على",
    "الى",
    "عن",
    "مع",
    "هذا",
    "هذه",
    "ذلك",
    "التي",
    "الذي",
    "اين",
    "ما",
    "صورة",
    "صور",
    "لقطة",
    "شاشة",
    "فيديو",
    "ملف",
    "مستند",
];

pub const KIND_WORDS: [(&str, FileKind); 43] = [
    ("screenshot", FileKind::Screenshot),
    ("screenshots", FileKind::Screenshot),
    ("photo", FileKind::Image),
    ("photos", FileKind::Image),
    ("picture", FileKind::Image),
    ("pictures", FileKind::Image),
    ("image", FileKind::Image),
    ("images", FileKind::Image),
    ("pdf", FileKind::Pdf),
    ("pdfs", FileKind::Pdf),
    ("paper", FileKind::Pdf),
    ("papers", FileKind::Pdf),
    ("document", FileKind::Doc),
    ("documents", FileKind::Doc),
    ("note", FileKind::Doc),
    ("notes", FileKind::Doc),
    ("video", FileKind::Video),
    ("videos", FileKind::Video),
    ("clip", FileKind::Video),
    ("movie", FileKind::Video),
    ("audio", FileKind::Audio),
    ("song", FileKind::Audio),
    ("podcast", FileKind::Audio),
    ("voice", FileKind::Audio),
    ("sound", FileKind::Audio),
    ("স্ক্রিনশট", FileKind::Screenshot),
    ("ছবি", FileKind::Image),
    ("পিডিএফ", FileKind::Pdf),
    ("নোট", FileKind::Doc),
    ("ভিডিও", FileKind::Video),
    ("অডিও", FileKind::Audio),
    ("গান", FileKind::Audio),
    ("রেকর্ডিং", FileKind::Audio),
    ("لقطة", FileKind::Screenshot),
    ("صورة", FileKind::Image),
    ("صور", FileKind::Image),
    ("مستند", FileKind::Doc),
    ("ملاحظات", FileKind::Doc),
    ("فيديو", FileKind::Video),
    ("مقطع", FileKind::Video),
    ("صوت", FileKind::Audio),
    ("تسجيل", FileKind::Audio),
    ("اغنية", FileKind::Audio),
];

/// The content words of a query, normalized as keyword search keeps its text
/// (Searcher.swift:398): split on non-alphanumerics, words of two scalars or more count,
/// kind words and stopwords drop out — they describe what to look for, not what's in it.
/// `names: true` (a "name:" search) keeps the stopwords: that list is a prose concept and
/// a name search's words are name fragments ("Invoices" starts with "in"). First
/// occurrence order, deduplicated, as the engine's `seen.insert(…).inserted` filter.
pub fn terms(text: &str, names: bool) -> Vec<String> {
    let folded = simple_lowercase(&normalized(text));
    let stops = stopwords();
    let mut seen: HashSet<String> = HashSet::new();
    folded
        .split(|c: char| !is_alphanumeric_scalar(c))
        .filter(|word| word.chars().count() >= 2 && (names || !stops.contains(*word)))
        .filter(|word| seen.insert((*word).to_string()))
        .map(String::from)
        .collect()
}

/// FTS5 patterns for the query's words (Searcher.swift:410): quoted, and as prefixes for
/// the last word while it's being typed and for words of languages that attach endings.
pub fn patterns(terms: &[String], prefix_last: bool) -> Vec<String> {
    terms
        .iter()
        .enumerate()
        .map(|(index, term)| {
            let prefix = (prefix_last && index == terms.len() - 1) || inflects(term);
            format!("\"{term}\"{}", if prefix { "*" } else { "" })
        })
        .collect()
}

/// The query's words as an FTS5 OR-query (Searcher.swift:425); nil when the query has no
/// words left after the stopword drop.
pub fn fts_query(text: &str, prefix_last: bool) -> Option<String> {
    let words = terms(text, false);
    if words.is_empty() {
        return None;
    }
    Some(patterns(&words, prefix_last).join(" OR "))
}

/// Kinds the query asks for ("…in a video", "screenshot of…", "ভিডিওতে"): a soft boost,
/// not a filter (Searcher.swift:443). Bengali attaches case endings, so a Bengali kind
/// word counts at the start of a longer word too (`inflects` keys of three scalars up).
/// `hasPrefix` is Swift's Character-wise test: "স্ক্রিনশটের" does NOT start with
/// "স্ক্রিনশট" — the words' graphemes split differently at the joined vowel ("্রি" is one
/// Character where the key has "্র" + "ি"), where a byte prefix would match. Measured in
/// ⑩a's cross-check (scenario `k124`).
pub fn kind_hints(text: &str) -> BTreeSet<FileKind> {
    let folded = simple_lowercase(&normalized(text));
    folded
        .split(|c: char| !is_alphanumeric_scalar(c))
        .filter_map(|word| {
            KIND_WORDS
                .iter()
                .find(|(key, _)| *key == word)
                .or_else(|| {
                    KIND_WORDS.iter().find(|(key, _)| {
                        inflects(key) && key.chars().count() >= 3 && grapheme_starts_with(word, key)
                    })
                })
                .map(|&(_, kind)| kind)
        })
        .collect()
}

fn grapheme_starts_with(word: &str, key: &str) -> bool {
    let mut word_g = word.graphemes(true);
    key.graphemes(true).all(|g| word_g.next() == Some(g))
}

/// Codes and IDs ("K7Q2LM", "TRX50") and long numbers ("4815162342") mean nothing to an
/// embedding but everything to keyword search (Searcher.swift:285): a query made of them
/// weighs words double and meaning half. Each term must carry a number, and letters or
/// four characters. `Character.isNumber` and `Character.isLetter` (measured in ⑩a's
/// cross-check, scenario `facts-grapheme`) test the **first scalar** of the Character:
/// isNumber is Nd∪Nl∪No, isLetter is the Letters, the Spacing and Enclosing Marks and the
/// Letter Numbers — plain digits, `²` and combining marks are NOT letters, so "123" is
/// not a lookup ("4815162342" is, by the four-character rule). Counts are graphemes.
pub fn is_lookup(terms: &[String]) -> bool {
    !terms.is_empty()
        && terms.iter().all(|term| {
            let firsts: Vec<char> = term
                .graphemes(true)
                .filter_map(|g| g.chars().next())
                .collect();
            let has_number = firsts.iter().any(|&c| is_number_character(c));
            if !has_number {
                return false;
            }
            let has_letter = firsts.iter().any(|&c| is_letter_character(c));
            has_letter || term.graphemes(true).count() >= 4
        })
}

// --------------------------------------------------------------------------------
// The Searcher class (Searcher.swift): the query pipeline over an IndexStore —
// meaning (vectors scored on the query's scale), words (FTS5 with the stemmer, the
// code identifier as one phrase), the fusion score, and the hit assembly with the
// code definition focus. ⑩c; the statics above are its query-side halves.
// --------------------------------------------------------------------------------

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;

use serde_json::Value;

use crate::code_lookup;
use crate::db::Sql;
use crate::index_store::{IndexStore, CODE_FILE};
use crate::model::{Modality, NameQuery, SegmentKind};
use crate::options::IndexOptions;
use crate::scoring::{self, VectorMatrix, WORD_WEIGHT, Z_GATE};
use crate::search_text::{snippet, trim_whitespaces_newlines};
use crate::vectors::DIMENSION;

/// `Searcher.codeWordWeight`: code's words count for less than a prose index's (they
/// are everywhere in code — the measured evals in Searcher.swift:103).
pub const CODE_WORD_WEIGHT: f64 = 1.0;

/// One search result (Searcher.swift:3): the file, the segment to point at, the
/// engine's own numbers (score, z, cosine) relayed verbatim, and what the app opens on.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    pub kind: FileKind,
    /// Fused rank score, for ordering only.
    pub score: f64,
    /// How far the best segment stands out for this query (`meaning`), when there is one.
    pub z: Option<f64>,
    pub cosine: Option<f64>,
    /// 1-based rank among the keyword matches.
    pub keyword_rank: Option<usize>,
    pub segment: SegmentKind,
    pub segment_id: i64,
    /// PDF page, or seconds into audio/video.
    pub loc: Option<f64>,
    pub loc_end: Option<f64>,
    pub excerpt: Option<String>,
    pub modified: f64,
    pub size: i64,
    pub info: HashMap<String, String>,
    /// Code: the line to open at within the stretch (`loc`…`loc_end`); nil: its first line.
    pub focus: Option<f64>,
}

/// The knobs of `search` (Searcher.swift:115), the engine's defaults spelled out.
#[derive(Debug, Clone)]
pub struct SearchOptions<'a> {
    pub kinds: Option<&'a std::collections::HashSet<FileKind>>,
    pub limit: usize,
    pub z_gate: f64,
    pub word_weight: Option<f64>,
    pub prefix_last: bool,
}

impl Default for SearchOptions<'_> {
    fn default() -> Self {
        Self {
            kinds: None,
            limit: 20,
            z_gate: Z_GATE,
            word_weight: None,
            prefix_last: false,
        }
    }
}

/// The hybrid searcher (Searcher.swift:31): meaning + words over one index. `is_code`
/// is the code index's searcher — `IndexStore.codeFile` decides, as in the engine.
pub struct Searcher<'a> {
    store: &'a IndexStore,
    matrix: Option<VectorMatrix>,
    row_file: Vec<i64>,
    row_segment: Vec<i64>,
    row_modality: Vec<Modality>,
    row_sets_scale: Vec<bool>,
    file_kind: HashMap<i64, FileKind>,
    pub vector_source: Option<String>,
    pub vectors_loaded: bool,
}

impl<'a> Searcher<'a> {
    pub fn new(store: &'a IndexStore, load_vectors: bool) -> Result<Self, crate::db::Error> {
        let mut searcher = Self {
            store,
            matrix: None,
            row_file: Vec::new(),
            row_segment: Vec::new(),
            row_modality: Vec::new(),
            row_sets_scale: Vec::new(),
            file_kind: HashMap::new(),
            vector_source: None,
            vectors_loaded: false,
        };
        if load_vectors {
            searcher.reload()?;
        } else {
            searcher.reload_files()?;
        }
        Ok(searcher)
    }

    /// `store.file.lastPathComponent == IndexStore.codeFile`.
    pub fn is_code(&self) -> bool {
        self.store.file.file_name() == Some(OsStr::new(CODE_FILE))
    }

    /// `vectorCount`.
    pub fn vector_count(&self) -> usize {
        self.row_file.len()
    }

    /// `reload` (Searcher.swift:57): re-read files + meta, then every vector with its
    /// segment's scale flags. `VectorStorage(rows:)` answers nil for zero rows — the
    /// "nothing to load" branch with `vectorsLoaded` set.
    pub fn reload(&mut self) -> Result<(), crate::db::Error> {
        self.release_vectors();
        self.reload_files()?;
        let mut rows = 0i64;
        self.store.db.query(
            "SELECT COUNT(*) FROM segments WHERE vec IS NOT NULL",
            [],
            |row| {
                rows = row.int(0);
                Ok(())
            },
        )?;
        let rows = rows as usize;
        let Some(mut matrix) = VectorMatrix::new(rows) else {
            self.vectors_loaded = true; // nothing to load
            return Ok(());
        };
        let steps = IndexOptions::default();
        // Streamed, as `Searcher.swift:74` does (`row.copyBlob(3, to: matrix.halves + row * dim, …)`
        // inside the query callback): the rows are written into the matrix and the side arrays
        // as they arrive, so the peak of a vector load is the matrix itself. Staging every
        // row first (`Vec<VectorRow>`: a 1,536-byte blob plus three Strings per row) doubled
        // the transient — ~350 MB alive at once on 100 k vectors, in the searcher daemon that
        // prewarms for 20-30 s at boot and is the long-lived process on a shared box.
        //
        // The fields are taken out for the query because the closure writes them while
        // `self.store` is borrowed to read; `matrix` and the counters are locals.
        let mut row_file = std::mem::take(&mut self.row_file);
        let mut row_segment = std::mem::take(&mut self.row_segment);
        let mut row_modality = std::mem::take(&mut self.row_modality);
        let mut row_sets_scale = std::mem::take(&mut self.row_sets_scale);
        self.store.db.query(
            "SELECT s.id, s.file, s.modality, s.vec, s.kind, s.loc, f.kind, CAST(json_extract(f.info, '$.pages') AS INTEGER) \
             FROM segments s JOIN files f ON f.id = s.file WHERE s.vec IS NOT NULL ORDER BY s.id",
            [],
            |row| {
                // Swift's guard `rowFile.count < rows, copyBlob(…)`: a full matrix is done;
                // a blob that is not a vector is dropped (the row is NOT appended, the next
                // row fills this matrix row).
                if row_file.len() >= rows {
                    return Ok(());
                }
                let Some(bytes) = row.blob(3) else {
                    return Ok(());
                };
                if !matrix.set_row(row_file.len(), &bytes) {
                    return Ok(());
                }
                let modality = row.text(2).unwrap_or_default();
                let kind = row.text(4).unwrap_or_default();
                let file_kind = row.text(6).unwrap_or_default();
                let pages = row.int(7);
                let loc_is_null = row.is_null(5);
                let loc = row.double(5);
                row_file.push(row.int(1));
                row_segment.push(row.int(0));
                row_modality.push(Modality::parse(&modality).unwrap_or(Modality::None));
                // Past the first step of a long PDF (its pages) or document (its chunks).
                let place = if loc_is_null { 0 } else { loc.trunc() as i64 };
                let later = if file_kind == "pdf" {
                    kind != SegmentKind::Chunk.as_str()
                        && pages > steps.big_pdf_pages
                        && place > steps.pdf_step
                } else {
                    kind == SegmentKind::Chunk.as_str() && place > steps.doc_step
                };
                row_sets_scale.push(!later);
                Ok(())
            },
        )?;
        self.row_file = row_file;
        self.row_segment = row_segment;
        self.row_modality = row_modality;
        self.row_sets_scale = row_sets_scale;
        self.matrix = Some(matrix);
        self.vectors_loaded = true;
        Ok(())
    }

    /// `releaseVectors` (Searcher.swift:82): free the vectors; keyword search keeps working.
    pub fn release_vectors(&mut self) {
        self.matrix = None;
        self.row_file = Vec::new();
        self.row_segment = Vec::new();
        self.row_modality = Vec::new();
        self.row_sets_scale = Vec::new();
        self.vectors_loaded = false;
    }

    /// `reloadFiles` (Searcher.swift:92): the file-kind map and `vector_source`.
    pub fn reload_files(&mut self) -> Result<(), crate::db::Error> {
        let mut rows: Vec<(i64, Option<String>)> = Vec::new();
        self.store
            .db
            .query("SELECT id, kind FROM files", [], |row| {
                rows.push((row.int(0), row.text(1)));
                Ok(())
            })?;
        self.file_kind.clear();
        for (id, kind) in rows {
            if let Some(kind) = kind.and_then(|k| FileKind::parse(&k)) {
                self.file_kind.insert(id, kind);
            }
        }
        self.vector_source = self.store.meta("vector_source")?;
        Ok(())
    }

    /// `score` (Searcher.swift:307): cosine of the query with every loaded vector. The
    /// engine scores `rowFile.count` rows, not the matrix's capacity rows.
    fn score(&self, query: &[f32]) -> Vec<f32> {
        let rows = self.row_file.len();
        let mut scores = vec![0.0f32; rows];
        if rows == 0 {
            return scores;
        }
        if let Some(matrix) = &self.matrix {
            let all = matrix.score(query);
            let n = rows.min(all.len());
            scores[..n].copy_from_slice(&all[..n]);
        }
        scores
    }

    /// `text(ofSegment:)` (Searcher.swift:376): the text as it's written.
    pub fn text_of_segment(&self, id: i64) -> Result<Option<String>, crate::db::Error> {
        let mut text = None;
        self.store.db.query(
            "SELECT coalesce(original, body) FROM fts WHERE rowid = ?",
            [Sql::Int(id)],
            |row| {
                text = row.text(0);
                Ok(())
            },
        )?;
        Ok(text)
    }

    /// `isIndexed` (Searcher.swift:421): keyword search finds `pattern` in some file.
    fn is_indexed(&self, pattern: &str, names_only: bool) -> Result<bool, crate::db::Error> {
        let sql = format!(
            "SELECT 1 FROM fts{} WHERE fts MATCH ?{} LIMIT 1",
            if names_only {
                " JOIN segments s ON s.id = fts.rowid"
            } else {
                ""
            },
            if names_only {
                " AND s.kind = 'name'"
            } else {
                ""
            }
        );
        let mut found = false;
        self.store
            .db
            .query(&sql, [Sql::Text(pattern.to_string())], |_| {
                found = true;
                Ok(())
            })?;
        Ok(found)
    }

    /// `codePlace` (Searcher.swift:263): the stretch to point at and the line to open at.
    /// A looked-up name points at the stretch that DEFINES it, when one does.
    fn code_place(
        &self,
        file: i64,
        segment: i64,
        identifier: Option<&[String]>,
        query: &str,
        terms: &[String],
        words_here: bool,
    ) -> Result<(i64, Option<f64>), crate::db::Error> {
        let name = trim_whitespaces_newlines(query).replace("()", "");
        if let Some(identifier) = identifier {
            let mut best: Option<(i64, i64, i32)> = None; // (segment, line, strength)
            let mut rows: Vec<(i64, f64, Option<String>)> = Vec::new();
            self.store.db.query(
                "SELECT s.id, s.loc, coalesce(fts.original, fts.body) FROM fts JOIN segments s ON s.id = fts.rowid \
                 WHERE fts MATCH ? AND s.file = ? ORDER BY s.loc LIMIT 300",
                [Sql::Text(code_lookup::phrase(identifier)), Sql::Int(file)],
                |row| {
                    rows.push((row.int(0), row.double(1), row.text(2)));
                    Ok(())
                },
            )?;
            for (segment, loc, text) in rows {
                if best.map(|b| b.2).unwrap_or(0) >= 2 {
                    break; // the guard `best?.strength ?? 0 < 2`
                }
                let Some(text) = text else { continue };
                let Some(found) = code_lookup::definition(&name, &text, loc.trunc() as i64) else {
                    continue;
                };
                if found.1 > best.map(|b| b.2).unwrap_or(0) {
                    best = Some((segment, found.0, found.1));
                }
            }
            if let Some((segment, line, _)) = best {
                return Ok((segment, Some(line as f64)));
            }
        }
        if !(words_here || identifier.is_some()) {
            return Ok((segment, None));
        }
        let Some(text) = self.text_of_segment(segment)? else {
            return Ok((segment, None));
        };
        let mut first = 1i64;
        self.store.db.query(
            "SELECT loc FROM segments WHERE id = ?",
            [Sql::Int(segment)],
            |row| {
                first = row.double(0).trunc() as i64;
                Ok(())
            },
        )?;
        let words: Vec<String> = match identifier {
            None => terms.to_vec(),
            Some(_) => vec![simple_lowercase(&name)],
        };
        Ok((
            segment,
            code_lookup::first_line(&words, &text, first).map(|line| line as f64),
        ))
    }

    /// The whole query pipeline (Searcher.swift:115): meaning + words + fusion + hits.
    pub fn search(
        &mut self,
        text: &str,
        query_vector: Option<&[f32]>,
        opts: &SearchOptions<'_>,
    ) -> Result<Vec<SearchHit>, crate::db::Error> {
        let is_code = self.is_code();
        let word_weight = opts.word_weight.unwrap_or(if is_code {
            CODE_WORD_WEIGHT
        } else {
            WORD_WEIGHT
        });
        // "name: …": names only, no query vector, the model never runs for it.
        let mut text = text.to_string();
        let mut query_vector: Option<Vec<f32>> = query_vector.map(|q| q.to_vec());
        let mut names_only = false;
        if let Some(names) = NameQuery::text_of(&text) {
            text = names;
            query_vector = None;
            names_only = true;
        }
        if query_vector.is_some() && !self.vectors_loaded {
            self.reload()?;
        }
        let allowed = |file: i64| -> bool {
            match opts.kinds {
                None => true,
                Some(kinds) => self
                    .file_kind
                    .get(&file)
                    .is_some_and(|kind| kinds.contains(kind)),
            }
        };

        // Meaning: every vector scored on this query's scale, each file's best segment kept.
        let mut best: HashMap<i64, (f64, f64, usize)> = HashMap::new();
        if let Some(query) = &query_vector {
            if query.len() == DIMENSION && !self.row_file.is_empty() {
                let scores = self.score(query);
                let meaning =
                    scoring::meaning(&scores, &self.row_modality, Some(&self.row_sets_scale));
                for row in 0..scores.len() {
                    if !allowed(self.row_file[row]) {
                        continue;
                    }
                    let z = meaning[row];
                    if z > best
                        .get(&self.row_file[row])
                        .map(|b| b.0)
                        .unwrap_or(f64::NEG_INFINITY)
                    {
                        best.insert(self.row_file[row], (z, f64::from(scores[row]), row));
                    }
                }
            }
        }

        // Words: FTS5 over names, folders, OCR, page and document text; a code name is
        // one phrase. The row scan is collected first, then filtered exactly as the
        // engine's row callback guards run: cap, kind filter, first sighting per file.
        let terms = terms(&text, names_only);
        let identifier = if is_code {
            code_lookup::identifier(&text)
        } else {
            None
        };
        let mut words: Vec<(i64, i64, Option<String>)> = Vec::new();
        let mut coverage: HashMap<i64, f64> = HashMap::new();
        let mut patterns: Vec<String> = match &identifier {
            Some(id) => vec![code_lookup::phrase(id)],
            None => patterns(&terms, opts.prefix_last),
        };
        if identifier.is_none() {
            // A word that is in no file as typed matches as the start of longer words.
            for index in 0..terms.len() {
                if patterns[index].ends_with('*') {
                    continue;
                }
                let count = terms[index].graphemes(true).count();
                if !(count >= 3 || (names_only && count >= 2)) {
                    continue;
                }
                if !self.is_indexed(&patterns[index], names_only)? {
                    patterns[index].push('*');
                }
            }
        }
        if !patterns.is_empty() {
            let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let sql =
                format!(
                "SELECT s.file, s.id, snippet(fts, {}, '«', '»', '…', 12), fts.original FROM fts \
                 JOIN segments s ON s.id = fts.rowid WHERE fts MATCH ?{} \
                 ORDER BY bm25(fts, 3.0, 1.0) LIMIT 1000",
                if names_only { 0 } else { 1 },
                if names_only { " AND s.kind = 'name'" } else { "" }
            );
            let mut rows: Vec<(i64, i64, Option<String>, Option<String>)> = Vec::new();
            self.store
                .db
                .query(&sql, [Sql::Text(patterns.join(" OR "))], |row| {
                    rows.push((row.int(0), row.int(1), row.text(2), row.text(3)));
                    Ok(())
                })?;
            for (file, segment, sql_snippet, original) in rows {
                if words.len() >= 100 {
                    continue;
                }
                if !allowed(file) {
                    continue;
                }
                if !seen.insert(file) {
                    continue;
                }
                // Text that normalizing changed is quoted as it's written.
                let snippet = match &original {
                    Some(text) => snippet(text, &terms, 12).or(sql_snippet),
                    None => sql_snippet,
                };
                words.push((file, segment, snippet));
            }
            if !words.is_empty() {
                let files: String = words
                    .iter()
                    .map(|word| word.0.to_string())
                    .collect::<Vec<String>>()
                    .join(",");
                for pattern in &patterns {
                    let sql = format!(
                        "SELECT DISTINCT s.file FROM fts JOIN segments s ON s.id = fts.rowid \
                         WHERE fts MATCH ? AND s.file IN ({}){}",
                        files,
                        if names_only {
                            " AND s.kind = 'name'"
                        } else {
                            ""
                        }
                    );
                    let mut hits: Vec<i64> = Vec::new();
                    self.store
                        .db
                        .query(&sql, [Sql::Text(pattern.clone())], |row| {
                            hits.push(row.int(0));
                            Ok(())
                        })?;
                    for file in hits {
                        *coverage.entry(file).or_insert(0.0) += 1.0 / patterns.len() as f64;
                    }
                }
            }
        }

        // A file qualifies by standing out in meaning, or by holding at least half the words.
        let lookup = is_lookup(&terms) || identifier.is_some();
        let (meaning_weight, words_weight) = if lookup {
            (0.5, word_weight * 2.0)
        } else {
            (1.0, word_weight)
        };
        let hinted = kind_hints(&text);
        let mut candidates: std::collections::HashSet<i64> = best
            .iter()
            .filter(|(_, scored)| scored.0 >= opts.z_gate)
            .map(|(file, _)| *file)
            .collect();
        for (file, share) in &coverage {
            if *share >= 0.5 {
                candidates.insert(*file);
            }
        }
        let mut scored: Vec<(i64, f64)> = Vec::new();
        for file in &candidates {
            let mut score = meaning_weight * best.get(file).map(|b| b.0).unwrap_or(0.0).max(0.0)
                + words_weight * coverage.get(file).copied().unwrap_or(0.0).powf(1.5);
            if let Some(kind) = self.file_kind.get(file) {
                if hinted.contains(kind) {
                    score += 1.0;
                }
            }
            scored.push((*file, score));
        }
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });

        // The keyword rank per file is its first sighting; meaning wins the pointer when it
        // stands out, else the segment the words matched.
        let mut word_rank: HashMap<i64, (usize, i64, Option<String>)> = HashMap::new();
        for (index, word) in words.iter().enumerate() {
            word_rank
                .entry(word.0)
                .or_insert((index + 1, word.1, word.2.clone()));
        }
        let mut hits: Vec<SearchHit> = Vec::new();
        for (file, score) in scored.into_iter().take(opts.limit) {
            let rank = hits.len();
            let semantic = best.get(&file).copied();
            let keyword = word_rank.get(&file);
            let use_meaning = semantic
                .map(|s| s.0 >= opts.z_gate || keyword.is_none())
                .unwrap_or(false);
            let mut segment_id = match (use_meaning, semantic, keyword) {
                (true, Some(s), _) => self.row_segment[s.2],
                (false, _, Some(k)) => k.1,
                _ => continue,
            };
            let mut focus: Option<f64> = None;
            if is_code {
                // The definition is looked for in the first 10 results only.
                let (segment, line) = self.code_place(
                    file,
                    segment_id,
                    if rank < 10 {
                        identifier.as_deref()
                    } else {
                        None
                    },
                    &text,
                    &terms,
                    keyword.map(|k| k.1 == segment_id).unwrap_or(false) || identifier.is_some(),
                )?;
                segment_id = segment;
                focus = line;
            }
            let mut path = String::new();
            let mut kind = FileKind::Doc;
            let mut modified = 0.0;
            let mut size = 0i64;
            let mut info_json: Option<String> = None;
            self.store.db.query(
                "SELECT path, kind, mtime, size, info FROM files WHERE id = ?",
                [Sql::Int(file)],
                |row| {
                    path = row.text(0).unwrap_or_default();
                    kind =
                        FileKind::parse(&row.text(1).unwrap_or_default()).unwrap_or(FileKind::Doc);
                    modified = row.double(2);
                    size = row.int(3);
                    info_json = row.text(4);
                    Ok(())
                },
            )?;
            // Swift casts the info JSON to [String: String]; any non-string value fails
            // the cast, and the whole map falls back to empty.
            let mut info: HashMap<String, String> = HashMap::new();
            if let Some(json) = &info_json {
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(json) {
                    let mut all_strings = true;
                    let mut entries: HashMap<String, String> = HashMap::new();
                    for (key, value) in map {
                        match value {
                            Value::String(text) => {
                                entries.insert(key, text);
                            }
                            _ => all_strings = false,
                        }
                    }
                    if all_strings {
                        info = entries;
                    }
                }
            }
            let mut segment = SegmentKind::Name;
            let mut loc: Option<f64> = None;
            let mut loc_end: Option<f64> = None;
            let mut stored_excerpt: Option<String> = None;
            self.store.db.query(
                "SELECT kind, loc, loc_end, excerpt FROM segments WHERE id = ?",
                [Sql::Int(segment_id)],
                |row| {
                    segment = SegmentKind::parse(&row.text(0).unwrap_or_default())
                        .unwrap_or(SegmentKind::Name);
                    loc = (!row.is_null(1)).then(|| row.double(1));
                    loc_end = (!row.is_null(2)).then(|| row.double(2));
                    stored_excerpt = row.text(3);
                    Ok(())
                },
            )?;
            // Quote the matched words only when they come from the segment we point at; a
            // segment with no place and no text of its own borrows them from its file's
            // OCR text — unless the match came from a name segment.
            let mut excerpt = keyword
                .filter(|k| k.1 == segment_id)
                .and_then(|k| k.2.clone())
                .filter(|text| text.contains("«"))
                .map(|text| crate::extract::excerpt(&text, 400));
            if excerpt.is_none() && stored_excerpt.is_none() && loc.is_none() {
                if let Some(matched) = keyword {
                    if let Some(text) = &matched.2 {
                        if text.contains("«") {
                            let mut matched_kind: Option<String> = None;
                            self.store.db.query(
                                "SELECT kind FROM segments WHERE id = ?",
                                [Sql::Int(matched.1)],
                                |row| {
                                    matched_kind = row.text(0);
                                    Ok(())
                                },
                            )?;
                            if matched_kind.as_deref() != Some(SegmentKind::Name.as_str()) {
                                excerpt = Some(crate::extract::excerpt(text, 400));
                            }
                        }
                    }
                }
            }
            hits.push(SearchHit {
                path,
                kind,
                score,
                z: semantic.map(|s| s.0),
                cosine: semantic.map(|s| s.1),
                keyword_rank: keyword.map(|k| k.0),
                segment,
                segment_id,
                loc,
                loc_end,
                excerpt: excerpt.or(stored_excerpt),
                modified,
                size,
                info,
                focus,
            });
        }
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candidate, Modality, SegmentKind};

    fn candidate(path: &str, kind: FileKind) -> Candidate {
        Candidate {
            path: path.to_string(),
            kind,
            size: 100,
            mtime: 1.0,
            inode: 7,
            device: 3,
        }
    }

    fn vector(value: f32) -> [f32; 768] {
        [value; 768]
    }

    /// The streamed vector load (`reload`), pinned at the two rules the loop implements:
    ///
    /// * a blob that is not a vector is DROPPED, the row is not appended, and the next row
    ///   takes the matrix slot it left (`Searcher.swift:74`'s guard + `copyBlob`), so the
    ///   matrix rows and the side arrays stay index-aligned with a short blob in the middle
    ///   of the crawl order;
    /// * the scale flags: the FIRST step of a long PDF (page 5 of a 145-page book, step 30)
    ///   and of a long document (chunk 1, step 40) is scale-corrected, and everything past
    ///   the first step is not — a page 61's vector was made alone, so z must not compare it
    ///   as part of the file's scale.
    #[test]
    fn reload_drops_a_short_blob_and_flags_the_rows_past_the_first_step() {
        let tmp = tempfile::tempdir().expect("a temp folder");
        let store = IndexStore::open(tmp.path(), crate::MAIN_FILE).expect("an index");

        let photo = store
            .insert(&candidate("/data/a.jpg", FileKind::Image))
            .unwrap();
        store
            .finish(photo, "done", None, HashMap::new())
            .expect("the photo is done");
        for (kind, modality, excerpt, value) in [
            (SegmentKind::Image, Modality::Image, "good image", 0.5f32),
            (SegmentKind::Ocr, Modality::Text, "good ocr", 0.25),
            (SegmentKind::Image, Modality::Image, "broken blob", 0.125),
        ] {
            store
                .add_segment(
                    photo,
                    kind,
                    modality,
                    None,
                    None,
                    Some(excerpt),
                    Some(excerpt),
                    Some(&vector(value)),
                )
                .expect("a segment");
        }
        // A 3-byte blob in the middle of the id order: the engine's `copyBlob` guard drops it.
        store
            .db
            .run(
                "UPDATE segments SET vec = ? WHERE excerpt = ?",
                [Sql::Blob(vec![1u8, 2, 3]), Sql::Text("broken blob".into())],
            )
            .expect("the blob is made too short to be a vector");

        let book = store
            .insert(&candidate("/data/books/long.pdf", FileKind::Pdf))
            .unwrap();
        store
            .finish(
                book,
                "partial",
                Some("read to page 60"),
                [("pages".to_string(), "145".to_string())]
                    .into_iter()
                    .collect(),
            )
            .expect("the book is partial");
        for page in [5.0, 61.0] {
            store
                .add_segment(
                    book,
                    SegmentKind::Page,
                    Modality::Text,
                    Some(page),
                    Some(page),
                    Some("page words"),
                    Some("page words"),
                    Some(&vector(0.2)),
                )
                .expect("a page");
        }

        let notes = store
            .insert(&candidate("/data/notes/big.txt", FileKind::Doc))
            .unwrap();
        store
            .finish(notes, "partial", None, HashMap::new())
            .expect("the document is partial");
        for chunk in [1.0, 45.0] {
            store
                .add_segment(
                    notes,
                    SegmentKind::Chunk,
                    Modality::Text,
                    Some(chunk),
                    Some(chunk),
                    Some("chunk words"),
                    Some("chunk words"),
                    Some(&vector(0.2)),
                )
                .expect("a chunk");
        }

        let searcher = Searcher::new(&store, true).expect("the searcher loads");
        // The rowids: files 1, 2, 3 (photo, book, notes); the photo's segments 2,3,4 — 4
        // carries the short blob — then pages 6,7 and chunks 9,10.
        assert_eq!(
            searcher.vector_count(),
            6,
            "the short blob is not in the matrix"
        );
        assert_eq!(
            searcher.row_segment,
            vec![2, 3, 6, 7, 9, 10],
            "segment 4 is dropped, and segment 6 fills the matrix slot it left — the side \
             arrays stay aligned with the matrix rows, which is what the streaming loop writes"
        );
        assert_eq!(
            searcher.row_file,
            vec![1, 1, 2, 2, 3, 3],
            "each matrix row's file rides with it"
        );
        assert_eq!(
            searcher.row_sets_scale,
            vec![true, true, true, false, true, false],
            "the first step of a long file is scale-corrected; page 61 of 145 and chunk 45 are not"
        );
        assert_eq!(
            searcher.row_modality,
            vec![
                Modality::Image,
                Modality::Text,
                Modality::Text,
                Modality::Text,
                Modality::Text,
                Modality::Text
            ],
            "the modality column rides along with the matrix row"
        );

        let mut searcher = searcher;
        searcher.release_vectors();
        assert_eq!(
            searcher.vector_count(),
            0,
            "releaseVectors empties the matrix"
        );
        searcher.reload().expect("and reload loads it again");
        assert_eq!(
            searcher.vector_count(),
            6,
            "the same six rows, in the same order"
        );
    }

    /// The child half of `the_vector_load_peaks_at_the_matrix`: open the index the parent
    /// built, load its vectors, and print the process's high-water mark. A fresh process is
    /// the point — `VmHWM` is the peak of everything that ran before it, so a child that
    /// builds the fixture and loads it in one process measures the fixture, not the load.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run by the_vector_load_peaks_at_the_matrix as a child: loads, then reports VmHWM"]
    fn loads_the_vectors_of_the_index_it_is_given() {
        let dir = std::env::var("DIGUP_PEAK_DB").expect("the index folder to load");
        println!("BEFORE {}", peak_kilobytes());
        let store =
            IndexStore::open(std::path::Path::new(&dir), crate::MAIN_FILE).expect("an index");
        let searcher = Searcher::new(&store, true).expect("the load runs");
        println!("ROWS {}", searcher.vector_count());
        println!("PEAK {}", peak_kilobytes());
    }

    /// The kernel's own high-water mark of this process (`VmHWM` in `/proc/self/status`).
    #[cfg(target_os = "linux")]
    fn peak_kilobytes() -> usize {
        std::fs::read_to_string("/proc/self/status")
            .expect("/proc/self/status")
            .lines()
            .find_map(|line| {
                line.strip_prefix("VmHWM:")
                    .and_then(|tail| tail.trim().trim_end_matches(" kB").parse::<usize>().ok())
            })
            .expect("the kernel's high-water mark")
    }

    /// The load's peak, measured in a process whose only work is the load: 8,192 segments of
    /// real 1,536-byte float16 vectors is a 12.6 MB matrix, and the window must cost the
    /// matrix plus a few MB of reader, not twice that. The shape at `audit-base` staged every
    /// row first (`Vec<VectorRow>`: the blob, three Strings and the fields per row, with `Vec`
    /// doubling on the way up), which is the searcher daemon's boot cost on top of a matrix
    /// that is already the index's size — on the 3M-segment class of indexes, hundreds of MB.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_vector_load_peaks_at_the_matrix() {
        use std::io::BufRead;

        let rows: usize = 8_192;
        let tmp = tempfile::tempdir().expect("a temp folder");
        let store = IndexStore::open(tmp.path(), crate::MAIN_FILE).expect("an index");
        let value = vector(0.5);
        store
            .db
            .transaction(|| {
                for n in 0..rows {
                    let file = store
                        .insert(&candidate(
                            &format!("/data/pictures/{n}.jpg"),
                            FileKind::Image,
                        ))
                        .expect("a file");
                    store
                        .finish(file, "done", None, HashMap::new())
                        .expect("done");
                    store
                        .add_segment(
                            file,
                            SegmentKind::Image,
                            Modality::Image,
                            None,
                            None,
                            Some("a picture"),
                            Some("a picture"),
                            Some(&value),
                        )
                        .expect("a vector");
                }
                Ok(())
            })
            .expect("the fixture is built");
        drop(store);

        let mut child = std::process::Command::new(
            std::env::current_exe().expect("the test binary, re-run as the loader"),
        )
        .args([
            "--ignored",
            "--exact",
            "searcher::tests::loads_the_vectors_of_the_index_it_is_given",
            "--nocapture",
        ])
        .env("DIGUP_PEAK_DB", tmp.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the loader child starts");
        let reader = std::io::BufReader::new(child.stdout.take().expect("the child's stdout"));
        let (mut before, mut peak) = (None, None);
        for line in reader.lines() {
            let line = line.expect("a line of the child's report");
            if let Some(tail) = line.strip_prefix("BEFORE ") {
                before = Some(tail.parse::<usize>().expect("a kilobyte count"));
            }
            if let Some(tail) = line.strip_prefix("PEAK ") {
                peak = Some(tail.parse::<usize>().expect("a kilobyte count"));
            }
        }
        let status = child.wait().expect("the child finished");
        let (before, peak) = (
            before.expect("the child reported its base"),
            peak.expect("the child reported its peak"),
        );
        assert!(status.success(), "the loader child exited cleanly");

        let matrix = rows * 768 * 2; // float16 halves; scored as float32
                                     // The window's own floor is TWO matrix sizes: the vectors are read through the
                                     // 1 GiB mmap (`db::MMAP_SIZE`), so touching the file's pages costs a second copy of
                                     // the blobs, and SQLite's pages and the allocator add 12 MB on top. Measured on this
                                     // box with this fixture and the same child shape: the streamed load costs 33,208 kB
                                     // above the child's 4,296 kB base; the shape at `audit-base`, which staged every row
                                     // (`Vec<VectorRow>`: the 1,536-byte blob, three Strings, and `Vec` doubling on the way
                                     // up) before writing the matrix, costs 46,712 kB — 13,504 kB for 8,192 rows, 1.7 kB
                                     // each, exactly the staged row's footprint. The pin is the difference.
        let floor = matrix / 1024 * 2 + (12 << 10);
        let grew = peak.saturating_sub(before);
        assert!(
            grew <= floor,
            "loading {rows} vectors cost {grew} kB on top of the child's {before} kB base; the \
             matrix is {} kB and the window's floor (file pages through the mmap, SQLite's \
             pages, the allocator) is {floor} kB — the rows were staged beside the matrix, \
             where the engine copies each blob straight into it (Searcher.swift:74)",
            matrix / 1024
        );
    }
}
