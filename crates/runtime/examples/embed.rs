//! Dev tool (not shipped): make a vector for a text with the Rust embedder, so a run can be
//! compared with the engine's numbers by eye.
//!
//! ```sh
//! DIGUP_MODELS=$HOME/digup-data/models cargo run -q -p digup-runtime --example embed -- "payment declined error"
//! DIGUP_MODELS=$HOME/digup-data/models cargo run -q -p digup-runtime --example embed -- --document notes.txt "the notes"
//! ```

use digup_runtime::{prompts, Embedder, LlamaEmbedder, LlamaModels};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let document = args.first().is_some_and(|a| a == "--document");
    let (title, text) = if document {
        (
            args.get(1).map(String::as_str),
            args.get(2).cloned().unwrap_or_default(),
        )
    } else {
        (
            None,
            args.first()
                .cloned()
                .unwrap_or_else(|| "warm up".to_string()),
        )
    };

    let models = LlamaModels::locate().expect("model files: set DIGUP_MODELS");
    let prompt = if document {
        prompts::document(title, &text)
    } else {
        prompts::query(&text)
    };

    let mut embedder = LlamaEmbedder::new(&models, true, None, None).expect("load the text model");
    let vectors = embedder.embed_texts(&[prompt]).expect("embed");
    let info = embedder.info().clone();
    embedder.close();

    let vector = &vectors[0];
    let length: f64 = vector
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt();
    println!(
        "model: {} ({}, llama.cpp {})",
        info.model,
        info.runtime,
        digup_runtime::build_tag()
    );
    println!(
        "loaded in {:?} s (text-only)",
        info.load_seconds.unwrap_or_default()
    );
    println!("dim: {}\nnorm: {:.6}", vector.len(), length);
    let head: Vec<String> = vector.iter().take(6).map(|x| format!("{x:.5}")).collect();
    println!("first: {}", head.join(", "));
}
