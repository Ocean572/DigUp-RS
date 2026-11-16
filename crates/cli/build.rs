//! The link the `digup` binary needs for the vendored C libraries its crates call. Cargo's
//! `rustc-link-lib` directives apply to the crate that emits them, so the binary repeats the
//! search path and the runpath for PDFium — exactly as the Swift package's linker settings do
//! (`Package.swift`: `-L Vendor/pdfium/lib` + the runpath), for the same reason: the loader
//! must find `libpdfium.so` at run time by the same path the native crate's rpath carries.

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pdfium = root.join("Vendor/pdfium/lib");
    println!("cargo:rustc-link-search=native={}", pdfium.display());
    // Cargo carries a dylib's *search* through the rlib graph, not its `-l` line: the binary
    // names the library itself, C++ standard library included (PDFium is C++), as the native
    // crate's own build.rs does.
    println!("cargo:rustc-link-lib=dylib=pdfium");
    // The C++ runtime PDFium needs: Apple's libc++ on macOS, GNU's libstdc++ on Linux; MSVC
    // links its C++ runtime automatically, so Windows names none.
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=dylib=c++");
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=dylib=stdc++");
    // Windows takes no rpath: the DLLs ride beside the binary (Package.swift's note).
    #[cfg(not(target_os = "windows"))]
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", pdfium.display());
    // The rpath travels to this crate's own test targets, once they exist (the native crate's
    // `build.rs` guards the same directive the same way). Windows has none.
    if Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").is_dir() {
        #[cfg(not(target_os = "windows"))]
        println!(
            "cargo::rustc-link-arg-tests=-Wl,-rpath,{}",
            pdfium.display()
        );
    }
    println!("cargo:rerun-if-changed=../../crates/native/src/pdf.rs");
}
