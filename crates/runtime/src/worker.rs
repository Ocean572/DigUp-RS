//! The embedding runtime's shapes, as the engine defines them (`Sources/DigUpKit/Worker.swift`).
//!
//! `WorkerInfo` goes into index fingerprints, so its strings are part of the contract: vectors
//! from a different model, revision, runtime or precision must never share an index. `Embedding`
//! carries a vector *or* why the file got none — one unreadable file must not sink its neighbours.

/// What every embedder reports (`WorkerInfo`, Worker.swift:4-26). `runtime` names the vector
/// pipeline version, so a llama.cpp update that keeps the vectors keeps the index.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerInfo {
    pub model: String,
    pub revision: String,
    pub runtime: String,
    pub dtype: String,
    pub dim: usize,
    pub text_only: bool,
    pub load_seconds: Option<f64>,
}

impl WorkerInfo {
    /// `vectorSource`: written into `meta`, so an index never mixes vectors from two pipelines.
    pub fn vector_source(&self) -> String {
        let revision: String = self.revision.chars().take(7).collect();
        format!("{}@{revision} {} {}", self.model, self.runtime, self.dtype)
    }
}

/// One file's vector, or why it has none (`Embedding`, Worker.swift:95-103). The error text is
/// capped at 300 characters by the engine, and goes into the file's `detail`.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding {
    pub vector: Option<Vec<f32>>,
    pub error: Option<String>,
}

impl Embedding {
    pub fn made(vector: Vec<f32>) -> Self {
        Self {
            vector: Some(vector),
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            vector: None,
            error: Some(error.into()),
        }
    }
}

/// The four calls every embedding runtime answers (the `Embedder` protocol, Worker.swift:31-38):
/// the model in this process (llama.cpp), or a JSON-lines worker process (Phase 3+).
///
/// One thread at a time, as in the engine: llama.cpp's context is not re-entrant.
pub trait Embedder {
    fn info(&self) -> &WorkerInfo;
    fn embed_texts(&self, texts: &[String]) -> crate::Result<Vec<Vec<f32>>>;
    fn embed_images(&self, paths: &[String], budget: i32) -> crate::Result<Vec<Embedding>>;
    fn embed_audio(&self, paths: &[String]) -> crate::Result<Vec<Embedding>>;
    /// Frees the model (about 1 GB, or 0.25 GB text-only). Safe to call twice.
    fn close(&mut self);
}
