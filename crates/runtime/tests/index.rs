//! ⑨ pinned suite: the cross-engine index build, replayed (the ⑧e media suite's pattern at
//! index scale). The corpus factory below rebuilds the ⑨ suite corpus byte-for-byte — the
//! manifest (`fixtures/index-corpus-manifest.jsonl`) pins every file's SHA-256, size and
//! mtime, and the rebuild must match before anything indexes. The Swift engine's answers
//! (measured host-to-host through the same vendored llama.cpp/PDFium and the pinned ffmpeg
//! 4.4.2) ride beside this file: `fixtures/index-{main,code}-events.jsonl` (the probe's
//! lines: crawl, sync, every event and step without its seconds, the pass tallies) and
//! `fixtures/index-{main,code}-answers.dump` (canonical dumps: files/segments+fts/counters/
//! segcounters/meta, `indexed_at` excluded, info JSON canonicalized, vectors as fp16 hex).
//!
//! Comparisons normalize the machine-bound facts: the corpus root (the Swift answers ran at
//! its fixed path, the suite indexes a temp folder — both sides print it `C`), the
//! device/inode pair (filesystem ids, `N` on both sides), number spelling (Swift's
//! JSONSerialization writes a whole-valued Double as an integer, serde_json keeps `.0` —
//! every number reads as an f64 before comparing, the equality the measured gate used),
//! and the walk order: the crawl reports files in readdir order — a property of the
//! folder's filesystem history, not of the engine, not recreatable in a fresh folder — so
//! candidates and the sync queue sort by path; the FILES lines lose their id and their
//! inode/device and sort by path; the SEGMENTS lines lose their ids (name rows are
//! created in insert order, so segment numbering rides the filesystem too — measured
//! 2026-11-12) and sort by (path, kind, loc, …). What the engine itself decides — the
//! deterministic (kind, mtime DESC) queue, the events, the per-file segment content, the
//! vectors — is compared exactly.
//! Vectors compare byte-exact; a hex that differs must keep cosine ≥ 0.999
//! (ReferenceVectorTests' bar — the audio tower quantizes with atomic reductions, so a
//! machine change can reorder them: measured 4/363 lines, min cosine 0.9978179332 across
//! container/host, 0 within one environment), and the count and minimum are printed.
//!
//! Needs the model files (skipped, with the reason printed, as `reference_vectors.rs` is).
//! The media seam runs the pinned ffmpeg 4.4.2 through `$DIGUP_FFMPEG` while indexing the
//! 13 media fixtures; the four screenshots ride on vendored bytes. Run it pinned:
//! `DIGUP_MODELS=… DIGUP_BACKENDS=…/Vendor/llama/lib LD_LIBRARY_PATH=…
//! DIGUP_FFMPEG=/tmp/opencode/ffmpeg44 cargo test -p digup-runtime --test index`.
//!
//! Unix-only (utimes for the pinned mtimes): the suite measures the Linux engine box.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use digup_core::index_store::{IndexStore, CODE_FILE, MAIN_FILE};
use digup_core::options::IndexOptions;
use digup_runtime::indexer::{Event, Indexer, Step};
use digup_runtime::{Embedder, LlamaEmbedder, LlamaModels};

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/",
            $name
        ))
    };
}
macro_rules! media_fixture {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../native/tests/fixtures/media-",
            $name
        ))
    };
}

/// The fixed mtime epoch of the corpus, 17 s apart in sorted-path order (the manifest pins
/// the offsets; the indexing order — kind, mtime DESC — is then a property of the corpus).
const STAMP: i64 = 1731000000;

/// Where the Swift answers were measured (the factory's corpus path — the vendored files
/// carry it; the suite replaces both engines' roots with `C` before comparing).
const ANSWER_ROOT: &str = "/tmp/opencode/indexcheck/suite-corpus";

const MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/index-corpus-manifest.jsonl"
));

const PHOTOS: [(&str, &[u8]); 8] = [
    ("big.jpg", fixture!("index-photo-big.jpg")),
    ("big.png", fixture!("index-photo-big.png")),
    ("gray.jpg", fixture!("index-photo-gray.jpg")),
    ("gray.tif", fixture!("index-photo-gray.tif")),
    ("five-seven.png", fixture!("index-photo-five-seven.png")),
    ("empty.png", fixture!("index-photo-empty.png")),
    ("anim.gif", fixture!("index-photo-anim.gif")),
    ("exif-o1.jpg", fixture!("index-photo-exif-o1.jpg")),
];
const PDFS: [(&str, &[u8]); 11] = [
    ("plain.pdf", fixture!("index-pdf-plain.pdf")),
    ("text.pdf", fixture!("index-pdf-text.pdf")),
    ("blank.pdf", fixture!("index-pdf-blank.pdf")),
    ("empty.pdf", fixture!("index-pdf-empty.pdf")),
    ("junk.pdf", fixture!("index-pdf-junk.pdf")),
    ("locked.pdf", fixture!("index-pdf-locked.pdf")),
    ("truncated.pdf", fixture!("index-pdf-truncated.pdf")),
    ("brokenpage.pdf", fixture!("index-pdf-brokenpage.pdf")),
    ("brokenfont.pdf", fixture!("index-pdf-brokenfont.pdf")),
    ("letters.pdf", fixture!("index-pdf-letters.pdf")),
    ("longrun.pdf", fixture!("index-pdf-longrun.pdf")),
];
const MEDIA: [(&str, &[u8]); 13] = [
    ("audioonly.mp4", media_fixture!("audioonly.mp4")),
    ("broken.mp4", media_fixture!("broken.mp4")),
    ("broken2.mp4", media_fixture!("broken2.mp4")),
    ("empty.mp4", media_fixture!("empty.mp4")),
    ("junk.mp4", media_fixture!("junk.mp4")),
    ("noaudio.mp4", media_fixture!("noaudio.mp4")),
    ("quiet.wav", media_fixture!("quiet.wav")),
    ("scenes.mp4", media_fixture!("scenes.mp4")),
    ("scenes.ts", media_fixture!("scenes.ts")),
    ("scenesaudio.mp4", media_fixture!("scenesaudio.mp4")),
    ("short.mp4", media_fixture!("short.mp4")),
    ("tone.wav", media_fixture!("tone.wav")),
    ("voice.m4a", media_fixture!("voice.m4a")),
];
/// Repo files relative to `repo/` (billing's nested package rides with them).
const REPO: [(&str, &[u8]); 9] = [
    ("retry.py", fixture!("index-repo-retry.py")),
    ("analysis.ipynb", fixture!("index-repo-analysis.ipynb")),
    ("broken.ipynb", fixture!("index-repo-broken.ipynb")),
    ("bundle.min.js", fixture!("index-repo-bundle.min.js")),
    ("id_ed25519", fixture!("index-repo-id_ed25519")),
    ("latin1.md", fixture!("index-repo-latin1.md")),
    ("blocks.c", fixture!("index-repo-blocks.c")),
    (
        "billing/README.md",
        fixture!("index-repo-billing-README.md"),
    ),
    (
        "billing/payments/retry.py",
        fixture!("index-repo-billing-payments-retry.py"),
    ),
];
const DOCS: [(&str, &[u8]); 6] = [
    ("prose.txt", fixture!("index-doc-prose.txt")),
    ("digits.txt", fixture!("index-doc-digits.txt")),
    ("big.txt", fixture!("index-doc-big.txt")),
    ("notes.md", fixture!("index-doc-notes.md")),
    ("stub.txt", fixture!("index-doc-stub.txt")),
    ("receipt.emlx", fixture!("index-doc-receipt.emlx")),
];
/// The four lavfi screenshots, vendored (the factory ran the pinned ffmpeg 4.4.2 once).
/// Test-time lavfi is impossible: `gradients` is NOT deterministic across runs even with a
/// pinned seed — measured on this box, three runs of `size=900x1600:seed=11` gave
/// 46588/46451/47649 bytes (the seed fixes the colors, not the geometry). The manifest
/// pins these exact PNGs, so the suite replays them as fixtures.
const SHOTS: [(&str, &[u8]); 4] = [
    (
        "Screenshot-Desk.png",
        fixture!("index-shot-Screenshot-Desk.png"),
    ),
    (
        "CleanShot-Map.png",
        fixture!("index-shot-CleanShot-Map.png"),
    ),
    ("SCR-042.png", fixture!("index-shot-SCR-042.png")),
    (
        "screenshot tiny.png",
        fixture!("index-shot-screenshot tiny.png"),
    ),
];

/// The generated repo texts (gen-corpus.py spells the same formats): the pyproject marker,
/// a numbers-only module, a data dump, and an amalgam past maxCodeBytes (1 MB) — the code
/// rules read it as machine-made and leave it out.
fn generated_repo_files() -> Vec<(&'static str, Vec<u8>)> {
    let pyproject = "[project]\nname = \"billing-sync\"\nversion = \"0.3.1\"\nrequires-python = \">=3.10\"\ndependencies = [\"northline>=2\", \"requests>=2.31\"]\n";
    let mut constants = String::new();
    for row in 0..14 {
        for col in 0..9 {
            constants.push_str(&format!(
                "RATE_{row:02} = ({:.4}, {:.4}, {:.4})\n",
                col as f64 * 1.5,
                col as f64 * 2.5,
                col as f64 * 3.5
            ));
        }
    }
    let mut csv = String::new();
    for row in 0..400 {
        csv.push_str(&format!(
            "{row},{},{:.2},{}\n",
            row * 3,
            row as f64 * 7.5,
            row * 11
        ));
    }
    let mut amalgam = String::with_capacity(1_150_000);
    for row in 0..40_000i64 {
        amalgam.push_str(&format!(
            "VALUE_{row:05} = {}, {}, {}\n",
            row * 13 % 997,
            row * 29 % 991,
            row * 7 % 883
        ));
    }
    vec![
        ("repo/pyproject.toml", pyproject.as_bytes().to_vec()),
        ("repo/constants.py", constants.into_bytes()),
        ("repo/data.csv", csv.into_bytes()),
        ("repo/amalgam.py", amalgam.into_bytes()),
    ]
}

fn set_mtime(path: &Path, when: i64) {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("utf-8 path");
    let times = [
        libc::timeval {
            tv_sec: when,
            tv_usec: 0,
        },
        libc::timeval {
            tv_sec: when,
            tv_usec: 0,
        },
    ];
    assert_eq!(
        unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) },
        0,
        "utimes({path:?})"
    );
}

fn sha256(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path).expect("the corpus file just written"));
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn collect_paths(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for entry in std::fs::read_dir(&folder).expect("the corpus folder") {
            let path = entry.expect("a corpus entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn write_corpus_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("create the folder");
    std::fs::write(path, bytes).expect("write the corpus file");
}

/// Rebuild the suite corpus, stamp the fixed mtimes, and prove byte-identity against the
/// manifest the Swift answers were measured over: every size, mtime and SHA-256 must match.
fn build_corpus(root: &Path) -> Vec<PathBuf> {
    for (name, bytes) in PHOTOS {
        write_corpus_file(&root.join("photos").join(name), bytes);
    }
    for (name, bytes) in PDFS {
        write_corpus_file(&root.join("pdf").join(name), bytes);
    }
    for (name, bytes) in MEDIA {
        write_corpus_file(&root.join("media").join(name), bytes);
    }
    for (name, bytes) in REPO {
        write_corpus_file(&root.join("repo").join(name), bytes);
    }
    for (name, bytes) in DOCS {
        write_corpus_file(&root.join("docs").join(name), bytes);
    }
    for (name, bytes) in generated_repo_files() {
        write_corpus_file(&root.join(name), &bytes);
    }
    for (name, bytes) in SHOTS {
        write_corpus_file(&root.join("Screenshots").join(name), bytes);
    }

    let paths = collect_paths(root);
    for (offset, path) in paths.iter().enumerate() {
        set_mtime(path, STAMP + 17 * offset as i64);
    }
    let rows: Vec<Value> = MANIFEST
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("a manifest line"))
        .collect();
    assert_eq!(
        rows.len(),
        paths.len(),
        "the manifest covers every corpus file"
    );
    for row in &rows {
        let name = row["name"].as_str().expect("a name");
        let path = root.join(name);
        let meta = std::fs::metadata(&path).expect("the corpus file the factory wrote");
        assert_eq!(
            meta.len() as i64,
            row["size"].as_i64().expect("a size"),
            "{name} size"
        );
        let mtime = meta
            .modified()
            .expect("a mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a positive mtime")
            .as_secs() as i64;
        assert_eq!(
            mtime,
            row["mtime"].as_i64().expect("an mtime"),
            "{name} mtime"
        );
        assert_eq!(
            sha256(&path),
            row["sha256"].as_str().expect("a digest"),
            "{name} sha256"
        );
    }
    paths
}

// MARK: Index + emit (the probe's lines)

fn models_or_skip() -> Option<LlamaModels> {
    if cfg!(llama_unavailable) {
        eprintln!("skipped: llama.cpp is not vendored (scripts/build-llama.sh).");
        return None;
    }
    match LlamaModels::locate() {
        Ok(models) if models.media.is_some() => Some(models),
        _ => {
            eprintln!("skipped: the EmbeddingGemma 2 files are not downloaded (the index suite needs the model).");
            None
        }
    }
}

/// Machine-bound facts out of one emitted line: the corpus root in any string (paths, and
/// the `leftOut`/`datasets`/`skippedFolders` keys, which ARE folder paths), and the
/// inode/device pair — filesystem ids, `N` on both sides.
fn normalize_value(value: &mut Value, root: &str) {
    match value {
        Value::Object(map) => {
            let renamed: Vec<String> = map
                .keys()
                .filter(|key| key.contains(root))
                .cloned()
                .collect();
            for key in renamed {
                let item = map.remove(&key).expect("the key just listed");
                map.insert(key.replace(root, "C"), item);
            }
            for (key, item) in map.iter_mut() {
                if key == "inode" || key == "device" {
                    *item = json!("N");
                } else {
                    normalize_value(item, root);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_value(item, root);
            }
        }
        Value::String(text) if text.contains(root) => *text = text.replace(root, "C"),
        _ => {}
    }
}

/// Swift's JSONSerialization writes a whole-valued Double as an integer, serde_json keeps
/// `.0`; the measured gate parsed both and compared numbers by value. Same rule here.
fn canonical_numbers(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for item in map.values_mut() {
                canonical_numbers(item);
            }
        }
        Value::Array(items) => {
            for item in items {
                canonical_numbers(item);
            }
        }
        Value::Number(number) => {
            if let Some(float) = number.as_f64() {
                *value = Value::from(float);
            }
        }
        _ => {}
    }
}

fn as_array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Sort the crawl candidates and the sync queue by path.
///
/// The crawl reports files in *walk order* — filesystem readdir order, a property of the
/// folder's own history (which folder the engine measured at, when, and how it was
/// created), never the engine's. Indexing order (kind, mtime DESC) is what drives the
/// events, and it rides on the pinned mtimes; the walk order cannot be recreated in a
/// fresh temp folder, so the suite normalizes it away: candidates sort by path, `queued`
/// id-lists become sorted path lists, file ids print `N` (their numbers come from insert
/// order — the mapping to paths is the fact, and it is compared). Segment ids stay exact:
/// the engine assigns them while processing the deterministic queue, so they do not
/// depend on the filesystem.
fn order_by_paths(lines: &mut [Value], files: &BTreeMap<i64, String>) {
    for line in lines {
        let kind = line["type"].as_str().unwrap_or("");
        match kind {
            "crawl" => {
                let mut candidates = as_array(line, "candidates");
                candidates.sort_by_key(|c| c["path"].as_str().unwrap_or("").to_string());
                line["candidates"] = Value::Array(candidates);
            }
            "sync" => {
                let mut queued: Vec<String> = as_array(line, "queued")
                    .iter()
                    .filter_map(|id| id.as_f64().map(|f| f as i64))
                    .map(|id| files.get(&id).cloned().unwrap_or_else(|| "?".to_string()))
                    .collect();
                queued.sort();
                line["queued"] = json!(queued);
            }
            _ => {}
        }
    }
}

/// The file-id → path map from dump lines (paths already root→C by the dump's spelling).
fn dump_file_map(lines: &[String]) -> BTreeMap<i64, String> {
    let mut map = BTreeMap::new();
    for line in lines {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.first().map(|head| *head == "FILES").unwrap_or(false) && parts.len() > 2 {
            if let Ok(id) = parts[1].parse::<i64>() {
                map.insert(id, parts[2].to_string());
            }
        }
    }
    map
}

/// Walk-order facts out of a dump, one spelling for both sides (see `order_by_paths`).
///
/// FILES loses its id (insert order is walk order; the line carries its path) and its
/// inode/device, and sorts by path. SEGMENTS loses the file id (the line carries the
/// path) AND its own id, and sorts by (path, kind, modality, loc, loc_end, excerpt):
/// `name` rows are created at insert time, in walk order, so the id numbering rides the
/// filesystem too — measured on the 2026-11-12 run, segment 14 was `pdf/brokenpage` in
/// Swift's folder and `media/scenesaudio` in a fresh one, the ids counting name rows.
/// The queue content itself is deterministic (the events proved it): same file → same
/// segments, so the sorted lines pair up exactly.
fn normalize_dump(lines: &[String]) -> Vec<String> {
    let files = dump_file_map(lines);
    let spell = |parts: &[&str]| -> String {
        let id: i64 = parts[2].parse().expect("a file id");
        let path = files.get(&id).cloned().unwrap_or_else(|| "?".to_string());
        format!("SEGMENTS|N|{path}|{}", parts[3..].join("|"))
    };
    let mut others: Vec<String> = Vec::new();
    let mut files_lines: Vec<(String, String)> = Vec::new();
    let mut names: Vec<(String, String)> = Vec::new();
    let mut content: Vec<String> = Vec::new();
    // Inside the content section (between the first content line and the counters block),
    // an unlabeled line is a body's newline continuation: it belongs to the content stream
    // and rides right after its segment's first line, never into the meta/counters blocks.
    let mut in_content = false;
    for line in lines {
        let parts: Vec<&str> = line.split('|').collect();
        if parts
            .first()
            .map(|head| *head == "COUNTERS")
            .unwrap_or(false)
        {
            in_content = false;
        }
        match parts.first().copied().unwrap_or("") {
            "FILES" if parts.len() > 11 => {
                let mut normalized: Vec<&str> = vec!["FILES", "N"];
                normalized.extend(&parts[2..6]);
                normalized.extend(["N", "N"]);
                normalized.extend(&parts[8..]);
                files_lines.push((parts[2].to_string(), normalized.join("|")));
            }
            "SEGMENTS" if parts.len() > 11 && parts[3] == "name" => {
                // Name rows: created at insert time (walk order), one physical line each.
                // They sort by the normalized line — SEGMENTS|N|<path>|… — i.e. by path.
                let normalized = spell(&parts);
                names.push((normalized.clone(), normalized));
            }
            "SEGMENTS" if parts.len() > 8 => {
                // Content rows: created while the deterministic queue runs, so their
                // encounter order IS the engine's order — kept as-is (their bodies'
                // newline continuations ride with them).
                content.push(spell(&parts));
                in_content = true;
            }
            _ if in_content => content.push(line.clone()), // a body's continuation line
            _ => others.push(line.clone()),
        }
    }
    files_lines.sort();
    names.sort();
    // Blocks where the dump puts them: USER_VERSION/META, name rows, content, counters.
    let split = others
        .iter()
        .position(|line| line.starts_with("COUNTERS"))
        .unwrap_or(others.len());
    let mut out: Vec<String> = others[..split].to_vec();
    out.extend(names.into_iter().map(|(_key, line)| line));
    out.extend(content);
    out.extend(others[split..].to_vec());
    out.extend(files_lines.into_iter().map(|(_path, line)| line));
    out
}

/// The root in path-word spelling (components joined by spaces): the `name` rows' fts
/// body is the file's folder path run through the keyword normalizer, which turns the
/// slashes into spaces — `C` must be spelled that way there too.
fn path_words(root: &str) -> String {
    root.split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
}

/// The file-id → path map (root spelled `C`) for the walk-order normalization.
fn file_paths(index: &Path, sqlite: &str, root: &str) -> BTreeMap<i64, String> {
    let db = rusqlite::Connection::open(index.join(sqlite)).expect("open the index");
    let mut rows = db
        .prepare("SELECT id, path FROM files")
        .expect("files for the id map");
    let map = rows
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?.replace(root, "C"),
            ))
        })
        .expect("id/path rows")
        .map(|row| row.expect("an id/path row"))
        .collect();
    map
}

/// Run the whole indexing (sync → run → finish_long_files) as probe.swift emits it; the
/// seconds are timing, not state — the events carry none.
fn index_all(corpus: &Path, index: &Path, code: bool, models: &LlamaModels) -> Vec<Value> {
    let root = corpus.to_str().expect("a utf-8 corpus root");
    let store = if code {
        IndexStore::open(index, CODE_FILE).expect("open the code store")
    } else {
        IndexStore::main(index).expect("open the main store")
    };
    let options = IndexOptions {
        code,
        ..IndexOptions::default()
    };
    let models = models.clone();
    let make = move || -> digup_runtime::Result<Box<dyn Embedder>> {
        Ok(Box::new(LlamaEmbedder::new(&models, false, None, None)?))
    };
    let mut indexer = Indexer::new(store, options, Box::new(make)).expect("the indexer");
    let mut lines: Vec<Value> = Vec::new();

    let summary = indexer
        .sync(&[corpus.to_path_buf()], &mut |_| {})
        .expect("sync");
    let report = &summary.crawl;
    lines.push(json!({
        "type": "crawl",
        "candidates": report.candidates.iter().map(|file| json!({
            "path": file.path, "kind": file.kind.as_str(), "size": file.size,
            "mtime": file.mtime, "inode": file.inode, "device": file.device,
        })).collect::<Vec<Value>>(),
        "datasets": serde_json::to_value(&report.datasets).expect("datasets JSON"),
        "leftOut": serde_json::to_value(&report.left_out).expect("leftOut JSON"),
        "skipped": serde_json::to_value(&report.skipped).expect("skipped JSON"),
        "skippedFolders": serde_json::to_value(&report.skipped_folders).expect("skippedFolders JSON"),
    }));
    lines.push(json!({
        "type": "sync", "added": summary.added, "changed": summary.changed,
        "moved": summary.moved, "removed": summary.removed, "unchanged": summary.unchanged,
        "queued": summary.queued,
    }));
    let pass1 = indexer
        .run(None, None, &|| false, &mut |event: &Event| {
            lines.push(json!({
                "type": "event", "done": event.done, "total": event.total, "path": event.path,
                "kind": event.kind.as_str(), "state": event.state, "note": event.note,
            }));
        })
        .expect("run");
    let pass2 = indexer
        .finish_long_files(&|| false, &mut |step: &Step| {
            lines.push(json!({
                "type": "step", "done": step.done, "total": step.total, "files": step.files,
                "path": step.path, "kind": step.kind.as_str(), "state": step.state,
                "note": step.note,
            }));
        })
        .expect("finish long files");
    lines.push(json!({"type": "states", "pass1": pass1, "pass2": pass2}));
    indexer.finish();
    for line in &mut lines {
        normalize_value(line, root);
        canonical_numbers(line);
    }
    let files = file_paths(index, if code { CODE_FILE } else { MAIN_FILE }, root);
    order_by_paths(&mut lines, &files);
    lines
}

// MARK: Dump (dump.py's canonical form)

fn cell(value: rusqlite::types::Value) -> String {
    use rusqlite::types::Value as Sql;
    match value {
        Sql::Null => "~".to_string(),
        Sql::Integer(n) => n.to_string(),
        // Python's `repr(float)` — what dump.py spells — is Rust's shortest round-trip
        // Debug for every value under 1e16 (the dump's range: mtimes, page numbers,
        // seconds), and Debug keeps the `.0` a whole number needs (`1731000017.0`).
        // `swift_double` is the *JSON* spelling (`1`), which is not what the dump says.
        Sql::Real(x) => format!("{x:?}"),
        Sql::Text(text) => text,
        Sql::Blob(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// Foundation's JSONEncoder escapes `/` as `\/` and serde_json does not — the documents are
/// equal, so `info` prints canonically: parsed, sorted keys, the `": "`/`", "` spacing
/// Python's json.dumps gives (the dump spells it that way on both sides).
fn canonical_json(text: &str) -> String {
    let value: Value = serde_json::from_str(text).expect("stored JSON");
    fn emit(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                out.push('{');
                for (index, (key, item)) in map.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&serde_json::to_string(key).expect("a JSON key"));
                    out.push_str(": ");
                    emit(item, out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    emit(item, out);
                }
                out.push(']');
            }
            _ => out.push_str(&serde_json::to_string(value).expect("a JSON value")),
        }
    }
    let mut out = String::new();
    emit(&value, &mut out);
    out
}

fn dump_lines(index: &Path, file: &str, root: &str) -> Vec<String> {
    let db = rusqlite::Connection::open(index.join(file)).expect("open the index for the dump");
    let mut lines = Vec::new();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("user_version");
    lines.push(format!("USER_VERSION|{version}"));

    let mut rows = db
        .prepare("SELECT key, value FROM meta ORDER BY key")
        .expect("meta");
    for row in rows
        .query_map([], |row| {
            Ok([
                row.get::<_, rusqlite::types::Value>(0)?,
                row.get::<_, rusqlite::types::Value>(1)?,
            ])
        })
        .expect("meta rows")
    {
        let row = row.expect("a meta row");
        lines.push(format!(
            "META|{}|{}",
            cell(row[0].clone()),
            cell(row[1].clone())
        ));
    }

    let mut rows = db
        .prepare(
            "SELECT id, path, kind, size, mtime, inode, device, bucket, state, detail, info \
             FROM files ORDER BY id",
        )
        .expect("files");
    for row in rows
        .query_map([], |row| {
            let mut cells = Vec::with_capacity(11);
            for i in 0..11 {
                cells.push(row.get::<_, rusqlite::types::Value>(i)?);
            }
            Ok(cells)
        })
        .expect("files rows")
    {
        let row = row.expect("a files row");
        // `info` carries folder facts too (a code file's info names its repo root) —
        // the canonical JSON gets the same `C` treatment as the path columns.
        let info = match &row[10] {
            rusqlite::types::Value::Text(text) => canonical_json(text).replace(root, "C"),
            other => cell(other.clone()),
        };
        lines.push(format!(
            "FILES|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            cell(row[0].clone()),
            cell(row[1].clone()).replace(root, "C"),
            cell(row[2].clone()),
            cell(row[3].clone()),
            cell(row[4].clone()),
            cell(row[5].clone()),
            cell(row[6].clone()),
            cell(row[7].clone()).replace(root, "C"),
            cell(row[8].clone()),
            cell(row[9].clone()).replace(root, "C"),
            info,
        ));
    }

    let mut rows = db
        .prepare(
            "SELECT s.id, s.file, s.kind, s.modality, s.loc, s.loc_end, s.excerpt, s.vec, \
             f.name, f.body, f.original \
             FROM segments s LEFT JOIN fts f ON f.rowid = s.id ORDER BY s.id",
        )
        .expect("segments");
    for row in rows
        .query_map([], |row| {
            let mut cells = Vec::with_capacity(11);
            for i in 0..11 {
                cells.push(row.get::<_, rusqlite::types::Value>(i)?);
            }
            Ok(cells)
        })
        .expect("segments rows")
    {
        let row = row.expect("a segments row");
        lines.push(format!(
            "SEGMENTS|{}",
            row.iter()
                .map(|value| cell(value.clone()))
                .collect::<Vec<String>>()
                .join("|")
        ));
    }

    for (label, sql, columns) in [
        (
            "COUNTERS",
            "SELECT bucket, kind, state, n, leftsum FROM counters ORDER BY bucket, kind, state",
            5,
        ),
        (
            "SEGCOUNTS",
            "SELECT bucket, hasvec, n FROM segcounters ORDER BY bucket, hasvec",
            3,
        ),
    ] {
        let mut rows = db.prepare(sql).expect(label);
        for row in rows
            .query_map([], |row| {
                let mut cells = Vec::with_capacity(columns);
                for i in 0..columns {
                    cells.push(row.get::<_, rusqlite::types::Value>(i)?);
                }
                Ok(cells)
            })
            .expect("counter rows")
        {
            let row = row.expect("a counter row");
            lines.push(format!(
                "{label}|{}",
                row.iter()
                    .map(|value| cell(value.clone()).replace(root, "C"))
                    .collect::<Vec<String>>()
                    .join("|")
            ));
        }
    }
    // The fts `name` rows carry the folder path in path-word spelling (see `path_words`).
    let words = path_words(root);
    lines
        .into_iter()
        .map(|line| line.replace(&words, "C"))
        .collect()
}

// MARK: Compare

fn f16(hex: &str) -> Vec<f64> {
    hex.as_bytes()
        .chunks(4)
        .map(|chunk| {
            let digits = std::str::from_utf8(chunk).expect("hex text");
            let bits = u16::from_str_radix(digits, 16).expect("a half float");
            half::f16::from_le_bytes(bits.to_le_bytes()).to_f64()
        })
        .collect()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn compare_events(tag: &str, got: &[Value], vendored: &[Value]) {
    assert_eq!(vendored.len(), got.len(), "{tag}: event line count");
    let mut differing = 0;
    let mut samples = Vec::new();
    for (a, b) in vendored.iter().zip(got) {
        if a == b {
            continue;
        }
        differing += 1;
        if samples.len() < 4 {
            // Point at the fact that differs (the xcheck driver's rule): the top-level key
            // whose value disagrees, with the whole values spelled out.
            let mut keys: Vec<&String> = a.as_object().into_iter().flat_map(|m| m.keys()).collect();
            keys.sort();
            keys.dedup();
            let key = keys
                .into_iter()
                .find(|key| a.get(key) != b.get(key))
                .cloned()
                .unwrap_or_else(|| "?".to_string());
            samples.push(format!(
                "   [{}] swift={}\n   {:>5} rust  ={}",
                key,
                a.get(&key)
                    .map(Value::to_string)
                    .unwrap_or_default()
                    .chars()
                    .take(400)
                    .collect::<String>(),
                "",
                b.get(&key)
                    .map(Value::to_string)
                    .unwrap_or_default()
                    .chars()
                    .take(400)
                    .collect::<String>()
            ));
        }
    }
    assert!(
        samples.is_empty(),
        "{tag}: {differing} of {} event lines differ\n{}",
        vendored.len(),
        samples.join("\n")
    );
    println!("{tag}: {} lines, 0 differing", vendored.len());
}

fn compare_answers(tag: &str, got: &[String], vendored: &[String]) {
    assert_eq!(vendored.len(), got.len(), "{tag}: dump line count");
    let mut identical = 0;
    let mut drift = 0;
    let mut non_vector = 0;
    let mut min_cosine = 1.0f64;
    let mut samples = Vec::new();
    for (a, b) in vendored.iter().zip(got) {
        if a == b {
            identical += 1;
            continue;
        }
        let fa: Vec<&str> = a.split('|').collect();
        let fb: Vec<&str> = b.split('|').collect();
        let fields: Vec<usize> = (0..fa.len().min(fb.len()))
            .filter(|i| fa[*i] != fb[*i])
            .collect();
        // Vector-only drift rides the quantization bar (vec is field 8 of a SEGMENTS line).
        if fa.len() != fb.len() || fa[0] != "SEGMENTS" || fields != [8] {
            non_vector += 1;
            if samples.len() < 4 {
                samples.push(format!(
                    "   swift={}\n   rust  ={}",
                    a.chars().take(200).collect::<String>(),
                    b.chars().take(200).collect::<String>()
                ));
            }
            continue;
        }
        drift += 1;
        let (x, y) = (f16(fa[8]), f16(fb[8]));
        let dot: f64 = x.iter().zip(&y).map(|(p, q)| p * q).sum();
        let norm: f64 = x.iter().map(|p| p * p).sum::<f64>().sqrt()
            * y.iter().map(|q| q * q).sum::<f64>().sqrt();
        if norm > 0.0 {
            min_cosine = min_cosine.min(dot / norm);
        }
    }
    assert!(
        samples.is_empty(),
        "{tag}: {non_vector} dump lines differ in non-vector fields\n{}",
        samples.join("\n")
    );
    assert!(
        min_cosine >= 0.999,
        "{tag}: {drift} vector lines drift below the 0.999 bar, measured min cosine {min_cosine:.10}"
    );
    println!(
        "{tag}: {} lines — {} identical, {drift} vector drift (min cosine {min_cosine:.10})",
        vendored.len(),
        identical
    );
}

/// The crawl + sync lines, checked without a model (sync touches neither database
/// contents nor the embedder) — the fast loop for the corpus factory's shape.
#[test]
fn crawl_line_matches() {
    let work = tempfile::tempdir().expect("a work folder");
    let corpus = work.path().join("corpus");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    build_corpus(&corpus);
    let root = corpus.to_str().expect("a utf-8 corpus root");
    let store = IndexStore::main(&index).expect("open the store");
    let nowhere = index.clone();
    let make = move || -> digup_runtime::Result<Box<dyn Embedder>> {
        Err(digup_runtime::Error::io(
            &nowhere,
            std::io::Error::other("no model here"),
        ))
    };
    let indexer =
        Indexer::new(store, IndexOptions::default(), Box::new(make)).expect("the indexer");
    let summary = indexer
        .sync(std::slice::from_ref(&corpus), &mut |_| {})
        .expect("sync");
    let report = &summary.crawl;
    let mut lines = vec![
        json!({
            "type": "crawl",
            "candidates": report.candidates.iter().map(|file| json!({
                "path": file.path, "kind": file.kind.as_str(), "size": file.size,
                "mtime": file.mtime, "inode": file.inode, "device": file.device,
            })).collect::<Vec<Value>>(),
            "datasets": serde_json::to_value(&report.datasets).expect("datasets JSON"),
            "leftOut": serde_json::to_value(&report.left_out).expect("leftOut JSON"),
            "skipped": serde_json::to_value(&report.skipped).expect("skipped JSON"),
            "skippedFolders": serde_json::to_value(&report.skipped_folders).expect("skippedFolders JSON"),
        }),
        json!({
            "type": "sync", "added": summary.added, "changed": summary.changed,
            "moved": summary.moved, "removed": summary.removed, "unchanged": summary.unchanged,
            "queued": summary.queued,
        }),
    ];
    for line in &mut lines {
        normalize_value(line, root);
        canonical_numbers(line);
    }
    let files = file_paths(&index, MAIN_FILE, root);
    order_by_paths(&mut lines, &files);
    let vendored_dump: Vec<String> =
        std::fs::read_to_string(fixture_path("index-main-answers.dump"))
            .expect("vendored answers")
            .lines()
            .map(|line| line.replace(ANSWER_ROOT, "C"))
            .collect();
    let vendored_files = dump_file_map(&vendored_dump);
    let vendored: Vec<Value> = std::fs::read_to_string(fixture_path("index-main-events.jsonl"))
        .expect("vendored events")
        .lines()
        .filter(|line| !line.is_empty())
        .take(2)
        .map(|line| {
            let mut value: Value = serde_json::from_str(line).expect("an answer line");
            normalize_value(&mut value, ANSWER_ROOT);
            canonical_numbers(&mut value);
            value
        })
        .collect();
    let vendored: Vec<Value> = {
        let mut lines = vendored;
        order_by_paths(&mut lines, &vendored_files);
        lines
    };
    let one = vendored[0]["candidates"].as_array().expect("candidates");
    let two = lines[0]["candidates"].as_array().expect("candidates");
    for (i, (x, y)) in one.iter().zip(two).enumerate() {
        if x != y {
            println!("candidate {i}:\n   swift={}\n   rust  ={}", x, y);
        }
    }
    println!("lengths swift={} rust={}", one.len(), two.len());
    compare_events("crawl+sync", &lines, &vendored);
}

#[test]
fn cross_engine_index_build() {
    let Some(models) = models_or_skip() else {
        return;
    };
    let work = tempfile::tempdir().expect("a work folder");
    let corpus = work.path().join("corpus");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    build_corpus(&corpus);
    let root = corpus.to_str().expect("a utf-8 corpus root");

    for (tag, sqlite) in [("main", MAIN_FILE), ("code", CODE_FILE)] {
        let got = index_all(&corpus, &index, tag == "code", &models);
        let answer_words = path_words(ANSWER_ROOT);
        let vendored_dump: Vec<String> =
            std::fs::read_to_string(fixture_path(&format!("index-{tag}-answers.dump")))
                .expect("vendored answers")
                .lines()
                .map(|line| {
                    line.replace(ANSWER_ROOT, "C")
                        .replace(answer_words.as_str(), "C")
                })
                .collect();
        let vendored_files = dump_file_map(&vendored_dump);
        let vendored: Vec<Value> =
            std::fs::read_to_string(fixture_path(&format!("index-{tag}-events.jsonl")))
                .expect("vendored events")
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| {
                    let mut value: Value = serde_json::from_str(line).expect("an answer line");
                    normalize_value(&mut value, ANSWER_ROOT);
                    canonical_numbers(&mut value);
                    value
                })
                .collect();
        let mut vendored = vendored;
        order_by_paths(&mut vendored, &vendored_files);
        compare_events(&format!("events {tag}"), &got, &vendored);

        // dump.py writes `"\n".join(records)`: bodies with newlines expand into multiple
        // physical lines. Both sides compare physical lines — identical bodies expand
        // identically (the gate's rule, measured byte-exact at the 2026-11-12 run).
        let got: Vec<String> = normalize_dump(&dump_lines(&index, sqlite, root))
            .iter()
            .flat_map(|line| line.split('\n').map(str::to_string))
            .collect();
        let vendored = normalize_dump(&vendored_dump);
        compare_answers(&format!("dump {tag}"), &got, &vendored);
    }
}
