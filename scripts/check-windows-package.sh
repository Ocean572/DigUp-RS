#!/bin/bash
# Checks DigUp's Windows installer the way someone first meets it: installed silently for the current user, a hidden
# first launch goes through setup by itself (the model from MODELS, downloaded there when it isn't), indexes a picture,
# a screenshot, a recording and a note with the model and finds the note from the search panel; then its helper runs
# with only Windows on PATH (everything else it needs is beside it). In Git for Windows' bash; the packages workflow
# runs it after building the installer.
#   MODELS=<folder> scripts/check-windows-package.sh <DigUp_x.y.z_x64-setup.exe>
set -euo pipefail
cd "$(dirname "$0")/.."
setup=${1:?the installer}
models=${MODELS:?set MODELS to a folder for the model files (they are downloaded there when missing)}
mkdir -p "$models"
models=$(cygpath -m "$models")
work=$(mktemp -d /d/a/_temp/digup-gate.XXXXXX)
# A LONG path on purpose: MSYS mktemp lands under /tmp, which maps to C:\Users\RUNNER~1 —
# the 8.3 short form. The store's rows keep the root's short form while its scope lookups
# canonicalize to the long one: the daemon's second sync of the same files then sees them
# as new ('UNIQUE constraint failed: files.path') and the counters' scope condition
# matches nothing (pending:0 on a fresh index with five rows added — measured 2026-11-19,
# daemon-probe events), so the daemon embeds nothing while the one-shot embeds fine.
trap 'rm -rf "$work"' EXIT
win() { cygpath -m "$1"; }

echo "installing $setup"
MSYS_NO_PATHCONV=1 "$setup" /S
installed=$(cygpath -u "$LOCALAPPDATA")/DigUp
ls "$installed/DigUp.exe" "$installed/helper/digup.exe" > /dev/null
# The licenses of what the installer carries, beside the app.
grep -q "GNU LESSER GENERAL PUBLIC LICENSE" "$installed/THIRD-PARTY-NOTICES.txt" || { echo "no license notices"; exit 1; }

mkdir -p "$work/files"
cp crates/runtime/tests/fixtures/reference/photo.jpg crates/runtime/tests/fixtures/reference/screen.png \
  crates/runtime/tests/fixtures/reference/speech.wav docs/logo.png "$work/files/"
cat > "$work/files/recipe.txt" <<'TEXT'
Lemon cake

Cream the butter and sugar, beat in three eggs, then fold in the flour, the zest of two lemons and a splash of milk.
Bake for forty minutes at 180 degrees and pour the lemon syrup over it while it is still warm.
TEXT

echo "first launch"
started=$(date +%s)
"$installed/DigUp.exe" --index "$(win "$work/index")" --models "$models" --settings "$(win "$work/settings.json")" \
  --offer "$(win "$work/files")" --hotkey "Ctrl+Alt+Shift+D" --debug-setup --debug-query "how long to bake the cake" \
  --debug-quit > "$work/out.txt" 2> "$work/err.txt" &
app=$!
report=
probe=$(( $(date +%s) + 30 ))
while kill -0 "$app" 2> /dev/null && (( $(date +%s) - started < 1800 )); do
  report=$(grep '^{' "$work/out.txt" 2> /dev/null | tail -1 || true)
  [[ -n $report ]] && break
  # The store's counters while embeds run, every 30 s: their growth separates an embed
  # that is slow on this machine's cores from one that is stuck (measured 2026-11-18:
  # a 0-vector report at 52 s with no growth curve behind it).
  if (( $(date +%s) >= probe )); then
    probe=$(( $(date +%s) + 30 ))
    echo "t=$(( $(date +%s) - started )) s:"
    "$installed/helper/digup.exe" status --index "$(win "$work/index")" 2>&1 | grep -E '^vectors:|^  [a-z]' | head -8 || true
  fi
  sleep 2
done
# The report comes just before DigUp quits; a DigUp still there a minute later is ended (a session without a desktop,
# as a service has, keeps it from quitting).
for _ in $(seq 30); do kill -0 "$app" 2> /dev/null || break; sleep 2; done
if kill -0 "$app" 2> /dev/null; then
  echo "DigUp didn't quit after its report: ending it"
  taskkill //F //T //IM DigUp.exe > /dev/null || true
fi
report=$(grep '^{' "$work/out.txt" | tail -1 || true)
echo "after $(( $(date +%s) - started )) s"
if [[ -z $report ]]; then
  echo "--- stdout"; tail -20 "$work/out.txt"
  echo "--- stderr"; tail -40 "$work/err.txt"
  echo "no report from the panel"
  exit 1
fi
echo "panel: $report"
if [[ $report != *'"recipe.txt"'* ]]; then
  echo "the panel didn't find recipe.txt"
  # The facts the gate owes a red run: the shell's index events (its stdout carries the
  # helper's "index:" lines) and the store's counters — which files made vectors, which
  # failed, and what the setup flow saw. (Measured 2026-11-18: a 0-hit report at 54 s
  # with no evidence behind it.)
  echo "--- shell stdout (index events)"; tail -40 "$work/out.txt"
  echo "--- shell stderr"; tail -40 "$work/err.txt"
  echo "--- index folder"; ls -la "$work/index" 2>/dev/null | head -12
  echo "--- worker.log"; tail -40 "$work/index/worker.log" 2>/dev/null || true
  echo "--- helper status"; "$installed/helper/digup.exe" status --index "$(win "$work/index")" 2>&1 | tail -12 || true
  # The panel fires its report at the crawl queue's drain and races the embeds; the
  # helper's one-shot index embeds every file before it answers, in the foreground,
  # with no shell and no panel in the way. Slow embeds finish here; a refused embedder
  # speaks in worker.log here. The payload listing shows the DLLs the worker imports
  # (measured 2026-11-18: worker ready in 1.84 s with 0 vectors — the embed step is
  # the open question, not the load).
  echo "--- one-shot index (helper embeds inline, no shell, no panel)"
  PATH="$installed/helper:/c/Windows/System32:/c/Windows" DIGUP_MODELS="$models" \
    "$installed/helper/digup.exe" index --index "$(win "$work/index")" "$(win "$work/files")" \
    > "$work/oneshot.txt" 2>&1 || echo "one-shot exit $?"
  tail -40 "$work/oneshot.txt"
  echo "--- worker.log after the one-shot"; tail -40 "$work/index/worker.log" 2>/dev/null || true
  echo "--- helper status after the one-shot"; "$installed/helper/digup.exe" status --index "$(win "$work/index")" 2>&1 | tail -12 || true
  echo "--- payload files"; ls "$installed/helper" | head -40
  # The daemon's own words on a fresh index: index-service speaks one JSON event per
  # line on stdout (crawl, model, embed progress). stdin stays OPEN for 240 s: EOF
  # means stop, and the /dev/null version stopped the daemon before its embed step
  # (measured 2026-11-19: sync — exit, no embed events). DIGUP_WORKER_DIR mirrors
  # what the shell's own daemon run sees.
  echo "--- daemon probe (index-service standalone, fresh index, live stdin 240 s)"
  PATH="$installed/helper:/c/Windows/System32:/c/Windows" DIGUP_MODELS="$models" DIGUP_WORKER_DIR="$installed/helper" \
    "$installed/helper/digup.exe" index-service --index "$(win "$work/index2")" "$(win "$work/files")" \
    --linger 150 < <(echo sync; sleep 240) > "$work/daemon.txt" 2> "$work/daemonerr.txt" || echo "daemon exit $?"
  head -40 "$work/daemon.txt" 2>/dev/null
  echo "--- daemon stderr"; tail -20 "$work/daemonerr.txt" 2>/dev/null || true
  echo "--- daemon worker.log"; tail -20 "$(win "$work/index2")/worker.log" 2>/dev/null || true
  echo "--- daemon index folder"; ls -la "$(win "$work/index2")" 2>/dev/null | head -12 || true
  exit 1
fi

echo "the helper with only Windows on PATH"
helper=$installed/helper/digup.exe
PATH="/c/Windows/System32:/c/Windows:/c/Windows/System32/Wbem" "$helper" status --index "$(win "$work/index")" \
  | tee "$work/status.txt"
grep -Eq '^vectors: 5 in 5 segments' "$work/status.txt" || { echo "not every file has a vector"; exit 1; }
PATH="/c/Windows/System32:/c/Windows:/c/Windows/System32/Wbem" DIGUP_MODELS="$models" \
  "$helper" search "how long to bake the cake" --index "$(win "$work/index")" -n 3
echo "ok"
