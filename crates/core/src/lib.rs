//! digup-core — the DigUp engine core in Rust (ARCHITECTURE_MAP.md §3).
//!
//! Phase 1 scope: the rusqlite storage foundation (`db`, `schema`, `index_store`) reproducing the
//! Swift engine's `index.sqlite` / `code.sqlite` at `user_version = 6` — the connection contract,
//! the FTS5 table + tokenizer, and the v6 counters + 8 triggers. The value model (`model`), text
//! normalization (`search_text`), and the vector format (`vectors`) support it. Crawler, extractors,
//! and search land in later phases.

pub mod code;
pub mod code_lookup;
pub mod costs;
pub mod crawler;
pub mod db;
pub mod doc;
pub mod estimator;
pub mod extract;
pub mod folder_selection;
pub mod index_store;
pub mod jsonline;
pub mod mail;
pub mod model;
pub mod office;
pub mod options;
pub mod paths;
pub mod rtf;
pub mod rules;
pub mod schema;
pub mod scoring;
pub mod search_text;
pub mod searcher;
pub mod uuid;
pub mod vectors;
pub mod xmlscan;
pub mod zipfile;

pub use db::{Database, Error, Result, Sql};
pub use index_store::{Counts, IndexStore, CODE_FILE, MAIN_FILE, SCHEMA_VERSION};
pub use model::{Candidate, FileKind, FileRecord, Modality, SegmentKind};
