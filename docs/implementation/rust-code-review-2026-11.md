# Code review — Rust tree after the port (2026-11-20)

Two full reviews of `crates/**` + `shell/src-tauri` (duplication audit + quality/efficiency
audit), then a fix pass. Engine gate: `cargo test --workspace` + `cargo clippy --workspace
--all-targets -D warnings` on rust:1.99.0 (Docker, this box; numbers in
IMPLEMENTATION_STATUS). The parity rules governed every fix: pinned strings, SQL text, float
operation order, CLI human output, JSON-lines replies and event order untouched; the Python
suite + twin gates are the contract, CI green on the tip is the proof.

## Fixed this pass (20 files)

Dedup:
- `services/lib.rs`: deleted the unused verb-list consts (`SEARCH_OPS`/`INDEX_COMMANDS`/
  `INDEX_EVENTS` — zero readers; the compiled `handle()` match arms are the verbs' home).
- `server/service.rs`: deleted dead `summaries()`/`raw()` (no callers; the Python suite talks
  to the engine daemon directly — recording it). `main.rs:32` `#![allow(dead_code)]` KEPT with
  a comment naming what it silences (`security::read_auth` is the known item — deleting it is
  a separate call, the allow documents it for now).
- CLI format twins (`number`/`tilde`/`duration`) deleted → `formats.rs` is the one home.
  One accepted divergence, recorded: with `HOME` unset the tilde twin printed `~<path>`,
  `formats::tilde` prints `<path>`.
- CLI `emit` twin deleted → `services::emit` (authoritative, jsonline bytes). Closed-pipe
  writes no longer panic in the CLI (the services contract, Swift's `try?` shape).
- `"DIGUP_DEVICE"` literal → `runtime::DEVICE_ENV`; model dir: `runtime::MODELS_ENV` const
  (server spells the env name as a literal by design — it links no engine crate, thin-client
  shape, comment records it).
- z-gate `2.0` literals (cli ×2, search_service ×1) → `core::scoring::Z_GATE`.
- Trailing-slash prefix rule: `paths::bound()` is the one spelling — `estimator`, `model`,
  `folder_selection`, `runtime/indexer` call it; `grep ends_with('/')` over `crates/**`
  (non-test) = exactly one hit (`paths.rs`).
- `"worker.log"`/`"worker-query.log"` → `paths::WORKER_LOG`/`WORKER_QUERY_LOG`; literals gone
  from crates; the shell (links nothing) carries a comment naming the home.
- `server/config.rs` estimate knob: `config::estimate_timeout()` is the one home (request-path
  parse kept — a default is for an UNSET knob, never a rescue of a typo'd one).
- `validate.rs::check_pixels` → `config::py_int` (one integer grammar; widening only on
  CPython-accurate underscore spellings; pinned test cases unchanged).
- `targets.rs`: `lexical`/`clean` twins merged into one fold (byte-identical behavior; the
  root-start `..` no-op preserved).
- `index_store.rs` `json_object`/`json_str`: KEPT with comments — these write the SQLite
  `info` column read back by `json_extract` (pinned bytes); NOT the jsonline wire emitter.
- `core/lib.rs` `pub mod zipfile`/`folder_selection`: KEPT pub — audit's "no cross-crate use"
  was wrong (`search_service` uses folder_selection; `core/tests/doc.rs` uses zipfile).
- `code.rs::known_language`: KEPT pub — integration tests are a separate crate.
- Stale `server/digup_server/` (15 files, all `__pycache__`): deleted.

Efficiency/quality:
- `zipfile.rs`: overflow-checked bounds (4 sites: `len→usize` via `try_from`, `total*46` via
  `checked_mul`, zip64 `local+30+comp` and `head+30+name+extra` via `checked_add` chains;
  a wrapped sum can no longer bypass the bounds check — it lands on the existing
  `not_a_zip`/`can't read` refusals).
- `searcher.rs` score(): one buffer per query (prefix truncate replaces the second
  `vec![0.0; rows]` + copy; ~7 MB/query less churn at 1.8 M rows; numeric order untouched;
  `load()` guarantees rows ≤ matrix rows).
- `doc.rs` RTF tag regexes: cached per tag in a `LazyLock<HashMap>` — 6 compiles per process,
  was one per tag per file; pattern bytes byte-identical.
- `media.rs` range regex → `LazyLock` (pattern identical).
- `main.rs` OPTIONS preflight: `HeaderValue::try_from` no longer `expect`s — malformed
  allowlist origins answer 204 without ACAO instead of panicking (same fall-through shape as
  `finish()`); valid origins byte-identical.
- `jobs.rs` log replay streams lines per rotation (no full-file buffering at boot).

## Deferred, with reasons (follow-up work, each needs its own tested task)

- **`"code:"` prefix rule diverges** between `cli::code_query_text` (ASCII lowercase) and
  `core::model::NameQuery::text_of` (grapheme-aware) — the riskiest audit finding; changing
  search text semantics needs a twin-gate run, not a drive-by.
- `status` materializes every file row per root (Swift `rootSummaries` parity; a GROUP BY
  aggregate changes pinned reply composition — suite + pilot measure first).
- Poison-cascade policy on server shared locks (`expect` on read locks): `into_inner`
  pattern exists in `runtime::backend`; sweeping it across `server` changes crash semantics —
  measure against the stop gates first.
- Slots twin (`server/slots.rs` ≡ `search_service::Slots`) and the protocol-client twin
  (server/protocol.rs ≡ shell/helper.rs): cross-graph by design (server links no engine
  crate, shell links nothing) — a shared crate is an architecture decision.
- packages.yml model-pin drift guard (the third copy of the pins has no guard; server.yml's
  guard fires on model bumps — add the packages guard as a CI task).
- render cache LRU cap, `/dev/urandom` pooling, `shell` unwrap census: measured low-severity,
  recorded, no action.
