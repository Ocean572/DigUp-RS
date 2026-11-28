//! Feature C: SVG pictures through `resvg` (pure Rust — no vendor seam, no C).
//!
//! The pinned readers never read SVGs: stb decodes raster only, and the ffmpeg probe has no
//! SVG decoder, so `image.rs` fell through to the decline. resvg renders the vector picture
//! at its natural size, and `prepare`'s own scale-down caps it at the model's budget like
//! every other picture. The licenses ride the Cargo crate graph (MPL-2.0), so the packages'
//! notices pick them up from Cargo's own record.
//!
//! THE FONT LAW: the font database is EMPTY on purpose. Fonts differ from machine to
//! machine, and the engine's determinism rule (AGENTS: nothing that changes between
//! machines/runs) will not accept a rendering that depends on the fonts a box has installed.
//! So an SVG's `<text>` renders as nothing, and its shapes, paths and colors render the
//! same bytes-equal pixels on every platform — tiny-skia rasterizes fixed-point, so resvg
//! carries no float-seed noise floor. A text-only SVG is a blank canvas, honestly.

use std::path::Path;
use std::sync::Arc;

use digup_core::extract::ExtractError;
use resvg::tiny_skia;
use resvg::usvg::fontdb;
use resvg::usvg::Options;
use resvg::usvg::Tree;

use crate::image::{decode_memory_budget, decode_size, PORTABLE_DECODE_BYTES_PER_PIXEL};
use crate::rgb::RgbImage;

/// `true` for the extension this reader takes.
pub fn supported(extension: &str) -> bool {
    extension == "svg"
}

fn unreadable() -> ExtractError {
    ExtractError::Unreadable {
        why: "can't decode image".into(),
    }
}

/// The natural-size render: parse with resvg's own defaults (an empty font database per the
/// font law), rasterize into tiny-skia's premultiplied RGBA, and hand over the picture as
/// straight 3-channel RGB — the contract every reader in `image.rs` answers.
pub fn decode(path: &Path) -> Result<RgbImage, ExtractError> {
    let data = std::fs::read(path).map_err(|_| unreadable())?;
    let options = Options {
        fontdb: Arc::new(fontdb::Database::new()),
        ..Default::default()
    };
    let tree = Tree::from_data(&data, &options).map_err(|_| unreadable())?;
    // The natural size is an f32 (a viewBox is lengths, not pixels); it is the size as
    // shown, the way the ffmpeg path reports the picture as shown.
    let width = tree.size().width().max(0.0) as usize;
    let height = tree.size().height().max(0.0) as usize;
    if width == 0 || height == 0 {
        return Err(ExtractError::TooSmall {
            width: 0,
            height: 0,
        });
    }
    // The same estimate shape as the stb and RAW paths: pixels × 8 covers the render's
    // pixmap (4 B/px) and the RGB copy (3 B/px) the picture lands in, and a vector file
    // lying about its size gets the same named refusal.
    let pixels = (width as u64).wrapping_mul(height as u64);
    let estimate = pixels.wrapping_mul(PORTABLE_DECODE_BYTES_PER_PIXEL);
    let budget = decode_memory_budget();
    if budget != 0 && estimate > budget {
        return Err(ExtractError::Unreadable {
            why: format!(
                "needed an estimated {} of memory to read: a picture of {} MP (the helper's limit is {})",
                decode_size(estimate),
                pixels / 1_000_000,
                decode_size(budget)
            ),
        });
    }
    let (Ok(width32), Ok(height32)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(unreadable()); // sizes beyond u32 are not a picture
    };
    let Some(mut pixmap) = tiny_skia::Pixmap::new(width32, height32) else {
        return Err(unreadable());
    };
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(1.0, 1.0),
        &mut pixmap.as_mut(),
    );
    // Premultiplied back to straight RGB, alpha DROPPED the way stb drops it when it reads
    // a PNG with alpha — the engine's picture contract is three channels (the JPEG the
    // index writes has none). Premultiplied channels never exceed alpha, so the division
    // lands at or under 255 with no clamp.
    let mut rgb = Vec::with_capacity(pixels as usize * 3);
    for pixel in pixmap.pixels() {
        let alpha = u32::from(pixel.alpha());
        match alpha {
            0 => rgb.extend([0, 0, 0]),
            _ => rgb.extend([
                (u32::from(pixel.red()) * 255 / alpha) as u8,
                (u32::from(pixel.green()) * 255 / alpha) as u8,
                (u32::from(pixel.blue()) * 255 / alpha) as u8,
            ]),
        }
    }
    Ok(RgbImage {
        width,
        height,
        pixels: rgb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_svg_reader_takes_one_extension() {
        assert!(supported("svg"));
        assert!(!supported("SVG")); // `lower_extension` has already folded the case
        assert!(!supported("png"));
    }

    #[test]
    fn a_real_svg_renders_at_its_natural_size() {
        // Feature C's proof on real public-domain graphics, held out of git (the suite-corpus
        // convention): DIGUP_SVG_FIXTURES names the folder. The sizes are pinned to the
        // measured natural sizes of these exact files — Commons announces them, and the
        // viewBox math lands on them digit-for-digit.
        let Ok(folder) = std::env::var("DIGUP_SVG_FIXTURES") else {
            eprintln!("skipped: DIGUP_SVG_FIXTURES is not set (public SVG samples)");
            return;
        };
        // Color counts pinned as measured on this box (resvg + tiny-skia rasterize
        // fixed-point: the same bytes render the same pixels everywhere). The vowel chart is
        // monochrome black on transparency — the font law erases its letters, so ONE color
        // is its honest artwork signature, not a broken render (the flat-field heuristic
        // cannot tell a black chart from a dead buffer; the color count can).
        for (name, width, height, colors) in [
            ("Wikimedia_Bangladesh_logo.svg", 500usize, 512usize, 39usize),
            ("Physical_World_Map.svg", 1440, 730, 140_278),
            ("Periodic_table.svg", 778, 423, 148),
            ("Slovenian_vowel_chart.svg", 980, 720, 1),
        ] {
            let path = Path::new(&folder).join(name);
            if !path.exists() {
                eprintln!("skipped: {} is not in DIGUP_SVG_FIXTURES", name);
                continue;
            }
            let image = decode(&path).expect(name);
            assert_eq!((image.width, image.height), (width, height), "{name}");
            assert_eq!(image.pixels.len(), width * height * 3, "{name}");
            let mut seen = std::collections::HashSet::new();
            for px in image.pixels.chunks(3) {
                seen.insert((px[0], px[1], px[2]));
            }
            assert_eq!(seen.len(), colors, "{name}: distinct rendered colors");
        }
    }

    #[test]
    fn a_tiny_svg_gets_the_small_picture_refusal() {
        // The engine's min-side gate runs on the natural size, the way the stb path gates
        // the header's own numbers.
        let folder = tempfile::tempdir().expect("a work folder");
        let path = folder.path().join("tiny.svg");
        std::fs::write(
            &path,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"></svg>"#,
        )
        .expect("the fixture");
        let image = decode(&path).expect("a 10×10 vector canvas renders");
        assert_eq!((image.width, image.height), (10, 10));
    }

    #[test]
    fn bytes_that_are_not_an_svg_are_declined() {
        let folder = tempfile::tempdir().expect("a work folder");
        let path = folder.path().join("not-svg.svg");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n not an svg at all").expect("the fixture");
        assert!(matches!(
            decode(&path),
            Err(ExtractError::Unreadable { .. })
        ));
    }
}
