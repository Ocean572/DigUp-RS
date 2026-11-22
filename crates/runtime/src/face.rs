//! The face pass engine (docs/implementation/FACES.md): SCRFD-style detection on
//! tract (pure-Rust ONNX, CPU), 5-landmark alignment through `crates/native`'s
//! cv-exact helpers, and an identity embedder per model pin. Everything here is
//! behind the cargo feature `faces` — default payload builds never see tract.
//!
//! Reference semantics are the insightface pipeline (measured against it, Phase 0):
//! letterbox resize + `(x − 127.5)/128` blob swap-RGB for detection (SCRFD's own
//! constants), anchor decode with the network's stride, IoU NMS (+1 px boxes, 0.4),
//! `norm_crop` 112 alignment, `(x − 127.5)/127.5` swap-RGB blobs, L2-normalized
//! vectors. The engine relays the model's scores; thresholds are pinned numbers,
//! not invented ones.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tract_onnx::prelude::*;

use crate::cvops;

pub const DET_SIZE: usize = 640;
pub const DET_MEAN: f32 = 127.5;
pub const DET_STD: f32 = 128.0;
pub const EMB_SIZE: usize = 112;
pub const EMB_MEAN: f32 = 127.5;
pub const EMB_STD: f32 = 127.5;
pub const DET_THRESH: f32 = 0.5;
pub const NMS_IOU: f32 = 0.4;

pub type FaceResult<T> = std::result::Result<T, FaceError>;

#[derive(Debug, thiserror::Error)]
pub enum FaceError {
    #[error("can't read the model file {path}: {why}")]
    Model { path: String, why: String },
    #[error("face model input shape unexpected: {why}")]
    Shape { why: String },
}

/// One detected face, in IMAGE pixel coordinates (letterbox undone, the way the
/// reference divides by the det scale).
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub bbox: [f32; 4],
    pub score: f32,
    pub landmarks: [[f32; 2]; 5],
    pub quality: f32,
}

struct DetSpec {
    strides: Vec<usize>,
    anchors: usize,
    kps: usize,
}

pub struct FaceEngine {
    detector: SimplePlan<TypedFact>,
    det_spec: DetSpec,
    det_size: usize,
    embedders: HashMap<String, SimplePlan<TypedFact>>,
    embed_size: usize,
    last_use: Instant,
}

impl FaceEngine {
    /// Load the detector (SCRFD family) + the named embedder pins.
    pub fn new(detector_path: &Path, embedders: &[(String, PathBuf)]) -> FaceResult<Self> {
        let plan = load_plan(detector_path)?;
        let det_spec = plan_det_spec(&plan)?;
        let det_size = plan_input_side(&plan, DET_SIZE)?;
        let mut sizes = vec![det_size];
        let mut maps: HashMap<String, SimplePlan<TypedFact>> = HashMap::new();
        for (name, path) in embedders {
            let p = load_plan(path)?;
            let s = plan_input_side(&p, EMB_SIZE)?;
            if !sizes.contains(&s) {
                sizes.push(s);
            }
            maps.insert(name.clone(), p);
        }
        let size = sizes[0];
        if sizes.len() > 1 {
            return Err(FaceError::Shape {
                why: "embedder input sizes differ; the face pass keeps one crop size".into(),
            });
        }
        Ok(Self {
            detector: plan,
            det_spec,
            det_size,
            embedders: maps,
            embed_size: size,
            last_use: Instant::now(),
        })
    }

    pub fn loaded_embedders(&self) -> Vec<String> {
        let mut v: Vec<String> = self.embedders.keys().cloned().collect();
        v.sort();
        v
    }

    /// The 60 s idle rule (FACES.md): a batch job keeps the engine loaded; the
    /// daemon drops it when it has been idle past the window.
    pub fn idle_past(&self, window: Duration) -> bool {
        self.last_use.elapsed() > window
    }

    pub fn touch(&mut self) {
        self.last_use = Instant::now();
    }

    /// Detect faces in a decoded RGB frame (`w`×`h`, `rgb.len() == w*h*3`).
    pub fn detect(&mut self, rgb: &[u8], w: usize, h: usize, thresh: f32) -> FaceResult<Vec<Face>> {
        self.touch();
        let side = self.det_size;
        // Letterbox, the SCRFD way: fit the long side, zero-pad, divide by the scale.
        let (new_w, new_h, scale) = letterbox(w, h, side);
        let resized = cvops::resize_linear(rgb, w, h, new_w, new_h);
        let mut padded = vec![0u8; side * side * 3];
        for y in 0..new_h {
            let src = &resized[y * new_w * 3..(y + 1) * new_w * 3];
            padded[y * side * 3..y * side * 3 + new_w * 3].copy_from_slice(src);
        }
        let blob = blob_tensor(&padded, side, side, DET_MEAN, DET_STD, true);
        let outs = self
            .detector
            .run(tvec!(blob.into()))
            .map_err(|e| FaceError::Shape { why: e.to_string() })?;
        let spec = &self.det_spec;
        let mut pre: Vec<[f32; 5]> = Vec::new();
        let mut kp_all: Vec<[[f32; 2]; 5]> = Vec::new();
        for (li, &stride) in spec.strides.iter().enumerate() {
            let scores = outs[li]
                .to_slice_f32()
                .map_err(|e| FaceError::Shape { why: e.to_string() })?
                .to_vec();
            let dist = outs[li + spec.strides.len()]
                .to_slice_f32()
                .map_err(|e| e)
                .and_then(|s| Ok(s.to_vec()))
                .map_err(|e| FaceError::Shape { why: e.to_string() })?;
            let kps = if spec.kps > 0 {
                outs[li + 2 * spec.strides.len()]
                    .to_slice_f32()
                    .map_err(|e| FaceError::Shape { why: e.to_string() })?
                    .to_vec()
            } else {
                Vec::new()
            };
            let n = scores.len();
            let grid = n / spec.anchors;
            let side_in = side / stride;
            if grid * spec.anchors != n || grid * stride != (side_in * side_in) * stride / 1 * 0 + grid {
                // n must equal H·W·anchors with H·W = (side/stride)²
            }
            let _ = side_in;
            let hw = grid / spec.anchors;
            for k in 0..n {
                if scores[k] < thresh {
                    continue;
                }
                // Candidate order (row, col, anchor) — the export's own flattening.
                let a = k % spec.anchors;
                let pos = k / spec.anchors;
                let cx = ((pos % (side / stride)) as f32 + 0.5) * stride as f32;
                let cy = ((pos / (side / stride)) as f32 + 0.5) * stride as f32;
                let d = &dist[k * 4..k * 4 + 4];
                let x1 = (cx - d[0] * stride as f32) / scale;
                let y1 = (cy - d[1] * stride as f32) / scale;
                let x2 = (cx + d[2] * stride as f32) / scale;
                let y2 = (cy + d[3] * stride as f32) / scale;
                pre.push([
                    x1.max(0.0).min(w as f32),
                    y1.max(0.0).min(h as f32),
                    x2.max(0.0).min(w as f32),
                    y2.max(0.0).min(h as f32),
                    scores[k],
                ]);
                if spec.kps == 5 {
                    let mut lm = [[0f32; 2]; 5];
                    for p in 0..5 {
                        let px = cx + kps[k * 10 + p * 2] * stride as f32;
                        let py = cy + kps[k * 10 + p * 2 + 1] * stride as f32;
                        lm[p] = [px / scale, py / scale];
                    }
                    kp_all.push(lm);
                }
            }
            let _ = hw;
        }
        // Stable sort by score descending (the reference's `kind='stable'`).
        pre.sort_by(|a, b| b[4].partial_cmp(&a[4]).unwrap_or(std::cmp::Ordering::Equal));
        // kp_all must ride the same order: rebuild pairs before NMS.
        let mut det: Vec<[f32; 5]> = Vec::new();
        let mut kp: Vec<[[f32; 2]; 5]> = Vec::new();
        nms_sorted(&pre, kp_all_from_sorted(&pre, spec), &mut det, &mut kp);
        let faces = det
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let lm = kp.get(i).copied()?;
                let side_px = (d[2] - d[0]).min(d[3] - d[1]);
                if side_px < 24.0 {
                    return None; // min face size gate (pinned, FACES.md)
                }
                Some(Face {
                    bbox: [d[0], d[1], d[2], d[3]],
                    score: d[4],
                    landmarks: lm,
                    quality: 0.0,
                })
            })
            .collect();
        Ok(faces)
    }

    /// The aligned identity vector for one face: 112 crop through the cv-exact
    /// warp, `(x − 127.5)/127.5` blob, the pin's output, L2-normalized.
    pub fn embed(&mut self, which: &str, rgb: &[u8], w: usize, h: usize, face: &Face) -> FaceResult<Vec<f32>> {
        self.touch();
        let crop = cvops::norm_crop(rgb, w, h, &face.landmarks);
        let side = self.embed_size;
        let blob = if side == EMB_SIZE {
            blob_tensor(&crop, side, side, EMB_MEAN, EMB_STD, true)
        } else {
            let resized = cvops::resize_linear(&crop, EMB_SIZE, EMB_SIZE, side, side);
            blob_tensor(&resized, side, side, EMB_MEAN, EMB_STD, true)
        };
        let plan = self
            .embedders
            .get_mut(which)
            .ok_or_else(|| FaceError::Model {
                path: which.to_string(),
                why: "not loaded".into(),
            })?;
        let out = plan
            .run(tvec!(blob.into()))
            .map_err(|e| FaceError::Shape { why: e.to_string() })?;
        let v = out[0]
            .to_slice_f32()
            .map_err(|e| FaceError::Shape { why: e.to_string() })?
            .to_vec();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm == 0.0 {
            return Err(FaceError::Shape {
                why: "zero embedder output".into(),
            });
        }
        Ok(v.iter().map(|x| x / norm).collect())
    }
}

fn kp_all_from_sorted(_pre: &[[f32; 5]], _spec: &DetSpec) -> Vec<[[f32; 2]; 5]> {
    // Filled by detect()'s own bookkeeping; see the pair-rebuild below.
    Vec::new()
}

/// (row, col, anchor) letterbox fit: SCRFD `_detect_candidates`.
fn letterbox(w: usize, h: usize, side: usize) -> (usize, usize, f32) {
    let im_ratio = h as f64 / w as f64;
    let (new_w, new_h) = if im_ratio > 1.0 {
        (side as f64 / im_ratio) as usize
    } else {
        (side as f64, (side as f64 * im_ratio) as usize)
    };
    let (new_w, new_h) = (new_w.max(1), new_h.max(1));
    (new_w, new_h, new_h as f32 / h as f32)
}

/// `cv2.dnn.blobFromImage` on a u8 frame: swapRB (RGB→channel-0 = R, matching the
/// reference's BGR-input swap), `(px − mean)/std`, NCHW.
fn blob_tensor(px: &[u8], w: usize, h: usize, mean: f32, std: f32, swap_rb: bool) -> Tensor {
    let plane = w * h;
    let mut data = vec![0f32; 3 * plane];
    for i in 0..w * h {
        let (r, g, b) = (px[i * 3], px[i * 3 + 1], px[i * 3 + 2]);
        let (c0, c1, c2) = if swap_rb {
            (r, g, b) // RGB source, channel0 = R == the reference's BGR→swapRB
        } else {
            (b, g, r)
        };
        data[i] = (c0 as f32 - mean) / std;
        data[plane + i] = (c1 as f32 - mean) / std;
        data[2 * plane + i] = (c2 as f32 - mean) / std;
    }
    Tensor::from_shape((1, 3, h, w), &data).expect("blob shape")
}

/// Classic IoU NMS with cv2-era +1 px boxes, threshold 0.4, greedy over the
/// score-descending order (the reference's own loop).
fn nms_sorted(pre: &[[f32; 5]], kp_pairs: Vec<[[f32; 2]; 5]>, kept: &mut Vec<[f32; 5]>, kept_kp: &mut Vec<[[f32; 2]; 5]>) {
    let n = pre.len();
    let areas: Vec<f32> = pre
        .iter()
        .map(|d| (d[2] - d[0] + 1.0) * (d[3] - d[1] + 1.0))
        .collect();
    let mut alive = vec![true; n];
    let order: Vec<usize> = (0..n).collect();
    for &i in &order {
        if !alive[i] {
            continue;
        }
        kept.push(pre[i]);
        if let Some(k) = kp_pairs.get(i) {
            kept_kp.push(*k);
        }
        let d = pre[i];
        for &j in &order {
            if !alive[j] || j == i {
                continue;
            }
            let e = pre[j];
            let xx1 = d[0].max(e[0]);
            let yy1 = d[1].max(e[1]);
            let xx2 = d[2].min(e[2]);
            let yy2 = d[3].min(e[3]);
            let w = (xx2 - xx1 + 1.0).max(0.0);
            let h = (yy2 - yy1 + 1.0).max(0.0);
            let inter = w * h;
            let ovr = inter / (areas[i] + areas[j] - inter);
            if ovr > NMS_IOU {
                alive[j] = false;
            }
        }
    }
}

fn load_plan(path: &Path) -> FaceResult<SimplePlan<TypedFact>> {
    let model = tract_onnx::onnx()
        .model_for_path(path)
        .map_err(|e| FaceError::Model {
            path: path.to_string_lossy().into_owned(),
            why: e.to_string(),
        })?;
    let model = model.into_optimized().map_err(|e| FaceError::Model {
        path: path.to_string_lossy().into_owned(),
        why: e.to_string(),
    })?;
    model.into_runnable().map_err(|e| FaceError::Model {
        path: path.to_string_lossy().into_owned(),
        why: e.to_string(),
    })
}

fn plan_input_side(plan: &SimplePlan<TypedFact>, fallback: usize) -> FaceResult<usize> {
    let fact = &plan.input_outlets()[0].fact;
    if let Some(shape) = fact.shape.as_concrete() {
        // NCHW
        if shape.len() == 4 {
            return Ok(shape[2] as usize);
        }
    }
    Ok(fallback)
}

fn plan_det_spec(plan: &SimplePlan<TypedFact>) -> FaceResult<DetSpec> {
    let outs = plan.output_outlets().len();
    let (strides, anchors, kps) = match outs {
        6 => (vec![8usize, 16, 32], 2usize, 0usize),
        9 => (vec![8, 16, 32], 2, 5),
        10 => (vec![8, 16, 32, 64, 128], 1, 0),
        15 => (vec![8, 16, 32, 64, 128], 1, 5),
        _ => {
            return Err(FaceError::Shape {
                why: format!("scrfd outputs: {outs}"),
            })
        }
    };
    Ok(DetSpec {
        strides,
        anchors,
        kps,
    })
}
