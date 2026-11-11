//! HTTP /api/v1 wrapper — Rust port. Scaffold in Phase 1.
//!
//! Phase 6+ reproduces the FastAPI surface (`/api/v1/{health,status,search,index,jobs,estimate,
//! embed_ahead,media/*,targets,tokens}`) as axum: same shapes, same single-writer `flock`, same
//! single-uvicorn invariant (media ids in one LRU), same tokens/targets scoping. This Phase-1 stub
//! just reports the schema version the server layer is built to speak.

fn main() {
    println!(
        "digup-server scaffold: /api/v1 lands in Phase 6 (schema v{}, engine v1)",
        digup_core::SCHEMA_VERSION
    );
}
