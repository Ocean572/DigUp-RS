//! digup-services — the JSON-lines services in Rust (ARCHITECTURE_MAP.md §1 "the seam").
//!
//! The services (search-service, index-service, worker, …) answer their verbs byte-for-byte so
//! `helper.rs` (Tauri) and `protocol.py` (FastAPI) stay compatible: spawn argv, route replies by
//! `id`, treat idle-exit as normal.
//!
//! The protocol (Sources/digup/SearchService.swift, IndexService.swift): one JSON object per line
//! in; the reply echoes the request's `id`; lines without an `id` are events. `index-service` also
//! takes plain-text commands, and closing stdin (EOF) stops it.
//!
//! The verb lists live where they are answered — `search_service.rs` and `index_service.rs`
//! are the compiled lists, `docs/implementation/README.md` §4 the prose contract. A scaffold
//! copy here with no reader was a second list to drift.

pub mod costs;
pub mod crashnotes;
pub mod emit;
pub mod index_service;
pub mod parentwatch;
pub mod search_service;
pub mod sharedembedder;
pub mod speed;
pub mod system_load;
pub mod watchdog;
pub mod worker;
