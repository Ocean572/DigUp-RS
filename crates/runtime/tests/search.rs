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
//! processes on one store; both channels measured on s315). Strings, kinds, segments
//! and integers stay digit-for-digit. `segment_id` (the store's rowid) and the engine's tie order are
//! crawl-order artifacts — hits are compared as the SET of rows the engine returned,
//! each pinned by path + segment kind + loc (see `canonicalize_hits`, measured on s316);
//! `keyword_rank` is pinned as a per-answer multiset, its row assignment among tied
//! identical rows is the same rowid artifact (measured on s339). A name segment's
//! excerpt is the file's folder path FTS-tokenized on `/` and `_`, so the corpus root
//! appears as words — the root's token prefix canonicalizes to `C` (measured on s343).
//!
//! Needs the engine's environment (DIGUP_MODELS, the pinned Vendor/llama, PDFium, the
//! pinned ffmpeg 4.4.2, DIGUP_DEVICE=cpu, DIGUP_THREADS=8) — the ⑨ suite's contract; a
//! missing model is a PANIC with the pin named, never a skip (the tools pin is
//! load-bearing: a different ffmpeg changes the media segments the index answers run on).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

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

/// Compare two normalized answers; collect every difference (numbers at the noise
/// floor, everything else exact) so a failure prints the exact field.
fn answer_diff(a: &Value, b: &Value, path: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if !numbers_close(x, y) {
                out.push(format!("{path}: number {x} vs {y}"));
            }
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
                answer_diff(&x[key], &y[key], &format!("{path}/{key}"), out);
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!("{path}: {} rows vs {}", x.len(), y.len()));
            }
            for (index, (u, v)) in x.iter().zip(y).enumerate() {
                answer_diff(u, v, &format!("{path}[{index}]"), out);
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
    build_corpus(&corpus_dir);
    let models = LlamaModels::locate().expect("the pinned model files (DIGUP_MODELS)");
    build_index(&corpus_dir, &index, false, &models);
    build_index(&corpus_dir, &index, true, &models);
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
        answer_diff(&a, &b, id, &mut diffs);
        assert!(
            diffs.is_empty(),
            "{id} {}\n swift={}\n rust={}\n differing fields: {diffs:?}",
            sc["fn"],
            serde_json::to_string(&a).expect("want JSON"),
            serde_json::to_string(&b).expect("got JSON")
        );
        checked += 1;
    }
    assert_eq!(checked, expected.len());
    assert!(checked >= 45, "the ⑩c set must have 45 scenarios");
    println!("⑩c pinned suite: {} scenarios, all identical", checked);
}

/// Silence the unused warnings when the file is shared with the harness (the `strings_of`
/// helper travels for the scenario spellings).
#[allow(dead_code)]
fn _harness_helper(sc: &Value) -> Vec<String> {
    strings_of(sc, "terms")
}
