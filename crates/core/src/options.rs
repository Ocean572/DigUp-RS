//! Indexing settings (`IndexOptions`, Model.swift:42-84): what gets indexed and how. The
//! crawler, estimator and (later) indexer read it; the fingerprint lands with the index verb.

use std::collections::HashSet;

/// The settings as the crawl and the estimate use them. Defaults are the engine's (`init()`).
#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// The code index: code folders, `classifyCode`, git-kept files only.
    pub code: bool,
    /// Source files count as documents in the main index (off by default: they flood it).
    pub include_code: bool,
    /// Index big machine-named media folders too (they are datasets by default).
    pub include_datasets: bool,
    /// Hand-written files can pass 256 KB, rarely 1 MB; bigger code is machine-made.
    pub max_code_bytes: i64,
    /// A notebook's file holds its outputs too; past this, it isn't read.
    pub max_notebook_bytes: i64,
    /// PDFs up to `big_pdf_pages` are read whole in the first pass; longer ones get a step.
    pub big_pdf_pages: i64,
    pub pdf_step: i64,
    pub doc_step: i64,
    /// Only machine-made giants get this far: pages/chunks past these are left out.
    pub max_pages: i64,
    pub max_chunks: i64,
    /// The first pass reads this many chunks of a long document.
    pub excluded_folders: Vec<String>,
    pub excluded_extensions: HashSet<String>,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            code: false,
            include_code: false,
            include_datasets: false,
            max_code_bytes: 1 << 20,
            max_notebook_bytes: 20 << 20,
            big_pdf_pages: 50,
            pdf_step: 30,
            doc_step: 40,
            max_pages: 5_000,
            max_chunks: 5_000,
            excluded_folders: Vec::new(),
            excluded_extensions: HashSet::new(),
        }
    }
}

impl IndexOptions {
    /// `IndexOptions.extensions`: "HEIC, .txt *.md" → {"heic", "txt", "md"}.
    pub fn extensions(text: &str) -> HashSet<String> {
        text.split(|c: char| c == ',' || c.is_whitespace())
            .map(|word| word.trim_start_matches(['*', '.']).to_lowercase())
            .filter(|word: &String| !word.is_empty())
            .collect()
    }
}
