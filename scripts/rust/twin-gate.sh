#!/bin/bash
# The port's parity gate in one command: two engines, ONE corpus, byte-compared answers.
#
#   scripts/rust/twin-gate.sh <swift-digup> <rust-digup> <corpus-folder> [--eval <testbed>]
#
# The corpus is indexed twice (once per engine) and both binaries answer the same probes;
# the answers must be byte-identical. The Rust port reproduces the engine's published
# interfaces, so this is the gate that says so: the JSON-lines verbs, the 768-dim float16
# vectors scored in float32, the 2.0σ z-gate + exact-word boost, the human-line formats
# (pad/lpad on UAX#29 grapheme clusters, the location() clock, the excerpt prefix), and the
# JSON writer's shape (2-space indent, "key" : value, "/" as "\/", uppercase \uXXXX,
# shortest-roundtrip numbers, keys sorted on write).
#
# Only measured seconds are normalized — the "ms" object the JSON replies carry, and the
# "… in 12 s" lines of the eval table — because two processes are not the same machine at
# the same moment. Strings, kinds, segments, ids, integers and hit ORDER stay exact: a
# floating-point pin demanding bit-exactness across processes passes only by seed luck (the
# engine's own z carries a 1–2 ulp noise floor), so this gate compares TEXT, and the z
# values inside it are compared as text too: they agree because the vector math agrees,
# which is the point.
#
# --eval runs the eval twin as well: the testbed is indexed ONCE (by <swift-digup>, main +
# --code) and both engines evaluate the evals/*.json suites on that same store, as the
# macOS-built testbeds do. It needs a testbed the engine's evals understand
# (scripts/rust/make-testbed-linux.py builds the Linux twin).
#
# Needs the engine's own toolchain env for BOTH binaries: the models at ModelFiles::installed()
# (or DIGUP_MODELS), DIGUP_BACKENDS at the llama libraries, DIGUP_FFMPEG at the pinned ffmpeg
# folder — the same recipe the suites measure with.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
SWIFT=${1:?usage: twin-gate.sh <swift-digup> <rust-digup> <corpus> [--eval <testbed>>}
RUST=${2:?usage: twin-gate.sh <swift-digup> <rust-digup> <corpus>}
CORPUS=${3:?usage: twin-gate.sh <swift-digup> <rust-digup> <corpus>}
MODE=${4:-}
TESTBED=${5:-}
[ -x "$SWIFT" ] && [ -x "$RUST" ] && [ -d "$CORPUS" ] || { echo "need two executables and a corpus folder"; exit 2; }
WORK=$(mktemp -d /var/tmp/digup-twin.XXXXXX)
trap 'rm -rf "$WORK"' EXIT
# The index logs outlive the run: DIGUP_GATE_LOG is a folder the caller keeps (the harness
# redirects this gate's own output there). A trap that deletes the work folder must not take
# the evidence of a failure with it — the first twin run died in `index --code` and its log
# was gone with the folder.
LOGS=${DIGUP_GATE_LOG:-$WORK}
mkdir -p "$LOGS"
export DIGUP_DEVICE=${DIGUP_DEVICE:-cpu} DIGUP_THREADS=${DIGUP_THREADS:-8}
ok=0; bad=0

index() { # index <binary> <store>
  local bin=$1 store=$2 name=$3
  mkdir -p "$store"
  nice -n 10 "$bin" index "$CORPUS" --index "$store" < /dev/null > "$LOGS/$name-main.log" 2>&1 \
    || { echo "FAIL: $bin index (main) — $LOGS/$name-main.log"; tail -12 "$LOGS/$name-main.log"; exit 1; }
  nice -n 10 "$bin" index "$CORPUS" --code --index "$store" < /dev/null >> "$LOGS/$name-code.log" 2>&1 \
    || { echo "FAIL: $bin index (code) — $LOGS/$name-code.log"; tail -12 "$LOGS/$name-code.log"; exit 1; }
}
echo "indexing the corpus twice (swift: $SWIFT, rust: $RUST)"
index "$SWIFT" "$WORK/sw" swift
index "$RUST" "$WORK/rs" rust

norm() { # measured seconds out; everything else stays exact
  # Machine-bound timings: the "ms" object of JSON replies, the "… in 12 s" duration lines,
  # and the search footer's measured milliseconds. Content stays exact: the hit lines, the
  # kinds, the vector counts, z, cosines, keyword ranks, excerpts and ids — a scoring
  # difference moves one of those, and only the stopwatch normalizes.
  # The end of the number+unit run is written as "the next character is not a letter or a
  # digit" plus an end-of-line variant, in POSIX ERE both seds understand. The shorthand `\b`
  # is GNU-sed-only and silently matches nothing on the BSD sed of macOS (and on the sed
  # Windows ships), where a footer differing only in its measured seconds was then reported as
  # a content FAIL; the shorthand `[[:>:]]` is the BSD-only mirror — GNU sed 4.8 refuses the
  # whole expression ("Invalid character class name", measured on ubuntu:22.04). A spelled-out
  # boundary is the shape that runs on all three.
  sed -E \
    -e '/"ms" : \{/,/^  \}/d' \
    -e 's/(in [0-9]+ s)([^a-zA-Z0-9])/in T s\2/g' -e 's/(in [0-9]+ s)$/in T s/' \
    -e 's/[0-9]+(\.[0-9]+)? s([^a-zA-Z0-9])/T s\2/g' -e 's/[0-9]+(\.[0-9]+)? s$/T s/' \
    -e 's/(search [0-9]+ ms)([^a-zA-Z0-9])/search T ms\2/g' -e 's/(search [0-9]+ ms)$/search T ms/'
}
probe() { # probe <label> <args…> — both engines, same store layout, byte-diffed
  local label=$1; shift
  nice -n 10 "$SWIFT" "$@" --index "$WORK/sw" < /dev/null 2>&1 | norm > "$WORK/a.txt"
  nice -n 10 "$RUST" "$@" --index "$WORK/rs" < /dev/null 2>&1 | norm > "$WORK/b.txt"
  if cmp -s "$WORK/a.txt" "$WORK/b.txt"; then
    echo "ok   $label ($(wc -l < "$WORK/a.txt") lines byte-identical)"; ok=$((ok+1))
  else
    echo "FAIL $label"; diff "$WORK/a.txt" "$WORK/b.txt" | head -12; bad=$((bad+1))
  fi
}

echo "== the search probes (human lines + --json) =="
probe "search: hits"              search penguin
probe "search: a phrase"          search campfire stories
probe "search: -n 3"              search -n 3 a list of things
probe "search: --kind doc"        search --kind doc notes
probe "search: bogus kind"        search --kind nosuchkind x
probe "search: -n 1"              search -n 1 penguin
probe "search: --zgate 1.0"       search --zgate 1.0 penguin
probe "search: --word-weight 3"   search --word-weight 3.0 penguin
probe "search: a miss"            search zzzzqqqxylophonewords
probe "search: code query"        search code: retry
probe "search: many words"        search the photo of a bird
probe "json: hits"                search --json penguin
probe "json: -n 5"                search --json -n 5 campfire stories
probe "json: --kind image"        search --json --kind image penguin
probe "json: --zgate 1.0"         search --json --zgate 1.0 penguin
probe "json: --word-weight 3"     search --json --word-weight 3.0 penguin
probe "json: a miss"              search --json zzzzqqqxylophonewords
probe "json: code query"          search --json code: gateway
probe "json: notes"               search --json notes

if [ "$MODE" = "--eval" ]; then
  echo "== the eval twin (ONE store, indexed by the reference engine) =="
  [ -d "$TESTBED" ] || { echo "FAIL: no testbed at $TESTBED"; exit 2; }
  export DIGUP_TESTBED="$TESTBED"
  store=$WORK/evals; mkdir -p "$store"
  nice -n 10 "$SWIFT" index "$TESTBED" --index "$store" < /dev/null > "$LOGS/eval-main.log" 2>&1 || { echo "FAIL: reference index of the testbed — $LOGS/eval-main.log"; tail -12 "$LOGS/eval-main.log"; exit 1; }
  nice -n 10 "$SWIFT" index "$TESTBED" --code --index "$store" < /dev/null >> "$LOGS/eval-code.log" 2>&1 || { echo "FAIL: reference code index of the testbed — $LOGS/eval-code.log"; tail -12 "$LOGS/eval-code.log"; exit 1; }
  for suite in synthetic multilingual code; do
    nice -n 10 "$SWIFT" eval "evals/$suite.json" --index "$store" < /dev/null 2>&1 | norm > "$WORK/ea.txt"
    nice -n 10 "$RUST"  eval "evals/$suite.json" --index "$store" < /dev/null 2>&1 | norm > "$WORK/eb.txt"
    if cmp -s "$WORK/ea.txt" "$WORK/eb.txt"; then
      echo "ok   eval $suite ($(wc -l < "$WORK/ea.txt") lines byte-identical)"; ok=$((ok+1))
    else
      echo "FAIL eval $suite"; diff "$WORK/ea.txt" "$WORK/eb.txt" | head -12; bad=$((bad+1))
    fi
  done
fi

echo
echo "twin gate: $ok ok / $bad FAIL"
[ "$bad" -eq 0 ] && exit 0
exit 1
