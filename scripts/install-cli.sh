#!/bin/bash
# Installs the `digup` CLI to ~/.local/bin/digup and installs the `digup` skill for Claude Code and Google Antigravity.
# macOS and Linux build it from this checkout (cargo, the Rust workspace; on Linux the binary finds llama.cpp and
# PDFium through the Vendor/ rpath baked into the build, so keep the checkout where it is — the payload form, the
# libraries beside the binary under a $ORIGIN rpath, is scripts/rust/package-linux.sh's job).
# On Windows, run scripts/install-cli.ps1 instead: it puts the installed app's digup.exe on PATH.
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s)" in
  Darwin)
    if [[ -z "${DEVELOPER_DIR:-}" && -d /Applications/Xcode.app ]]; then
      export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
    fi
    ;;
  Linux)
    scripts/fetch-vendor.sh   # SQLite, stb, miniz and PDFium (macOS has Apple's frameworks instead)
    ;;
  MINGW*|MSYS*|CYGWIN*)
    echo "On Windows, run: powershell -ExecutionPolicy Bypass -File scripts\\install-cli.ps1" >&2
    exit 1
    ;;
esac

scripts/build-llama.sh

# The Swift-era build baked the Vendor rpath with Package.swift's `-Xlinker -rpath`; the Rust
# build.rs rpath covers only the runtime crate's own targets (AGENTS.md), so the installed copy
# gets its rpath from the link flags here. A missing folder (macOS' pdfium: Apple's PDFKit) is
# simply a path the loader never finds.
export RUSTFLAGS="-C link-arg=-Wl,-rpath,$PWD/Vendor/llama/lib -C link-arg=-Wl,-rpath,$PWD/Vendor/pdfium/lib"
cargo build --release -p digup-cli --bin digup

mkdir -p "$HOME/.local/bin"
cp -f target/release/digup "$HOME/.local/bin/digup"
if [[ "$(uname -s)" == Darwin ]]; then
  codesign --force --sign - "$HOME/.local/bin/digup" >/dev/null 2>&1 || true
fi
echo "Installed CLI: $HOME/.local/bin/digup"
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) echo "Add ~/.local/bin to PATH to run it as \`digup\`." ;;
esac

SKILL_SRC=".agents/skills/digup/SKILL.md"
if [[ -f "$SKILL_SRC" ]]; then
  for dest in \
    ".claude/skills/digup" \
    "$HOME/.claude/skills/digup" \
    "$HOME/.gemini/config/skills/digup"; do
    mkdir -p "$dest"
    cp -f "$SKILL_SRC" "$dest/SKILL.md"
    echo "Installed skill: $dest/SKILL.md"
  done
fi
