//! digup-runtime — EmbeddingGemma 2 on the vendored llama.cpp (ARCHITECTURE_MAP.md §3, Phase 2).
//!
//! Mirrors `Sources/LlamaRuntime`: `LlamaEmbedder` (the batch, pooling and normalization that make
//! a vector), `LlamaModels` + `ModelFiles` (which files, pinned by sha256), the device rules
//! (`cpuOnly`, `hasGPU`, threads) and the fp16 vector format. The llama.cpp it binds is the pinned
//! build `scripts/build-llama.sh` writes into `Vendor/llama` (tag b11461 plus
//! `scripts/llama.cpp-no-logits.patch`); without that build the crate still compiles and the
//! embedder reports why it is unavailable (`cfg(llama_unavailable)`).
//!
//! Dev runs need the vendored libraries on the loader path, as the Swift payload does:
//!
//! ```sh
//! scripts/build-llama.sh                                   # once: Vendor/llama
//! LD_LIBRARY_PATH=Vendor/llama/lib DIGUP_MODELS=/path/to/models \
//!   cargo run -q -p digup-runtime --example embed -- "payment declined error"
//! ```

pub mod image_sizing;
pub mod indexer;
pub mod models;
pub mod prompts;
pub mod worker;

pub use models::{LlamaModels, ModelFile, ModelFiles};
pub use prompts::{code_query, document, query};
pub use worker::{Embedder, Embedding, WorkerInfo};

#[cfg(not(llama_unavailable))]
mod backend;
#[cfg(not(llama_unavailable))]
mod embedder;
#[cfg(not(llama_unavailable))]
pub use backend::{
    backend_summary, cpu_only, has_gpu, init as backend_init, line as log_line, set_cpu_only,
    set_log, threads, BACKENDS_ENV, DEVICE_ENV, THREADS_ENV,
};
#[cfg(not(llama_unavailable))]
pub use embedder::{model_info, LlamaEmbedder, BUDGETS, VECTOR_VERSION};

use std::path::Path;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Worker(String),
    /// A model file is missing, so an indexing run stops and its files stay pending
    /// (`WorkerError.notFound`, LlamaModels.swift:36).
    #[error("not found: {0}")]
    NotFound(String),
    /// llama.cpp said no (a model that won't load, a batch that filled, a failed encode).
    #[error("llama.cpp: {0}")]
    Llama(String),
    #[error("io: {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    /// An index built with another model or other settings, with no reset-on-change set
    /// (`IndexerError`, Indexer.swift:3): the run stops, its files stay pending.
    #[error(transparent)]
    Indexer(#[from] indexer::FingerprintMismatch),
    /// The store said no (`DatabaseError`): shown verbatim, as Swift's `\(error)` spells it.
    #[error(transparent)]
    Core(#[from] digup_core::db::Error),
}

impl Error {
    pub fn worker(message: impl Into<String>) -> Self {
        Self::Worker(message.into())
    }

    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

/// `LlamaEmbedder.buildTag` (`llama-build.h`, written by scripts/build-llama.sh): for logs and
/// `digup status`. Indexes record `VECTOR_VERSION`, not this.
pub fn build_tag() -> &'static str {
    #[cfg(not(llama_unavailable))]
    {
        std::ffi::CStr::from_bytes_with_nul(ffi::LLAMA_BUILD_TAG)
            .ok()
            .and_then(|s| s.to_str().ok())
            .unwrap_or("unknown")
    }
    #[cfg(llama_unavailable)]
    {
        "llama.cpp not vendored"
    }
}

/// The vendored, pinned llama.cpp's C surface (bindgen over `Vendor/llama/include`).
#[cfg(not(llama_unavailable))]
pub mod ffi {
    #![allow(non_upper_case_globals)]
    #![allow(non_camel_case_types)]
    #![allow(non_snake_case)]
    #![allow(dead_code)]
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/llama_bindings.rs"));
}
