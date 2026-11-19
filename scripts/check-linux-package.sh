#!/bin/bash
# Checks an installed DigUp for Linux the way someone first meets it, without a desktop: the shell and its helper find
# every library they need, and a hidden first launch under a virtual screen (Xvfb) with a session bus goes through setup
# by itself (the model from MODELS, downloaded there when it isn't), indexes a picture, a screenshot, a recording and a
# note with the model, and finds the note from the search panel; launched again, the panel shows the recording, whose
# preview starts WebKit's media player. The packages workflow runs it on each distribution on the AppImage, then on the
# .deb once installed.
#   MODELS=<folder> scripts/check-linux-package.sh [/usr/bin/digup-shell | <DigUp.AppImage>]
# Needs xvfb-run (xvfb, xauth) and dbus-run-session (dbus).
set -euo pipefail
cd "$(dirname "$0")/.."
app=$(realpath "${1:-/usr/bin/digup-shell}")
models=${MODELS:?set MODELS to a folder for the model files (they are downloaded there when missing)}
mkdir -p "$models"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [[ $app == *.AppImage ]]; then
  # A container has no FUSE to mount an AppImage, and the auto-extraction cleans its
  # /tmp/.mount_* root away when the app exits — the red-path facts must not depend on
  # it (measured 2026-11-18: the roots dump printed nothing for exactly that reason).
  # Unpack here, launch the AppRun the AppImage itself would run, probe the payload
  # from a stable path — and run the .deb's missing-libraries check over it (the
  # AppImage gate never had one).
  export APPIMAGE_EXTRACT_AND_RUN=1
  (cd "$work" && "$app" --appimage-extract > "$work/extract.log" 2>&1) ||
    { echo "appimage --appimage-extract failed"; tail -20 "$work/extract.log"; exit 1; }
  extract=$work/squashfs-root
  [[ -x $extract/AppRun ]] || { echo "no AppRun in the extracted root"; find "$extract" -maxdepth 2 | head -20; exit 1; }
  app=$extract/AppRun
  helper=$extract/usr/lib/DigUp/helper/digup
  [[ -f $helper ]] || helper=
  missing=$(for file in $(find "$extract/usr/lib" -type f ! -L \( -name '*.so*' -o -name 'digup*' \) 2>/dev/null | head -60); do
    # ffmpeg and ffprobe are static: ldd has nothing to say about them. The Vulkan backend is loaded only where
    # Vulkan is (the package recommends it).
    ldd "$file" 2>&1 | grep "not found" | grep -v "libvulkan.so.1" | sed "s|^|$file: |"
  done || true)
  if [[ -n $missing ]]; then
    echo "missing libraries in the AppImage payload:"
    echo "$missing"
    exit 1
  fi
else
  helper=$(dirname "$app")/../lib/DigUp/helper/digup
  missing=$(for file in "$app" "$(dirname "$helper")"/*; do
    # ffmpeg and ffprobe are static: ldd has nothing to say about them. The Vulkan backend is loaded only where
    # Vulkan is (the package recommends it).
    [[ -f $file && ! -L $file ]] && ldd "$file" 2>&1 | grep "not found" | grep -v "libvulkan.so.1" | sed "s|^|$file: |"
  done || true)
  if [[ -n $missing ]]; then
    echo "missing libraries:"
    echo "$missing"
    exit 1
  fi
  # The licenses of what the package carries: beside the app, and where Debian keeps a package's.
  for notices in "$(dirname "$app")/../lib/DigUp/THIRD-PARTY-NOTICES.txt" "$(dirname "$app")/../share/doc/dig-up/copyright"; do
    grep -q "GNU LESSER GENERAL PUBLIC LICENSE" "$notices" || { echo "no license notices in $notices"; exit 1; }
  done
fi

mkdir -p "$work/files"
cp crates/runtime/tests/fixtures/reference/photo.jpg crates/runtime/tests/fixtures/reference/screen.png \
  crates/runtime/tests/fixtures/reference/speech.wav docs/logo.png "$work/files/"
cat > "$work/files/recipe.txt" <<'TEXT'
Lemon cake

Cream the butter and sugar, beat in three eggs, then fold in the flour, the zest of two lemons and a splash of milk.
Bake for forty minutes at 180 degrees and pour the lemon syrup over it while it is still warm.
TEXT

# Launches the app hidden with these arguments until the panel reports what it showed ($report), for at most $2 s.
# While it is still running at 120 s with no report, prints the facts of the moment — the store rows, the daemon's
# own log, its counters — with the daemon alive; a dump after the app exits is a dump after the embed pipeline died
# with it (measured 2026-11-18).
launch() {
  local name=$1 seconds=$2 started status=0 pid probed=0
  shift 2
  started=$(date +%s)
  timeout "$seconds" xvfb-run -a -s "-screen 0 1280x800x24" dbus-run-session -- "$app" --index "$work/index" \
    --models "$models" --settings "$work/settings.json" "$@" --debug-quit > "$work/$name.out" 2> "$work/$name.err" &
  pid=$!
  while kill -0 "$pid" 2> /dev/null; do
    local now=$(( $(date +%s) - started ))
    if [[ $probed -eq 0 && $now -ge 120 && -n ${helper:-} ]]; then
      probed=1
      echo "--- live at $now s (no report yet)"
      echo "--- live index folder"; ls -la "$work/index" 2>/dev/null | head -12
      echo "--- live worker.log"; tail -30 "$work/index/worker.log" 2>/dev/null || true
      echo "--- live helper status"; timeout 60 "$helper" status --index "$work/index" 2>&1 | head -12 || true
    fi
    sleep 5
  done
  wait "$pid" || status=$?
  echo "exit $status after $(( $(date +%s) - started )) s"
  report=$(grep '^{' "$work/$name.out" | tail -1 || true)
  if [[ $status != 0 || -z $report ]]; then
    echo "--- stdout"; tail -20 "$work/$name.out"
    echo "--- stderr"; tail -40 "$work/$name.err"
    # The facts the daemon keeps for exactly this moment: its log rides beside the
    # index (worker.log — the engine's own lines, embedder included) and the store's
    # rows. The engine opens worker.log only when its embedder opens: a missing file
    # is an embedder that never opened, and the one-shot below is where it must say why.
    echo "--- index folder"; ls -la "$work/index" 2>/dev/null | head -12
    echo "--- worker.log"; tail -40 "$work/index/worker.log" 2>/dev/null || true
    if [[ -n ${helper:-} ]]; then
      # The panel fires its report at the crawl queue's drain and races the embeds;
      # the helper's one-shot index embeds every file before it answers, in the
      # foreground, with no shell and no panel in the way. Slow embeds finish here;
      # a refused embedder speaks here.
      echo "--- one-shot index (helper embeds inline, no shell, no panel)"
      DIGUP_MODELS="$models" timeout 900 "$helper" index --index "$work/index" "$work/files" \
        > "$work/oneshot.out" 2>&1 || echo "one-shot exit $?"
      tail -40 "$work/oneshot.out"
      echo "--- worker.log after the one-shot"; tail -40 "$work/index/worker.log" 2>/dev/null || true
      echo "--- status after the one-shot"; "$helper" status --index "$work/index" 2>&1 | head -12 || true
      echo "--- payload files (helper folder, then its parent)"
      ls -la "$(dirname "$(dirname "$helper")")" 2>/dev/null | head -30
      ls -la "$(dirname "$helper")" | head -30
      # The daemon's own words on a fresh index: index-service speaks one JSON event
      # per line on stdout (crawl, model, embed progress). stdin stays OPEN for 240 s
      # (EOF means stop, and an /dev/null stdin stopped this probe before the embed
      # step ran — measured 2026-11-19: sync, then exit, no embed events).
      echo "--- daemon probe (index-service standalone, fresh index, live stdin 240 s)"
      DIGUP_MODELS="$models" timeout 300 "$helper" index-service --index "$work/index2" "$work/files" --linger 150 \
        < <(echo sync; sleep 240) > "$work/daemon.out" 2> "$work/daemon.err" || echo "daemon exit $?"
      head -40 "$work/daemon.out" 2>/dev/null
      echo "--- daemon stderr"; tail -20 "$work/daemon.err" 2>/dev/null
      echo "--- daemon worker.log"; tail -20 "$work/index2/worker.log" 2>/dev/null || true
      echo "--- daemon index folder"; ls -la "$work/index2" 2>/dev/null | head -12
    fi
    echo "no report from the panel"
    exit 1
  fi
  echo "panel: $report"
}

echo "first launch: $app"
launch first 1800 --offer "$work/files" --hotkey "Ctrl+Alt+Shift+D" --debug-setup \
  --debug-query "how long to bake the cake"
if [[ $report != *'"recipe.txt"'* ]]; then
  echo "the panel didn't find recipe.txt"
  exit 1
fi
if [[ -n $helper ]]; then
  "$helper" status --index "$work/index" | tee "$work/status.txt"
  # Every file has its vector: the pictures (with the logo), the recording and the note.
  grep -Eq '^vectors: 5 in 5 segments' "$work/status.txt" || { echo "not every file has a vector"; exit 1; }
fi

# The recording's preview starts WebKit's media player, on GStreamer (the AppImage's own, the system's for the .deb):
# without the plugins it asks for, WebKit's page dies and the panel stops answering. (The preview itself doesn't load:
# WebKit's player turns down the files' asset:// addresses.)
echo "second launch: the recording's preview"
launch second 300 --debug-query "speech"
if [[ $report != *'"selected":"speech.wav"'* || $report != *'"tag":"audio"'* ]]; then
  echo "the panel didn't show the recording"
  exit 1
fi
if grep -h "GStreamer element .* not found\|Failed to load plugin" "$work/second.out" "$work/second.err"; then
  echo "GStreamer is missing a plugin or a library"
  exit 1
fi
echo "ok"
