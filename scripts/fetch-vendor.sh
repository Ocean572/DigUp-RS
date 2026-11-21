#!/bin/bash
# Linux and Windows: the C libraries the engine needs where Apple's frameworks aren't there, at pinned versions, laid out
# in Vendor/ (out of git): SQLite with FTS5 (storage), stb (images), miniz (docx/odt, xlsx/pptx/ods/odp), PDFium
# (PDF text and page renders; prebuilt by bblanchon/pdfium-binaries; a fat arm64+x86_64 dylib on macOS). The Rust engine's
# portable readers (crates/native) call the same vendored libraries on every platform, so
# the Rust macOS payload fetches them too. Does nothing when the pinned set is already there. Needs curl, tar and python3 (to unzip);
# on Windows, Git for Windows' bash; on macOS, lipo and codesign (Xcode's) for the fat PDFium.
set -euo pipefail
cd "$(dirname "$0")/.."
SQLITE_ZIP=2026/sqlite-amalgamation-3540000.zip       # SQLite 3.54.0
STB_COMMIT=2c980bb59875b0d32144a71867fbdebb2f77cd20   # nothings/stb, 2026-08-02
MINIZ_VERSION=3.1.2
PDFIUM_TAG=chromium/8086
# Hashes in vendor-checksums.json are reviewed pins, never fetched alongside the downloads.
# Update the version and its hashes together when upgrading a dependency.

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) PDFIUM=pdfium-linux-x64 ;;
  Linux-aarch64) PDFIUM=pdfium-linux-arm64 ;;
  # macOS: BOTH mac builds, lipo'd into one fat dylib below. The Rust payload is a
  # universal binary (arm64 + x86_64) and links this dylib at launch; a host-arch
  # PDFium makes the x86_64 half of the payload fail to link (measured 2026-11-17:
  # "ignoring file … found architecture 'arm64', required architecture 'x86_64'"
  # over every FPDF_* symbol).
  Darwin-*) PDFIUM="pdfium-mac-arm64 pdfium-mac-x64" ;;
  MINGW*|MSYS*|CYGWIN*) PDFIUM=pdfium-win-x64 ;;
  *) echo "no PDFium build known for $(uname -s)-$(uname -m)"; exit 1 ;;
esac
# On Windows "python3" can be the Microsoft Store's stub, which only opens the Store.
PYTHON=python3
python3 -c 'import zipfile' 2>/dev/null || PYTHON=python
PINS=$("$PYTHON" -c 'import hashlib; print(hashlib.sha256(open("scripts/vendor-checksums.json", "rb").read()).hexdigest())')
STAMP="verified-v1 $PDFIUM sqlite $SQLITE_ZIP stb $STB_COMMIT miniz $MINIZ_VERSION pdfium $PDFIUM_TAG $PINS"
if [[ "$(cat Vendor/VENDOR 2>/dev/null)" == "$STAMP" ]]; then exit 0; fi
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p Vendor
# Verify bytes before extracting an archive or installing a header. curl errors also fail closed.
# Bounded retries: GitHub's release-asset CDN answers 403 (abuse detection) to runners at random
# (measured on the Packages run 2026-11-21: miniz 403 mid-build, the same URL 200 from the box
# minutes later; the Vulkan SDK download is wrapped the same way in packages.yml). The pin check
# runs on the final bytes — a retried download is verified exactly like a first-attempt one.
download() {
  local name="$1" url="$2"
  curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
    --retry 6 --retry-delay 15 --retry-all-errors -o "$T/$name" "$url"
  "$PYTHON" - "$name" "$T/$name" <<'PY'
import hashlib, json, sys
name, path = sys.argv[1:]
with open('scripts/vendor-checksums.json') as pins:
    expected = json.load(pins)[name]  # a missing pin is an error, not permission to skip verification
with open(path, 'rb') as source:
    digest = hashlib.file_digest(source, 'sha256').hexdigest() if hasattr(hashlib, 'file_digest') else hashlib.sha256(source.read()).hexdigest()
if digest != expected:
    sys.exit(f'SHA-256 mismatch for {name}: expected {expected}, got {digest}')
PY
}
unzip_py() { "$PYTHON" -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$1" "$2"; }

echo "SQLite ($SQLITE_ZIP)"
download sqlite.zip "https://sqlite.org/$SQLITE_ZIP"
unzip_py "$T/sqlite.zip" "$T/sqlite"
rm -rf Vendor/sqlite && mkdir -p Vendor/sqlite/include
cp "$T"/sqlite/*/sqlite3.c Vendor/sqlite/ && cp "$T"/sqlite/*/sqlite3.h Vendor/sqlite/include/

echo "stb ($STB_COMMIT)"
rm -rf Vendor/stb && mkdir -p Vendor/stb/include
for h in stb_image.h stb_image_write.h stb_image_resize2.h; do
  download "$h" "https://raw.githubusercontent.com/nothings/stb/$STB_COMMIT/$h"
  cp "$T/$h" "Vendor/stb/include/$h"
done
cat > Vendor/stb/stb.c <<'C'
// The stb implementations, once (the headers alone are declarations). Written by scripts/fetch-vendor.sh.
#define STB_IMAGE_IMPLEMENTATION
#define STB_IMAGE_WRITE_IMPLEMENTATION
#define STB_IMAGE_RESIZE_IMPLEMENTATION
#include "stb_image.h"
#include "stb_image_write.h"
#include "stb_image_resize2.h"
C

echo "miniz ($MINIZ_VERSION)"
download miniz.zip "https://github.com/richgel999/miniz/releases/download/$MINIZ_VERSION/miniz-$MINIZ_VERSION.zip"
unzip_py "$T/miniz.zip" "$T/miniz"
rm -rf Vendor/miniz && mkdir -p Vendor/miniz/include
cp "$T"/miniz/miniz.c Vendor/miniz/ && cp "$T"/miniz/*.h Vendor/miniz/include/ && cp "$T"/miniz/LICENSE Vendor/miniz/

echo "PDFium ($PDFIUM_TAG, $PDFIUM)"
for name in $PDFIUM; do
  download "$name.tgz" "https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_TAG/\//%2F}/$name.tgz"
  mkdir -p "$T/$name" && tar -xzf "$T/$name.tgz" -C "$T/$name"
done
rm -rf Vendor/pdfium && mkdir -p Vendor/pdfium
cp -R "$T/${PDFIUM%% *}/." Vendor/pdfium/     # headers, LICENSE, licenses/ from the first list entry
if [[ $PDFIUM == *" "* ]]; then
  # macOS: every slice into one fat dylib. The vendored dylib carries the install-name
  # `./libpdfium.dylib` (pdfium-binaries' relative id — measured byte-level on this box:
  # LC_ID_DYLIB is that string in both mac slices), and ld copies it verbatim into the
  # load command: dyld resolves it against the CWD and every rpath is ignored (measured
  # on the payload gate 2026-11-17: "Library not loaded: ./libpdfium.dylib"). Rewrite
  # it to the @rpath form so rpaths work, then ad-hoc sign — the merge and the edit
  # write new bytes, and the arm64 slice refuses to load a dylib whose signature no
  # longer fits. install_name_tool on a fat file edits the id in every slice.
  slices=()
  for name in $PDFIUM; do slices+=("$T/$name/lib/libpdfium.dylib"); done
  lipo -create "${slices[@]}" -output Vendor/pdfium/lib/libpdfium.dylib
  install_name_tool -id @rpath/libpdfium.dylib Vendor/pdfium/lib/libpdfium.dylib
  codesign --force --sign - Vendor/pdfium/lib/libpdfium.dylib
fi
# Windows: the DLL is in bin/, its import library is lib/pdfium.dll.lib; the module map links "pdfium".
if [[ -f Vendor/pdfium/lib/pdfium.dll.lib ]]; then cp Vendor/pdfium/lib/pdfium.dll.lib Vendor/pdfium/lib/pdfium.lib; fi
ls Vendor/pdfium/lib

rm -rf "$T"
echo "$STAMP" > Vendor/VENDOR
echo "Fetched into Vendor/: sqlite, stb, miniz, pdfium"
