//! `SharedEmbedder` (IndexService.swift:403): one model for both indexes, started
//! when the first file needs it and closed when the helper is done. The indexers get
//! it as an `Embedder` whose `close` leaves it be, since the other may be using it.
//!
//! Issue #155: it starts **text-only** — the image and audio encoders (555 MB of
//! projector on this box's reference model) stay unloaded until a run says its queue
//! holds files that need them (`note(needs_media:)`, from `pending_kinds`), or one
//! shows up where the queue said text — a scanned PDF's page, read as a picture,
//! upgrades on the spot through `Kept`. A helper indexing only text and code never
//! pays for the encoders, in the run *or* in the idle linger after it, where the
//! model is kept warm. Once media has been wanted the shape is sticky: the encoders
//! are not dropped and paid for twice on the next image.
//!
//! Embedding runs on the service's one loop thread (both parts), so the lock guards
//! the shape's state.

use std::sync::{Arc, Mutex, OnceLock};

use digup_runtime::worker::{Embedder, Embedding, WorkerInfo};
use digup_runtime::Result;

use crate::emit::emit;

/// The factory: `text_only` picks the shape (the media encoders load with `false`).
/// The model is an `Arc`: the `Kept` handle and the shared owner both keep it, and
/// `Kept::close` — like the engine's — leaves it be, since the other part may use it.
pub type MakeModel = Arc<dyn Fn(bool) -> Result<Arc<dyn Embedder>> + Send + Sync>;

pub struct SharedEmbedder {
    make: MakeModel,
    lock: Mutex<Shape>,
}

#[derive(Default)]
struct Shape {
    model: Option<Arc<dyn Embedder>>,
    /// Decided: a media embed has been wanted (sticky).
    wants_full: bool,
    /// `model` as loaded holds the media encoders.
    full: bool,
    /// The shape info last handed out; `Kept::info` answers with it.
    last_info: Option<WorkerInfo>,
}

/// The write side of the shape, under one lock. (The reader in `use_model` can't
/// hold the guard across the `body` call — `swap` re-enters the lock — so it takes
/// an Arc clone of the live model; a concurrent `close` may leave the old model
/// loaded for the call's duration, where Swift's lock-for-the-call would free it
/// the instant the call ends. Single-threaded services never hit this.)
fn shape<T>(lock: &Mutex<Shape>, write: impl FnOnce(&mut Shape) -> T) -> T {
    let mut shape = lock.lock().expect("the model lock");
    write(&mut shape)
}

impl SharedEmbedder {
    pub fn new(make: MakeModel) -> Self {
        Self {
            make,
            lock: Mutex::new(Shape::default()),
        }
    }

    pub fn is_running(&self) -> bool {
        self.lock.lock().expect("the model lock").model.is_some()
    }

    /// At a run's start: the queue's verdict, and the running model re-shaped to it
    /// *before* any file is read. A start that fails here is kept for `embedder()`,
    /// where its error marks files the way a model failure always has.
    pub fn note(&self, needs_media: bool) {
        let restart = shape(&self.lock, |shape| {
            shape.wants_full = shape.wants_full || needs_media;
            shape.wants_full && shape.model.is_some() && !shape.full
        });
        if restart {
            let _ = self.swap();
        }
    }

    pub fn embedder(self: &Arc<Self>) -> Result<Box<dyn Embedder>> {
        let (start, text_only, restart) = {
            let shape = self.lock.lock().expect("the model lock");
            let start = shape.model.is_none();
            (
                start,
                !shape.wants_full,
                // Swift's `let restart = !start && wantsFull && !full` — a full model
                // serves text calls too (the Kept doc: "text calls run on either
                // shape"), so a text request on the sticky full model is no restart.
                !start && shape.wants_full && !shape.full,
            )
        };
        if restart {
            self.swap()?;
        }
        if start {
            let started = (self.make)(text_only)?;
            let info = started.info().clone();
            let full = !text_only;
            shape(&self.lock, |shape| {
                shape.model = Some(started);
                shape.full = full;
                shape.last_info = Some(info);
            });
            if !text_only {
                emit(&serde_json::json!({"event": "model", "full": true}));
            }
        }
        Ok(Box::new(Kept {
            owner: self.clone(),
        }))
    }

    /// The model re-shaped to full. Called from an embedding call (`Kept`) or a
    /// run's start. A failed load falls back to the text-only shape (the file that
    /// asked for media fails as any model failure does); without the fallback a
    /// failed swap would strand the session: `wants_full` sticky, model gone, every
    /// later text embed answering "the model is closed".
    fn swap(&self) -> Result<()> {
        let old = shape(&self.lock, |shape| shape.model.take());
        drop(old);
        match (self.make)(false) {
            Ok(started) => {
                let info = started.info().clone();
                shape(&self.lock, |shape| {
                    shape.model = Some(started);
                    shape.full = true;
                    shape.wants_full = true;
                    shape.last_info = Some(info);
                });
                emit(&serde_json::json!({"event": "model", "full": true}));
                Ok(())
            }
            Err(e) => {
                shape(&self.lock, |shape| shape.wants_full = false);
                Err(e)
            }
        }
    }

    pub fn close(&self) {
        let model = shape(&self.lock, |shape| shape.model.take());
        drop(model);
    }

    /// `Kept`'s calls: the live model (media calls bring the encoders in now), or a
    /// model re-shaped to what `wants_full` decided. Swift's `use(media:_:)` sets the
    /// sticky flag only when a media call asks for it — a text call leaves the decision
    /// of an earlier media call standing ("the encoders are not dropped and paid for
    /// twice on the next image"), and text runs on either shape.
    fn use_model<T>(
        &self,
        media: bool,
        body: impl FnOnce(&dyn Embedder) -> Result<T>,
    ) -> Result<T> {
        let wants = shape(&self.lock, |shape| shape.wants_full);
        if media && !wants {
            shape(&self.lock, |shape| shape.wants_full = true);
            self.swap()?; // the first media embed: bring the encoders in now
        }
        let restart = {
            let shape = self.lock.lock().expect("the model lock");
            shape.model.is_some() && shape.wants_full && !shape.full
        };
        if restart {
            self.swap()?;
        }
        let model = {
            let shape = self.lock.lock().expect("the model lock");
            shape.model.clone()
        };
        // The indexers' `Embedder` is `Arc<dyn Embedder>`: a model made by a factory
        // that shares it is one `Arc` shared by both parts.
        let model = model.ok_or_else(|| {
            digup_runtime::Error::NotFound("the model is closed".to_string())
        })?;
        body(model.as_ref())
    }

    /// `Kept.info`'s answer: the live model's shape, or the last one seen. `embedder()`
    /// hands out a `Kept` only once a model is set, and every set updates `last_info`,
    /// so this never panics on a closed model. `Kept::info` must answer by borrow, so
    /// the answer lives in a process-wide cell updated at every set — the same value
    /// Swift's computed property returns per call.
    fn info_now(&self) -> &'static WorkerInfo {
        let info = {
            let shape = self.lock.lock().expect("the model lock");
            shape
                .model
                .as_ref()
                .map(|m| m.info().clone())
                .or_else(|| shape.last_info.clone())
                .unwrap_or_else(|| WorkerInfo {
                    model: "digup".to_string(),
                    revision: String::new(),
                    runtime: String::new(),
                    dtype: String::new(),
                    dim: 0,
                    text_only: true,
                    load_seconds: None,
                })
        };
        let cell = INFO.get_or_init(|| Mutex::new(info.clone()));
        *cell.lock().expect("the info lock") = info;
        let guard = cell.lock().expect("the info lock");
        // Leak per call is a WorkerInfo per embedding call; the indexers copy the
        // fields they need and drop them. Box::leak keeps `info` a `&'static` borrow
        // as the trait requires; the cell keeps exactly one copy alive.
        Box::leak(Box::new(guard.clone()))
    }
}

static INFO: OnceLock<Mutex<WorkerInfo>> = OnceLock::new();

/// The indexers' view: always the *current* model — media calls upgrade it on the
/// spot (issue #155: a scanned PDF page from a queue that said text), text calls run
/// on either shape.
struct Kept {
    owner: Arc<SharedEmbedder>,
}

impl Embedder for Kept {
    fn info(&self) -> &WorkerInfo {
        self.owner.info_now()
    }

    fn embed_texts(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.owner.use_model(false, |m| m.embed_texts(texts))
    }

    fn embed_images(&self, paths: &[String], budget: i32) -> Result<Vec<Embedding>> {
        self.owner.use_model(true, |m| m.embed_images(paths, budget))
    }

    fn embed_audio(&self, paths: &[String]) -> Result<Vec<Embedding>> {
        self.owner.use_model(true, |m| m.embed_audio(paths))
    }

    fn close(&mut self) {}
}
