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
# belonged to the Swift app (Sources/DigUpApp, branch swift-reference: a Sparkle
# update needs an Info.plist SUFeedURL and a SwiftPM-built app — a Rust build has
# neither); the Sparkle path is not ported by pretending to be one.
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
  # Faces (FACES.md): the desktop People tab needs the faces verbs compiled in —
  # the flag ships code, never weights: the face packs stay user-supplied, the
  # consent record gates every verb (what the UI gates, the engine gates).
  cargo build --release --target "$target" -p digup-cli -p digup-server --features digup-cli/faces
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
# The PDFium dylib is a PREBUILT download (fetch-vendor): it arrives unsigned, and arm64
# refuses to load an unsigned dylib — measured 2026-11-20 on Apple Silicon, a payload run
# on a user machine DYLD-aborts with "library load disallowed by system policy" (the
# runner never sees this: its build-time rpath folder exists there, the loader never
# reaches the @loader_path copy the signature rule judges). Sign every payload member
# the binaries load, same last-step rule as the two binaries above; ffmpeg/ffprobe carry
# the linker's own ad-hoc signature and are re-signed for the same reason (a copied byte
# can lose it).
codesign --force --sign - "$out/payload/libpdfium.dylib" "$out/payload/ffmpeg" "$out/payload/ffprobe"
# Verify the code members only — the payload folder also carries LICENSE and the notices,
# and codesign on a text file dies "code object is not signed at all" (measured on the
# runner 2026-11-21: the folder glob failed the macos leg exactly there).
codesign --verify --strict "$out/payload/digup" "$out/payload/digup-server" \
    "$out/payload/libpdfium.dylib" "$out/payload/ffmpeg" "$out/payload/ffprobe"
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
  # The Tauri shell as a macOS .app (and a .dmg): the payload folder rides in the app's
  # Contents/MacOS/helper — the path options.rs' default_helper looks in ("beside this
  # program", the installers' layout; Tauri's own resource staging lands in
  # Contents/Resources, so the helper is copied into Contents/MacOS after the build) and
  # Tauri's resources carry the notices beside the app. The app is universal when both
  # toolchains built: tauri builds each target's .app, the merge lipos the shell binary
  # and keeps the (already fat) helper folder — resources are target-independent. Every
  # byte change (lipo, the helper copy) precedes the deep ad-hoc signature, which is
  # applied last and covers the nested binaries and dylibs — the same lipo/codesign
  # order as the payload above. LSMinimumSystemVersion matches the payload's macOS 14
  # floor. The .app.tar.gz the updater installs is signed with the release key when the
  # environment carries it (the Linux/Windows shells' rule: TAURI_SIGNING_PRIVATE_KEY,
  # plugins.updater.pubkey; a build without it offers no in-app updates).
  helper=shell/src-tauri/helper
  rm -rf "$helper"; mkdir -p "$helper"
  cp "$out"/payload/* "$helper/"
  python3 scripts/notices.py macos shell/src-tauri/THIRD-PARTY-NOTICES.txt
  signing=
  if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    if grep -q '"pubkey": ""' shell/src-tauri/tauri.conf.json; then
      echo "the signing key is set, but plugins.updater.pubkey in shell/src-tauri/tauri.conf.json is empty" >&2
      exit 1
    fi
    signing=',"createUpdaterArtifacts":true'
  fi
  tauri=(npx --yes @tauri-apps/cli@2.12.2)
  if cargo tauri --version > /dev/null 2>&1; then tauri=(cargo tauri); fi
  rm -rf shell/src-tauri/target/release/bundle/macos shell/src-tauri/target/release/bundle/dmg
  resources='"resources":{"helper/":"helper/","THIRD-PARTY-NOTICES.txt":"THIRD-PARTY-NOTICES.txt"}'
  # Tauri v2.12's schema types bundle.macOS.infoPlist as null|string — a PATH to a plist
  # whose entries Tauri merges into the generated Info.plist (v1's object map is gone;
  # a90e574 renamed the key but kept v1's shape, and the CLI refused it: `"infoPlist":
  # {...} is not of types "null", "string"`). The entries live in Info.plist, beside
  # tauri.conf.json.
  plist='"infoPlist":"Info.plist"'
  appdirs=()
  for target in "${targets[@]}"; do
    (cd shell/src-tauri && "${tauri[@]}" build --target "$target" --bundles app \
      --config "{\"bundle\":{$resources$signing,\"macOS\":{$plist}}}")
    appdirs+=("shell/src-tauri/target/$target/release/bundle/macos/DigUp.app")
  done
  app=shell/src-tauri/target/release/bundle/macos/DigUp.app
  rm -rf "$app"; mkdir -p "$(dirname "$app")"
  cp -R "${appdirs[0]}" "$app"
  # The bundle's executable is the BINARY's name — digup-shell. The Swift-era app
  # binary was named DigUp and the lipo paths were written to it; the Rust shell
  # bundle puts digup-shell in Contents/MacOS (measured 2026-11-19: lipo "can't
  # open input file: .../Contents/MacOS/DigUp" after both .app bundles built fine).
  exe=$(ls "${appdirs[0]}/Contents/MacOS/" | head -1)
  if [ "${#targets[@]}" = 2 ]; then
    lipo -create "${appdirs[0]}/Contents/MacOS/$exe" "${appdirs[1]}/Contents/MacOS/$exe" \
      -output "$app/Contents/MacOS/$exe"
  fi
  # The helper beside the binary — where default_helper looks; Tauri's copy sits in
  # Contents/Resources too (the resources entry), so an installed app finds it either way.
  cp -R "$helper" "$app/Contents/MacOS/helper"
  codesign --force --deep --sign - "$app"
  codesign --verify --deep --strict "$app"
  "$app/Contents/MacOS/helper/digup" --help | sed -n '1p'   # the helper answers from the bundle
  if [ "${#targets[@]}" = 2 ]; then
    for f in "$app/Contents/MacOS/$exe" "$app/Contents/MacOS/helper/digup" "$app/Contents/MacOS/helper/libpdfium.dylib"; do
      archs=$(lipo -archs "$f")
      # Two independent checks: lipo lists slices in its own order, the membership is
      # what matters (the payload's assert above uses the same pair).
      case " $archs " in *" arm64 "*) ;; *) echo "bundle $f: no arm64 slice ($archs)"; exit 1 ;; esac
      case " $archs " in *" x86_64 "*) ;; *) echo "bundle $f: no x86_64 slice ($archs)"; exit 1 ;; esac
    done
  fi
  # The updater artifact: the merged app's tarball, named the way update-feed.py reads
  # (DigUp_<version>_<arch>.app.tar.gz + .sig). The per-target .app.tar.gz files tauri
  # built are single-slice — the merged tarball is what ships.
  case "${#targets[@]}" in 2) shellarch=universal;; *) shellarch=$(uname -m);; esac
  [ "$shellarch" = arm64 ] && shellarch=aarch64
  tarball=shell/src-tauri/target/release/bundle/macos/DigUp_${version}_${shellarch}.app.tar.gz
  tar -czf "$tarball" -C "$(dirname "$app")" "$(basename "$app")"
  if [[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    (cd shell/src-tauri && "${tauri[@]}" signer sign "$tarball" \
      -k "$TAURI_SIGNING_PRIVATE_KEY" -p "$TAURI_SIGNING_PRIVATE_KEY_PASSWORD")
  fi
  # The .dmg a person drags to Applications: the app plus the Applications symlink.
  staging=$(mktemp -d)
  cp -R "$app" "$staging/"
  ln -sf /Applications "$staging/Applications"
  # The output folder: the rm -rf at the top of --shell cleared bundle/, and --bundles app
  # (deliberately — the dmg merge is ours) creates only macos/. hdiutil's "create failed -
  # No such file or directory" is its output path missing the dmg/ folder (measured 2026-11-19).
  mkdir -p shell/src-tauri/target/release/bundle/dmg
  hdiutil create -volname DigUp -srcfolder "$staging" -ov -format UDZO \
    "shell/src-tauri/target/release/bundle/dmg/DigUp_${version}_${shellarch}.dmg" > /dev/null
  rm -rf "$staging"
  echo "shell bundles: shell/src-tauri/target/release/bundle/macos/ + dmg/"
fi
