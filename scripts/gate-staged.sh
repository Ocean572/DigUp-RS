#!/bin/bash
# The staged-tree fixture gate (user's standing rule, 2026-11-28): test fixtures and vendor
# binaries NEVER enter the pushed tree. Run it before every commit:
#
#     scripts/gate-staged.sh              # checks what is staged
#     scripts/gate-staged.sh --all        # checks every tracked file (the CI shape)
#
# Fixture shapes: the file types the env-gated suites read (camera RAW, tiled TIFF, legacy
# Word, SVG samples) and the vendor binaries built outside the repo. A file is REFUSED when it
# is NEW in the tree; the tracked exceptions below are the corpus and asset files the repo
# carries on purpose (`crates/runtime/tests/fixtures/index-photo-gray.tif`, the app icons —
# public, in-tree fixtures the pinned suites read).
#
# The companion law, pinned here because this is where it is checked: tests fetch NOTHING and
# build NOTHING — every fixture test skips with a printed note when its env is unset, and
# every build-time download is hash-pinned in scripts/vendor-checksums.json (fetch-vendor.sh
# exits when a download's bytes do not match its pin; a missing pin is an error, not permission).
set -euo pipefail
cd "$(dirname "$0")/.."

CASE="${1:-staged}"
PATTERN='\.(doc|docm|nef|cr2|arw|dng|raf|orf|rw2|srw|ptx|3fr|tif|tiff|svg)$'
BINARY='(^|/)(antiword|ffprobe-static|target-ffprobe)$'
# The tracked files that ARE allowed to carry these shapes (append when a fixture is adopted
# into the pinned corpus on purpose, with its source noted next to it).
EXEMPT='^(crates/runtime/tests/fixtures/|shell/src-tauri/assets/|website/static/img/)'

if [[ $CASE == "--all" ]]; then
  mapfile -t paths < <(git ls-files)
else
  mapfile -t paths < <(git diff --cached --name-only --diff-filter=ACM)
  mapfile -t untracked < <(git status --porcelain | awk '$1=="??" {print $2}')
  paths+=("${untracked[@]:-}")
fi

refused=()
for path in "${paths[@]}"; do
  [[ -z "$path" ]] && continue
  [[ "$path" =~ $EXEMPT ]] && continue
  if [[ "$path" =~ $PATTERN || "$path" =~ $BINARY ]]; then
    # Tracked, already in HEAD: a pre-existing corpus file, not this round's doing.
    if git cat-file -e "HEAD:$path" 2>/dev/null; then
      continue
    fi
    refused+=("$path")
  fi
done

if ((${#refused[@]} > 0)); then
  printf 'gate-staged: REFUSED — fixture/vendor shapes in the tree (fixtures live under /tmp/opencode, out of git):\n' >&2
  printf '  %s\n' "${refused[@]}" >&2
  exit 1
fi
echo "gate-staged: clean (${#paths[@]} paths checked)"
