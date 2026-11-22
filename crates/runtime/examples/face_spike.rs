//! Phase-0 cost/parity spike (FACES.md, dev harness — not shipped): runs the native
//! face pipeline (SCRFD detect → 5-point align → embed) over the public Wikimedia
//! face fixtures and prints the per-photo / per-face milliseconds plus the peak
//! resident memory, writing per-photo JSON lines for the agreement comparison
//! against the Python reference harness (`insightface` on onnxruntime CPU).
//!
//! ```text
//! LD_LIBRARY_PATH=Vendor/llama/lib cargo run -q -p digup-runtime --features faces \
//!   --example face_spike -- /tmp/opencode/face-fixtures /tmp/opencode/face-rust
//! ```
//!
//! Models come from `DIGUP_FACE_MODELS` (default `/tmp/opencode/face-models`):
//! `scrfd_10g_bnkps.onnx` and the embedders listed in `DIGUP_FACE_EMBEDS`
//! (comma-separated basenames; default `w600k_mbf.onnx`).

use std::path::Path;
use std::time::Instant;

use digup_native::rgb::RgbImage;
use digup_runtime::face::{Embedder, Scrfd};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture_arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/opencode/face-fixtures".to_string());
    let out_arg = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/tmp/opencode/face-rust".to_string());
    let fixtures = Path::new(&fixture_arg);
    let out_dir = Path::new(&out_arg);
    std::fs::create_dir_all(out_dir)?;
    let models = std::env::var("DIGUP_FACE_MODELS")
        .unwrap_or_else(|_| "/tmp/opencode/face-models".to_string());
    let embed_names: Vec<String> = std::env::var("DIGUP_FACE_EMBEDS")
        .unwrap_or_else(|_| "w600k_mbf.onnx".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let mut files: Vec<_> = std::fs::read_dir(fixtures)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map(|e| matches!(e.to_str(), Some("jpg") | Some("jpeg") | Some("png")))
                .unwrap_or(false)
        })
        .collect();
    files.sort();

    let t0 = Instant::now();
    let scrfd_path = Path::new(&models).join("scrfd_10g_bnkps.onnx");
    let scrfd = Scrfd::load(&scrfd_path, (640, 640))?;
    let mut embedders = Vec::new();
    for name in &embed_names {
        let t = Instant::now();
        let e = Embedder::load(&Path::new(&models).join(name))?;
        eprintln!(
            "load {} {:.1} ms dim {}",
            name,
            t.elapsed().as_secs_f64() * 1e3,
            e.output_dim()
        );
        embedders.push((name.clone(), e));
    }
    let loads_ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!("models loaded in {loads_ms:.1} ms");

    let mut total_detect_ms = 0f64;
    let mut total_embed_ms = 0f64;
    let mut total_decode_ms = 0f64;
    let mut n_photos = 0usize;
    let mut n_faces = 0usize;
    for path in &files {
        let td = Instant::now();
        let Some(img) = RgbImage::load(path) else {
            eprintln!("skip (decode fail) {:?}", path);
            continue;
        };
        let orientation = digup_native::exif::read(path)
            .map(|e| e.orientation)
            .unwrap_or(1);
        let img = img.oriented(orientation);
        total_decode_ms += td.elapsed().as_secs_f64() * 1e3;

        let t = Instant::now();
        let faces = scrfd.detect(&img.pixels, img.width, img.height)?;
        let detect_ms = t.elapsed().as_secs_f64() * 1e3;
        total_detect_ms += detect_ms;

        let dump = std::env::var("DIGUP_FACE_DUMP").is_ok();
        let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
        let mut out_faces = Vec::new();
        for (j, f) in faces.iter().enumerate() {
            let crop = digup_runtime::face::norm_crop(&img.pixels, img.width, img.height, f);
            if dump {
                // Raw RGB 112×112×3, named like the reference's `{stem}_{i:02d}_crop112.npy`
                // (index = our score-desc order; face_agree matches pairs by IoU).
                std::fs::write(
                    out_dir.join(format!("{stem}_{j:02}_crop112.rust.rgb8")),
                    &crop,
                )?;
            }
            let mut vectors = serde_json::Map::new();
            for (name, e) in &embedders {
                let t = Instant::now();
                let v = e.embed(&crop)?;
                total_embed_ms += t.elapsed().as_secs_f64() * 1e3;
                vectors.insert(name.clone(), serde_json::json!(v));
            }
            out_faces.push(serde_json::json!({
                "bbox": f.box_, "score": f.score, "kps": f.kps, "vector": vectors,
            }));
        }
        n_faces += faces.len();
        n_photos += 1;
        let stem = path.file_stem().unwrap().to_str().unwrap();
        let rec = serde_json::json!({
            "file": path.file_name().unwrap().to_str().unwrap(),
            "w": img.width, "h": img.height,
            "faces": out_faces,
        });
        std::fs::write(
            out_dir.join(format!("{stem}.rust.json")),
            serde_json::to_string(&rec)?,
        )?;
        println!("{} faces {} detect {:.1} ms", stem, faces.len(), detect_ms);
    }

    let peak_kb = std::fs::read_to_string("/proc/self/status")?
        .lines()
        .find_map(|l| {
            l.strip_prefix("VmHWM:").map(|v| {
                v.split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .to_string()
            })
        })
        .unwrap_or_default();
    println!(
        "SUMMARY photos {} faces {} decode {:.1} ms detect {:.1} ms embed {:.1} ms \
         per-photo {:.1} ms per-face {:.1} ms VmHWM {} kB",
        n_photos,
        n_faces,
        total_decode_ms,
        total_detect_ms,
        total_embed_ms,
        (total_decode_ms + total_detect_ms + total_embed_ms) / n_photos.max(1) as f64,
        total_embed_ms / n_faces.max(1) as f64,
        peak_kb,
    );
    Ok(())
}
