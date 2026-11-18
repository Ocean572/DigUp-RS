//! The stored reference vectors, in Rust (`ReferenceVectorTests.swift`): what this build makes for
//! fixed inputs must stay within cosine 0.999 of `Reference/vectors.json`, the file the engine's
//! suite reads. That is the llama.cpp upgrade gate: a new build that moves the vectors must bump
//! `VECTOR_VERSION` (every index then embeds again), or keep the old build.
//!
//! The inputs go in as the indexer hands them over — the app's prompts on the text side; a
//! screenshot as PNG at 280 tokens, a photo as JPEG at 140 and 70 (a video frame), and a 16 kHz
//! WAV, all read by llama.cpp's own decoders, exactly as `ReferenceVectorTests` feeds them. The
//! gate covers the prompt, the cut rule, `ImageSizing`, the chunk flow, the pooling and the
//! normalization, because every one of them shows up in the numbers.
//!
//! Needs the model files: skipped (with the reason printed) without them, as the engine's suite is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use digup_runtime::{prompts, Embedder, LlamaEmbedder, LlamaModels, VECTOR_VERSION};

/// `ReferenceVectorTests.agreement`: runtimes that agree measure ≥ 0.999, and less is a change
/// people would see in their results.
const AGREEMENT: f32 = 0.999;

#[derive(serde::Deserialize)]
struct Reference {
    #[serde(rename = "vector_version")]
    vector_version: i32,
    #[allow(dead_code)]
    #[serde(rename = "made_with")]
    made_with: String,
    vectors: HashMap<String, Vec<f32>>,
}

fn reference_file() -> PathBuf {
    let folder = std::env::var_os("DIGUP_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference"));
    folder.join("vectors.json")
}

fn cosine(one: &[f32], other: &[f32]) -> f32 {
    let dot: f64 = one
        .iter()
        .zip(other)
        .map(|(a, b)| (*a as f64) * (*b as f64))
        .sum();
    let norms = one
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt()
        * other
            .iter()
            .map(|x| (*x as f64) * (*x as f64))
            .sum::<f64>()
            .sqrt();
    if norms > 0.0 {
        (dot / norms) as f32
    } else {
        0.0
    }
}

/// The text inputs, made by the same rules as `ReferenceVectorTests.texts`: the app's prompts, the
/// long one built by joining the same 60 notes, about 1,800 tokens — past the sliding attention
/// window, so the global layers count as well.
fn text_inputs() -> Vec<(String, String)> {
    let long = (1..=60)
        .map(|note| {
            format!(
                "Note {note}: the review moved to room {} because the projector in the main hall failed again, \
                 and the team agreed to send the slides the night before.",
                note % 7 + 2
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        ("query".to_string(), prompts::query("payment declined error")),
        ("query-bn".to_string(), prompts::query("ইলিশ মাছ রান্না")),
        ("query-ar".to_string(), prompts::query("متى تغلق المكتبة")),
        (
            "document".to_string(),
            prompts::document(
                Some("Tokyo itinerary"),
                "Flight to Tokyo Haneda on November 3. Then the train to Kamakura to see the Great Buddha, and back for dinner in Shibuya.",
            ),
        ),
        ("document-long".to_string(), prompts::document(None, &long)),
    ]
}

/// `ReferenceVectorTests.images` plus the 16 kHz WAV: the same files, the same budgets
/// (`scripts/testbed/make_reference_inputs.py` made them).
fn media_inputs() -> Vec<(&'static str, &'static str, i32)> {
    vec![
        ("screen-280", "screen.png", 280),
        ("photo-140", "photo.jpg", 140),
        ("frame-70", "photo.jpg", 70),
    ]
}

#[test]
fn vectors_match_the_reference() {
    if cfg!(llama_unavailable) {
        eprintln!("skipped: llama.cpp is not vendored (scripts/build-llama.sh).");
        return;
    }
    let url = reference_file();
    if !url.is_file() {
        eprintln!("skipped: no reference at {}.", url.display());
        return;
    }
    let models = match LlamaModels::locate() {
        Ok(models) if models.media.is_some() => models,
        Ok(_) => {
            eprintln!(
                "skipped: {} is not downloaded yet (the media inputs need it).",
                LlamaModels::MEDIA_FILE
            );
            return;
        }
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let reference: Reference =
        serde_json::from_str(&std::fs::read_to_string(&url).unwrap()).unwrap();
    assert_eq!(
        reference.vector_version, VECTOR_VERSION,
        "the reference is for vectors {}; this build makes {}",
        reference.vector_version, VECTOR_VERSION
    );

    // The engine's suite runs the full model (`textOnly: false`): the media inputs need the
    // encoders, and the long document is past a 512-token query budget on purpose.
    let mut embedder = LlamaEmbedder::new(&models, false, None, None).expect("load the model");
    let mut made: HashMap<String, Vec<f32>> = HashMap::new();

    let inputs = text_inputs();
    let texts: Vec<String> = inputs.iter().map(|(_, text)| text.clone()).collect();
    for (index, (name, _)) in inputs.iter().enumerate() {
        let vectors = embedder
            .embed_texts(&texts[index..index + 1])
            .expect("embed the reference texts");
        made.insert(name.clone(), vectors[0].clone());
    }
    for (name, file, budget) in media_inputs() {
        let path = url.with_file_name(file).to_string_lossy().into_owned();
        let made_one = embedder
            .embed_images(std::slice::from_ref(&path), budget)
            .expect("embed_images")[0]
            .clone();
        made.insert(
            name.to_string(),
            made_one.vector.unwrap_or_else(|| {
                panic!("{file} at {budget}: {}", made_one.error.unwrap_or_default())
            }),
        );
    }
    let speech = {
        let path = url
            .with_file_name("speech.wav")
            .to_string_lossy()
            .into_owned();
        embedder.embed_audio(&[path]).expect("embed_audio")[0].clone()
    };
    made.insert(
        "speech".to_string(),
        speech
            .vector
            .unwrap_or_else(|| panic!("speech.wav: {}", speech.error.unwrap_or_default())),
    );
    embedder.close();

    assert_eq!(
        made.keys().collect::<std::collections::BTreeSet<_>>(),
        reference
            .vectors
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        "the inputs this suite makes must match the reference's"
    );
    for name in reference.vectors.keys() {
        let expected = &reference.vectors[name];
        let cosine = cosine(&made[name], expected);
        println!(
            "{name}: cosine {cosine:.5} (vectors {}, {}, llama.cpp {})",
            reference.vector_version,
            reference.made_with,
            digup_runtime::build_tag()
        );
        assert!(
            cosine >= AGREEMENT,
            "{name}: cosine {cosine} with vectors {} — llama.cpp {} makes other vectors: keep the old \
             llama.cpp, or bump VECTOR_VERSION",
            reference.vector_version,
            digup_runtime::build_tag(),
        );
    }
}
