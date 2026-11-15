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
| 9 | major | crates/runtime/tests/search.rs:64, crates/runtime/tests/index.rs:53 | the pinned suites name the harness's scratch root (`ANSWER_ROOT`) as the prefix the vendored answers carry: the fixtures are only comparable on a machine with that folder, and a local path sits in tracked content | fixed — the fixtures carry the NEUTRAL PLACEHOLDER `/fixtures/suite-corpus` (the answers were RE-MEASURED at the harness root and vendored under the token, 2026-11-15): the plain forms (dump/search/event paths, 90 + 29 ESCAPED `\/` event paths — the events jsonl escapes its paths, the first rewrite missed them and the index suite failed on the escaped miss) and the FTS SPACE forms (`var tmp digup-check …` -> `fixtures suite-corpus …`); the suites derive canonicalization from the ANSWER_ROOT constant (string replace + `path_words`/`canonicalize_hits` token-count mechanisms), so the placeholder rides the same mechanism as the suite's own temp root; `audit.sh` 11's scratch-root leg now finds no hits. Side sites: config.rs f08-idx -> `/tmp/…`, jsonline fixture folder -> `/fixture/…`, media.rs panic hint + doc examples -> `<the pinned 4.4.2 ffmpeg build>` (no copyable harness path). Gate: index 2 ok (421 s) + search 1 ok (429 s) + corpus 0 ok + native suites ok + jsonline 9 ok + server bins 22 ok, exit 0 |
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

## Wave 2 (core + runtime + tools) — findings F-01..F-15, landed as commits

Keyed by commit (the review's F-numbers live in the review notes; each row's finding
is what the commit fixed, verified against the Swift twin before the diff landed):

| # | sev | commit | finding | disposition |
|---|-----|--------|---------|-------------|
| 28 | major | `ba7c4fa` | a refused `BEGIN` left the daemon at transaction depth 1, and depth 1 is a trap (the write path commits a phantom) | fixed — RAII depth: the guard ends the transaction on every exit path; runtime suite pins the refusal shape |
| 29 | major | `58250bb` | `hasWords`/`hasProse` counted `L*` (letters) alone — the engine counts letters AND marks, so Indic/Cyrillic-script prose was scored as noise | fixed+test — the letter set is `Letters` AND the marks; scoring pins at the word-gate edges |
| 30 | minor | `219fef5` | `paths::real_path` called the unix symbol `realpath` unconditionally — `crates/core` did not compile for Windows at all | fixed — cfg split; the Windows path rides `GetFinalPathNameByHandle`, pinned by shape |
| 31 | minor | `f7a456e` | the ffmpeg/ffprobe program lookup ran per call and missed the Windows `.exe` shape | fixed — one lookup, resolved once per process, Windows-correct |
| 32 | major | `bb5c917` | the vector load staged every row into an intermediate `Vec` — 46,748 kB resident FAILED the 60-photo suite where the engine's straight copy peaks 33,208 kB | fixed — each blob copies straight into the matrix (the engine's shape); suite re-gated at the streamed footprint |
| 33 | polish | `5adbbed` | no ledger, no battery: the audit had no written bar | fixed — this ledger + `scripts/rust/audit.sh` |
| 34 | polish | `3c90486`, `539847e` | `audit.sh` failed its own shellcheck leg (SC2164); shipped `scripts/rust/*.sh` had unparseable shapes since landing | fixed — both legs pass `shellcheck` |
| 35 | minor | `22d0ba1` | `costs.json` saved with a truncating write: a crash mid-save loses the tallies (the speed gate's own record) | fixed — write-then-replace, atomic on the same filesystem; pin covers the crash-shape |
| 36 | major | `4229bf2` | `excerpt` walked the whole file to build a preview it truncates to the prefix it shows — a 4 MB text file paid full OCR-scale work per hit; the refusal texts also diverged (engine words verified) | fixed+test — prefix-only walk; the refusal texts ride the engine's strings |
| 37 | major | `df71163` | the pinned Swift fixture opened READ-WRITE: a reading test RECOVERED the index (crash frames folded: 4,096 -> 81,920 bytes, `-wal` truncated) — the fixture mutated the artifact it was scoring | fixed — read-only open (`SQLITE_OPEN_READONLY` at the store seam); the 130-row replay is byte-identical on the untouched fixture |
| 38 | polish | `fe5ab43` | the fallback page count measures the tail window (a bounded window over a huge PDF underestimates pages) and the doc said it was the document's count | fixed (doc) — the doc says what it measures; page-count pins record the window shape |
| 39 | polish | `5af72d5` | four copies of "is this path inside a root" (crawler, store, server, CLI) with the store's `db::ok` a pass-through; the store facts the pins reach were unpinned | fixed — one `is_inside` in core; `db::ok` removed; store-fact pins (vectors files/counts/unfinished on the fixture) |
| 40 | major | `c3caa38` | `download-model` asked Hugging Face `…/main/<file>` — HF answers 404 text/html (52,885 B); the file lives under `/resolve/` | fixed+test — the URL carries `/resolve/`; pinned at the source: 200 application/octet-stream, 309,855,456 B (the byte count the ModelFiles.swift pin carries) |
| 41 | major | `a42f2a9` | a rendered PDF page copy that can't be written was SKIPPED — Swift's `try image.write` throws and the file is marked failed | fixed+test — `made_page -> Result<Page, ExtractError>`, "can't write <path>"; fails-before measured (Ok with 0 pictures, 0 files on disk) |
| 42 | major | `40755eb` | audio windows kept a head index into the samples Vec — streaming a 3-hour podcast grew 118,964,224 B RSS and the watchdog ended a healthy run | fixed+test — the consumed hop drains (`drain(..n).for_each(drop)`, capacity kept): 3,121,152 B measured; engine arithmetic pinned (n = floor((total-window)/hop)+1; 45 s = 2 windows) |
| 43 | major | `b454d01` | the pinned PDF replay ran in-suite: PDFium's process-global fallback cache moves the broken-font page's pixels (cold 945be28e = pinned = Swift; warmed-by-self 86271d6c; warmed-by-suite 06f9a565) — the suite passed alone, failed in parallel | fixed+test — replay runs in a FRESH CHILD PROCESS (libtest self-spawn `--ignored --exact --test-threads=1`); driver pinned, core `#[ignore]`d |
| 44 | major | `479fec3` | five wire sites count characters as SCALARS where Swift's `count`/`prefix`/`dropFirst` count grapheme clusters: doc "chars" (81 vs 41), ocr_chars + note (24 vs 12), the embedder 300-cap (150 vs 300 on a Devanagari batch), PDF minText, code `place()` | fixed+test — grapheme clusters at the five sites; pins lock the answers AND unit-consistency (the LATENT traps: this box's PDFium drops combining marks — rule aligned, fixture pinned; `place()` was unit-consistent, parity held — the trap is mixing units) |

## Wave 3 (crawl/extract + services seams) — landed as commits

| # | sev | commit | finding | disposition |
|---|-----|--------|---------|-------------|
| 45 | major | `d14cc84` | folder symlinks: Foundation's `URL` values are lstat-based on Linux, so a folder symlink takes the FILE branch; and the walk must run on STANDARDIZED roots (progress `root:`, `leftOut` keys, candidate paths, dataset-roots exclusion) | fixed+test — file-branch on Linux (measured against the twin), roots through `paths::standardize` |
| 46 | major | `ecbc59a` | ZipArchive validated per ENTRY where miniz validates the ARCHIVE at init (strict zip64-extra walk, heapsort+binary-search locate): encrypted Office files, corrupt offsets, multidisk layouts answered `can't read …`/`is 4095 MB` where the engine answers `not a zip file` | fixed+test — archive-level validation at init, the zip64-extra strictness, the locate algorithm; refusal texts pinned byte-for-byte |
| 47 | major | `19c8b24` | whole-file slurp per zip entry: opening a 200 MB .pptx cost 204,804 kB resident where the engine (FileReader + miniz) peaks at positioned windows + central directory + one entry | fixed — positioned windows; footprint measured at the seam |
| 48 | minor | `22ebb7c` | `isProjectRoot` lowercased the pathExtension before matching: a bundle spelled `Foo.XcodeProj` did NOT mark the folder — the audit-base skipped a whole tree's assets as "inside a code project" | fixed+test — RAW extension vs lowercase markers is case-SENSITIVE (Rules.swift:179); pin covers the mixed-case bundle |
| 49 | major | `338afa2` | `char::is_whitespace` (Unicode White_Space) where the engine trims Foundation sets: ZWSP U+200B IS in Foundation `.whitespaces` on Linux (Darwin: not) and is NOT White_Space; LF/VT/FF/CR/NEL/LS/PS are White_Space and NOT `.whitespaces` | fixed — the measured sets at the query/snippet/summary + office/doc/mail trims (F9, scalar-by-scalar, swift-corelibs 6.4); the closer-verdict flip F9 pointed at proved masked |
| 50 | major | `34f7947` | `load_media` passed a PATH string on Windows: mtmd's `fopen` reads the ANSI code page, a non-ASCII path fails where the engine feeds BYTES via `init_from_buf` | fixed — bytes through `init_from_buf` on Windows, matching the engine |
| 51 | major | `8dbfed6` | PDF `prepare` opened the document TWICE (a count open + a render open) where Swift's range closure plans the step range inside the one read that reads the pages | fixed — one open; the range is planned by the render pass |
| 52 | major | `cf3061f` | the ffmpeg reader folded a read ERROR into EOF and answered success mid-stream (truncated output accepted as complete); the "close" line was a no-op | fixed — a read error ENDS THE RUN AS AN ERROR; kill path at the reader's error site, as Swift's `terminate()` |
| 53 | polish | `5db09cf` | `LlamaLog` unwrapped a mutex INSIDE an `extern "C"` callback — a panic unwinds across the FFI boundary llama.cpp called through (abort) | fixed — `into_inner`, never unwrap |
| 54 | minor | `33ad257` | `oriented()` copied 12 MB of pixels for orientation 1 — the identity case is a move | fixed — by value; orientation 1 is a move (measured at the seam) |
| 55 | polish | `3d6fecc` | raw FFI sites in `stb.rs` carried no SAFETY notes | fixed — notes at every raw site (coverage, not behavior) |
| 56 | polish | `fee1564` | two platform-only constants and a dead duration twin in the native crate; `lower_extension` loose | fixed — retired; tightened (mechanical, gates byte-identical) |
| 57 | major | `16de43c` | `threads()` re-read `DIGUP_THREADS` per call — the engine reads it ONCE per process (`static let`); a helper's mid-run env change moved row layout | fixed — one read per process, pinned by the env-var test (all scenarios in one fn) |
| 58 | minor | `58fc21d` | media windows feed reviewed as "per-read Vec allocation, sloppy" — the engine allocates the same per-read bytes Vec | not-a-bug — engine shape verified; kept |
| 59 | major | `a46a1b5` | 206 partial streams: the span buffer allocated the WHOLE file per request in the uncapped arm, and one `read` per chunk truncated bodies under davfs2's short reads | fixed+test — one read per piece (loop-until-filled), no span buffer (capped reads relay the engine's byte ranges) |
| 60 | major | `79167dc` | the four lazy-supervision endpoints ran `scope_for` on a tokio worker — a cold target's searcher spawn (seconds) blocks a worker; AGENTS.md: spawn is blocking IO | fixed — all four ride `spawn_blocking` (the searcher-spawn class) |
| 61 | major | `bffe418` | the path guard + ADMIN_ROUTES/OPEN_PATHS tests read the RAW request target: starlette/uvicorn DECODE `scope["path"]` and rstrip `/` BEFORE routing — `%2F`-encoded shapes bypassed the audit-base guard | fixed+test — guard + route tests read the DECODED path, rstripped; the encoded probe fails-before, passes-after |
| 62 | major | `0f58865` | one-shot engine children read stdout AFTER the exit poll — a >64 KiB output line fills the pipe, deadlocks the child into a false 502 (the `estimate --json` line for a big tree exceeds 64 KiB) | fixed+test — pipes drain CONCURRENTLY with the wait (Python `communicate` shape); pinned at the seam with the >64 KiB line |

## Wave 4 (server/CLI + core edges) — findings F-01..F-15 (wave-4 numbering), landed as commits

| # | sev | finding | disposition |
|---|-----|---------|-------------|
| W4-F1 | critical | RTF code page: the windows1252 map was a 32-entry array (`special[byte - 0x80]`) indexed over the WHOLE 0x80...0xFF half — ANY byte 0xA0...0xFF in a .rtf (`\'ff`, `\'e9`, `\'a0` — the accented letters every Windows document writes) PANICS the indexer ("index out of bounds: len is 32, index is 32", measured), and the four slots Swift LEAVES OUT of its dictionary (0x81/0x8D/0x90/0x9D) mapped to C1 controls Rust's `char::from_u32` accepts | fixed+test `881d0c2` — the dictionary SHAPE reproduced (sparse keys, the holes, 0xA0-0xFF pass-through); pin fires the panic-shape on the audit-base, passes on the twin's words |
| W4-F2 | minor | RTF `\'hh` radix window — claimed divergence from `from_str_radix` on `"+41"`, `"1_2"` | fixed `8c08df2` — EXHAUSTIVE oracle (f7all.swift/f7all.rs, ALL 1,089 two-char windows): 1,088 AGREE; the one divergence is `"-0"` (Swift answers 0). The claimed shapes were 3-character `Int(_:radix:)` facts the 2-char window never sees — REFUTED (Clean-verified row) |
| W4-F3 | major | the shared `trim_ws` ran on Unicode White_Space where the mail/office/doc sites trim Foundation sets (ZWSP trims here and not there; VT/FF/NEL/LS/PS the reverse) | fixed `6e89b02` — the F9-measured sets at every rider site |
| W4-F4 | major | mail charset `split(':').next()` keeps the empty head: `":iso-8859-1"` named `""` -> UTF-8 default -> replacement characters (issue #146's exact defect, back on the leading-colon form) | fixed+test `9835661` — Foundation's split OMITS empty pieces: first NON-EMPTY colon piece, filtered whole clusters; scope recorded (the canonical `iso-8859-1:1987` form has a non-empty head — equivalence row) |
| W4-F5 | minor | the Q word's `=XX` escape used the general Swift-Int grammar (parse_radix) over a window the engine reads as `UInt8(prefix(2), radix: 16)` | fixed+test `3bc2279` — from_str_radix + the measured `"-0"->0` branch; `=-0` is byte 0x00 to the engine |
| W4-F6 | major | estimator: the probe read stdout and never asked the exit code — ffprobe prints the duration THEN fails on a broken stream (container header parses, body does not): the engine counts 0 (`try?` -> nil -> `?? 0`), the port relayed the printed number, biasing the forecast the server ships verbatim | fixed+test `8821068` — a probe that fails after printing counts as zero |
| W4-F7 | major | worker daemon casts: `inputs as? [String] ?? []` per-element filter (Swift is ALL-OR-NOTHING); budget `as_i64.unwrap_or(280)` (Swift is the NSNumber bridge — measured 2026-11-15): mixed arrays embed rows the engine embeds NOTHING for; floats/bools default to a silent 280-embed where the engine embeds at 140.0 or refuses | fixed+test `cc58872` — the measured cast + bridge matrices pinned; live daemon head-to-head (count:2/6144 B vs count:0/""; ok:true-280 vs `budget must be one of …`) |
| W4-F8 | major | the port boots shapes the reference REFUSES at startup: empty/absent `DIGUP_SERVER_ROOTS` (api.py:304 RuntimeError — the port booted a healthy ZERO-HIT server); absent `DIGUP_SERVER_INDEX` (KeyError kills the boot — the port defaulted to /data/index, opening/creating an index nobody configured); the AGENTS.md shape that cost an hour | fixed+test `688a53c` — five fail-fasts, the reference sentences verbatim (empty INDEX value boots on BOTH sides — measured; presence is the check) |
| W4-F9 | major | keyed reads: the reference SUBSCRIPTS engine reply fields (status's `cli["kinds"]`/`fingerprints`/`build_tag`/`vectors_version`/`index_size`, the live splat; search's `hit["path"]`/`hit["kind"]`/…) — an unmapped exception is Starlette's plain 500 (measured on the pinned fastapi 0.162/starlette 1.10); the port answered 200 with the field `null` | fixed+test `d5233af` — relay as 500 at the handler seams (`hit_fields`) |
| W4-F10 | major | OPTIONS requests: claimed state-changing-method handling diverged | NOT A DEFECT — Python's `STATE_CHANGING` is exactly `{POST,PUT,PATCH,DELETE}` (no OPTIONS); OPTIONS 204/405 measured identical on both sides. No change (Clean-verified row) |
| W4-F11 | critical | entropy fallbacks: `urlsafe_token`/`random_hex12` answered Ok from a timestamp^pid seed + 16 ZERO bytes; `secrets.token_urlsafe(24)` and `uuid4` have NO fallback (security.py:335,344; jobs.py:99; api.py:281 — boot death / plain 500) | fixed+test `a874802` — Result everywhere, the refusals relay the reference's shape; the minted key's signature measured: the audit-base key ENDS with 21 literal `A` chars (pin replicates the audit-base verbatim); job-id mint moved under the busy lock after the busy check (jobs.py:96-99 order — a busy target never mints) |
| W4-F12 | major | UI default baked `env!("CARGO_MANIFEST_DIR")` — the build tree's path compiled in: a copied payload's default names a tree that is not there (its shipped bundle never mounts); on the build box it mounted the git-ignored/docker-ignored host dist. Python's default is install-relative (api.py:936) | fixed+test `16db4cd` — `<exe dir>/ui/dist`; Dockerfile ships dist payload-adjacent (`/opt/digup/ui/dist`) and drops the env knob; the three answers of api.py:937-941 pinned (`ui_mount`) |
| W4-F13 | major | "ready handshake" divergence (claim) | THE HEADLINE DOES NOT REPRODUCE — the daemon `ready` line matches field-for-field. Three residuals found REAL defects: (a) CLI path seams skip the URL standardize — fixed `c7ea750` (`url_path`: tilde-first, CWD-absolutize, `paths::standardize`; pin incl. the lexical climb above the CWD); (b) error Display families SWAPPED (Worker raw / Llama prefixed vs Worker.swift:86-92, LlamaEmbedder.swift:391-394) — fixed (six sites remapped, pins at the relays); (c) previews rebuilt BEFORE the store open — a failed start wipes the preview cache the reference leaves UNTOUCHED (measured: Swift marker intact, audit-base previews empty) — fixed (`run()` order + 2 pins) |

## Clean-verified

The places the reviewers checked and found correct (proof of coverage, not praise):

Measured and verified, the reviewers' claims that do NOT move the port (proof of coverage):

- **F7 radix window, exhaustive oracle** (2026-11-15, f7all.swift + f7all.rs on swift-corelibs
  6.4): `UInt8(_, radix:16)` and `u8::from_str_radix` AGREE on 1,088 of ALL 1,089 two-char
  windows of the reader's alphabet. The spot-probe divergences the review cited ("4_"->4,
  "_41"->65) are THREE-character `Int(_:radix:)` facts the reader's 2-char window never sees.
  The one real divergence: `"-0"` — Swift answers 0. (Fixed in `8c08df2`/`3bc2279`; the
  claims recorded here.)
- **F-10 OPTIONS** (wave 4): Python's `STATE_CHANGING` is exactly `{POST,PUT,PATCH,DELETE}` —
  OPTIONS is not in it; OPTIONS answers 204/405 measured identical Python vs Rust. No change.
- **F-13 ready handshake**: the `ready` event line matches field-for-field (id, index string,
  timing shape) — the headline does not reproduce. Its residuals (a/b/c) were real, fixed, and
  are in the wave-4 table; the handshake itself is parity.
- **W4-F7's budget-refusal direction** (measured on swift-corelibs 6.4, f7cast.swift +
  f7cast2.swift, 2026-11-15): the review wrote that Swift REFUSES a fractional budget — it
  does not. `as? Int` on `2.5` answers nil, `?? 280` SILENTLY embeds at 280; Swift is
  coercing, and the fixed port relays exactly that. The real divergences (all fixed, pinned,
  live-measured) were the per-element inputs cast and serde's `as_i64` refusing whole-valued
  doubles and bools. A refusal claim needs the refusal's DIRECTION measured before a rule
  is transcribed.
- **Canonical charset forms** (with W4-F4): Foundation's split and Rust's first-non-empty
  agree on the CANONICAL registered form `iso-8859-1:1987` (non-empty head — the year suffix
  never reaches the empty-piece shape); the divergence needs the leading-colon form. Scope
  recorded at the site.
- **emlx pre-decode parity**: the emlx reader's pre-decode (base64 payload detection,
  windows1252 fallback window) scored digit-for-digit against the Swift twin on the corpus
  fixture (s315 family) — the port's decode lands byte-equal; the audit-base's radix widening
  is where the divergence lived (fixed with W4-F2; the base64 windows are parity).
- **Swift error Display facts** (measured 2026-11-15, the files, not from memory):
  Worker.swift:86-92 — `.failed` displays `"worker error: {why}"`, `.notFound`
  `"worker not found: {why}"`, `.died` `"worker stopped: {why}"`; LlamaError
  (LlamaEmbedder.swift:391-394) displays the RAW description — NO prefix. WorkerError.failed
  sites: "can't load" x2 (:63,:99), "can't create a llama.cpp context" (:82),
  "llama_process failed ({status})" (:309), "no pooled embedding for input {sequence}" (:312),
  the text-only/budget guards (:153,:154,:159); IndexService.swift:511 throws
  `.notFound("the model is closed")`. These facts drove the Display remap.
- **Daemon startup-failure stderr** (measured, corrupt index.sqlite): Swift
  `file is not a database (in: PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;
  PRAGMA foreign_keys = ON;)` vs Rust `file is not a database` — stdout contract identical
  (no `ready`, exit 1; the server treats both as a failed spawn). The text is operator-visible,
  not parsed: recorded, not chased.
- **Swift's `"\( expr )"`** (compiled probe): interpolation emits the VALUE only — the
  source's parens are syntax, not characters. The transcribed SQL `" AND id IN ({}))"`
  carried a stray paren the engine never emits (pending run died on a fresh index's first
  sync; the fixed shape ships).

## Tree shape

The repo-shape decision (stay additive here vs a Rust-only publish layout) is deferred to the
end, per the user. The inventory with each path's purpose and consumer is written when the
report is filed.
