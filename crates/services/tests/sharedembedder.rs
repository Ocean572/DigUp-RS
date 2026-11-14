//! The shared-embedder seam (issue #155) and the shape info it answers with.
//!
//! Two things are measured here, both from the defect list of the audit
//! (docs/RUST_PORT_AUDIT.md, wave 1 row 13):
//!
//! 1. `Kept::info` answers with the SHAPE's own value and allocates nothing per call.
//!    At `audit-base` this seam answered a `&'static` borrow with a `Box::leak` per
//!    embedding call — a 1.8M-file run embeds some 2M times, so a daemon meant to sit
//!    warm between runs grew a little at every file. The measurement is the process's
//!    own resident memory across 200 k calls, read with `watchdog::footprint`: the tool
//!    that ends a runaway helper on this box, not a proxy.
//!
//! 2. The two #155 stickiness rules: a restart only when a full model is wanted and the
//!    loaded model is not full; a MEDIA call is what sets the sticky flag, and a later
//!    text call must not clear it (the model is never paid for twice).
//!
//! No model, no index: the factory hands out a fake embedder, so this measures the
//! seam's own bookkeeping.

use std::sync::{Arc, Mutex};

use digup_runtime::worker::{Embedder, Embedding, WorkerInfo};
use digup_services::sharedembedder::SharedEmbedder;
use digup_services::watchdog::footprint;

/// The fake: `text_only` decides the shape it reports, and every build is counted, so a
/// restart the rules do not call for shows up as a second build.
struct Fake {
    info: WorkerInfo,
}

impl Fake {
    fn new(text_only: bool) -> Self {
        Self {
            info: WorkerInfo {
                model: "fake-gemma".to_string(),
                revision: "testrev0000".to_string(),
                runtime: "llama.cpp-test".to_string(),
                dtype: "q8_0".to_string(),
                dim: 768,
                text_only,
                load_seconds: Some(0.01),
            },
        }
    }
}

impl Embedder for Fake {
    fn info(&self) -> &WorkerInfo {
        &self.info
    }
    fn embed_texts(&self, texts: &[String]) -> digup_runtime::Result<Vec<Vec<f32>>> {
        Ok(vec![vec![0.0; 768]; texts.len()])
    }
    fn embed_images(
        &self,
        paths: &[String],
        _budget: i32,
    ) -> digup_runtime::Result<Vec<Embedding>> {
        Ok(paths
            .iter()
            .map(|_| Embedding::made(vec![0.0; 768]))
            .collect())
    }
    fn embed_audio(&self, paths: &[String]) -> digup_runtime::Result<Vec<Embedding>> {
        Ok(paths
            .iter()
            .map(|_| Embedding::made(vec![0.0; 768]))
            .collect())
    }
    fn close(&mut self) {}
}

fn seam(builds: &Arc<Mutex<Vec<bool>>>) -> Arc<SharedEmbedder> {
    let counter = builds.clone();
    Arc::new(SharedEmbedder::new(Arc::new(move |text_only: bool| {
        counter.lock().unwrap().push(text_only);
        Ok(Arc::new(Fake::new(text_only)) as Arc<dyn Embedder>)
    })))
}

fn counts(builds: &Arc<Mutex<Vec<bool>>>) -> (usize, usize) {
    let built = builds.lock().unwrap();
    (built.len(), built.iter().filter(|t| !*t).count())
}

#[test]
fn info_answers_from_the_shape_and_allocates_nothing_per_call() {
    let builds = Arc::new(Mutex::new(Vec::new()));
    let model = seam(&builds);
    let kept = model.embedder().expect("a text-only model starts");
    assert!(
        kept.info().text_only,
        "a run that wants no media pays for none"
    );

    let before = footprint().expect("/proc/self/statm is readable on Linux");
    let mut seen = 0usize;
    for round in 0..200_000usize {
        let info = kept.info();
        // The answer is the shape's, and it is the SAME value every call — the borrow
        // pins that it is one value, not a fresh copy (a copy would let a field drift).
        assert_eq!(info.dim, 768);
        seen += info.dim / 768;
        std::hint::black_box(info.model.as_str());
        std::hint::black_box(round);
    }
    let after = footprint().expect("/proc/self/statm is readable on Linux");
    assert_eq!(seen, 200_000);
    assert_eq!(
        counts(&builds),
        (1, 0),
        "info() must not restart the model: the shape is decided by the queue and by media calls"
    );
    let grew = after.saturating_sub(before);
    assert!(
        grew < 1 << 20,
        "200 k info() calls grew the process by {grew} bytes; the answer must be the shape's \
         own borrow (at audit-base this seam leaked a WorkerInfo per call)"
    );
}

#[test]
fn a_media_call_buys_the_encoders_once_and_text_calls_keep_them() {
    let builds = Arc::new(Mutex::new(Vec::new()));
    let model = seam(&builds);
    let kept = model.embedder().expect("text-only first");
    assert!(kept.info().text_only);

    // The queue said text; a scanned page shows up as a picture. The call upgrades the
    // shape on the spot (#155) — and the answer says so.
    kept.embed_images(&["/var/tmp/a/page-3.jpg".to_string()], 1)
        .expect("the media call runs on the full model");
    assert!(
        !kept.info().text_only,
        "a media call brings the encoders in"
    );
    assert_eq!(
        counts(&builds),
        (2, 1),
        "exactly one full-model build for the media call"
    );

    // Text after media runs on the full model: no second full build, no drop-and-reload.
    for _ in 0..3 {
        kept.embed_texts(&["the notes".to_string()])
            .expect("text runs on either shape");
    }
    assert_eq!(
        counts(&builds),
        (2, 1),
        "the encoders are not dropped and paid for twice"
    );
    assert!(!kept.info().text_only);

    // A second helper's `embedder()` on a shape already decided full: one more handle,
    // no third build (`!start && wants_full && !full` is the restart rule).
    let _again = model
        .embedder()
        .expect("the sticky shape serves the next handle");
    assert_eq!(counts(&builds), (2, 1), "no restart of a full model");
}

#[test]
fn close_leaves_the_shared_model_to_the_other_part() {
    let builds = Arc::new(Mutex::new(Vec::new()));
    let model = seam(&builds);
    let mut kept = model.embedder().expect("a model starts");
    kept.close(); // `Kept::close` leaves it be: the other part may be using it
    assert!(model.is_running(), "a Kept close does not free the model");
    kept.embed_texts(&["the notes".to_string()])
        .expect("and the handle still serves");
    model.close();
    assert!(!model.is_running(), "the helper's own close frees it");
}
