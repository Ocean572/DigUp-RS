# Rust port — code audit

One document: the audit of the Rust port (`crates/`, `scripts/rust/`, `deploy/`, the Rust
stages of `server/docker/Dockerfile`, `.github/workflows/rust*.yml`, `Cargo.*`). Baseline tag:
**`audit-base`**. Scope set by the user (2026-11-13): *the Rust port and what it ships* — the
Swift/Python/shell trees are upstream's, read-only here (AGENTS.md), and appear in this
document only as the reference the parity is measured against.

## The bar

`scripts/rust/audit.sh` runs the battery and exits non-zero on any failure: `cargo fmt`,
`cargo clippy --all-targets -D warnings`, `cargo doc -D warnings`, a warning-free release
build, `cargo test --workspace`, no duplicate dependency versions, `cargo machete`,
`cargo audit`, the payload's notices crate count against Cargo's own graph, `shellcheck`,
`ruff`, the four compose shapes, repo hygiene (no build output tracked, no CRLF, no trailing
whitespace, exec bits), and the standing privacy rule (no local paths, usernames, GPU UUIDs in
tracked files or commit messages). A missing tool prints **NOT MEASURED** and fails the run:
a battery that green-lights itself by skipping is the artifact this project refuses.

Parity is measured, not asserted: `scripts/rust/twin-gate.sh <swift> <rust> <corpus>
[--eval <testbed>]` indexes one corpus twice (once per engine) and byte-compares the answers
of 19 search probes (11 human lines + 8 `--json`, incl. `--zgate`, `--word-weight`, `-n`, a
bogus `--kind`, a miss, a `code:` query) and the three eval suites on one store. Only
measured seconds are normalized; strings, kinds, segments, ids, integers, hit order and the z
values inside them stay exact.

## Baseline (measured at `audit-base`)

```
workspace            97 .rs files, 35,639 lines, 7 crates (core 13,064 · server 6,160 ·
                     native 5,591 · runtime 5,093 · services 3,167 · cli 2,557 · platform 7)
rustfmt              2,734 diff lines  → fixed in adc1605 (25 files, +858/−424)
clippy               warnings in 5 targets (chunks_exact_to_as_chunks and friends)
dependencies         154 lines of duplicate-version entries (cargo tree -d)
counts               196 unwrap() · 370 expect( · 19 panic! · 136 unsafe · 0 #[allow]
                     0 TODO/FIXME · 2,165 doc-comment lines of 4,102 comment lines
```

## The revision discipline

One class of change per commit, each with its gate line: ① `cargo fmt` → ② clippy → ③ dead
code → ④ dedup/behavior-preserving refactor → ⑤ bug fixes (each with a test that fails
before, passes after) → ⑥ comments/docs → ⑦ repo shape. Gates per commit: the workspace
suite + the twin gate byte-identical; checkpoint gates: the contract suite (0 failed,
0 skipped) and the image gates (CPU + CUDA, SIGTERM 143). No cleanup may move a contract
byte — the twin gate is the proof.

Weak areas are **rewritten in place** (user's instruction), not decorated: when a module's
structure is the defect, the fix restructures it and re-runs the parity gates.

## Findings

Severity: `critical` (contract byte, data loss, crash, security) · `major` (real defect in an
edge path, drift, deadlock, unbounded cost) · `minor` (defect with a bounded effect) ·
`polish` (craft, redundancy, naming, slop). Disposition: `fixed`, `fixed+test`,
`kept-by-design` (with the reason), `not-a-bug` (the reviewer's claim did not reproduce).

Every finding below was verified against the code by me before a diff landed; independent
reviews (fresh-eyes reviewers, given the Swift anchors) are the source of most rows.

| # | sev | file:line | finding | disposition |
|---|-----|-----------|---------|-------------|
| 1 | critical | crates/cli/src/main.rs:737 (at `audit-base`) | `index --code` opened `index.sqlite`, so a code run into a folder holding a media index refused itself on the media fingerprint (exit 1: "index: … photo140 … now: … code1 lines1800") | fixed+test — `Arguments::store_file()` mirrors `Arguments.store()` (main.swift:101-103); `crates/cli/tests/cli.rs` runs the verbs against the binary (fails before: exit 1; passes after: exit 0, rows in `code.sqlite`), `crates/cli/src/main.rs` unit-pins the flag→store map. Found by `scripts/rust/twin-gate.sh` |
| 2 | major | crates/cli/src/main.rs:867 | `status --code` read `index.sqlite` where `status` (main.swift:403) reads `args.store()` — the wrong store's vectors, kinds and size reported | fixed (same helper; the integration test pins both `status` shapes) |
| 3 | polish | scripts/rust/twin-gate.sh | the first run normalized JSON `"ms"` objects and `"N s"` durations, not the search footer's measured milliseconds: 10 probes failed on `search 1 ms` vs `search 0 ms` alone (Swift measures, Rust is faster on 3 vectors) | fixed — footer ms normalizes; content lines, z, cosines, keyword ranks, excerpts, ids stay exact, which is what makes a scoring difference fail the gate |
| 4 | polish | scripts/rust/twin-gate.sh | a `mktemp -d` trap deleted the index logs a failing run had just written — the evidence of the failure vanished with the work folder | fixed — `DIGUP_GATE_LOG` keeps the index logs in the folder the caller chose, and a failed index prints its last 12 lines |
| 5 | minor | scripts/rust/audit.sh | the hygiene legs were self-deceiving: `if [ -f … ] \|\| true` (a guard that cannot be false), `git ls-files -s … \| grep -vc '^100755'` (mode is field 1, path is field 4 — the exec-bit leg could only pass), trailing-whitespace scan on binary fixtures (a `.ttf` "has" 1,695 trailing blanks), and a privacy scan that flagged synthetic test paths (`/home/user/x`, `/var/tmp/a/b.jpg`) and the repo's documented `$DIGUP_*` conventions | fixed — each leg rewritten; a scan that cries wolf trains people to ignore it, so the patterns name *machine-local* facts (real home, harness scratch root, full GPU UUIDs), not placeholders |
| 6 | minor | crates/platform/Cargo.toml, crates/services/Cargo.toml | unused dependencies: `digup-core` in a doc-only scaffold crate, `serde` where only `serde_json` is used (`cargo machete`, exit 1) | fixed — removed with a note at the seam explaining when the dependency arrives (with the first reader) |
| 7 | minor | crates/runtime/src/lib.rs:14, crates/runtime/examples/embed.rs:5-6 | doc comments told developers to point `DIGUP_MODELS` at one machine's data folder | fixed — the examples name a placeholder (`/path/to/models`); `LlamaModels::folders()` documents the real search order |
| 8 | minor | deploy/dav/entrypoint.sh | tracked 100644: the compose service's entrypoint arrives non-executable in a fresh clone (the audit leg measures the INDEX, not the worktree) | fixed — `git update-index --chmod=+x` (100755) |
| 9 | major | crates/runtime/tests/search.rs:64, crates/runtime/tests/index.rs:53 | the pinned suites name the harness's scratch root (`ANSWER_ROOT`) as the prefix the vendored answers carry: the fixtures are only comparable on a machine with that folder, and a local path sits in tracked content | open — the pinned roots become a neutral placeholder token in the fixtures, with the suites substituting their own temp root; the FTS name-excerpt prefix (measured s343) canonicalizes by token count, so a placeholder of any token count works |
| 10 | major | Cargo.lock (libsqlite3-sys 0.30.1) | RUSTSEC-2026-0459 (CVE in the C SQLite the `bundled` feature compiles; 6.8 medium; "Upgrade to >=0.39.0"). The shipped payload carries it | open — rusqlite 0.41.0 is the first release whose `libsqlite3-sys` requirement (>= 0.39.0) clears the advisory; 0.32 → 0.41 touches only `crates/core/src/db.rs`'s surface, re-gated by the schema/storage suites, the ⑨ index dumps and the twin gate |
| 11 | minor | Cargo.lock | 8 crates at two distinct versions (syn 2.0.119 + 3.0.10, hashbrown 0.14.5 + 0.17.1 …); identical versions in two subgraphs (regex, regex-automata, regex-syntax, memchr, libc, bitflags) compile once and are reported, not failed | open — measured per crate: the split rides proc-macro and dev-only graphs; each row gets a fix or a written reason |
| 12 | polish | 25 files at `audit-base` | the workspace was not rustfmt-clean (2,734 diff lines) | fixed `adc1605` — one mechanical commit, gates re-run byte-identical |

## Wave 1 — native + runtime (reviewer session, 17 findings)

Every row below was checked against the tree by me; the check is named, because the point of
an audit is that the reader can re-run it. 13 of the reviewer's 17 findings did **not**
reproduce: five of its citations name files that do not exist in this workspace
(`native/src/ffi.rs`, `native/src/audio.rs`, `native/src/llama_store.rs`,
`runtime/src/llama.rs`, `core/src/vectors.rs`), and a dozen anchors sit past the end of the
file they name (`Media+Portable.swift` is 200 lines; its 239-255/318-323/408-411/440-458
citations are fiction). Fresh eyes are worth having; they are not a substitute for
verification, and the audit accepts a finding only after reproducing it.

| # | sev | cited → real | finding | disposition |
|---|-----|--------------|---------|-------------|
| 13 | major | services/src/sharedembedder.rs:204-208 (**self-found, same seam**) | `info_now` `Box::leak`s a fresh `WorkerInfo` on every embedding call | fixed+test — a 1.8M-file run embeds ~2M times: a leak per call is unbounded growth in a daemon meant to sit warm. The leak now belongs to the model SET (a start, a swap: a handful per run), the copy living in `Shape::leaked`. Measured: 200 k `info()` calls grew the process **54,358,016 bytes** before, **< 1 MiB** after — `crates/services/tests/sharedembedder.rs` reads `/proc/self/statm` through the port's own `watchdog::footprint` and refuses a megabyte |
| 14 | polish | services/src/sharedembedder.rs:51,64,162,169 | `expect` on a mutex in library code | fixed — recover with `into_inner` (the port's own pdf.rs:179 pattern); a panic elsewhere must not become a second panic naming a lock |
| 15 | polish | services/src/sharedembedder.rs:161-167 | the restart re-check right after a successful `swap` is dead work (`swap` sets `full`) | fixed — the media path skips the re-check when the swap it just did succeeded: one lock round-trip per media embed, not two |
| 16 | — | runtime/src/indexer.rs (preview cache) | "keeps every vector it has built for a run" | not-a-bug — `Indexer` (indexer.rs:202-226) has no preview cache; the only preview store is the server's capped LRU (server/src/media.rs:24-60: capacity + the evict loop) |
| 17 | — | native/src/pdf.rs:179 (lock poisoning) | a pdfium panic aborts the process | not-a-bug — `LOCK.lock().unwrap_or_else(\|e\| e.into_inner())` recovers; the cited `ffi.rs` has no pdfium lock |
| 18 | — | native/src/image.rs:174-199 (ImageMagick path) | no memory guard on the RAW/magick path; stderr piped → pipe deadlock | not-a-bug — there is no ImageMagick path in the port: RAW/TIFF/HEIC ride `decode_with_ffmpeg`, which enforces the budget on every 64 KB read and stops ffmpeg, byte-matching Swift's `decodeWithFFmpeg`. No tesseract in either tree (`grep -rn tesseract Sources crates` → nothing), so the OCR-pipe finding has no subject |
| 19 | — | native/src/media.rs:104-132 (WAV u32) | the header overflows past 4 GiB of samples | not-a-bug — `write_wav` is only ever handed a `window_samples` window (media.rs:148-164, 210-244): at 16 kHz a window is 480 k samples; Swift writes the same Int32 header. Shared arithmetic, not a port defect |
| 20 | — | native/src/media.rs:195-253 (duration × rate) | floor vs round; the `-ss 1e9` clamp; the audio window seam | not-a-bug — the port streams windows exactly as Swift `AudioExtractor.windows` (Media+Portable.swift:84-131): there is no duration×rate slicing; `(window*rate) as usize` truncates like `Int(window*sampleRate)`; `frame_at` (media.rs:353-375) is Swift's `frame(at:)` (179-198) with the same `-ss`/lead/fallback and no clamp on either side |
| 21 | — | native/src/media.rs:273-284 (keyframe windows) | the 1.0/5.0 floor rule and the slot rule differ | not-a-bug — read digit-for-digit against Media+Portable.swift:142-152: `step = max(interval, duration/maxFrames)`, `first = min(0.5, duration/2)`, `fps=fps=1/step:start_time=first:round=up`, slots at `first + index*step`, slots past the duration dropped. Identical |
| 22 | major | native/src/ffmpeg.rs:106-162 (no child timeout) | a wedged ffmpeg/ffprobe stalls the service, no `wait_timeout` | kept-by-design — Swift's `FFmpeg.run` (Media+Portable.swift:24-52) has no timeout either: adding one changes *when a file fails*, i.e. a contract change, not a cleanup. The measured cure exists (services/src/watchdog.rs, which ends the helper with 75 and names the file); the exposure — a child wedged before the first read leaves the watchdog with no "file being read" record — is recorded for the report, not fixed here |
| 23 | minor | native/src/ffmpeg.rs:116-138 (kill on drop) | a panic unwinding out of `run` leaves the child writing to a full pipe | kept-by-design — Foundation's `Process` has no kill-on-deinit either, and the port kills exactly where Swift calls `terminate()` (the reader's error path, 131-134). Kill-on-drop would move exit codes and refusal texts, which is parity, not cleanup |
| 24 | — | core/src/db.rs:177-191 ("re-prepares per bind") | `run_keeping` re-parses the statement per row | not-a-bug — it prepares once (182) and the closure reuses `stmt` (183-188) |
| 25 | — | services/src/watchdog.rs:32-77 | Swift's shared `Arc<Mutex<Vec>>` watchdog shape is duplicated with a drain in the index transaction | not-a-bug — the port's watchdog has one owner and reads `indexer::now_reading()`; no drain, no second structure |
| 26 | — | runtime/indexer.rs:1054, 1406 | a `PRAGMA wal_checkpoint(TRUNCATE)` per event; `read_long_file` swallows its error | not-a-bug — neither symbol exists: `grep -rn wal_checkpoint crates/core/src crates/runtime/src` → no hits; the long-file path is `finish_long_files` (indexer.rs:481-525), which propagates with `map_err(Error::from)` |
| 27 | — | pooling OOB clamp, `set_keep_alive(n_batch)`, `sample_points` unbounded, `u32::try_from` silent skip, model-path OnceLock | five cited defects | not-a-bug — none of these symbols exists in this workspace (`grep -rn "set_keep_alive\|sample_points\|u32::try_from\|pool_type" crates` → the pooling lines cited are `llama_pooling_type_*` constants and a 30 s audio cap; there is no `keep_alive` call in either engine, no `sample_points`, no path cache in `models.rs`) |

What wave 1's review of the REAL seams bought: the shared-embedder seam is correct on both
#155 rules (the restart is `!start && wants_full && !full`, sharedembedder.rs:90; a media
call sets `wants_full`, a text call leaves it standing, 156-160), and its one real defect
was found here, not by the reviewer: the per-call leak (row 13).

## Clean-verified

The places the reviewers checked and found correct (proof of coverage, not praise):

*(filled as the waves land)*

## Tree shape

The repo-shape decision (stay additive here vs a Rust-only publish layout) is deferred to the
end, per the user. The inventory with each path's purpose and consumer is written when the
report is filed.
