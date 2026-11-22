//! The face pipeline's pixel helpers, ported to match OpenCV's exact fixed-point
//! arithmetic (measured byte-parity gate, docs/implementation/FACES.md Phase 0 #3):
//! the reference face pipeline is cv2's, so `resize(INTER_LINEAR)`, `warpAffine(
//! INTER_LINEAR, BORDER_CONSTANT=0)`, and the 5-point similarity fit scikit-image's
//! `SimilarityTransform` computes must land on the same numbers, not the same
//! "concept". The engine's face pass runs these; the Python dev harness runs cv2
//! over the same inputs, and `tests/cvops_parity.rs` compares them digit-for-digit
//! against the recorded fixtures.
//!
//! Transcribed from the OpenCV 4.12 sources (measured 2026-11-22, the wheel's
//! built-in path with `OPENCV_IPP=disabled`):
//! - resize INTER_LINEAR 8u = `hal::resize` (resize.cpp:4097-4193): per-axis map
//!   `fx = (float)((dx+0.5)·scale − 0.5)` (one f32 rounding), `sx = cvFloor(fx)`,
//!   head/tail clamp, weights `w1 = cvRound(fx_f32·2048)`, `w0 = cvRound((1−fx)·2048)`;
//!   horizontal pass full-precision int; vertical per `resizeGeneric_Invoker`'s
//!   clamped rows and `VResizeLinear<uchar,int,short,FixedPtCast<int,uchar,22>>`'s
//!   tail `((b0·(H0>>4))>>16 + (b1·(H1>>4))>>16 + 2)>>2`. cv2's upscale *last* dst
//!   row (map row sy ≥ h−1) reads `src.ptr(h)` past the Mat buffer — the value is
//!   allocator-dependent, so it is masked out of the gate (fixture `tail_rows`).
//! - warpAffine INTER_LINEAR BORDER_CONSTANT 0 = `cv::warpAffine`'s inversion in
//!   the source's expression order (imgwarp.cpp:2736-2743), `hal::warpAffine` map
//!   quantization `ad = round(a11·x·1024)`, `X0 = round((a12·y + b1)·1024) + 16`,
//!   `X = (X0 + ad) >> 5` (2594-2608, 2181-2182, 2684-2689); `BilinearTab_i`
//!   weights `(32−dy)(32−dx)·32` (initInterTab2D — exact products, isum = 32768,
//!   no correction); per-tap `BORDER_CONSTANT` 0 (remapBilinear 818-868); the
//!   8u cast is round-half-to-even of Σ/32768 (measured: 202.5→202, 199.5→200).
//!   The wheel's remap SIMD dispatch diverges from this port by ≤4 LSB at the map
//!   quantization boundary pixels — the parity fixtures pin the exact divergence
//!   counts (`cv2_diffs`, `cv2_max_delta`) beside both outputs.

/// cv2's `cvRound` is round-half-to-even; Rust's `f64::round` ties away from zero.
pub(crate) fn round_even(x: f64) -> i64 {
    let f = x - x.floor();
    if f == 0.5 {
        let fl = x.floor();
        if (fl as i64) % 2 == 0 {
            fl as i64
        } else {
            (fl + 1.0) as i64
        }
    } else {
        x.round() as i64
    }
}

const Q: i64 = 2048;

/// The map taps of cv2's `hal::resize` for one axis: `(sx, w0, w1)` per dst index,
/// with the head/tail clamps (map row `sy ≥ h−1` keeps `sx = h−1` — cv2 reads the
/// row past the Mat there, which the gate masks).
fn resize_taps(inv_scale: f64, size: usize, n: usize) -> (Vec<usize>, Vec<i64>, Vec<i64>) {
    let scale = 1.0 / inv_scale;
    let mut sx = Vec::with_capacity(n);
    let mut w0 = Vec::with_capacity(n);
    let mut w1 = Vec::with_capacity(n);
    for dx in 0..n {
        let fx = ((dx as f64 + 0.5) * scale - 0.5) as f32;
        let mut s = fx.floor() as i64;
        let mut f = fx - s as f32;
        if s < 0 {
            s = 0;
            f = 0.0;
        } else if s >= size as i64 - 1 {
            s = (size - 1) as i64;
            f = 0.0;
        }
        let f = f as f64;
        sx.push(s as usize);
        w1.push((round_even(f * Q as f64)).clamp(0, Q));
        w0.push((round_even((1.0f32 - f as f32) as f64 * Q as f64)).clamp(0, Q));
    }
    (sx, w0, w1)
}

/// `cv2.resize(src, (dst_w, dst_h))` at `INTER_LINEAR` over a packed RGB `src`
/// (`w*h*3` bytes); returns the `dst_w*dst_h*3` buffer. See the module header for
/// the OpenCV path this transcribes.
pub fn resize_linear(src: &[u8], w: usize, h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    if dst_w == w && dst_h == h {
        return src.to_vec();
    }
    let (sx, wx0, wx1) = resize_taps(dst_w as f64 / w as f64, w, dst_w);
    let (sy, wy0, wy1) = resize_taps(dst_h as f64 / h as f64, h, dst_h);
    // Horizontal pass: full-precision column sums per source row (cv2 keeps the
    // line buffers in int; the only rounding is the vertical tail cast).
    let mut rows: Vec<Vec<i64>> = Vec::with_capacity(h);
    for row in src.chunks(w * 3) {
        let mut cols = vec![0i64; dst_w * 3];
        for x in 0..dst_w {
            let x0 = sx[x].min(w - 1);
            let x1 = (x0 + 1).min(w - 1);
            let (w0, w1) = (wx0[x], wx1[x]);
            for c in 0..3 {
                cols[x * 3 + c] = row[x0 * 3 + c] as i64 * w0 + row[x1 * 3 + c] as i64 * w1;
            }
        }
        rows.push(cols);
    }
    // Vertical pass: `resizeGeneric_Invoker` clamps the tap rows and keeps the
    // full β pair; `VResizeLinear`'s tail casts via `>>4` + two `>>16` halves.
    let mut out = vec![0u8; dst_w * dst_h * 3];
    for y in 0..dst_h {
        let y0 = sy[y].min(h - 1);
        let y1 = (sy[y] + 1).min(h - 1);
        let (c0, c1) = (wy0[y], wy1[y]);
        let (row0, row1) = (&rows[y0], &rows[y1]);
        for x in 0..dst_w * 3 {
            let t = ((c0 * (row0[x] >> 4)) >> 16) + ((c1 * (row1[x] >> 4)) >> 16);
            out[y * dst_w * 3 + x] = (((t + 2) >> 2).clamp(0, 255)) as u8;
        }
    }
    out
}

/// The forward affine map cv2 carries in `warpAffine`: 2×3 row-major, src→dst.
pub type Affine23 = [f64; 6];

/// `cv::warpAffine`'s inverse map, in the source's exact expression order
/// (imgwarp.cpp:2736-2743) so the doubles land bit-identically.
pub fn invert_affine(m: &Affine23) -> Affine23 {
    let d = m[0] * m[4] - m[1] * m[3];
    let d = if d != 0.0 { 1.0 / d } else { 0.0 };
    let a11 = m[4] * d;
    let a22 = m[0] * d;
    let a12 = m[1] * -d;
    let a21 = m[3] * -d;
    let b1 = (-a11) * m[2] - a12 * m[5];
    let b2 = (-a21) * m[2] - a22 * m[5];
    [a11, a12, b1, a21, a22, b2]
}

/// `cv2.warpAffine(src, M, (dst_w, dst_h), INTER_LINEAR, BORDER_CONSTANT = 0)`.
/// The map is the AB_BITS=10 fixed-point quantization of `hal::warpAffine`; the
/// interpolation kernel is `BilinearTab_i` with per-tap border 0, cast by
/// round-half-to-even. See the module header for the divergence pin.
pub fn warp_affine_linear(
    src: &[u8],
    w: usize,
    h: usize,
    m: &Affine23,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let inv = invert_affine(m);
    let ab: f64 = 1024.0;
    // Per-column x-map (adelta) and the per-row x-offset (X0), 1/32-px units.
    let mut ad = Vec::with_capacity(dst_w);
    let mut bd = Vec::with_capacity(dst_w);
    for x in 0..dst_w {
        ad.push(round_even(inv[0] * x as f64 * ab));
        bd.push(round_even(inv[3] * x as f64 * ab));
    }
    let mut out = vec![0u8; dst_w * dst_h * 3];
    // A tap's byte base, `-1` for out-of-frame taps (border 0, per tap).
    let tap = |sx: i64, sy: i64| -> i64 {
        if sx < 0 || sy < 0 || sx >= w as i64 || sy >= h as i64 {
            -1
        } else {
            sy * (w as i64) * 3 + sx * 3
        }
    };
    for y in 0..dst_h {
        let x0q = round_even((inv[1] * y as f64 + inv[2]) * ab) + 16;
        let y0q = round_even((inv[4] * y as f64 + inv[5]) * ab) + 16;
        for x in 0..dst_w {
            let mx = (x0q + ad[x]) >> 5;
            let my = (y0q + bd[x]) >> 5;
            let (x0, y0) = (mx >> 5, my >> 5);
            let (dx, dy) = (mx & 31, my & 31);
            let bases = [
                tap(x0, y0),
                tap(x0 + 1, y0),
                tap(x0, y0 + 1),
                tap(x0 + 1, y0 + 1),
            ];
            for c in 0..3 {
                let mut acc = 0i64;
                for (k, wgt) in [
                    (0usize, (32 - dy) * (32 - dx)),
                    (1, (32 - dy) * dx),
                    (2, dy * (32 - dx)),
                    (3, dy * dx),
                ] {
                    if bases[k] >= 0 {
                        acc += src[(bases[k] + c as i64) as usize] as i64 * wgt;
                    }
                }
                // BilinearTab weights are ×32 of these; the 8u cast rounds the
                // exact /32768 quotient half-to-even.
                out[y * dst_w * 3 + x * 3 + c] =
                    (round_even(acc as f64 * 32.0 / 32768.0).clamp(0, 255)) as u8;
            }
        }
    }
    out
}

/// The 5-point similarity fit (scale + rotation + translation, least squares) —
/// the exact estimator scikit-image's `SimilarityTransform().estimate` runs
/// (Umeyama 1999, `estimate_scale = True`), on float64. `src` and `dst` are
/// `[x, y]` point sets of equal length (the face pipeline passes 5).
pub fn similarity_from_points(src: &[[f64; 2]], dst: &[[f64; 2]]) -> Affine23 {
    let n = src.len() as f64;
    let mu_s = [
        src.iter().map(|p| p[0]).sum::<f64>() / n,
        src.iter().map(|p| p[1]).sum::<f64>() / n,
    ];
    let mu_d = [
        dst.iter().map(|p| p[0]).sum::<f64>() / n,
        dst.iter().map(|p| p[1]).sum::<f64>() / n,
    ];
    // Cross-covariance C (2×2), C[r][k] = mean( (d_r − mu_d_r)·(s_k − mu_s_k) ).
    let (mut a, mut b, mut d, mut e) = (0f64, 0f64, 0f64, 0f64);
    for i in 0..src.len() {
        let (dsx, dsy) = (dst[i][0] - mu_d[0], dst[i][1] - mu_d[1]);
        let (ssx, ssy) = (src[i][0] - mu_s[0], src[i][1] - mu_s[1]);
        a += dsx * ssx;
        b += dsx * ssy;
        d += dsy * ssx;
        e += dsy * ssy;
    }
    (a, b, d, e) = (a / n, b / n, d / n, e / n);
    // Closed-form 2×2 SVD C = U diag(σ1,σ2) Vᵗ.
    // V = eigenvectors of CᵗC (symmetric 2×2, quadratic in λ); U = C V / σ.
    let (m00, m01, m11) = (a * a + d * d, a * b + d * e, b * b + e * e);
    let tr = m00 + m11;
    let det_b = m00 * m11 - m01 * m01;
    let disc = (tr * tr - 4.0 * det_b).max(0.0).sqrt();
    let (lam1, lam2) = ((tr + disc) / 2.0, (tr - disc) / 2.0);
    let (sig1, sig2) = (lam1.max(0.0).sqrt(), lam2.max(0.0).sqrt());
    // Eigenvector of CᵗC for λ1: (m01, λ1 − m00), falling back to (1,0) when
    // m01 is nil; v2 is v1 rotated 90°.
    let (v1x, v1y) = if m01.abs() > 1e-12 {
        let (vx, vy) = (m01, lam1 - m00);
        let l = (vx * vx + vy * vy).sqrt();
        (vx / l, vy / l)
    } else {
        (1.0f64, 0.0f64)
    };
    let (v2x, v2y) = (-v1y, v1x);
    // Columns of U: C·v / σ (guarding a nil σ — then that column plays no role).
    let (u1x, u1y) = if sig1 > 1e-12 {
        ((a * v1x + b * v1y) / sig1, (d * v1x + e * v1y) / sig1)
    } else {
        (1.0, 0.0)
    };
    let (u2x, u2y) = if sig2 > 1e-12 {
        ((a * v2x + b * v2y) / sig2, (d * v2x + e * v2y) / sig2)
    } else {
        (0.0, 1.0)
    };
    // Umeyama's reflection fix: S = diag(1, sign(det U · det V)).
    let det_u = u1x * u2y - u1y * u2x;
    let det_v = v1x * v2y - v1y * v2x;
    let s2 = if det_u * det_v < 0.0 { -1.0 } else { 1.0 };
    // R = U diag(1,s2) Vᵗ.
    let (u2x, u2y) = (u2x * s2, u2y * s2);
    let (r00, r01) = (u1x * v1x + u2x * v2x, u1x * v1y + u2x * v2y);
    let (r10, r11) = (u1y * v1x + u2y * v2x, u1y * v1y + u2y * v2y);
    // scale = (σ1 + s2·σ2) / var(src); t = mu_d − scale·R·mu_s.
    let var_s: f64 = src
        .iter()
        .map(|p| (p[0] - mu_s[0]).powi(2) + (p[1] - mu_s[1]).powi(2))
        .sum::<f64>()
        / n;
    let scale = (sig1 + s2 * sig2) / var_s;
    let (sr00, sr01, sr10, sr11) = (scale * r00, scale * r01, scale * r10, scale * r11);
    [
        sr00,
        sr01,
        mu_d[0] - (sr00 * mu_s[0] + sr01 * mu_s[1]),
        sr10,
        sr11,
        mu_d[1] - (sr10 * mu_s[0] + sr11 * mu_s[1]),
    ]
}

/// The insightface `arcface_dst` template — the 5 canonical landmark positions at 112 px.
pub const ARCFACE_TEMPLATE: [[f64; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// `face_align.norm_crop`: the 112×112 aligned face crop `cv2.warpAffine` produces
/// (INTER_LINEAR, BORDER_CONSTANT 0) from the source RGB frame and 5 landmarks.
pub fn norm_crop(src: &[u8], w: usize, h: usize, lmk: &[[f64; 2]; 5]) -> Vec<u8> {
    let m = similarity_from_points(lmk, &ARCFACE_TEMPLATE);
    warp_affine_linear(src, w, h, &m, 112, 112)
}
