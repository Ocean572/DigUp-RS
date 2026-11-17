#!/bin/bash
# Builds the Rust engine's macOS payload, arm64 and x86_64 (a universal binary when
# both toolchains are present): `digup` + `digup-server` with the runtime C libraries
# beside them — the vendored fat libpdfium.dylib (the Rust engine's PDF reader links it
# at launch on macOS too; the loader finds it by the @loader_path rpath the script adds,
# the twin of Linux's $ORIGIN) and the small ffmpeg + ffprobe the media pipeline spawns
# (found beside the executable, Media+Portable's lookup). llama.cpp links statically from
# Vendor/llama/lib/libllamacpp.a — built fat (arm64 + x86_64) under DIGUP_UNIVERSAL=1,
# which this script exports, so the payload carries one dylib and no .so files.
#
#   scripts/rust/package-macos.sh [--shell]      → dist/digup-<version>-macos.tar.gz
#                                                 (payload/, universal when both
#                                                 toolchains are present)
#                                                 (+ the Tauri .app/.dmg, --shell)
#
# Run it on a Mac, on the CPU architecture the package is for (§11: native builds;
# the universal payload runs both scripts). Needs the pinned Rust toolchain, Xcode's
# clang, lipo, install_name_tool and codesign, and scripts/fetch-vendor.sh +
# scripts/build-llama.sh run for the target (this script runs the llama build itself;
# DIGUP_UNIVERSAL=1 makes it hand out the fat static archive).
# ffmpeg/ffprobe are host-arch (build-ffmpeg.sh builds native, §11): on the Apple
# Silicon slice media works, on an x86_64-only Mac the media pipeline answers the
# engine's own "can't run ffmpeg" refusal — text, pictures and PDFs are unaffected.
#
# Updates: the Rust shell is the Tauri app, and Tauri's updater plugin reads the
# per-platform feeds scripts/update-feed.py writes (updates/macos-aarch64.json …),
# the mechanism the Windows and Linux shells already ship. Sparkle's appcast
# (scripts/appcast.py, Sparkle's sign_update under .build/) belongs to the Swift
# app in Sources/DigUpApp: .build/ is a SwiftPM artifact, so a Rust build has
# nothing for appcast.py to sign. A Rust-built macOS app updates through Tauri's
# feed; the Sparkle path is not ported by pretending to be one.
set -euo pipefail
cd "$(dirname "$0")/../.." || exit 1
# macOS 14 is the payload's floor — the same one the vendored llama.cpp builds with.
# MACOSX_DEPLOYMENT_TARGET reaches rustc's link and the cc-rs objects alike (measured
# 2026-11-17: unset, the x86_64 link lands on rustc's 10.12 floor and every Vendor
# object warns "built for newer 'macOS' version (26.5) than being linked (10.12)").
# DIGUP_UNIVERSAL asks scripts/build-llama.sh for the fat archive (its own export
# below reaches the scripts it runs; the CI seeding step passes the env from the
# workflow, so the seeding and this script agree on the first run).
export MACOSX_DEPLOYMENT_TARGET=14.0
export DIGUP_UNIVERSAL=1
# The version regexes in scripts/rust/notices-rust.py read scripts/build-ffmpeg.sh, so
# it runs here too (its stamp makes it a no-op when the small ffmpeg is already built).
scripts/build-ffmpeg.sh > /dev/null
scripts/build-llama.sh > /dev/null
targets=()
# Rust's canonical triple is aarch64-apple-darwin — "arm64" is Apple's
# marketing name, not a rustup target (rustup target list has no
# arm64-apple-darwin; measured on the mac payload leg 2026-11-17:
# "no arm64-apple-darwin toolchain" after a green ffmpeg + llama build).
for arch in aarch64 x86_64; do
  target=$arch-apple-darwin
  rustup target list --installed | grep -q "^$target$" || { echo "no $target toolchain (rustup target add $target)"; exit 1; }
  cargo build --release --target "$target" -p digup-cli -p digup-server
  targets+=("$target")
done
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
case "${#targets[@]}" in 2) archsuffix=universal;; *) archsuffix=$(uname -m);; esac
out=dist/digup-$version-macos-$archsuffix
rm -rf "$out"; mkdir -p "$out/payload"
if [ "${#targets[@]}" = 2 ]; then
  lipo -create "target/${targets[0]}/release/digup" "target/${targets[1]}/release/digup" -output "$out/payload/digup"
  lipo -create "target/${targets[0]}/release/digup-server" "target/${targets[1]}/release/digup-server" -output "$out/payload/digup-server"
else
  cp "target/${targets[0]}/release/digup" "target/${targets[0]}/release/digup-server" "$out/payload/"
fi
chmod +x "$out/payload"/*
cp LICENSE "$out/payload/LICENSE"
# The runtime libraries ride beside the binaries: PDFium's fat dylib (the binaries ask
# for it at launch — a payload without it is a dyld abort on the user's machine, where
# the build machine's Vendor path in the rpath does not exist) and the media tools.
cp Vendor/pdfium/lib/libpdfium.dylib Vendor/ffmpeg/bin/ffmpeg Vendor/ffmpeg/bin/ffprobe "$out/payload/"
for b in digup digup-server; do
  # @loader_path: the payload's own folder, what the linux payload does with $ORIGIN
  # (patchelf there, install_name_tool here). lipo's merge and this edit both invalidate
  # the linker's ad-hoc signature, which the arm64 slice enforces at launch: re-sign last.
  otool -l "$out/payload/$b" | grep -q '@loader_path' \
    || install_name_tool -add_rpath @loader_path "$out/payload/$b"
  codesign --force --sign - "$out/payload/$b"
done
if [ "${#targets[@]}" = 2 ]; then
  for f in digup digup-server libpdfium.dylib; do
    archs=$(lipo -archs "$out/payload/$f")
    case " $archs " in *" arm64 "*) ;; *) echo "payload $f: no arm64 slice ($archs)"; exit 1 ;; esac
    case " $archs " in *" x86_64 "*) ;; *) echo "payload $f: no x86_64 slice ($archs)"; exit 1 ;; esac
  done
fi
python3 scripts/rust/notices-rust.py macos "$out/payload/THIRD-PARTY-NOTICES.txt"
"$out/payload/digup" --help > /dev/null          # the payload answers from its own folder
"$out/payload/digup" --help | sed -n '1p'
test -x "$out/payload/digup-server"
tar -C "$out" -czf "dist/$(basename "$out").tar.gz" payload
echo "payload: dist/$(basename "$out").tar.gz"

if [ "${1:-}" = "--shell" ]; then
  helper=shell/src-tauri/helper
  rm -rf "$helper"; mkdir -p "$helper"
  cp "$out"/payload/* "$helper/"
  tauri=(npx --yes @tauri-apps/cli@2.12.2)
  if cargo tauri --version > /dev/null 2>&1; then tauri=(cargo tauri); fi
  "${tauri[@]}" build
  echo "shell bundles: shell/src-tauri/target/release/bundle/"
fi
