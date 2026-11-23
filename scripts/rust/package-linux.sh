#!/bin/bash
# Builds the Rust engine's Linux payload: `digup` + `digup-server` with the native
# libraries beside themselves ($ORIGIN), the same file set and layout the Docker
# payload stage ships (server/docker/Dockerfile, rust-engine-payload), as a tarball
# a box can untar anywhere. The Tauri shell's own bundles (.deb/.AppImage) are a
# separate, opt-in step (--shell): they run scripts/package-linux.sh's bundling with
# the Rust helper in shell/src-tauri/helper, which the shell spawns over the same
# JSON-lines protocol.
#
#   scripts/rust/package-linux.sh [--shell]      → dist/digup-<version>-linux-<arch>.tar.gz
#                                                 (+ shell/src-tauri/target/release/bundle/…)
#
# Run it on the CPU architecture the package is for (AGENTS §11: native builds only,
# never under emulation), after scripts/fetch-vendor.sh and scripts/build-llama.sh.
# Needs the pinned Rust toolchain (rustup, the workspace's floor is 1.89 — raised
# 2026-11-16 by File::try_lock, the Windows writer lock; sha2 0.11
# says so on a 1.85 cargo, measured 2026-11-13), patchelf, and what
# scripts/build-ffmpeg.sh needs (it builds the small ffmpeg the payload carries).
set -euo pipefail
cd "$(dirname "$0")/../.."
scripts/build-ffmpeg.sh
# Faces (FACES.md): the desktop People tab needs the faces verbs compiled in —
# the flag ships code, never weights: the face packs stay user-supplied, the
# consent record gates every verb.
# Build the engine binaries: DIGUP_SKIP_BUILD=1 uses the binaries already in
# target/release (the CUDA payload flow builds them in the CUDA devel container
# first — glibc-matched, linked against the CUDA-flavored llama — and the host
# script only assembles the payload).
if [ "${DIGUP_SKIP_BUILD:-0}" != 1 ]; then
cargo build --release -p digup-cli -p digup-server --features digup-cli/faces
fi

# The engine asks for its C libraries by name (libllama.so.0 …): before the payload
# exists, the probe points at Vendor/; inside the payload it needs nothing, because
# patchelf gives every binary the $ORIGIN rpath.
probe_env() { LD_LIBRARY_PATH="$PWD/Vendor/llama/lib:$PWD/Vendor/pdfium/lib" PATH="$PWD/Vendor/ffmpeg/bin:$PATH" "$@"; }
# The llama tree to ship: default Vendor/llama/lib (the CPU flavor — the default
# build stays byte-identical). A GPU payload names the CUDA-flavored Vendor tree
# (built with the CUDA devel container) and stages the CUDA runtime libraries from
# the CUDA RUNTIME base (docs/implementation/gpu-handoff.md's measured recipe).
llama_lib=${DIGUP_LLAMA_LIB:-Vendor/llama/lib}
arch=$(uname -m)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
out=dist/digup-$version-linux-$arch
payload=$out/payload
rm -rf "$out"
mkdir -p "$payload"
cp target/release/digup target/release/digup-server \
   Vendor/pdfium/lib/libpdfium.so Vendor/ffmpeg/bin/ffmpeg Vendor/ffmpeg/bin/ffprobe "$payload/"
for file in "$llama_lib"/*.so*; do
  name=${file##*/}
  case "$name" in
    # Once, under the name the binary asks for (libllama.so.0), not the full version.
    *.so.[0-9]*) [ -L "$file" ] && cp -L "$file" "$payload/$name" ;;
    # llama.cpp's backends (libggml-cpu-*.so, libggml-cuda.so …), loaded by path at run time.
    *.so) [ ! -L "$file" ] && cp "$file" "$payload/" ;;
  esac
done
# CUDA payload (GPU indexing, 2026-11-23): exactly the runtime libraries
# libggml-cuda.so asks for — libcudart, libcublas, libcublasLt, libnvrtc,
# libnvrtc-builtins + libnccl — staged from the CUDA RUNTIME base with `cp -a`
# (symlink + target: the server CUDA image's measured recipe, gpu-handoff.md §4).
# Never the toolkit, never a second libggml*, never libcuda.so.1 (the host
# driver's own). Opt-in: an unset DIGUP_CUDA_LIBS keeps the CPU payload verbatim.
if [ -n "${DIGUP_CUDA_LIBS:-}" ]; then
  cp -a "${DIGUP_CUDA_LIBS}"/lib* "$payload/"
fi
for file in "$payload"/*; do
  case "$file" in *.so*) patchelf --set-rpath '$ORIGIN' "$file" ;; esac
done
patchelf --set-rpath '$ORIGIN' "$payload/digup" "$payload/digup-server"

# Every library the payload carries, with its license, beside the binaries — the
# Rust twin of scripts/notices.py (the crate graph of this workspace, not the shell's).
python3 scripts/rust/notices-rust.py linux "$payload/THIRD-PARTY-NOTICES.txt"
if [ -n "${DIGUP_CUDA_LIBS:-}" ]; then
  cat >> "$payload/THIRD-PARTY-NOTICES.txt" <<'NOTICE'

CUDA runtime libraries (libcudart, libcublas, libcublasLt, libnvrtc, libnvrtc-builtins)
  license: NVIDIA License Agreement for CUDA Toolkit (redistributable components)
  https://docs.nvidia.com/cuda/eula/index.html
libnccl
  license: NVIDIA Slackware Public License / BSD 3-Clause (NCCL 2.x)
  https://github.com/NVIDIA/nccl
Shipped per the server CUDA image recipe (docs/implementation/gpu-handoff.md §4):
the runtime libraries only, `cp -a`'d from the CUDA runtime base — the toolkit and
libcuda.so.1 (the host driver's own) are never bundled.
NOTICE
fi
cp LICENSE "$payload/LICENSE"

# The gate is the payload answering from its own folder: PATH points at it alone,
# so a missing library means the version line never prints.
PATH="$payload:/usr/bin:/bin" "$payload/digup" --help > /dev/null
PATH="$payload:/usr/bin:/bin" "$payload/digup" --help | sed -n '1p'
test -x "$payload/digup-server"

tar -C "$out" -czf "dist/$(basename "$out").tar.gz" payload
echo "payload: dist/$(basename "$out").tar.gz"

if [ "${1:-}" = "--shell" ]; then
  # The desktop shell (shell/, Tauri 2) with this payload as its helper folder: the
  # shell spawns `digup` helpers over the JSON-lines protocol, and the helper finds
  # llama.cpp's backends and ffmpeg in its own folder (the Swift package's layout).
  helper=shell/src-tauri/helper
  rm -rf "$helper"
  mkdir -p "$helper"
  cp "$payload"/* "$helper/"
  tauri=(npx --yes @tauri-apps/cli@2.12.2)
  if cargo tauri --version > /dev/null 2>&1; then tauri=(cargo tauri); fi
  "${tauri[@]}" build
  echo "shell bundles: shell/src-tauri/target/release/bundle/"
fi
