#!/bin/bash
# Builds the Rust engine's macOS payload, arm64 and x86_64 (a universal binary when
# both toolchains are present): `digup` + `digup-server` with nothing beside them —
# on macOS the engine uses Apple's own media stack (AVFoundation, PDFKit, ImageIO),
# and llama.cpp links statically from Vendor/llama/lib/libllamacpp.a, so the payload
# carries no .so files at all (the same fact the Swift payload relies on).
#
#   scripts/rust/package-macos.sh [--shell]      → dist/digup-<version>-macos-<arch>.tar.gz
#                                                 (+ the Tauri .app/.dmg, --shell)
#
# Run it on a Mac, on the CPU architecture the package is for (§11: native builds;
# the universal payload runs both scripts). Needs the pinned Rust toolchain, Xcode's
# clang, and scripts/build-llama.sh run for the target (its macOS branch makes the
# static archives).
#
# Updates: the Rust shell is the Tauri app, and Tauri's updater plugin reads the
# per-platform feeds scripts/update-feed.py writes (updates/macos-aarch64.json …),
# the mechanism the Windows and Linux shells already ship. Sparkle's appcast
# (scripts/appcast.py, Sparkle's sign_update under .build/) belongs to the Swift
# app in Sources/DigUpApp: .build/ is a SwiftPM artifact, so a Rust build has
# nothing for appcast.py to sign. A Rust-built macOS app updates through Tauri's
# feed; the Sparkle path is not ported by pretending to be one.
set -euo pipefail
cd "$(dirname "$0")/../.."
# The version regexes in scripts/rust/notices-rust.py read scripts/build-ffmpeg.sh, so
# it runs here too (its stamp makes it a no-op when the small ffmpeg is already built).
scripts/build-ffmpeg.sh > /dev/null
scripts/build-llama.sh > /dev/null
targets=()
for arch in arm64 x86_64; do
  if rustup target list --installed 2>/dev/null | grep -q "apple-darwin"; then :; fi
  target=$arch-apple-darwin
  rustup target list --installed | grep -q "^$target$" || { echo "no $target toolchain (rustup target add $target)"; exit 1; }
  cargo build --release --target "$target" -p digup-cli -p digup-server
  targets+=("$target")
}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
out=dist/digup-$version-macos
rm -rf "$out"; mkdir -p "$out/payload"
if [ "${#targets[@]}" = 2 ]; then
  lipo -create "target/${targets[0]}/release/digup" "target/${targets[1]}/release/digup" -output "$out/payload/digup"
  lipo -create "target/${targets[0]}/release/digup-server" "target/${targets[1]}/release/digup-server" -output "$out/payload/digup-server"
else
  cp "target/${targets[0]}/release/digup" "target/${targets[0]}/release/digup-server" "$out/payload/"
fi
chmod +x "$out/payload"/*
cp LICENSE "$out/payload/LICENSE"
python3 scripts/rust/notices-rust.py macos "$out/payload/THIRD-PARTY-NOTICES.txt"
"$out/payload/digup" --help > /dev/null          # the payload answers from its own folder
"$out/payload/digup" --help | sed -n '1p'
tar -C "$out" -czf "dist/$(basename "$out").tar.gz" payload
echo "payload: dist/$(basename "$out").tar.gz"

if [ "${1:-}" = "--shell" ]; then
  helper=shell/src-tauri/helper
  rm -rf "$helper"; mkdir -p "$helper"
  cp "$out"/payload/digup "$out"/payload/digup-server "$helper/"
  tauri=(npx --yes @tauri-apps/cli@2.12.2)
  if cargo tauri --version > /dev/null 2>&1; then tauri=(cargo tauri); fi
  "${tauri[@]}" build
  echo "shell bundles: shell/src-tauri/target/release/bundle/"
fi
