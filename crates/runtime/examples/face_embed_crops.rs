//! Phase-0 isolation probe (FACES.md, dev harness — not shipped): embeds the
//! *reference harness's* aligned 112×112 crops with the Rust/tract embedder.
//! On byte-identical crops the cosine Rust↔reference is the model-numerics fact
//! alone (tract f32 vs onnxruntime f32) — with all upstream geometry removed.
//!
//! ```text
//! LD_LIBRARY_PATH=Vendor/llama/lib cargo run -q -p digup-runtime --features faces \
//!   --example face_embed_crops -- /tmp/opencode/face-ref-crops-rgb /tmp/opencode/face-rust-crops
//! ```

use std::path::Path;

use digup_runtime::face::Embedder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let crop_arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/opencode/face-ref-crops-rgb".to_string());
    let out_arg = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/tmp/opencode/face-rust-crops".to_string());
    let crops_dir = Path::new(&crop_arg);
    let out_dir = Path::new(&out_arg);
    std::fs::create_dir_all(out_dir)?;
    let models = std::env::var("DIGUP_FACE_MODELS")
        .unwrap_or_else(|_| "/tmp/opencode/face-models".to_string());
    let embed_names: Vec<String> = std::env::var("DIGUP_FACE_EMBEDS")
        .unwrap_or_else(|_| "w600k_mbf.onnx".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let mut embedders = Vec::new();
    for name in &embed_names {
        embedders.push((
            name.clone(),
            Embedder::load(&Path::new(&models).join(name))?,
        ));
    }

    let mut files: Vec<_> = std::fs::read_dir(crops_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "rgb8").unwrap_or(false))
        .collect();
    files.sort();
    let mut n = 0usize;
    for path in &files {
        let bytes = std::fs::read(path)?;
        if bytes.len() != 112 * 112 * 3 {
            eprintln!("skip (size {}) {:?}", bytes.len(), path);
            continue;
        }
        let mut vectors = serde_json::Map::new();
        for (name, e) in &embedders {
            let v = e.embed(&bytes)?;
            vectors.insert(name.clone(), serde_json::json!(v));
        }
        let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        std::fs::write(
            out_dir.join(format!("{stem}.embed.json")),
            serde_json::to_string(&serde_json::json!({"crop": stem, "vector": vectors}))?,
        )?;
        n += 1;
    }
    println!("embedded {n} crops");
    Ok(())
}
