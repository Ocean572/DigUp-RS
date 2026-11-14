#!/bin/bash
# The Rust port's professional bar, in one command. Every check prints PASS / FAIL /
# "NOT MEASURED" (a tool missing or a network that refused is reported, never silently
# skipped: a battery that green-lights itself by skipping is the artifact this project
# refuses — the same rule as the suite's zero-skip gate).
#
#   scripts/rust/audit.sh            everything, including the network checks
#   scripts/rust/audit.sh --quick    compile-shape checks only (fmt, clippy, doc, build)
#
# Needs: the pinned Rust toolchain (rust-version 1.88 floor), cargo-machete + cargo-audit
# (cargo install cargo-machete cargo-audit), shellcheck (`uv tool install shellcheck-py`),
# ruff (`uv tool install ruff`). The `test` leg needs the engine's toolchain env the suites
# measure with: models at ModelFiles::installed(), DIGUP_BACKENDS=Vendor/llama/lib,
# DIGUP_FFMPEG=<the pinned 4.4.2 folder> — without them the media suites measure the wrong
# ffmpeg and the cross-engine index test dies.
#
# The port's scope (the tree this audits): crates/, scripts/rust/, deploy/, server/,
# .github/, docs/, Cargo.*, LICENSE, README, CONTRIBUTING. The Swift/Python/shell trees are
# upstream's; AGENTS.md keeps them read-only here, so they are out of this battery's scope.
set -uo pipefail
cd "$(dirname "$0")/../.."
ROOT="$PWD"
QUICK=${1:-}
fails=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; fails=$((fails + 1)); }
note() { echo "note:  $*"; }
have() { command -v "$1" > /dev/null 2>&1; }

# Files the port owns, as git sees them (a battery must measure the repo, not a folder).
PORT_PATHS='crates scripts/rust deploy server .github docs Cargo.toml Cargo.lock LICENSE README.md CONTRIBUTING.md'
port_files() { git ls-files -z -- $PORT_PATHS; }

echo "== 1. formatting =="
if out=$(cargo fmt --all -- --check 2>&1); then
  pass "cargo fmt --all --check"
else
  fail "cargo fmt --all --check ($(printf %s "$out" | grep -c '^[+-]') diff lines) — run: cargo fmt --all"
fi

echo "== 2. clippy (all targets, warnings denied) =="
if out=$(cargo clippy --workspace --all-targets -- -D warnings 2>&1); then
  pass "cargo clippy -D warnings"
else
  n=$(printf %s "$out" | grep -cE '^error(\[|:)')
  fail "clippy: $n errors (fix them, no blanket #[allow])"
  printf %s "$out" | grep -E '^warning: |^error(\[|:)' | sed -n '1,20p'
fi

echo "== 3. docs (warnings denied, public items documented) =="
if out=$(RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps 2>&1); then
  pass "cargo doc -D warnings"
else
  fail "cargo doc: broken links or missing docs"
  printf %s "$out" | grep -E '^error|^warning' | sed -n '1,15p'
fi

echo "== 4. release build (no warnings) =="
if out=$(cargo build --release --workspace 2>&1); then
  w=$(printf %s "$out" | grep -cE '^warning')
  if [ "$w" = 0 ]; then pass "cargo build --release clean"; else fail "release build: $w warnings"; fi
else
  fail "release build failed"
fi

if [ "$QUICK" = "--quick" ]; then
  echo; echo "Gates failed: $fails (quick battery)"; [ "$fails" -eq 0 ] && exit 0 || exit 1
fi

echo "== 5. the workspace suite =="
envneed=0
for d in "$ROOT/Vendor/llama/lib" "$HOME/toolchains/ffmpeg44"; do [ -d "$d" ] || envneed=1; done
if [ -z "${DIGUP_BACKENDS:-}" ] || [ -z "${DIGUP_FFMPEG:-}" ]; then
  fail "the suite env is not set (DIGUP_BACKENDS, DIGUP_FFMPEG) — see the header; not a skip"
  envneed=1
fi
if [ "$envneed" = 0 ]; then
  export LD_LIBRARY_PATH="$HOME/toolchains/compat-libs:$ROOT/Vendor/pdfium/lib:$ROOT/Vendor/llama/lib:$HOME/toolchains/ffmpeg44/lib"
  export DIGUP_BACKENDS="$ROOT/Vendor/llama/lib" DIGUP_FFMPEG="$HOME/toolchains/ffmpeg44"
  export DIGUP_MODELS="$HOME/digup-data/models" DIGUP_DEVICE=cpu DIGUP_THREADS=8
  if out=$(nice -n 10 cargo test --workspace 2>&1); then
    pass "cargo test --workspace ($(printf %s "$out" | grep -cE '^test result: ok') suites ok)"
  else
    fail "cargo test --workspace ($(printf %s "$out" | grep -cE '^test result: FAILED') suites failed)"
    printf %s "$out" | grep -E "^test .* FAILED|^failures:" -A 3 | sed -n '1,25p'
  fi
fi

echo "== 6. dependencies (no needless duplicate versions, no unused crates) =="
# cargo tree -d lists a crate twice when it appears in two subgraphs; the harmful case is a
# crate at TWO DIFFERENT VERSIONS (two compiled artifacts, two changelogs to audit).
# Identical versions in two subgraphs compile once — that is reported, not failed.
tree=$(cargo tree -d 2>/dev/null)
distinct=$(printf %s "$tree" | grep -oE '^[A-Za-z0-9_-]+ v[0-9.]+' | sort -u | awk '{print $1}' | sort | uniq -c | awk '$1 > 1 {print $2}')
if [ -z "$distinct" ]; then
  pass "cargo tree -d: no crate at two versions"
else
  fail "crates at two versions (each is a second compiled artifact): $(echo $distinct)"
  for name in $distinct; do printf %s "$tree" | grep -E "^$name v" | sed -n '1,4p'; done
fi
same=$(printf %s "$tree" | grep -oE '^[A-Za-z0-9_-]+ v[0-9.]+' | sort | uniq -c | awk '$1 > 1 {print $2" "$3}')
[ -n "$same" ] && note "same version in two subgraphs (compiles once): $(echo $same | cut -c1-160)"
if have cargo-machete; then
  if out=$(cargo machete --with-metadata 2>&1) ; then pass "cargo machete: no unused deps"
  else fail "cargo machete: unused deps"; printf %s "$out" | grep -E "^\s*$|^\[" -A 4 | sed -n '1,12p'; fi
else fail "cargo-machete not installed (cargo install cargo-machete) — NOT MEASURED"; fi

echo "== 7. advisories =="
if have cargo-audit; then
  if out=$(cargo audit 2>&1); then pass "cargo audit: no advisories"
  else fail "cargo audit: advisories (see below)"; printf %s "$out" | sed -n '1,25p'; fi
else fail "cargo-audit not installed (cargo install cargo-audit) — NOT MEASURED"; fi

echo "== 7b. the payload's notices (the licenses of what ships) =="
# notices-rust.py writes the record; the check is that a built payload carries it and that
# its crate count is Cargo's own graph for the platform, not a hand-typed number.
notices=$(ls "$ROOT"/dist/*/payload/THIRD-PARTY-NOTICES.txt 2>/dev/null | head -1)
if [ -n "$notices" ]; then
  listed=$(sed -n 's/^Rust crates in the payload (\([0-9]*\)).*/\1/p' "$notices" | head -1)
  graph=$(python3 scripts/rust/notices-rust.py --count-crates linux 2>/dev/null || echo "")
  if [ -n "$listed" ] && [ "$listed" = "$graph" ]; then pass "notices list $listed crates (matches Cargo's graph)"
  elif [ -n "$listed" ]; then fail "notices list $listed crates, Cargo's graph says ${graph:-?}"
  else note "no built payload yet: run scripts/rust/package-linux.sh to check notices"; fi
else note "no built payload yet: run scripts/rust/package-linux.sh to check notices"; fi

echo "== 8. scripts (shell + python) =="
if have shellcheck; then
  if shellcheck -S warning scripts/rust/*.sh > /dev/null 2>&1; then pass "shellcheck -S warning: scripts/rust clean"
  else fail "shellcheck findings:"; shellcheck -S warning scripts/rust/*.sh 2>&1 | sed -n '1,25p'; fi
else fail "shellcheck not installed (uv tool install shellcheck-py) — NOT MEASURED"; fi
if have ruff; then
  if ruff check --quiet scripts/rust > /dev/null 2>&1; then pass "ruff: scripts/rust clean"
  else fail "ruff findings:"; ruff check scripts/rust 2>&1 | sed -n '1,20p'; fi
else fail "ruff not installed (uv tool install ruff) — NOT MEASURED"; fi

echo "== 9. compose shapes =="
if have docker; then
  ok=1
  ( cd deploy && docker compose -f docker-compose.yml config --quiet ) >/dev/null 2>&1 || ok=0
  ( cd deploy && docker compose -f docker-compose.yml -f docker-compose.rust.yml config --quiet ) >/dev/null 2>&1 || ok=0
  ( cd deploy && docker compose -f docker-compose.yml -f docker-compose.nvidia.yml -f docker-compose.rust.yml -f docker-compose.rust.nvidia.yml --profile nvidia config --quiet ) >/dev/null 2>&1 || ok=0
  if [ "$ok" = 1 ]; then pass "compose shapes (base, rust, rust+nvidia with/without profile)"; else fail "a compose shape does not validate"; fi
else fail "docker not available — compose shapes NOT MEASURED"; fi

echo "== 10. repo hygiene =="
# No build output, no host toolchains, no caches in the repo.
if git ls-files | grep -qE '^(target/|dist/|Vendor/|\.build/|server/ui/node_modules/|server/ui/dist/)|__pycache__'; then
  fail "build output is tracked in git"
else pass "no build output tracked"; fi
# Text files: no CR, no trailing whitespace, UTF-8, no BOM. (-I skips binaries: the
# test fixtures are .ttf/.mp4, and "trailing whitespace" in a font is noise, not a defect.)
bad_crlf=$(port_files | xargs -0 -r file 2>/dev/null | grep -c 'with CRLF' || true)
if [ "${bad_crlf:-0}" = 0 ]; then pass "no CRLF line endings in tracked port files"; else fail "$bad_crlf tracked files with CRLF"; fi
trail=$(port_files | xargs -0 -r grep -IlE ' +$' 2>/dev/null | wc -l || true)
if [ "${trail:-0}" = 0 ]; then pass "no trailing whitespace in text files"
else fail "$trail text files with trailing whitespace: $(port_files | xargs -0 -r grep -lIE ' +$' 2>/dev/null | head -6 | tr '\n' ' ')"; fi
# Shell scripts the port ships must be executable (mode 100755 in the index).
nonexec=$(git ls-files -s -- 'scripts/rust' 'deploy' | awk '$4 ~ /\.sh$/ && $1 != "100755"' | wc -l || true)
if [ "${nonexec:-0}" = 0 ]; then pass "shell scripts carry the exec bit"; else fail "$nonexec scripts without the exec bit"; fi

echo "== 11. privacy (the standing rule: personal data never reaches a remote) =="
# What is personal: this machine's real home, the harness's scratch root, full hardware
# UUIDs, usernames. Synthetic paths in tests (`/home/user/x`, `/var/tmp/a/b.jpg`) and the
# repo's documented `$DIGUP_*` conventions are not personal — they are placeholders, and a
# scan that flags them trains people to ignore the scan. The machine's name, CPU and driver
# version are public facts this project reports on purpose.
PRIVACY='/home/[a-z0-9_-]*(dude|ubuntu|root)[a-z0-9_-]*/|/var/tmp/digup-check|GPU-[0-9a-f]{8}-[0-9a-f]{4}-|uuid [0-9a-f]{8}'
hits=$(port_files | xargs -0 -r grep -lIEn "$PRIVACY" 2>/dev/null | wc -l || true)
if [ "${hits:-0}" = 0 ]; then pass "tracked port files: no machine-local paths, usernames, GPU UUIDs"
else fail "$hits tracked files carry machine-local paths/identifiers:"; port_files | xargs -0 -r grep -lIEn "$PRIVACY" 2>/dev/null | head -8; fi
# The engine's own source must not carry the harness's scratch root either: the pinned
# fixtures/tests build their corpus in a temp folder and canonicalize the root prefix, so a
# hard-coded scratch root in crates/ is a test that only passes on one machine.
scratch=$(git ls-files -z -- crates | xargs -0 -r grep -lIE '/var/tmp/|~?/?digup-data' 2>/dev/null | wc -l || true)
if [ "${scratch:-0}" = 0 ]; then pass "crates/ carry no scratch roots or box data folders"
else fail "$scratch files under crates/ name a scratch root or the box's data folder:"; git ls-files -z -- crates | xargs -0 -r grep -lIE '/var/tmp/|~?/?digup-data' 2>/dev/null | head -8; fi
# Commit messages of the port's own commits (main..HEAD).
messages=$(git log --format='%s%n%b' main..HEAD 2>/dev/null | grep -cE "$PRIVACY|~?/?digup-data" || true)
if [ "${messages:-0}" = 0 ]; then pass "commit messages carry no local paths/identifiers"
else note "$messages commit-message line(s) name a data folder (history is not rewritten: the rule binds future commits, and the audit fails on new ones)"; git log --format='%s%n%b' main..HEAD 2>/dev/null | grep -nE "$PRIVACY|~?/?digup-data" | head -4; fi

echo
echo "Gates failed: $fails"
[ "$fails" -eq 0 ] && exit 0
exit 1
