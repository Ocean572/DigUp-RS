//! Builds the C side the native readers call: the vendored stb (the same single translation unit
//! `scripts/fetch-vendor.sh` writes, implementation defines included, compiled `-w` as Package
//! .swift's `CStb` target passes) and, once `pdf.rs` exists, the prebuilt PDFium
//! (`Vendor/pdfium/lib/libpdfium.so`, which Package.swift links with `-L Vendor/pdfium/lib`).
//!
//! Each C library is linked with the module that calls it: a crate that does not use PDFium
//! should not carry a load-time dependency on it (the same rule the engine follows by putting
//! ImageIO/PDFKit behind `canImport` in the platform split).

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    // The stb public symbols are RENAMED (`digup_`-prefixed) on every platform. The reason
    // is measured on the macOS payload link (2026-11-17): llama's mtmd-helper embeds its
    // OWN vendored stb (the load subset, `extern "C"`), and the static link mixes it with
    // this crate's pinned stb — ld reported `duplicate symbol '_stbi_info' … '_stbi_load'
    // … '_stbi_load_from_memory' … '_stbi_image_free'` and picked a winner by link order,
    // so the engine's picture decoding on macOS answered to llama's stb copy, not the
    // pinned one (Linux/Windows never see the clash: mtmd rides the shared lib/DLL there).
    // Renaming pins the resolution by name: `crates/native` calls its pinned copy on every
    // platform, mtmd keeps its own, and a name change is codegen-neutral where nothing
    // collided. Keep these seven names in sync with `src/stb.rs`'s `#[link_name]`s.
    let mut stb = cc::Build::new();
    stb.warnings(false)
        .flag_if_supported("-w")
        .include(root.join("Vendor/stb/include"))
        .define("stbi_info", "digup_stbi_info")
        .define("stbi_load", "digup_stbi_load")
        .define("stbi_load_from_memory", "digup_stbi_load_from_memory")
        .define("stbi_image_free", "digup_stbi_image_free")
        .define("stbir_resize_uint8_srgb", "digup_stbir_resize_uint8_srgb")
        .define("stbi_write_png", "digup_stbi_write_png")
        .define("stbi_write_jpg", "digup_stbi_write_jpg")
        .file(root.join("Vendor/stb/stb.c"));
    stb.compile("stb");
    println!("cargo:rerun-if-changed=../../Vendor/stb/stb.c");
    println!("cargo:rerun-if-changed=../../Vendor/stb/include");

    // PDFium is C++ (Chromium's): its archive needs the C++ standard library, and the library is
    // found by search path + rpath exactly as the Swift package's `-L Vendor/pdfium/lib` +
    // runpath settings find it.
    let pdfium = root.join("Vendor/pdfium/lib");
    // The PDFium flags enter the link only once pdf.rs exists — and the script must re-run
    // when it appears, so the fingerprint watches the file it tests for.
    println!("cargo::rerun-if-changed=src/pdf.rs");
    println!("cargo::rerun-if-changed=tests");
    if Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/pdf.rs")
        .is_file()
    {
        println!("cargo:rustc-link-search=native={}", pdfium.display());
        println!("cargo:rustc-link-lib=dylib=pdfium");
        // PDFium is C++: Apple's libc++ on macOS, GNU's libstdc++ on Linux; MSVC links its C++
        // runtime automatically, so Windows names none.
        #[cfg(target_os = "macos")]
        println!("cargo:rustc-link-lib=dylib=c++");
        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-lib=dylib=stdc++");
        // Windows takes no rpath: the DLLs ride beside the binary (Package.swift's note).
        #[cfg(not(target_os = "windows"))]
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", pdfium.display());
    }
    // The rpath travels to the crate's own test targets, where the C libraries must be found.
    // Windows has none.
    if Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").is_dir() {
        #[cfg(not(target_os = "windows"))]
        println!(
            "cargo::rustc-link-arg-tests=-Wl,-rpath,{}",
            pdfium.display()
        );
    }

    // LibRaw (#152, camera RAW): the static library `scripts/build-libraw.sh` builds beside
    // its source tree. The shim is compiled with the C++ compiler here and the archive links
    // after it (the archive depends on the C++ runtime and zlib); static, so the payload
    // carries no extra file. Windows keeps the notReadYet decline: no shim, no link, and the
    // `digup_libraw` cfg — the raw module's single answer — stays off.
    println!("cargo::rustc-check-cfg=cfg(digup_libraw)");
    println!("cargo::rerun-if-changed=rawshim.cpp");
    println!("cargo::rerun-if-changed=../../Vendor/libraw/lib/libraw.a");
    let libraw = root.join("Vendor/libraw");
    let host = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if host != "windows" && libraw.join("lib/libraw.a").is_file() {
        let mut raw = cc::Build::new();
        raw.cpp(true)
            .warnings(false)
            .flag_if_supported("-w")
            .flag_if_supported("-std=c++11")
            .include(&libraw)
            .file("rawshim.cpp");
        raw.compile("rawshim");
        println!(
            "cargo:rustc-link-search=native={}",
            libraw.join("lib").display()
        );
        println!("cargo:rustc-link-lib=static=raw");
        println!("cargo:rustc-link-lib=dylib=z"); // the deflate decompressor (Makefile.devel's)
                                                  // The C++ runtime, same naming as the PDFium block above: MSVC links its own.
        #[cfg(target_os = "macos")]
        println!("cargo:rustc-link-lib=dylib=c++");
        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-cfg=digup_libraw");
    }
}
