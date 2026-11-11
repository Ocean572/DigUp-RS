//! The speed check's fixtures and arithmetic, pinned: the BMP and WAV bytes the engine's
//! `ModelSpeed.bitmap` / `ModelSpeed.wave` writers make (hashed — the same fixture on every
//! machine is the point), the `Times`/`factors`/`agreement` rules, and the `measure` loop's
//! stops (`agree`, the rival's rule) against a stub embedder.

use std::collections::{BTreeMap, HashMap};

use digup_runtime::worker::{Embedder, Embedding, WorkerInfo};
use digup_services::speed::{agreement, bitmap, factors, measure, wave, Times, AGREE};

/// FNV-1a 64, the fingerprint `examples/fixtures.rs` prints and the Swift-side transcription
/// (transcribed from `ModelSpeed.swift` and run beside it) agrees with.
fn fnv(data: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[test]
fn the_fixtures_are_the_engine_bytes() {
    let picture = bitmap(960, 720);
    assert_eq!(picture.len(), 2_073_654, "BM header 54 + 2160·720");
    assert_eq!(
        fnv(&picture),
        0x3e62_166e_54e9_341e,
        "the BMP the Swift writer makes, byte for byte"
    );
    let sound = wave(30);
    assert_eq!(
        sound.len(),
        960_044,
        "WAV header 44 + 30 s of 16 kHz mono 16-bit"
    );
    assert_eq!(
        fnv(&sound),
        0xca26_6a8f_4040_0d92,
        "the WAV the Swift writer makes, byte for byte"
    );
}

#[test]
fn times_scale_the_unmeasured_inputs_on_the_picture() {
    // The screenshot and the frame "go as the picture does": they take the picture's ratio.
    let times = Times::from_three(1.0, 0.246, 3.0); // picture is twice the M4 Pro's
    assert_eq!(times.picture, 0.246);
    assert_eq!(times.screen, 0.265 * 2.0);
    assert_eq!(times.frame, 0.067 * 2.0);
    let fields = times.fields();
    assert_eq!(fields.len(), 5);
    let back = Times::from_fields(&fields).expect("fields read back");
    assert_eq!(back.text, 1.0);
    assert_eq!(back.sound, 3.0);
    let mut partial = fields.clone();
    partial.remove("frame");
    assert!(
        Times::from_fields(&partial).is_none(),
        "a missing field is no Times"
    );
}

#[test]
fn factors_at_the_baseline_are_one() {
    let baseline = Times {
        text: 0.026,
        picture: 0.123,
        screen: 0.265,
        frame: 0.067,
        sound: 0.220,
    };
    let kinds = factors(&baseline);
    for name in [
        "image",
        "pdfPage",
        "docChunk",
        "codeStretch",
        "audioSecond",
        "videoSecond",
    ] {
        let factor = kinds[name];
        assert!(
            (factor - 1.0).abs() < 1e-12,
            "{name} ×{factor} at the baseline"
        );
    }
    // The M4 Pro's screenshot factor includes Vision's OCR, which this machine has not: the
    // screenshot here is the picture part alone (the engine's `(cost − OCR) / cost`).
    assert!(
        (kinds["screenshot"] - 0.265 / 0.32).abs() < 1e-12,
        "screenshot {}",
        kinds["screenshot"]
    );
}

#[test]
fn agreement_is_the_least_cosine_of_the_three_inputs() {
    let mut one: HashMap<String, Vec<f32>> = HashMap::new();
    let mut other: HashMap<String, Vec<f32>> = HashMap::new();
    for name in ["text", "picture", "sound"] {
        one.insert(name.to_string(), vec![1.0, 0.0, 0.0]);
        other.insert(name.to_string(), vec![1.0, 0.0, 0.0]);
    }
    assert!((agreement(&one, &other) - 1.0).abs() < 1e-6);
    // The weakest input sets the number: 0.8 on sound is the agreement.
    other.insert("sound".to_string(), vec![0.8, 0.6, 0.0]); // unit, cosine 0.8 with a
    let expect = 0.8_f64;
    assert!(
        (agreement(&one, &other) - expect).abs() < 1e-6,
        "agreement {}",
        agreement(&one, &other)
    );
    // A missing input is 0 — the device test fails closed.
    other.remove("picture");
    assert_eq!(agreement(&one, &other), 0.0);
}

/// A stub model: times are canned, so `measure`'s stops are testable without a GPU.
struct StubEmbedder {
    seconds: f64,
}

impl Embedder for StubEmbedder {
    fn info(&self) -> &WorkerInfo {
        unimplemented!("the speed check does not ask")
    }

    fn embed_texts(&self, texts: &[String]) -> digup_runtime::Result<Vec<Vec<f32>>> {
        std::thread::sleep(std::time::Duration::from_secs_f64(self.seconds));
        Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
    }

    fn embed_images(
        &self,
        paths: &[String],
        _budget: i32,
    ) -> digup_runtime::Result<Vec<Embedding>> {
        std::thread::sleep(std::time::Duration::from_secs_f64(self.seconds));
        Ok(paths
            .iter()
            .map(|_| Embedding::made(vec![0.0, 1.0, 0.0]))
            .collect())
    }

    fn embed_audio(&self, paths: &[String]) -> digup_runtime::Result<Vec<Embedding>> {
        std::thread::sleep(std::time::Duration::from_secs_f64(self.seconds));
        Ok(paths
            .iter()
            .map(|_| Embedding::made(vec![0.0, 0.0, 1.0]))
            .collect())
    }

    fn close(&mut self) {}
}

#[test]
fn measure_keeps_three_vectors_and_stops_when_the_best_matches() {
    let stub = StubEmbedder { seconds: 0.01 };
    let made = measure(&stub, 5, None).expect("the check runs");
    for name in ["text", "picture", "sound"] {
        assert_eq!(made.vectors[name].len(), 3, "the {name} vector");
    }
    // Every try takes the same time: the first matches the second, so the loop stops early.
    assert_eq!(made.tries.len(), 2, "matched after the second try");
    assert!(made.steady);
    // The totals agree within `agree`: the best is the second's by definition.
    let fastest = made.tries.iter().fold(f64::INFINITY, |a, &b| a.min(b));
    assert!(
        made.tries[0] <= fastest * AGREE * 1.001,
        "the tries {:?} (agree {AGREE})",
        made.tries
    );
    assert!(made.total() > 0.0);
    assert!(made.total() < 1.0);
}

#[test]
fn measure_stops_for_a_rival_it_cannot_beat() {
    // Two tries clearly (×1.3) slower than the rival's total: the rival is kept whatever
    // these times are, so the check stops without matching.
    let stub = StubEmbedder { seconds: 0.2 };
    let rival = 0.05; // far faster than 3 × 0.2 s
    let made = measure(&stub, 9, Some(rival)).expect("the check runs");
    assert_eq!(
        made.tries.len(),
        2,
        "two tries clearly over ×1.3 of the rival stop the check"
    );
    assert!(
        made.steady,
        "the tries match each other, so the check reads as steady"
    );
    assert!(made.total() > rival * 1.3);
}

/// The `Times` the `factors` of a measured run produce, as `costs.json` keeps them: BTreeMap
/// for the stable order the Swift encoder's `.sortedKeys` gives.
#[test]
fn factors_cover_every_cost_field() {
    let times = Times::from_three(0.05, 0.3, 0.5);
    let kinds = factors(&times);
    let names: BTreeMap<String, ()> = [
        "image",
        "screenshot",
        "pdfPage",
        "docChunk",
        "codeStretch",
        "audioSecond",
        "videoSecond",
    ]
    .iter()
    .map(|name| (name.to_string(), ()))
    .collect();
    assert_eq!(
        kinds.keys().collect::<Vec<_>>(),
        names.keys().collect::<Vec<_>>(),
        "the same seven fields costs.json keeps"
    );
    for factor in kinds.values() {
        assert!(factor.is_finite() && *factor > 0.0, "factor {factor}");
    }
}
