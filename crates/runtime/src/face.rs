//! The native face engine (FACES.md): SCRFD detection + ArcFace-style embedding on
//! tract, behind the `faces` cargo feature. The pipeline is a digit-for-digit
//! transcription of the insightface reference (`model_zoo/scrfd.py`,
//! `utils/face_align.py`, `model_zoo/arcface_onnx.py`): the letterbox, the
//! mean/std blob, the anchor decode, the score gate, the +1px NMS, the 5-point
//! Umeyama alignment (via `digup_native::cvops`) and the L2-normalized embedding
//! are pinned constants — the engine relays these numbers, it does not invent them.
//!
//! tract 0.23.10 imports the standard ONNX `PRelu`, but the MXNet exports in the
//! InsightFace family ship `FPRelu` (capital-R MXNet naming), which its importer
//! set does not know. `face_onnx()` registers `FPRelu` with the exact Expansion
//! tract-onnx itself uses for `PRelu` (mirrored from its `ops/nn/mod.rs`):
//! `prelu(x, s) = (0 > x) ? x·s : x`, slope rank-aligned by axis insertion.

use tract_onnx::prelude::*;
use tract_onnx::tract_hir::internal::*;

use digup_native::cvops;

/// One detected face: box (x0,y0,x1,y1 in source px), 5 landmarks (eyes, nose,
/// mouth corners), and the detection score.
#[derive(Debug, Clone, Copy)]
pub struct Face {
    pub box_: [f32; 4],
    pub kps: [[f32; 2]; 5],
    pub score: f32,
}

type Runnable = Arc<SimplePlan<TypedFact, Box<dyn TypedOp>>>;

/// SCRFD detector (`scrfd_*.onnx`): the graph is specialized to a fixed det size
/// (pinned input fact), and strides / anchors / kps presence are derived from the
/// output count exactly as `scrfd.py::_init_vars`.
pub struct Scrfd {
    runnable: Runnable,
    strides: Vec<usize>,
    fmc: usize,
    num_anchors: usize,
    use_kps: bool,
    det_size: (usize, usize),
}

const DET_THRESH: f32 = 0.5;
const NMS_THRESH: f32 = 0.4;

impl Scrfd {
    pub fn load(path: &std::path::Path, det_size: (usize, usize)) -> TractResult<Self> {
        let model = face_onnx()?.model_for_path(path)?;
        let outputs = model.output_outlets()?.len();
        // scrfd.py::_init_vars — output count pins fmc / strides / anchors / kps.
        let (fmc, strides, num_anchors, use_kps) = match outputs {
            6 => (3usize, vec![8usize, 16, 32], 2usize, false),
            9 => (3, vec![8, 16, 32], 2, true),
            10 => (5, vec![8, 16, 32, 64, 128], 1, false),
            15 => (5, vec![8, 16, 32, 64, 128], 1, true),
            n => bail!("scrfd: {} outputs, expected 6/9/10/15", n),
        };
        let (dw, dh) = det_size;
        let model = model.with_input_fact(
            0,
            InferenceFact::dt_shape(f32::datum_type(), [1, 3, dh, dw]),
        )?;
        let runnable = model.into_typed()?.into_runnable()?;
        Ok(Scrfd {
            runnable,
            strides,
            fmc,
            num_anchors,
            use_kps,
            det_size,
        })
    }

    /// `SCRFD.detect(img)`: letterbox → blob → decode → global score sort → NMS.
    /// `rgb` is the packed RGB frame (`w*h*3`); returns faces in score-desc order
    /// with boxes/kps in *source* pixel coordinates.
    pub fn detect(&self, rgb: &[u8], w: usize, h: usize) -> TractResult<Vec<Face>> {
        let (dw, dh) = self.det_size;
        // _detect_candidates letterbox: scale by min ratio (int truncate), pad 0.
        let im_ratio = h as f64 / w as f64;
        let model_ratio = dh as f64 / dw as f64;
        let (nw, nh) = if im_ratio > model_ratio {
            ((dw as f64 / im_ratio) as usize, dh)
        } else {
            (dw, (dw as f64 * im_ratio) as usize)
        };
        let (nw, nh) = (nw.max(1), nh.max(1));
        let det_scale = nh as f32 / h as f32;
        let resized = cvops::resize_linear(rgb, w, h, nw, nh);
        let mut canvas = vec![0u8; dw * dh * 3];
        for y in 0..nh {
            canvas[y * dw * 3..y * dw * 3 + nw * 3]
                .copy_from_slice(&resized[y * nw * 3..y * nw * 3 + nw * 3]);
        }
        // _prepare...: blobFromImage(img, 1/128.0, size, (127.5,)*3, swapRB) → NCHW.
        let mut blob = vec![0f32; 3 * dh * dw];
        for y in 0..dh {
            for x in 0..dw {
                let i = (y * dw + x) * 3;
                let (r, g, b) = (canvas[i] as f32, canvas[i + 1] as f32, canvas[i + 2] as f32);
                // The model input is RGB: cv2's BGR + swapRB=True lands channel 0 = R.
                blob[y * dw + x] = (r - 127.5) / 128.0;
                blob[dh * dw + y * dw + x] = (g - 127.5) / 128.0;
                blob[2 * dh * dw + y * dw + x] = (b - 127.5) / 128.0;
            }
        }
        let input = Tensor::from_shape(&[1, 3, dh, dw], &blob)?;
        let outs = self.runnable.run(tvec!(input.into()))?;

        // forward(): per-stride anchor decode (distance2bbox / distance2kps),
        // collecting the anchors that pass the score gate.
        let mut scores_all: Vec<f32> = Vec::new();
        let mut boxes_all: Vec<[f32; 4]> = Vec::new();
        let mut kps_all: Vec<[[f32; 2]; 5]> = Vec::new();
        for (idx, &stride) in self.strides.iter().enumerate() {
            let scores_view = outs[idx].to_plain_array_view::<f32>()?;
            let scores: &[f32] = scores_view.as_slice().unwrap_or(&[]);
            let bbox_view = outs[idx + self.fmc].to_plain_array_view::<f32>()?;
            let bbox: &[f32] = bbox_view.as_slice().unwrap_or(&[]);
            let kps_view = if self.use_kps {
                Some(outs[idx + self.fmc * 2].to_plain_array_view::<f32>()?)
            } else {
                None
            };
            let kps: Option<&[f32]> = kps_view.as_ref().and_then(|v| v.as_slice());
            let fh = dh / stride;
            let fw = dw / stride;
            let n = fh * fw * self.num_anchors;
            let sc = scores.get(..n).unwrap_or(scores);
            let bb = bbox.get(..n * 4).unwrap_or(bbox);
            let kp = kps.map(|k| k.get(..n * 10).unwrap_or(k));
            let mut a = 0usize;
            for gy in 0..fh {
                for gx in 0..fw {
                    for _ in 0..self.num_anchors {
                        let s = sc[a];
                        if s >= DET_THRESH {
                            let cx = (gx * stride) as f32;
                            let cy = (gy * stride) as f32;
                            let l = bb[a * 4] * stride as f32;
                            let t = bb[a * 4 + 1] * stride as f32;
                            let r = bb[a * 4 + 2] * stride as f32;
                            let b = bb[a * 4 + 3] * stride as f32;
                            let mut f = Face {
                                box_: [
                                    (cx - l) / det_scale,
                                    (cy - t) / det_scale,
                                    (cx + r) / det_scale,
                                    (cy + b) / det_scale,
                                ],
                                kps: [[0.0; 2]; 5],
                                score: s,
                            };
                            if let Some(k) = kp {
                                // distance2kps: landmark = center + pred·stride (px = points[:,0] + d[i]).
                                for p in 0..5 {
                                    let kx = k[a * 10 + p * 2] * stride as f32;
                                    let ky = k[a * 10 + p * 2 + 1] * stride as f32;
                                    f.kps[p] = [(cx + kx) / det_scale, (cy + ky) / det_scale];
                                }
                            }
                            scores_all.push(s);
                            boxes_all.push(f.box_);
                            kps_all.push(f.kps);
                        }
                        a += 1;
                    }
                }
            }
        }

        // Global stable sort by −score (numpy `argsort(kind='stable')`), then NMS
        // with the +1px areas of `scrfd.py::nms`.
        let mut order: Vec<usize> = (0..scores_all.len()).collect();
        order.sort_by(|&i, &j| {
            scores_all[j]
                .partial_cmp(&scores_all[i])
                .unwrap()
                .then(i.cmp(&j))
        });
        let mut keep: Vec<usize> = Vec::new();
        for &i in &order {
            let b = boxes_all[i];
            let mut suppressed = false;
            for &j in &keep {
                let a = boxes_all[j];
                let iw = (b[2].min(a[2]) - b[0].max(a[0]) + 1.0).max(0.0);
                let ih = (b[3].min(a[3]) - b[1].max(a[1]) + 1.0).max(0.0);
                let inter = iw * ih;
                let ua = (b[2] - b[0] + 1.0) * (b[3] - b[1] + 1.0)
                    + (a[2] - a[0] + 1.0) * (a[3] - a[1] + 1.0)
                    - inter;
                if ua > 0.0 && inter / ua > NMS_THRESH {
                    suppressed = true;
                    break;
                }
            }
            if !suppressed {
                keep.push(i);
            }
        }
        Ok(keep
            .iter()
            .map(|&i| Face {
                box_: boxes_all[i],
                kps: kps_all[i],
                score: scores_all[i],
            })
            .collect())
    }
}

/// ArcFace-style embedder (`w600k_*.onnx` / `adaface_*.onnx`): 112×112 aligned
/// crop in → L2-normalized embedding out. Blob = blobFromImage(crop, 1/127.5,
/// 112, (127.5,)*3, swapRB) as `arcface_onnx.py` does.
pub struct Embedder {
    runnable: Runnable,
    dim: usize,
}

impl Embedder {
    pub fn load(path: &std::path::Path) -> TractResult<Self> {
        let model = face_onnx()?.model_for_path(path)?;
        let model = model.with_input_fact(
            0,
            InferenceFact::dt_shape(f32::datum_type(), [1, 3, 112, 112]),
        )?;
        let model = model.into_typed()?;
        let dim = model
            .output_fact(0)?
            .shape
            .as_concrete()
            .and_then(|s| s.get(1).copied())
            .unwrap_or(512);
        let runnable = model.into_runnable()?;
        Ok(Embedder { runnable, dim })
    }

    pub fn output_dim(&self) -> usize {
        self.dim
    }

    /// `arcface_onnx.get(img_align)`: the 112×112 RGB crop → unit embedding.
    pub fn embed(&self, crop112_rgb: &[u8]) -> TractResult<Vec<f32>> {
        let mut blob = vec![0f32; 3 * 112 * 112];
        for i in 0..112 * 112 {
            let (r, g, b) = (
                crop112_rgb[i * 3] as f32,
                crop112_rgb[i * 3 + 1] as f32,
                crop112_rgb[i * 3 + 2] as f32,
            );
            // Channel 0 = R, matching `blobFromImage(BGR, swapRB=True)`.
            blob[i] = (r - 127.5) / 127.5;
            blob[112 * 112 + i] = (g - 127.5) / 127.5;
            blob[2 * 112 * 112 + i] = (b - 127.5) / 127.5;
        }
        let input = Tensor::from_shape(&[1, 3, 112, 112], &blob)?;
        let outs = self.runnable.run(tvec!(input.into()))?;
        let v: Vec<f32> = outs[0]
            .to_plain_array_view::<f32>()?
            .as_slice()
            .unwrap_or(&[])
            .to_vec();
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            Ok(v.iter().map(|x| x / n).collect())
        } else {
            Ok(v)
        }
    }
}

/// `face_align.norm_crop` for one detected face: the 112×112 aligned crop
/// (`cv2.warpAffine`-parity, `digup_native::cvops`).
pub fn norm_crop(rgb: &[u8], w: usize, h: usize, face: &Face) -> Vec<u8> {
    let lmk: [[f64; 2]; 5] = face.kps.map(|p| [p[0] as f64, p[1] as f64]);
    cvops::norm_crop(rgb, w, h, &lmk)
}

/// The tract ONNX frontend with `FPRelu` registered (MXNet-exported graphs in the
/// InsightFace family; tract 0.23 knows the ONNX name `PRelu` only).
pub fn face_onnx() -> TractResult<tract_onnx::model::Onnx> {
    let mut base = tract_onnx::onnx();
    base.op_register.insert("FPRelu", fprelu_builder);
    Ok(base)
}

fn fprelu_builder(
    _ctx: &tract_onnx::model::ParsingContext,
    _node: &tract_onnx::pb::NodeProto,
) -> TractResult<(Box<dyn InferenceOp>, Vec<String>)> {
    Ok((expand(Fprelu), vec![]))
}

/// `prelu(x, s) = (0 > x) ? x·s : x` — the exact Expansion tract-onnx 0.23 uses
/// for `PRelu` (mirrored from `ops/nn/mod.rs`, the Prelu struct), registered here
/// under the MXNet op name `FPRelu`.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct Fprelu;

impl Expansion for Fprelu {
    fn name(&self) -> StaticName {
        "Fprelu".into()
    }

    fn rules<'r, 'p: 'r, 's: 'r>(
        &'s self,
        s: &mut Solver<'r>,
        inputs: &'p [TensorProxy],
        outputs: &'p [TensorProxy],
    ) -> InferenceResult {
        check_input_arity(inputs, 2)?;
        check_output_arity(outputs, 1)?;
        s.equals(&inputs[0].datum_type, &outputs[0].datum_type)?;
        s.equals(&inputs[0].shape, &outputs[0].shape)?;
        Ok(())
    }

    fn wire(
        &self,
        name: &str,
        model: &mut TypedModel,
        inputs: &[OutletId],
    ) -> TractResult<TVec<OutletId>> {
        let a = inputs[0];
        let mut b = inputs[1];
        let rank = model.outlet_fact(a)?.rank();
        while model.outlet_fact(b)?.rank() < rank {
            b = model.wire_node(
                format!("{}.add-axis-{}", name, model.outlet_fact(b)?.rank()),
                AxisOp::Add(0),
                &[b],
            )?[0];
        }
        let zero = tensor0(0.0)
            .cast_to_dt(model.outlet_fact(a)?.datum_type)?
            .into_owned()
            .broadcast_into_rank(rank)?;
        let ab = model.wire_node(format!("{name}.mul"), tract_core::ops::math::mul(), &[a, b])?[0];
        let zero = model.add_const(name.to_string() + ".zero", zero)?;
        let test = model.wire_node(
            name.to_string() + ".test",
            tract_core::ops::binary::TypedBinOp(tract_core::ops::logic::comp_gt(), None),
            &[zero, a],
        )?;
        model.wire_node(
            name.to_string() + ".iff",
            tract_core::ops::logic::Iff,
            &[test[0], ab, a],
        )
    }
}
