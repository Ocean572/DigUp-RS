//! The C-backed readers of the Rust port (`crates/native`): stb pictures, PDFium pages, ffmpeg
//! media. The engine's platform split is the guide — `Platform/Portable/*` is what Linux and
//! Windows run, and this crate is its Rust side, calling the SAME vendored C libraries
//! (`Vendor/stb`, `Vendor/pdfium`) rather than reimplementing their codecs in Rust: a port that
//! decodes with a different JPEG/PNG reader would produce pictures the vision encoder has never
//! seen, and the vector parity gate would be measuring the port's opinions, not the engine's.
//!
//! Scope follows `Images+Portable.swift` (stb decode → EXIF turn → scale → write, the #182 memory
//! preflight), `PDF+Portable.swift` (PDFium text and page renders) and `Media+Portable.swift`
//! (ffmpeg/ffprobe: audio windows, video keyframes, preview frames).

pub mod exif;
pub mod ffmpeg;
pub mod image;
pub mod rgb;
pub mod stb;
