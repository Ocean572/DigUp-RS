//! digup-runtime — LlamaRuntime in Rust (ARCHITECTURE_MAP.md §1, §3).
//!
//! Scaffold only in Phase 1. Later: llama.cpp via Rust bindings; the 768×fp16 EmbeddingGemma 2
//! embedder, `--text-only` shape, and the measured device choice (`speed --model`, cosine ≥ 0.999).

pub const DIMENSION: usize = digup_core::vectors::DIMENSION;
