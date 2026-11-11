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
    /// Characters a stretch of code holds at most (whole lines; a longer line is a stretch of its own).
    pub code_stretch: i64,
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
    /// The main index's media settings: they are part of the fingerprint, so they ride with the
    /// options from the first commit of the port on (`Model.swift:57-73`).
    pub min_image_side: i64,
    /// Fewer characters than this on a page: embed the rendered page instead.
    pub min_page_text: i64,
    /// Vision soft tokens: 70 / 140 / 280 / 560 / 1120.
    pub photo_budget: i64,
    pub screenshot_budget: i64,
    pub page_budget: i64,
    pub frame_budget: i64,
    /// Seconds between sampled video frames; the model ignores audio past `audio_window` of one input.
    pub frame_interval: f64,
    pub audio_window: f64,
    pub audio_hop: f64,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            code: false,
            include_code: false,
            include_datasets: false,
            max_code_bytes: 1 << 20,
            max_notebook_bytes: 20 << 20,
            code_stretch: 1800,
            big_pdf_pages: 50,
            pdf_step: 30,
            doc_step: 40,
            max_pages: 5_000,
            max_chunks: 5_000,
            excluded_folders: Vec::new(),
            excluded_extensions: HashSet::new(),
            min_image_side: 256,
            min_page_text: 200,
            photo_budget: 140,
            screenshot_budget: 280,
            page_budget: 280,
            frame_budget: 70,
            frame_interval: 3.0,
            audio_window: 30.0,
            audio_hop: 25.0,
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

    /// `IndexOptions.fingerprint` (Model.swift:86-91): an index never mixes settings. Media settings
    /// print as Swift prints a Double ("3.0", "25.0", "1.5"), which is what the stored fingerprint
    /// text carries.
    pub fn fingerprint(&self) -> String {
        if self.code {
            return format!("code1 lines{}", self.code_stretch);
        }
        format!(
            "photo{} shot{} page{} frame{}/{}s audio{}/{}s",
            self.photo_budget,
            self.screenshot_budget,
            self.page_budget,
            self.frame_budget,
            swift_seconds(self.frame_interval),
            self.audio_window,
            swift_seconds(self.audio_hop)
        )
    }
}

/// Swift's `"\(3.0)"` is "3.0", and `"\(1.5)"` is "1.5": a whole number keeps one decimal.
fn swift_seconds(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}
