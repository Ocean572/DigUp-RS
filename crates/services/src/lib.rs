//! digup-services — the JSON-lines services in Rust (ARCHITECTURE_MAP.md §1 "the seam").
//!
//! Scaffold: the verb lists a port must reproduce byte-for-byte so `helper.rs` (Tauri) and
//! `protocol.py` (FastAPI) stay compatible. The reader/writer loops (spawn argv, route replies by
//! `id`, treat idle-exit as normal) land in Phase 3+.
//!
//! The protocol (Sources/digup/SearchService.swift, IndexService.swift): one JSON object per line
//! in; the reply echoes the request's `id`; lines without an `id` are events. `index-service` also
//! takes plain-text commands, and closing stdin (EOF) stops it.

/// `search-service` request ops (SearchService.swift:9-32).
pub const SEARCH_OPS: [&str; 10] = [
    "search",
    "embed_ahead",
    "text",
    "render",
    "reload",
    "summaries",
    "folders",
    "prewarm",
    "status",
    "quit",
];

/// `index-service` plain-text commands (IndexService.swift / docs §4).
pub const INDEX_COMMANDS: [&str; 4] = ["sync", "sync code", "stop", "backfill on"];

/// `index-service` events out.
pub const INDEX_EVENTS: [&str; 10] = [
    "scanning", "sync", "file", "rest", "summary", "waiting", "idle", "error", "exit", "reindex",
];
