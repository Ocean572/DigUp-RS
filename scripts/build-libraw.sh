#!/bin/bash
# Linux and macOS: LibRaw (camera RAW, #152) for DigUp's packages, built as the engine calls it:
# the vendor tree's own Makefile.devel builds the static libraries (single- and multi-threaded) —
# no configure, no shared objects, and the package needs no extra runtime files (the crate links
# `libraw.a` + the C++ standard library into the binary). → Vendor/libraw/lib (out of git).
# Does nothing when the pinned version is already built. Windows keeps the notReadYet DECLINE:
# this script exits 0 with a note there (the make flow is the unix toolchain's).
# Needs a C++ compiler and make; zlib's headers (the deflate decompressor, USE_ZLIB in the
# tree's own Makefile.devel). The tree's default compiler is clang (Makefile.devel's CC/CXX);
# a box without it inherits an exported CXX.
set -euo pipefail
cd "$(dirname "$0")/.."
LIBRAW_VERSION=0.22.3

case "$(uname -s)" in
  Linux*|Darwin*) ;;
  *) echo "libraw: no build known for $(uname -s) (camera RAW keeps the notReadYet decline)"; exit 0 ;;
esac

# The macOS universal payload (scripts/rust/package-macos.sh / rust-release.yml set
# DIGUP_UNIVERSAL=1): the archive must carry the arm64 AND the x86_64 slice, the same law
# build-llama.sh follows for libllamacpp.a — an arm64-only C set makes the x86_64 half of
# the universal payload fail to link (measured 2026-11-17).
UNIVERSAL=0
[[ "$(uname -s)" == Darwin && "${DIGUP_UNIVERSAL:-0}" == 1 ]] && UNIVERSAL=1

STAMP="libraw $LIBRAW_VERSION universal=$UNIVERSAL"
if [[ "$(cat Vendor/libraw/LIBRAW 2>/dev/null)" == "$STAMP" && -f Vendor/libraw/lib/libraw.a ]]; then exit 0; fi
if [[ ! -f Vendor/libraw/Makefile.devel ]]; then
  echo "libraw: Vendor/libraw is not fetched — run scripts/fetch-vendor.sh first" >&2
  exit 1
fi

JOBS="$(nproc 2>/dev/null || sysctl -n hw.ncpu)"
CXX_BASE="${CXX:-clang++}"
echo "building LibRaw $LIBRAW_VERSION (static libraries)"
if [[ $UNIVERSAL == 1 ]]; then
  # A command-line CXX assignment overrides the file's default wholesale — the -arch flag
  # rides the compiler the way Makefile.devel's own "macOS dual arch" comment intends.
  # Makefile.devel's `clean` empties object/ between the slices; the archives are filed
  # per slice before the next pass deletes them.
  for arch in arm64 x86_64; do
    make -C Vendor/libraw -f Makefile.devel clean >/dev/null
    make -C Vendor/libraw -f Makefile.devel library "-j$JOBS" "CXX=$CXX_BASE -arch $arch"
    mv "Vendor/libraw/lib/libraw.a"     "Vendor/libraw/lib/libraw-$arch.a"
    mv "Vendor/libraw/lib/libraw_r.a"   "Vendor/libraw/lib/libraw_r-$arch.a"
  done
  lipo -create -output Vendor/libraw/lib/libraw.a \
    Vendor/libraw/lib/libraw-arm64.a Vendor/libraw/lib/libraw-x86_64.a
  lipo -create -output Vendor/libraw/lib/libraw_r.a \
    Vendor/libraw/lib/libraw_r-arm64.a Vendor/libraw/lib/libraw_r-x86_64.a
else
  if [[ -n "${CXX:-}" ]]; then
    make -C Vendor/libraw -f Makefile.devel library "-j$JOBS" "CXX=$CXX_BASE"
  else
    make -C Vendor/libraw -f Makefile.devel library "-j$JOBS"
  fi
fi
test -f Vendor/libraw/lib/libraw.a
echo "$STAMP" > Vendor/libraw/LIBRAW
echo "built Vendor/libraw/lib: libraw.a libraw_r.a"
