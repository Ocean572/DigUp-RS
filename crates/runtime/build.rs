//! Binds the **vendored, pinned** llama.cpp (tag b11461 + `scripts/llama.cpp-no-logits.patch`) that
//! `scripts/build-llama.sh` puts in `Vendor/llama` (out of git), the way `Sources/CLlama*/module.modulemap`
//! binds it for Swift: `llama.h`, `mtmd.h`, `mtmd-helper.h`, plus `llama-build.h` for the tag.
//!
//! When `Vendor/llama` isn't built, this does not fail the workspace: it sets `cfg(llama_unavailable)`,
//! so a fresh clone builds and the embedder's tests report why they skipped.

use std::path::{Path, PathBuf};

/// Where the vendored llama.cpp is: `$DIGUP_LLAMA_DIR`, else `<workspace root>/Vendor/llama` —
/// the folder `scripts/build-llama.sh` writes.
fn llama_dir(manifest: &Path) -> PathBuf {
    if let Some(dir) = std::env::var_os("DIGUP_LLAMA_DIR") {
        return PathBuf::from(dir);
    }
    manifest
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("Vendor").join("llama"))
        .unwrap_or_else(|| PathBuf::from("Vendor/llama"))
}

/// The translation unit bindgen expands: the headers the Swift module map lists, and nothing else.
fn wrapper_header(out: &Path, headers: [&str; 4]) -> PathBuf {
    let file = out.join("llama-wrapper.h");
    let mut text = String::new();
    for header in headers {
        text.push_str(&format!("#include \"{header}\"\n"));
    }
    std::fs::write(&file, text).expect("write the bindgen wrapper header");
    file
}

/// Find the newest `libclang-*.so*` in the usual places and return a folder holding it under the
/// name clang-sys tries first (`libclang.so`). bindgen's `runtime` feature loads libclang at run
/// time; Ubuntu's clang ships `libclang-21.so.21` and no plain `libclang.so`.
fn libclang_shim(out: &Path) -> Option<PathBuf> {
    let dirs = [
        "/usr/lib/llvm-21/lib",
        "/usr/lib/llvm-20/lib",
        "/usr/lib/llvm-19/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib",
        "/usr/local/lib",
        "/opt/homebrew/opt/llvm/lib",
    ];
    let found = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            name.starts_with("libclang-") && name.contains(".so") && !name.contains("cpp")
        })
        .max()?;
    let shim = out.join("libclang-shim");
    std::fs::create_dir_all(&shim).ok()?;
    let link = shim.join("libclang.so");
    std::fs::remove_file(&link).ok();
    std::os::unix::fs::symlink(&found, &link).ok()?;
    Some(shim)
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=DIGUP_LLAMA_DIR");
    println!("cargo::rerun-if-env-changed=LIBCLANG_PATH");
    println!("cargo::rustc-check-cfg=cfg(llama_unavailable)");
    println!("cargo::rustc-check-cfg=cfg(llama_metal)");

    let vendor = llama_dir(&manifest);
    let (include, lib) = (vendor.join("include"), vendor.join("lib"));
    let headers = ["llama.h", "mtmd.h", "mtmd-helper.h", "llama-build.h"];
    if !headers.iter().all(|h| include.join(h).is_file()) {
        println!(
            "cargo::warning=llama.cpp is not vendored at {}: run scripts/build-llama.sh, then build \
             again — the embedder is compiled out until it is there",
            include.display()
        );
        println!("cargo::rustc-cfg=llama_unavailable");
        std::fs::write(
            out.join("llama_bindings.rs"),
            "// scripts/build-llama.sh has not run.\n",
        )
        .expect("write an empty bindings file");
        return;
    }
    for header in headers {
        println!("cargo::rerun-if-changed={}", include.join(header).display());
    }
    if std::fs::read_to_string(vendor.join("BUILD"))
        .unwrap_or_default()
        .contains("metal")
    {
        println!("cargo::rustc-cfg=llama_metal");
    }
    let wrapper = wrapper_header(&out, headers);
    let target = std::env::var("TARGET").unwrap_or_default();
    let includes = format!("-I{}", include.display());
    let build = || {
        bindgen::Builder::default()
            .header(wrapper.to_string_lossy().to_string())
            .clang_arg("-x")
            .clang_arg("c")
            .clang_arg("-std=c11")
            .clang_arg(format!("--target={target}"))
            .clang_arg(includes.clone())
            // Everything the four vendored headers declare, and nothing else: bindgen auto-blocklists
            // system headers, so this is llama.cpp's API, mtmd's (the image and audio encoders),
            // ggml's backend registry (which loads ggml-cpu-*/ggml-cuda/ggml-vulkan at run time) and
            // ggml's log hook. Enum constants keep their C names.
            .derive_debug(false)
            .derive_default(false)
            .layout_tests(false)
            .generate()
    };

    let bindings = match build() {
        Ok(bindings) => bindings,
        Err(first) => {
            let shim =
                libclang_shim(&out).unwrap_or_else(|| panic!("no libclang for bindgen: {first}"));
            std::env::set_var("LIBCLANG_PATH", &shim);
            build().unwrap_or_else(|e| panic!("bindgen over {}: {e}", include.display()))
        }
    };
    bindings
        .write_to_file(out.join("llama_bindings.rs"))
        .expect("write the bindings");

    // Link the vendored libraries with the vendor lib folder as an rpath, so the ggml backend
    // modules that load at run time sit where the loader already looks (Package.swift:31 does the
    // same with `-Xlinker -rpath`).
    println!("cargo::rustc-link-search=native={}", lib.display());
    for name in [
        "llama",
        "mtmd",
        "ggml",
        "ggml-base",
        "stdc++",
        "m",
        "pthread",
        "dl",
    ] {
        println!("cargo::rustc-link-lib=dylib={name}");
    }
    // One rpath on the vendor lib folder for this crate's targets (its tests and examples link
    // it). A downstream binary finds the same libraries through LD_LIBRARY_PATH=Vendor/llama/lib,
    // which is how the Swift helper's payload is run too (`Package.swift:31` uses `-Xlinker -rpath`).
    println!("cargo::rustc-link-arg=-Wl,-rpath,{}", lib.display());

    // ⑨ (the Indexer): this crate now links `crates/native` for the prepare stage (stb
    // pictures, PDFium pages, ffmpeg media). Cargo carries a dylib's *search* through the
    // rlib graph, not its `-l` line — so this crate names PDFium (and its C++ runtime)
    // itself, the way crates/cli/build.rs repeats it for the binary, with the same rpath.
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .map(|root| root.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("../.."));
    let pdfium = root.join("Vendor").join("pdfium").join("lib");
    println!("cargo::rustc-link-search=native={}", pdfium.display());
    println!("cargo::rustc-link-lib=dylib=pdfium");
    println!("cargo::rustc-link-lib=dylib=stdc++");
    println!("cargo::rustc-link-arg=-Wl,-rpath,{}", pdfium.display());
}
