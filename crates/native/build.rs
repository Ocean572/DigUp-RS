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

    cc::Build::new()
        .warnings(false)
        .flag_if_supported("-w")
        .include(root.join("Vendor/stb/include"))
        .file(root.join("Vendor/stb/stb.c"))
        .compile("stb");
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
        println!("cargo:rustc-link-lib=dylib=stdc++");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", pdfium.display());
    }
    // The rpath travels to the crate's own test targets, where the C libraries must be found.
    if Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").is_dir() {
        println!(
            "cargo::rustc-link-arg-tests=-Wl,-rpath,{}",
            pdfium.display()
        );
    }
}
