+++
title = "Contributing"
description = "How to propose a change, what a pull request needs, how to build DigUp from source, and where things are in the code."
section = "Project"
order = 1
+++

DigUp is a small project with one maintainer and a plan for where it's going. Reports, ideas and fixes are welcome, and a few notes up front save everyone time.

## Open an issue first

Please open an issue before you write any code, for a fix as much as for a feature. The same fix or feature may already be in progress and not pushed yet. An issue gets you a clear "yes", "not now" or "here's how it should fit" before you spend an evening on it. A pull request that arrives without one may be closed with a link back here.

DigUp keeps everything on your computer, so changes that send data elsewhere, like telemetry or cloud services, aren't a fit.

## Pull requests

Once an issue says go:

- Keep it to one fix or one feature, with the reason in the description.
- Make sure `cargo test --workspace` passes (the engine suite in `crates/`), and `cargo test` in `shell/src-tauri` if you change the Windows/Linux/macOS app.
- If it changes what search returns, run the evals in `evals/` before and after, and include both results.
- For speed or memory changes, measure before and after on the same computer.

Not every pull request will be merged, even good ones. The project has a direction, and some changes won't fit it; others get reworked to fit work in progress, and then you're credited in the release notes. Replies can take a few days: a quiet week means busy, not ignored, and every issue gets an answer.

## Build from source

:::: tabs
::: tab Any system
The engine is Rust on every system. You'll need a Rust toolchain (the workspace floor is
1.89; CI uses 1.99.1), git, and CMake — on Linux also clang with libclang (bindgen) and
the usual build tools.

```sh
git clone {{ site.github }} && cd DigUp
scripts/fetch-vendor.sh             # SQLite/FTS5, stb, miniz, PDFium at pinned hashes
scripts/build-llama.sh              # llama.cpp at a pinned tag, with a one-line patch
scripts/build-ffmpeg.sh             # the small ffmpeg the packages carry
cargo test --workspace              # the engine suite; model-pinned tests skip without the models
cargo build --release -p digup-cli  # target/release/digup — the CLI and the shell's helper
```
:::
::: tab Windows and Linux
`scripts/package-linux.sh` makes the .deb and the AppImage, `scripts/package-windows.sh` the
NSIS installer: each builds the Rust helper, stages the C libraries beside it, and bundles the
Tauri shell. `scripts/rust/package-*.sh` ship the payload tarballs (the same file set the Docker
payload stage ships). On Windows run them in Git for Windows' bash inside a Visual Studio
developer environment. [The packages workflow]({{ site.github }}/blob/main/.github/workflows/packages.yml)
has every step, down to the packages each system needs.
:::
::::

`scripts/fetch-vendor.sh` checks every download against pinned SHA-256 values before using it. There's also a command-line tool, `target/release/digup`, that indexes, searches and runs the evals from a terminal (`digup --help`), and `scripts/rust/make-testbed-linux.py` builds a folder of test files with known content to try it on.

## Where things are

The search engine is the Rust `crates/` workspace — extraction, scoring and vectors in `core`, the llama.cpp / ffmpeg / PDFium / stb seams in `native`, the embedders and the index/search loop in `runtime`, the JSON-lines services in `services`, the `digup` CLI in `cli` and the HTTP server in `server`. It is the same code on every system. The app is `shell/` (Tauri: Rust, with plain HTML and JavaScript for its windows) on Windows, Linux and macOS, which runs the engine as a helper program. The self-hosted server is `server/` (a FastAPI wrapper and a Lit web UI, plus the pure-Rust `digup-server`) with the compose deployment in `deploy/`. `scripts/` builds llama.cpp, FFmpeg and the packages, and `scripts/rust/` is the port's own tooling. The Swift original rides branch `swift-reference` as the parity reference.

This website is in `website/`: Markdown pages in `website/content/`, built with `uv run website/build.py --serve`. A fix to the docs is as welcome as a fix to the code.

## Supporting the project

DigUp is free and stays that way. If it helps you, you can [support it]({{ site.support }}). Stars, good bug reports and telling people about it help just as much.
