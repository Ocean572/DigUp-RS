#!/bin/bash
# Builds the Rust engine's Windows x86_64 payload: `digup.exe` + `digup-server.exe`
# with the DLLs they ask for beside themselves — llama.cpp's libraries (the ggml
# backends among them, loaded by path), PDFium's, and the small ffmpeg
# scripts/build-ffmpeg.sh builds. The same file set the shell's installer bundles
# (shell/src-tauri/helper), and the same layout the Docker/CI payloads use; Windows
# has no $ORIGIN, so the loader's rule is "the executable's own folder first", which
# is exactly where these land.
#
#   scripts/rust/package-windows.sh [--shell]     → dist\digup-<version>-win-x64.zip
#                                                   (+ the NSIS installer, --shell)
#
# Run it in Git for Windows' bash inside the MSVC environment (vcvars64) — the same
# environment scripts/build-llama.sh documents for its Windows branch — on the pinned
# Rust toolchain (the MSVC host target installed). §11: build it on the arch you ship.
set -euo pipefail
cd "$(dirname "$0")/../.."
scripts/fetch-vendor.sh
scripts/build-ffmpeg.sh
scripts/build-llama.sh
cargo build --release -p digup-cli -p digup-server

# Before the payload exists the DLLs come from Vendor/ (Windows loads the exe's own
# folder first, then PATH — the payload gate needs nothing extra, that is where its
# DLLs land).
probe_env() { PATH="$PWD/Vendor/llama/lib:$PWD/Vendor/pdfium/lib:$PWD/Vendor/ffmpeg/bin:$PATH" "$@"; }
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
out=dist/digup-$version-win-x64
rm -rf "$out"
mkdir -p "$out/payload"
cp target/release/digup.exe target/release/digup-server.exe "$out/payload/"
cp Vendor/llama/lib/*.dll "$out/payload/"
cp Vendor/pdfium/lib/*.dll "$out/payload/"
cp Vendor/ffmpeg/bin/ffmpeg.exe Vendor/ffmpeg/bin/ffprobe.exe "$out/payload/"
python3 scripts/rust/notices-rust.py windows "$out/payload/THIRD-PARTY-NOTICES.txt"
cp LICENSE "$out/payload/LICENSE"
# The gate: the payload answers from its own folder alone (PATH points at it), so a
# missing DLL means the version line never prints.
PATH="$out/payload:/usr/bin:/bin" "$out/payload/digup.exe" --help > /dev/null
PATH="$out/payload:/usr/bin:/bin" "$out/payload/digup.exe" --help | sed -n '1p'
python3 - "$out" "dist/$(basename "$out").zip" <<'PY'
import os, sys, zipfile
root, out_zip = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out_zip, "w", zipfile.ZIP_DEFLATED) as archive:
    for name in sorted(os.listdir(os.path.join(root, "payload"))):
        archive.write(os.path.join(root, "payload", name), "payload/" + name)
PY
echo "payload: dist/$(basename "$out").zip"

if [ "${1:-}" = "--shell" ]; then
  helper=shell/src-tauri/helper
  rm -rf "$helper"; mkdir -p "$helper"
  cp "$out"/payload/* "$helper/"
  tauri=(npx --yes @tauri-apps/cli@2.12.2)
  if cargo tauri --version > /dev/null 2>&1; then tauri=(cargo tauri); fi
  "${tauri[@]}" build
  echo "shell bundles: shell/src-tauri/target/release/bundle/"
fi
