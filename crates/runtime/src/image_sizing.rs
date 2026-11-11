//! How the reference sizes an image for the vision encoder, so llama.cpp sees the same pixels
//! (`ImageSizing.swift`): transformers' Gemma resize target, then Pillow bit's bicubic.
//!
//! mtmd (llama.cpp's multimodal library) fixes the token budget per context, loads the encoder
//! weights per context, and rescales only images outside its range — while the reference *always*
//! fills the budget. So images are resized here and mtmd takes them as they are (its
//! `image_min_tokens`/`image_max_tokens` are set so it never touches them). The bicubic is
//! Pillow's, ported from mtmd's copy of it: kernel a = −0.5, widened when shrinking, 22-bit
//! fixed-point weights, `u8` clip — checked against Pillow bit for bit in `tests/image_sizing.rs`.

/// transformers' Gemma 4 image processor: scale to fill `budget` soft tokens (each 3 × 3 patches
/// of 16 px) keeping the aspect ratio, each side rounded down to a multiple of 48 px.
pub fn target_size(height: usize, width: usize, budget: i32) -> Option<(usize, usize)> {
    // a broken file's zero sides would divide by zero below
    if height == 0 || width == 0 {
        return None;
    }
    let (patch, pooling) = (16usize, 3usize);
    let max_patches = (budget.max(0) as usize) * pooling * pooling;
    let factor = ((max_patches * patch * patch) as f64 / (height * width) as f64).sqrt();
    let side = pooling * patch;
    let mut target_height = (factor * height as f64 / side as f64).floor() as usize * side;
    let mut target_width = (factor * width as f64 / side as f64).floor() as usize * side;
    if target_height == 0 && target_width == 0 {
        return None;
    }
    let max_side = (max_patches / (pooling * pooling)) * side;
    if target_height == 0 {
        // One side rounds to zero: it gets one 48 px row, the other capped at the budget.
        target_height = side;
        target_width = (width / height * side).min(max_side);
    } else if target_width == 0 {
        target_width = side;
        target_height = (height / width * side).min(max_side);
    }
    Some((target_height, target_width))
}

/// Pillow's bicubic resampling of packed RGB (a = −0.5, the kernel widened when shrinking,
/// 22-bit fixed point): what the reference gets from PIL's Image.resize. `source` is
/// `width * height * 3` bytes; the result is `to_width * to_height * 3`.
pub fn resize_bicubic(
    source: &[u8],
    width: usize,
    height: usize,
    to_width: usize,
    to_height: usize,
) -> Vec<u8> {
    let need_width = to_width != width;
    let need_height = to_height != height;
    match (need_width, need_height) {
        (true, true) => {
            let pass = horizontal(source, width, height, to_width);
            vertical(&pass, to_width, height, to_height)
        }
        (true, false) => horizontal(source, width, height, to_width),
        (false, true) => vertical(source, width, height, to_height),
        (false, false) => source.to_vec(),
    }
}

const PRECISION: u32 = 22;

fn filter(value: f64) -> f64 {
    let x = value.abs();
    let a = -0.5;
    if x < 1.0 {
        return ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0;
    }
    if x < 2.0 {
        return (((x - 5.0) * x + 8.0) * x - 4.0) * a;
    }
    0.0
}

/// For each output pixel: the first contributing input pixel, how many contribute, and their
/// weights, as `(bounds, weights, size)` — `bounds[out*2]` is the first input pixel,
/// `bounds[out*2+1]` the count, and the weights live `size` apart per output.
fn coefficients(in_size: usize, out_size: usize) -> (Vec<usize>, Vec<i32>, usize) {
    let scale = in_size as f64 / out_size as f64;
    let filter_scale = scale.max(1.0);
    let support = 2.0 * filter_scale;
    let size = (support.ceil() as usize) * 2 + 1;
    let mut bounds = vec![0usize; out_size * 2];
    let mut weights = vec![0i32; out_size * size];
    let mut row = vec![0.0f64; size];
    for out in 0..out_size {
        let center = (out as f64 + 0.5) * scale;
        let first = (center - support + 0.5).max(0.0) as usize;
        let count = (center + support + 0.5) as usize;
        let count = count.min(in_size).saturating_sub(first);
        let mut total = 0.0;
        for (x, slot) in row.iter_mut().enumerate() {
            *slot = if x < count {
                filter(((x + first) as f64 - center + 0.5) / filter_scale)
            } else {
                0.0
            };
            total += *slot;
        }
        for x in 0..size {
            let weight = if x < count && total != 0.0 {
                row[x] / total
            } else {
                row[x]
            };
            weights[out * size + x] = (weight * (1i64 << PRECISION) as f64
                + if weight < 0.0 { -0.5 } else { 0.5 })
                as i32;
        }
        bounds[out * 2] = first;
        bounds[out * 2 + 1] = count;
    }
    (bounds, weights, size)
}

/// The fixed-point sums, rounded by the +2²¹ bias and truncated, clamped to the byte range.
fn clip(value: i32) -> u8 {
    u8::try_from(value >> PRECISION).unwrap_or(if value >> PRECISION < 0 { 0 } else { 255 })
}

/// Scale each row: `rows` rows of `in_width` pixels in, `out_width` pixels per row out.
fn horizontal(src: &[u8], in_width: usize, rows: usize, out_width: usize) -> Vec<u8> {
    let (bounds, weights, size) = coefficients(in_width, out_width);
    let mut out = vec![0u8; out_width * rows * 3];
    for y in 0..rows {
        for x in 0..out_width {
            let mut p = (y * in_width + bounds[x * 2]) * 3;
            let half: i32 = 1 << (PRECISION - 1);
            let (mut r, mut g, mut b) = (half, half, half);
            for k in 0..bounds[x * 2 + 1] {
                let w = weights[x * size + k];
                r = r.wrapping_add(i32::from(src[p]) * w);
                g = g.wrapping_add(i32::from(src[p + 1]) * w);
                b = b.wrapping_add(i32::from(src[p + 2]) * w);
                p += 3;
            }
            let o = (y * out_width + x) * 3;
            out[o] = clip(r);
            out[o + 1] = clip(g);
            out[o + 2] = clip(b);
        }
    }
    out
}

/// Scale the rows themselves: rows of `row_width` pixels (the same width throughout) in,
/// `out_height` rows out.
fn vertical(src: &[u8], row_width: usize, in_height: usize, out_height: usize) -> Vec<u8> {
    let (bounds, weights, size) = coefficients(in_height, out_height);
    let stride = row_width * 3;
    let mut out = vec![0u8; stride * out_height];
    let mut sums = vec![0i32; stride];
    for y in 0..out_height {
        let half: i32 = 1 << (PRECISION - 1);
        for sum in sums.iter_mut() {
            *sum = half;
        }
        for k in 0..bounds[y * 2 + 1] {
            let row = (bounds[y * 2] + k) * stride;
            let w = weights[y * size + k];
            for i in 0..stride {
                sums[i] = sums[i].wrapping_add(i32::from(src[row + i]) * w);
            }
        }
        for i in 0..stride {
            out[y * stride + i] = clip(sums[i]);
        }
    }
    out
}
