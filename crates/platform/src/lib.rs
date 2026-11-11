//! digup-platform — per-OS file readers (ARCHITECTURE_MAP.md §1).
//!
//! Scaffold. The Swift engine reads media with Apple's frameworks (ImageIO, PDFKit, AVFoundation,
//! Vision) on macOS via `Platform/Apple`, and with the C libraries (PDFium, ffmpeg, stb, miniz)
//! everywhere else via `Platform/Portable`. The Rust port keeps the same split: `objc2` bridges on
//! macOS, the C libraries (bindgen) elsewhere. Readers land in Phase 4+; extraction rules and the
//! platform boundary are decided there.
