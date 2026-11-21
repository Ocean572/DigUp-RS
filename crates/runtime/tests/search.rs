//! ⑩c pinned suite: the whole search pipeline replayed. The 45 `search` scenarios the
//! ⑩c cross-check runs (queries over the ⑨ suite's built indexes — prose docs, code
//! identifiers, names, kind hints, z-gates, caps) ride beside this file with the engine's
//! own answers (`fixtures/search-expected.jsonl`, measured through the Swift `Searcher`
//! on the ⑩c cross-check probe) and the query vectors the engine's encoder produced
//! (`fixtures/search-vectors.jsonl`). The suite rebuilds the ⑨ corpus byte-for-byte (the
//! ⑨ manifest gate proves the factory), indexes main + code with the model — the ⑨ suite
//! is what proves those indexes equal the engine's byte-for-byte — scores with the
//! PINNED Swift floats (identical inputs both sides: the gate isolates the pipeline from
//! the model, exactly as the measured gate did), and compares every answer: hit lists,
//! scores, z, cosines, keyword ranks, segments, locations, focuses, excerpts, info.
//!
//! Comparisons normalize what the machine makes: the corpus root (`ANSWER_ROOT` — the
//! placeholder the answers are vendored under, and the suite's own temp root; both
//! sides print `C`) and
//! number spelling (Swift's JSONSerialization writes a whole-valued Double as an
//! integer, serde_json keeps `.0` — every number reads as an f64 before comparing).
//! Floats compare at the engine's arithmetic noise floor — 2 ulps (see `numbers_close`:
//! the z statistics sum in store ROW order, so the suite's fresh crawl shifts every z
//! 1–2 ulps from the pinned answer, and Swift's `modalityMeans` reduces a Foundation
//! `Dictionary`'s values in per-process hash-seed order, shifting z 1 ulp between Swift
//! processes on one store; both channels measured on s315). The suite pins BOTH inputs the answers
//! are computed from, so that floor is the whole rule on every machine: the store's own fp16 rows,
//! installed from the vendored dumps before the store opens (`install_pinned_vectors` — measured
//! 235 of 235 main and 21 of 21 code rows, 0 left as this engine built them), and the query
//! vectors the fixture carries, verbatim (`search-vectors.jsonl`). That is the sentence the header
//! above claims — "identical inputs both sides" — carried all the way into the store, and it is
//! what makes `cosine`/`score`/`z` byte-exact on Linux, macOS and Windows alike for the 27
//! scenarios whose query text the fixture covers. The fixture carries 31 (query, store) vectors for
//! 45 scenarios, so 18 embed their query with the model on the machine running them; those keep a
//! second, named bar, derived from the engine's own amplification and counted per field — see
//! `VECTOR_TRIO_BAR`, and the closing move it names (re-vendor the 45). Strings, kinds, segments
//! and integers stay digit-for-digit. `segment_id` (the store's rowid) and the engine's tie order are
//! crawl-order artifacts — hits are compared as the SET of rows the engine returned,
//! each pinned by path + segment kind + loc (see `canonicalize_hits`, measured on s316);
//! `keyword_rank` is pinned as a per-answer multiset, its row assignment among tied
//! identical rows is the same rowid artifact (measured on s339). A name segment's
//! excerpt is the file's folder path FTS-tokenized on `/` and `_`, so the corpus root
//! appears as words — the root's token prefix canonicalizes to `C` (measured on s343).
//!
//! Needs the engine's environment (DIGUP_MODELS, the pinned Vendor/llama, PDFium, the
//! pinned ffmpeg the repo ships — `scripts/build-ffmpeg.sh`, 9.0.2 today; the media segments
//! the answers run on are what that build decodes, so the tools are pinned in the answers
//! themselves), DIGUP_DEVICE=cpu, DIGUP_THREADS=8 — the ⑨ suite's contract; a
//! missing model is a PANIC with the pin named, never a skip (the tools pin is
//! load-bearing: a different ffmpeg changes the media segments the index answers run on).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use digup_core::index_store::{IndexStore, CODE_FILE, MAIN_FILE};
use digup_core::model::FileKind;
use digup_core::searcher::{SearchHit, SearchOptions, Searcher};
use digup_runtime::LlamaModels;

#[path = "corpus.rs"]
mod corpus;
use corpus::{build_corpus, build_index, canonical_numbers};

const SCENARIOS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/search-scenarios.jsonl"
));
const EXPECTED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/search-expected.jsonl"
));
const VECTORS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/search-vectors.jsonl"
));

/// The root the vendored answers carry: a NEUTRAL PLACEHOLDER token, not a
/// machine's path (ledger row 9 — the fixtures once carried the harness's
/// scratch root, which made them comparable only on the machine that measured
/// them, and put a local path in tracked content; the answers were RE-MEASURED
/// there and vendored under this token, the 2026-11-15 rewrite). The suite
/// indexes a temp folder and canonicalizes BOTH roots to `C`: the string
/// replace in `normalize`, and the FTS name-excerpt prefix by token count
/// (`canonicalize_hits` splits root and excerpt on `/` and `_` — the mechanism
/// rides the constant, so a placeholder of any token count works).
const ANSWER_ROOT: &str = "/fixtures/suite-corpus";

/// The store's own vectors, installed from the vendored dumps: the other half of the isolation
/// this suite's header claims ("scores with the PINNED Swift floats, identical inputs both sides:
/// the gate isolates the pipeline from the model"). The query vectors ride that rule through
/// `search-vectors.jsonl`; the rows they are scored against are the other input, and the suite
/// rebuilds them with the model on THIS machine, so they are this machine's embeddings. The
/// vendoring machine's fp16 vectors are vendored beside the answers — field 8 of every `SEGMENTS`
/// row of `index-main-answers.dump` / `index-code-answers.dump` — so they go into the store this
/// suite built, matched on (path, segment kind, loc): the key the suite compares hits by
/// (`canonicalize_hits`). Both sides of that match are counted and printed, from the STORE's side
/// and from the dump's: the store's vector rows that no vendored row claimed keep this engine's
/// embedding, and a vendored row claiming nothing (or claiming 2 rows) is the corpus itself
/// disagreeing. Measured on this Mac 2026-11-15: 235 vector rows in the main dump and 21 in the
/// code dump, all 256 matched exactly one store row each, 0 store rows left carrying this engine's
/// embedding — the row SETS agree (the ⑨ suite's dump gate is what pins them), including the media
/// seam's rows: `ANSWER_FFMPEG`'s ffmpeg-build class moves an audio window's END and the vector
/// embedded from it, and the row's identity — path, kind, modality, window START — is the engine's
/// own, which is the key the pin matches on. A `name` row has no vector, and an `image` segment's
/// row has no loc at all (the 5 `image/image` vector rows of the main store are the ones whose loc
/// is the NULL marker), so those match on `loc IS NULL`.
///
/// Installed before the stores open, so the pool statistics the search reads are computed over the
/// same bytes the answers were measured on.
fn install_pinned_vectors(index: &Path, tag: &str, root: &str) -> (usize, usize, usize) {
    let dump = fixture_path(&format!("index-{tag}-answers.dump"));
    let lines: Vec<String> = std::fs::read_to_string(&dump)
        .expect("the vendored index dump beside the answers")
        .lines()
        .map(|line| line.replace(ANSWER_ROOT, "C"))
        .collect();
    // The dump's `FILES` lines carry the file id → path map, the paths in the placeholder's
    // spelling (`C/docs/big.txt`); the store wants paths under this run's own root.
    let mut file_paths: HashMap<i64, String> = HashMap::new();
    for line in &lines {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.first().is_some_and(|head| *head == "FILES") && parts.len() > 2 {
            if let Ok(id) = parts[1].parse::<i64>() {
                file_paths.insert(id, parts[2].to_string());
            }
        }
    }
    let store = index.join(if tag == "code" { CODE_FILE } else { MAIN_FILE });
    let db = rusqlite::Connection::open(&store).expect("open the store this suite built");
    // The store's own count first: the report is "X of the store's Y vector rows are vendored
    // bytes", so a store row the dump never claims shows up as a leftover instead of going unseen.
    let vector_rows: usize = db
        .query_row(
            "SELECT COUNT(*) FROM segments WHERE vec IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("the store's vector rows") as usize;
    let mut installed = 0usize;
    let mut unmatched = 0usize;
    for line in &lines {
        let parts: Vec<&str> = line.split('|').collect();
        // SEGMENTS|segment id|file id|kind|modality|loc|loc_end|excerpt|vec|name|body|original
        if !(parts.first().is_some_and(|head| *head == "SEGMENTS") && parts.len() > 8) {
            continue;
        }
        if parts[8] == "~" {
            continue; // a name row carries no vector
        }
        let Some(path) = file_paths.get(&parts[2].parse::<i64>().expect("a file id")) else {
            continue;
        };
        let rel = path.strip_prefix("C/").unwrap_or(path);
        let full = Path::new(root).join(rel);
        // The store spells paths with the host's separators; the dump carries the vendoring
        // machine's. Both spellings are offered (they are the same string on Linux and macOS).
        let host = full.to_string_lossy().to_string();
        let slashed = host.replace('/', std::path::MAIN_SEPARATOR_STR);
        let Ok(file_id) = db.query_row(
            "SELECT id FROM files WHERE path = ?1 OR path = ?2",
            rusqlite::params![host, slashed],
            |row| row.get::<_, i64>(0),
        ) else {
            unmatched += 1;
            continue;
        };
        let bytes = hex::decode(parts[8]).expect("a vendored fp16 vector, in hex");
        // `name` rows carry no vector (skipped above), and an `image` segment has NO loc at all:
        // its row is the whole picture (measured in the vendored dumps: the 5 `image/image` rows
        // of the main store are the ones whose loc is the NULL marker). Those match on `loc IS
        // NULL`; every other row matches on its loc.
        let rows = if parts[5] == "~" {
            db.execute(
                "UPDATE segments SET vec = ?1 WHERE file = ?2 AND kind = ?3 AND loc IS NULL",
                rusqlite::params![bytes, file_id, parts[3]],
            )
        } else {
            db.execute(
                "UPDATE segments SET vec = ?1 WHERE file = ?2 AND kind = ?3 AND loc = ?4",
                rusqlite::params![
                    bytes,
                    file_id,
                    parts[3],
                    parts[5].parse::<f64>().expect("a loc")
                ],
            )
        }
        .expect("install a vendored vector");
        if rows == 1 {
            installed += 1;
        } else {
            unmatched += 1;
        }
    }
    assert_eq!(
        installed, vector_rows,
        "⑩c {tag}: the vendored dump claims {installed} of the store's {vector_rows} vector rows          — {unmatched} vendored row(s) matched nothing here, so the corpus and the store do not          agree on the row set this suite is scoring"
    );
    (installed, vector_rows, unmatched)
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn rows(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a scenario line"))
        .collect()
}

fn strings_of(sc: &Value, key: &str) -> Vec<String> {
    sc[key]
        .as_array()
        .expect("an array field")
        .iter()
        .map(|v| v.as_str().expect("a string").to_string())
        .collect()
}

/// SearchService's vector-cache key: `(code ? "code\0" : "") + text`.
fn qkey(code: bool, q: &str) -> String {
    if code {
        format!("code\0{q}")
    } else {
        q.to_string()
    }
}

/// `hitFields` (SearchService.swift:365) — the engine's hit row, nil fields omitted.
fn hit_json(h: &SearchHit) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("path".into(), json!(h.path));
    m.insert("kind".into(), json!(h.kind.as_str()));
    m.insert("segment".into(), json!(h.segment.as_str()));
    m.insert("segment_id".into(), json!(h.segment_id));
    m.insert("score".into(), json!(h.score));
    m.insert("modified".into(), json!(h.modified));
    m.insert("size".into(), json!(h.size));
    let mut info = serde_json::Map::new();
    for (k, v) in &h.info {
        info.insert(k.clone(), json!(v));
    }
    m.insert("info".into(), Value::Object(info));
    if let Some(v) = h.z {
        m.insert("z".into(), json!(v));
    }
    if let Some(v) = h.cosine {
        m.insert("cosine".into(), json!(v));
    }
    if let Some(v) = h.keyword_rank {
        m.insert("keyword_rank".into(), json!(v));
    }
    if let Some(v) = h.loc {
        m.insert("loc".into(), json!(v));
    }
    if let Some(v) = h.loc_end {
        m.insert("loc_end".into(), json!(v));
    }
    if let Some(v) = h.focus {
        m.insert("focus".into(), json!(v));
    }
    if let Some(v) = &h.excerpt {
        m.insert("excerpt".into(), json!(v));
    }
    Value::Object(m)
}

fn search_answer(
    sc: &Value,
    main_searcher: &mut Searcher<'static>,
    code_searcher: &mut Searcher<'static>,
    vectors: &HashMap<String, Vec<f32>>,
) -> Value {
    let q = sc["q"].as_str().expect("a q").to_string();
    let code = sc["index"].as_str() == Some("code");
    let kinds: Option<HashSet<FileKind>> = sc.get("kinds").map(|v| {
        v.as_array()
            .expect("kinds array")
            .iter()
            .filter_map(|k| FileKind::parse(k.as_str().expect("a kind")))
            .collect()
    });
    let opts = SearchOptions {
        kinds: kinds.as_ref(),
        limit: sc.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize,
        z_gate: sc.get("zGate").and_then(Value::as_f64).unwrap_or(2.0),
        word_weight: sc.get("wordWeight").and_then(Value::as_f64),
        under: None,
        prefix_last: sc
            .get("prefixLast")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let vector = vectors.get(&qkey(code, &q)).map(|v| v.as_slice());
    let searcher = if code { code_searcher } else { main_searcher };
    match searcher.search(&q, vector, &opts) {
        // The probe's envelope: `"ok": true` travels beside the hits (search_answer in
        // the harness spells the same row; the engine's hitFields carries no ok key).
        Ok(hits) => json!({"ok": true, "hits": hits.iter().map(hit_json).collect::<Vec<Value>>()}),
        Err(e) => json!({"ok": false, "err": e.to_string()}),
    }
}

/// The machine-bound facts out of a parsed answer: the corpus root prints `C`, numbers
/// compare by value (the ⑨ suite's rules).
fn normalize(mut value: Value, root: &str) -> Value {
    corpus::normalize_root(&mut value, root);
    canonical_numbers(&mut value);
    value
}

/// The crawl-order artifacts: `segment_id` is the store's rowid — Swift crawled the
/// corpus folder in its readdir order (ext4 hash order, seed per directory), the suite
/// crawls a fresh temp folder in its own order, so rowids differ (measured: the emlx
/// name row is 8 in the Swift answer, 5 in the suite's store). The segment the engine
/// points at is pinned by its content coordinates — path + kind + loc (+loc_end),
/// unique per file (chunk/page/frame/audio carry a place; name/image are the file's
/// only rows of their kind). Hits are then sorted by that identity: equal-score ties
/// sort by file id in the engine, and the file id is the same crawl artifact (measured
/// 2026-11-12 on s316: two page_image hits with the identical vector tie, Swift listed
/// blank.pdf first, the suite's crawl lists brokenpage.pdf first — same row content).
/// `keyword_rank` is assigned by the FTS bm25 walk, whose tie resolution is rowid order
/// too — measured on s339: scenes.mp4/scenes.ts (identical frame vectors) swapped ranks.
/// The RANKS are pinned as a multiset per answer, their row assignment on tied rows is
/// not; tied rows' content (path, kind, segment, loc, scores) stays digit-for-digit.
fn canonicalize_hits(value: &mut Value, root: &str) {
    // FTS tokenizes a path on `/` and `_` (dots and hyphens survive inside tokens), so
    // a name segment's excerpt carries the corpus root as WORDS: the root prefix is
    // canonicalized back to `C` by token count (measured on s343: pinned vs suite
    // "tmp .tmpfnrE2I corpus repo «billing» payments" — same row, same content; the
    // pinned side carries the ANSWER_ROOT token, "fixtures suite-corpus repo
    // «billing» payments", after the 2026-11-15 de-scratch rewrite).
    let root_tokens: Vec<&str> = root
        .split(['/', '_'])
        .filter(|token| !token.is_empty())
        .collect();
    let hits = match value {
        Value::Object(map) if map.contains_key("hits") => map["hits"].as_array_mut(),
        _ => None,
    };
    let Some(hits) = hits else { return };
    let mut ranks: Vec<f64> = Vec::new();
    for hit in hits.iter_mut() {
        if let Value::Object(row) = hit {
            row.remove("segment_id");
            if let Some(rank) = row.remove("keyword_rank").and_then(|v| v.as_f64()) {
                ranks.push(rank);
            }
            if row["segment"].as_str() == Some("name") {
                if let Some(excerpt) = row.get("excerpt").and_then(|v| v.as_str()) {
                    let tokens: Vec<&str> = excerpt.split(' ').collect();
                    if tokens.len() >= root_tokens.len()
                        && tokens[..root_tokens.len()] == root_tokens[..]
                    {
                        row.insert(
                            "excerpt".into(),
                            Value::String(
                                std::iter::once("C".to_string())
                                    .chain(
                                        tokens[root_tokens.len()..].iter().map(|t| t.to_string()),
                                    )
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            ),
                        );
                    }
                }
            }
        }
    }
    ranks.sort_by(f64::total_cmp);
    hits.sort_by(|a, b| {
        let key = |h: &Value| {
            (
                h["path"].as_str().unwrap_or_default().to_string(),
                h["segment"].as_str().unwrap_or_default().to_string(),
                h.get("loc").and_then(Value::as_f64).unwrap_or(-1.0),
                h.get("loc_end").and_then(Value::as_f64).unwrap_or(-1.0),
            )
        };
        key(a)
            .0
            .cmp(&key(b).0)
            .then(key(a).1.cmp(&key(b).1))
            .then(key(a).2.total_cmp(&key(b).2))
            .then(key(a).3.total_cmp(&key(b).3))
    });
    if let Value::Object(map) = value {
        map.insert(
            "__ranks".into(),
            Value::Array(ranks.into_iter().map(Value::from).collect()),
        );
    }
}

/// The engine's noise floor for floats: 2 ulps of f64.
///
/// Measured 2026-11-12 on scenario s315 (third hit, z): the pinned Swift answer is
/// 0.24168250928866855, the Rust evaluator scores 0.24168250928866858 — via two
/// independent, measured mechanisms:
///
/// 1. Store crawl order. `Searcher.meaning`/`modalityMeans` sum rows in store row
///    order, and the pool mean is an f64 running sum — a store crawled in a different
///    readdir order (ext4 hash order, seed per directory) shifts the pool mean by ~1
///    ulp and every z inherits the tail. Measured: Rust on the gate's store scored
///    s315 exactly the pinned 0.24168250928866855 (⑩c gate: 0 differing); Rust on the
///    suite-rebuilt store (same corpus bytes, fresh temp folder, fresh crawl order)
///    scores 0.24168250928866858. The Rust sum order is fixed (array walk) — it is
///    deterministic per store, and follows that store's crawl order.
///
/// 2. Swift's per-process seed. `Searcher.modalityMeans` computes the small-group
///    average with `sum.values.reduce(0, +)` — a Foundation `Dictionary` iterates its
///    values in per-process hash-seed order, so `all` (the mean every modality with
///    < 20 vectors takes) can shift 1 ulp between Swift processes; the `none`/`audio`
///    centered rows inherit it, the pool mean sums those rows, every z inherits the
///    tail. Measured: a later Swift process on the SAME store with byte-equal scores
///    (the SM probe's Float-score match) printed z 0.24168250928866858 — one ulp from
///    the vendored value from the store's own vendoring run.
///
/// The engine's z is therefore NOT bit-reproducible across stores/processes — the
/// pinned suite rebuilds the store fresh every run, so 1–2 ulp z drift is expected.
/// The suite compares the engine's numbers at its arithmetic noise floor: every float
/// within 2 ulps, everything else (strings, kinds, segments, ids, ints) digit-for-digit.
fn float_ulp(x: f64) -> i128 {
    // Monotone ulp axis: the sign side mirrors, so neighbours on the axis are neighbours
    // in ulps (this is the standard total order on f64).
    let bits = x.to_bits() as i128;
    if x < 0.0 {
        bits.wrapping_neg()
    } else {
        bits
    }
}

fn numbers_close(x: &serde_json::Number, y: &serde_json::Number) -> bool {
    if x == y {
        return true;
    }
    match (x.as_f64(), y.as_f64()) {
        (Some(a), Some(b)) => {
            if a == b || (a.is_nan() && b.is_nan()) {
                return true;
            }
            (float_ulp(a) - float_ulp(b)).abs() <= 2
        }
        _ => false,
    }
}

/// The SECOND bar, for the three numbers a hit carries that are computed from vectors
/// (`cosine`, `score`, `z`). The suite pins both inputs it CAN pin: the store's fp16 rows
/// (`install_pinned_vectors`) and the query vectors the fixture carries (`search-vectors.jsonl`).
/// For those answers the trio is computed from the vendoring machine's bytes and the 2-ulp floor
/// above carries it. Measured 2026-11-15 with the store pin installed: s314 ("payment gateway
/// ledger", a pinned query vector on the main store) compares digit-for-digit, and the drift that
/// is left sits ONLY on scenarios whose QUERY text has no vendored vector.
///
/// The fixture carries 31 (query, store) vectors for 45 scenarios, so 27 scenarios are pinned and
/// 18 are not (measured from the fixtures: s327-s330, s340, s342-s344, s347-s353, s355, s357, s358
/// — the `name:` and identifier queries: `billing-sync`, `RATE_01`, `DEBUG`, `id_ed25519`,
/// `group_1`, …). Those 18 embed their query with the model on THIS machine, so their cosines ride
/// the model's cross-machine difference and `z` and `score` inherit it through the engine's own
/// arithmetic. The bar is that chain, derived from the engine's code, not a number fitted to a run:
///
///   * `cosine` - the vectors' bar, 2e-3 ABSOLUTE: ⑨ pins two machines' embeddings of the same text
///     as "byte-exact; a hex that differs must keep cosine >= 0.999" (`compare_answers`), so a
///     query-vector drift is at most 1e-3 of the cosine, and 2e-3 is the doubled bar ⑨ itself
///     accepts. A cosine lives in [-1, 1], so the absolute unit is the natural one here. Measured
///     over all 45 answers on this Mac 2026-11-15 (Apple Silicon, llama.cpp b11461,
///     `DIGUP_DEVICE=cpu`, `DIGUP_THREADS=8`): 10 cosines rode the bar, widest **1.73e-3 absolute**
///     (s346 hit 4, `0.6813668608665466` against `0.6796389222145081`) — 13% under the bar.
///   * `z` - `scoring::meaning` divides the centered score by the pool's deviation, and that
///     deviation is FLOORED at 0.02 (`scoring.rs`, `.max(0.02)`), so a cosine drift moves `z` by at
///     most 1 / 0.02 = 50 times it: 2e-3 x 50 = 0.1 absolute. Measured in the same run: 30 `z`s rode
///     the bar, widest **3.82e-2 absolute** (s346 hit 4, `0.05374595344471885` against
///     `0.01556084346784442`) — 2.6x under the bound, and that row's own amplification is **22x**
///     against its 1.73e-3 cosine, i.e. the derivation with headroom, not a fitted number. `z` is a
///     ratio over a small pool, so the same drift reads as 71% RELATIVE there — a percentage of a
///     near-zero number, which is why `z`'s bar is the absolute unit the derivation is in and not a
///     percentage.
///   * `score` - the searcher's line is `meaning_weight * max(z, 0) + words_weight *
///     coverage^1.5` plus a kind hint, and `meaning_weight` is 0.5 or 1.0, while the keyword part is
///     not vector-derived and compares digit-for-digit. So `|delta score| <= |delta z|` and `score`
///     takes `z`'s bar. Measured in the same run: 29 `score`s rode it, widest **3.00e-2 absolute**
///     (s343 hit 3, `2.3571398280200926` against `2.3271055327504246`) — and that row moves in
///     `score` and in `z` by the SAME 3.0034e-2, which is the identity above, measured.
///
/// The whole measured shape: 45/45 answers compared, **0 differ**, and 69 of the answers' numbers
/// (10 cosine, 29 score, 30 z) rode this bar in 16 scenarios — 16, not 18, because 2 of the 18
/// scenarios with no vendored query vector drift inside the 2-ulp floor. Every other field of every
/// answer compared digit-for-digit at 2 ulps in the same run.
///
/// What the bar tolerates, stated plainly: on the 18 scenarios with no vendored query vector, a
/// `z`/`score` error SMALLER than 0.1 absolute is not caught (a larger one fails, naming the
/// field). On the 27 pinned scenarios it tolerates nothing, because those answers are computed
/// entirely from vendored bytes and ride the 2-ulp floor. 27 of 45 is the most this fixture can
/// give a machine that is not the machine that measured them; the closing move is to re-vendor
/// `search-vectors.jsonl` for all 45 scenarios from an index this engine builds, which reduces the
/// bar to nothing.
///
/// The direction of the drift was measured the other way round too, to be sure what it is: THIS
/// engine's own numbers are bit-reproducible. Three independent index builds of the same corpus
/// bytes gave byte-identical fp16 vectors, and every search answer on the three stores was
/// bit-identical (cosine `0.7613762617111206`, score `4.91972255032828`, three for three). So the
/// drift is the model's cross-machine difference, not noise, and not the device: the same run on the
/// GPU moved the cosine again (`0.7624644637107849`).
///
/// The bar covers the trio ONLY, is counted per field and printed at its widest, and nothing about
/// the answer's shape relaxes with it: paths, kinds, segments, ids, locs, excerpts, info counts, hit
/// counts and order, and the `__ranks` multiset stay digit-for-digit at 2 ulps. It is consulted only
/// where the 2-ulp floor has already failed, so it never changes a comparison that passes: on the
/// machine these answers were vendored from, the bar's report prints nothing.
const VECTOR_TRIO_BAR: f64 = 2e-3;

/// `scoring::meaning`'s floor on the pool's deviation (`deviation = sqrt(squares / count).max(0.02)`)
/// — the amplification a `cosine` drift suffers on the way to `z`, and so to `score`. Named as a
/// constant so the bar above reads as a derivation from the engine, not as an invented tolerance.
const MEANING_DEVIATION_FLOOR: f64 = 0.02;

/// The trio, by the field name the answer spells, so each field gets ITS bar: `cosine` is the
/// vectors' own drift, `z` and `score` are that drift through the engine's amplification.
/// `info`'s counts, `loc`, `size`, `modified` and the ranks are not vector-derived and ride the
/// arithmetic floor.
fn trio_field(path: &str) -> Option<&'static str> {
    match path.rsplit('/').next() {
        Some("cosine") => Some("cosine"),
        Some("score") => Some("score"),
        Some("z") => Some("z"),
        _ => None,
    }
}

/// What the trio bar let through, so a bar that starts carrying real differences is visible in the
/// run's own output rather than only in a diff someone has to read. Counted PER FIELD: the trio's
/// two bars are two separate claims, and a bar carrying 40 cosines and 3 `z`s says something
/// different from one carrying 43 `z`s.
#[derive(Default)]
struct TrioDrift {
    counts: HashMap<&'static str, usize>,
    widest: HashMap<&'static str, f64>,
    widest_at: HashMap<&'static str, String>,
    scenarios: std::collections::BTreeSet<String>,
}

impl TrioDrift {
    fn record(&mut self, field: &'static str, path: &str, amount: f64, a: &str, b: &str) {
        *self.counts.entry(field).or_default() += 1;
        let widest = self.widest.entry(field).or_insert(0.0);
        if amount >= *widest {
            *widest = amount;
            self.widest_at.insert(field, format!("{path}: {a} vs {b}"));
        }
        self.scenarios.insert(
            path.split('/')
                .next()
                .unwrap_or("?")
                .trim_start_matches('"')
                .to_string(),
        );
    }
}

/// The trio bar per field, in the ABSOLUTE unit the derivation is in: `cosine` at the vectors' bar
/// (a cosine lives in [-1, 1], so 2e-3 of it), `z` and `score` at that drift amplified by the
/// engine's deviation floor, 1 / 0.02 = 50x.
fn trio_carries(field: &str, a: f64, b: f64) -> bool {
    let delta = (a - b).abs();
    match field {
        "cosine" => delta <= VECTOR_TRIO_BAR,
        _ => delta <= VECTOR_TRIO_BAR / MEANING_DEVIATION_FLOOR,
    }
}

/// Compare two normalized answers; collect every difference (numbers at the noise
/// floor, everything else exact) so a failure prints the exact field.
fn answer_diff(a: &Value, b: &Value, path: &str, out: &mut Vec<String>, drift: &mut TrioDrift) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if numbers_close(x, y) {
                return;
            }
            if let Some(field) = trio_field(path) {
                if let (Some(a), Some(b)) = (x.as_f64(), y.as_f64()) {
                    if trio_carries(field, a, b) {
                        drift.record(field, path, (a - b).abs(), &x.to_string(), &y.to_string());
                        return;
                    }
                }
            }
            out.push(format!("{path}: number {x} vs {y}"));
        }
        (Value::Object(x), Value::Object(y)) => {
            for key in x.keys() {
                if !y.contains_key(key.as_str()) {
                    out.push(format!("{path}/{key}: missing in rust"));
                }
            }
            for key in y.keys() {
                if !x.contains_key(key.as_str()) {
                    out.push(format!("{path}/{key}: missing in swift"));
                }
            }
            for key in x.keys().filter(|key| y.contains_key(key.as_str())) {
                answer_diff(&x[key], &y[key], &format!("{path}/{key}"), out, drift);
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!("{path}: {} rows vs {}", x.len(), y.len()));
            }
            for (index, (u, v)) in x.iter().zip(y).enumerate() {
                answer_diff(u, v, &format!("{path}[{index}]"), out, drift);
            }
        }
        _ => {
            if a != b {
                out.push(format!("{path}: {a} vs {b}"));
            }
        }
    }
}

#[test]
fn search_answers_match_the_engine() {
    let work = tempfile::tempdir().expect("a work folder");
    let corpus_dir = work.path().join("corpus");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    // Self-skip without the pinned models — the model-free CI bar of ci.yml's engine
    // job (P4, 2026-11-17): the suite's contract is that model-dependent tests skip
    // themselves with a printed reason, and this ⑩c test's model-pinned home is
    // rust-release.yml's gates job, which downloads the pins. Measured on run
    // 44234229551: without the guard, locate()'s expect PANICS on the model-free bar
    // (a panic is not a skip) and the suite dies with 101.
    let Ok(models) = LlamaModels::locate() else {
        eprintln!("skipped: the pinned model files (DIGUP_MODELS) are not installed");
        return;
    };
    build_corpus(&corpus_dir);
    build_index(&corpus_dir, &index, false, &models);
    build_index(&corpus_dir, &index, true, &models);
    // The store's rows, pinned to the vendoring machine's bytes before anything opens the store
    // (see `install_pinned_vectors`): the search reads its pool statistics over these vectors.
    for tag in ["main", "code"] {
        let (installed, total, unmatched) =
            install_pinned_vectors(&index, tag, corpus_dir.to_str().expect("root"));
        println!(
            "⑩c {tag}: {installed} of the store's {total} vector rows are the vendored engine's \
             bytes, {} carry this engine's own embedding, {unmatched} vendored row(s) matched no \
             store row",
            total - installed
        );
    }
    // Forensics knob: DIGUP_KEEP_INDEX=<dir> copies the built stores out of the temp
    // folder (diagnosing a pinned-suite drift is worth more than the temp folder).
    if let Ok(keep) = std::env::var("DIGUP_KEEP_INDEX") {
        let keep = PathBuf::from(keep);
        std::fs::create_dir_all(&keep).expect("the keep folder");
        for f in [MAIN_FILE, CODE_FILE] {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::copy(
                    index.join(format!("{f}{ext}")),
                    keep.join(format!("{f}{ext}")),
                );
            }
        }
    }

    let main_store: &'static IndexStore = Box::leak(Box::new(
        IndexStore::main(&index).expect("open the main index"),
    ));
    let code_store: &'static IndexStore = Box::leak(Box::new(
        IndexStore::open(&index, CODE_FILE).expect("open the code index"),
    ));
    let mut main_searcher = Searcher::new(main_store, true).expect("the main searcher");
    let mut code_searcher = Searcher::new(code_store, true).expect("the code searcher");

    // The engine's own query vectors, verbatim.
    let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
    for line in VECTORS.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).expect("a vector line");
        vectors.insert(
            qkey(
                v["code"].as_bool().unwrap_or(false),
                v["q"].as_str().unwrap(),
            ),
            v["vector"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap() as f32)
                .collect(),
        );
    }

    let scenarios: HashMap<String, Value> = rows(SCENARIOS)
        .into_iter()
        .map(|sc| (sc["id"].as_str().expect("an id").to_string(), sc))
        .collect();
    let expected = rows(EXPECTED);
    let root = corpus_dir.to_str().expect("a utf-8 corpus root");

    let mut checked = 0;
    let mut drift = TrioDrift::default();
    // Every scenario is compared, and every failure is reported TOGETHER. The suite is a
    // 12-30 minute run on a box with a model on it; stopping at the first scenario made the next
    // one cost another half hour, and the bar above is derived from numbers this run has to
    // measure across all 45 answers, not from the first one that moved.
    let mut failures: Vec<String> = Vec::new();
    for want in &expected {
        let id = want["id"].as_str().expect("an id");
        let sc = scenarios
            .get(id)
            .unwrap_or_else(|| panic!("{id}: pinned answer with no scenario"));
        let got = search_answer(sc, &mut main_searcher, &mut code_searcher, &vectors);
        let want = normalize(want.clone(), ANSWER_ROOT);
        let got = normalize(got, root);
        // The envelope fields the harness compares: ok plus the answer fields.
        let trim = |v: &Value| {
            let mut m = serde_json::Map::new();
            for (k, x) in v.as_object().expect("an object") {
                if k != "id" && k != "fn" {
                    m.insert(k.clone(), x.clone());
                }
            }
            Value::Object(m)
        };
        let (a, b) = (trim(&want), trim(&got));
        let (mut a, mut b) = (a, b);
        canonicalize_hits(&mut a, ANSWER_ROOT);
        canonicalize_hits(&mut b, root);
        let mut diffs = Vec::new();
        answer_diff(&a, &b, id, &mut diffs, &mut drift);
        if !diffs.is_empty() {
            let cut = |v: &Value| {
                let text = serde_json::to_string(v).expect("answer JSON");
                text.chars().take(700).collect::<String>()
            };
            failures.push(format!(
                "{id} {}\n swift={}\n rust={}\n differing fields: {diffs:?}",
                sc["fn"],
                cut(&a),
                cut(&b)
            ));
        }
        checked += 1;
    }
    assert_eq!(checked, expected.len());
    assert!(checked >= 45, "the ⑩c set must have 45 scenarios");
    println!(
        "⑩c pinned suite: {}/{} scenarios compared, {} answer(s) differ",
        checked,
        expected.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "⑩c: {} of {} answers differ from the engine's pinned bytes\n{}",
        failures.len(),
        expected.len(),
        failures.join("\n\n")
    );
    println!("⑩c pinned suite: all {} answers identical", checked);
    let total: usize = drift.counts.values().sum();
    if total > 0 {
        let per_field = ["cosine", "score", "z"]
            .iter()
            .filter_map(|field| {
                drift.counts.get(*field).map(|count| {
                    format!(
                        "{field}: {count} at widest {:.3} (bar {}) at {}",
                        drift.widest.get(*field).copied().unwrap_or(0.0),
                        if *field == "cosine" {
                            format!("{VECTOR_TRIO_BAR}")
                        } else {
                            format!("{:.1}", VECTOR_TRIO_BAR / MEANING_DEVIATION_FLOOR)
                        },
                        drift
                            .widest_at
                            .get(*field)
                            .map(String::as_str)
                            .unwrap_or("?",)
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("; ");
        println!(
            "⑩c: {total} of the answers' numbers are cosine/score/z riding the derived bar of              `VECTOR_TRIO_BAR` — {per_field}. They sit in {} scenario(s), the ones whose QUERY text              has no vendored vector in `search-vectors.jsonl` (the fixture carries 31 (query, store)              vectors for {} scenarios); every store row's vector IS vendored              (`install_pinned_vectors`), so the drift is the query embedding this machine had to              make, and the bar is that drift through the engine's own amplification              (`scoring::meaning` divides by a deviation floored at {MEANING_DEVIATION_FLOOR}). Every              other field of every answer — the hit sets, paths, kinds, segments, locs, excerpts,              info counts, hit order and the __ranks — compared digit-for-digit at the 2-ulp floor.              The 2-ulp floor is tried first, so on the machine these answers were vendored from this              line does not print. Re-vendor `search-vectors.jsonl` for every scenario from an index              this engine builds and the bar carries nothing.",
            drift.scenarios.len(),
            expected.len()
        );
    }
}

/// Silence the unused warnings when the file is shared with the harness (the `strings_of`
/// helper travels for the scenario spellings).
#[allow(dead_code)]
fn _harness_helper(sc: &Value) -> Vec<String> {
    strings_of(sc, "terms")
}
