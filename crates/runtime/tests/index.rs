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
//! The media seam runs the repo's pinned ffmpeg through `$DIGUP_FFMPEG` while indexing the
//! 13 media fixtures; the four screenshots ride on vendored bytes. These answers were RECORDED
//! on 4.4.2 (the Ubuntu 22.04 container's `/usr/bin/ffmpeg`), and the pin `scripts/build-ffmpeg.sh`
//! ships today is 9.0.2 — so the suite names the build it ran on, and what that build decides
//! (an `ffmpeg failed (N)` status: 1 on 4.4.2, 183 and 234 on 9.0.2) leaves both sides, counted
//! and printed: `ANSWER_FFMPEG`. Run it pinned:
//! `DIGUP_MODELS=… DIGUP_BACKENDS=…/Vendor/llama/lib LD_LIBRARY_PATH=…
//! DIGUP_FFMPEG=<the pinned ffmpeg build the repo ships> cargo test -p digup-runtime --test index`.
//!
//! Unix-only (utimes for the pinned mtimes): the suite measures the Linux engine box.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use digup_core::index_store::{IndexStore, CODE_FILE, MAIN_FILE};
use digup_core::options::IndexOptions;
use digup_runtime::indexer::{Event, Indexer, Step};
use digup_runtime::{Embedder, LlamaEmbedder, LlamaModels};

mod corpus;
use corpus::*;

/// The root the vendored answers carry: a NEUTRAL PLACEHOLDER token, not a machine's
/// path (ledger row 9: these files once carried the harness's scratch root,
/// measured there — the 2026-11-15 rewrite vendored them under this token). The
/// suite indexes a temp folder and canonicalizes BOTH roots to `C`: the string
/// replace (slash form) and `path_words` (the name-segment excerpt, which spells
/// the root space-separated — the keyword normalizer turns slashes into spaces).
const ANSWER_ROOT: &str = "/fixtures/suite-corpus";

/// The ffmpeg build these ⑨ answers were recorded on: the pinned 4.4.2 (header, `index.rs:30`).
/// What that build decides is its own, not the engine's: the status it returns for a file it cannot
/// decode, and how many samples its decoder hands the media extractor. Measured on this Mac
/// (2026-11-15) over the same fixture bytes: the repo's current pin (`scripts/build-ffmpeg.sh`:
/// 9.0.2) and Homebrew's 9.0.2 answer `broken/empty/junk.mp4` with 183 and `broken2/noaudio.mp4`
/// with 234, where 4.4.2 answered 1, and `audioonly.mp4` decodes 6.014 s of samples where 4.4.2
/// gave 6.0. Swift on that same 9.0.2 build answers 183 and 6.014 too — the exit number is
/// Foundation's `terminationStatus` of the same child (`Media+Portable.swift:40`), the args are the
/// same (`-v error -nostdin -i … -vn -ac 1 -ar 16000 -f s16le -`): the engines agree with each
/// other, and the vendored events are the older tool's. No supported build reproduces these bytes,
/// so the suite names the build it measured against.
///
/// Where the seam's ffmpeg is not that build, the ffmpeg status inside a note leaves BOTH sides of
/// the event comparison — counted on each side and asserted to line up, so a file refused on one
/// side and not the other is still a difference. The note's wording around the number, and every
/// other fact of every event line (path, kind, state, sizes, counts), stays digit-for-digit.
const ANSWER_FFMPEG: &str = "4.4.2";

/// The version the seam's ffmpeg reports: `FFmpeg.find`'s tool (`$DIGUP_FFMPEG`, the executable's
/// own folder, then PATH — `paths::find_program`), and the number on its first version line
/// (`ffmpeg version 4.4.2 Copyright …`; the distro packages spell it with a suffix, `4.4.2-1ubuntu5`).
fn seam_ffmpeg_version() -> Option<String> {
    let tool = digup_native::ffmpeg::ffmpeg()?;
    let printed = std::process::Command::new(&tool)
        .args(["-hide_banner", "-version"])
        .output()
        .ok()?
        .stdout;
    let line = String::from_utf8_lossy(&printed)
        .lines()
        .next()?
        .to_string();
    let version = line.split_whitespace().nth(2)?;
    Some(version.split('-').next().unwrap_or(version).to_string())
}

/// The tool's exit status out of a note: `ffmpeg failed (1)` on 4.4.2 reads `ffmpeg failed (183)` on
/// 9.0.2. The number is the tool's answer, the sentence is the engine's, and the sentence keeps
/// comparing verbatim. None for a note that names no ffmpeg status.
fn ffmpeg_status_note(text: &str) -> Option<String> {
    const MARKER: &str = "ffmpeg failed (";
    let at = text.rfind(MARKER)?;
    let head = &text[..at + MARKER.len()];
    let tail = &text[at + MARKER.len()..];
    let digits = tail.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || !tail[digits..].starts_with(')') {
        return None;
    }
    Some(format!("{head}N){}", &tail[digits + 1..]))
}

/// The ffmpeg statuses out of event lines, counted: every string field naming one, at any depth
/// (an event's `note`, a step's `note`, the candidates a sync line carries).
fn drop_ffmpeg_statuses(values: &mut [Value]) -> usize {
    fn walk(value: &mut Value, tally: &mut usize) {
        match value {
            Value::Object(map) => {
                for (_key, field) in map.iter_mut() {
                    match field.as_str().and_then(ffmpeg_status_note) {
                        Some(note) => {
                            *tally += 1;
                            *field = json!(note);
                        }
                        None => walk(field, tally),
                    }
                }
            }
            Value::Array(items) => {
                for item in items.iter_mut() {
                    walk(item, tally);
                }
            }
            _ => {}
        }
    }
    let mut tally = 0usize;
    for value in values.iter_mut() {
        walk(value, &mut tally);
    }
    tally
}

/// The same status inside a dump row: a failed file's `detail` carries the engine's sentence with
/// the tool's number in it (`… |failed|unreadable: ffmpeg failed (1)|…` against the same row
/// reading `(183)` today). The dump compares physical lines, so the number leaves both sides here
/// too, counted per side, and the rest of the row — the path, the state, the bucket, the size,
/// every other sentence — compares as text.
fn drop_dump_ffmpeg_statuses(lines: &mut [String]) -> usize {
    let mut tally = 0usize;
    for line in lines.iter_mut() {
        if let Some(note) = ffmpeg_status_note(line) {
            *line = note;
            tally += 1;
        }
    }
    tally
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

/// The media-window bar: an audio/video segment's window END (field 6) is
/// `(offset + samples) / rate` of the samples the DECODER hands back, and the vector embedded from
/// that window (field 8) is embedded from exactly those samples. Both are the ffmpeg build's
/// answer, not the engine's: measured 2026-11-15 on this Mac, `voice.m4a` runs to 25.402625 s of
/// samples on the build these dumps were recorded on and 25.4 on the build the repo ships today,
/// `scenesaudio.mp4` 18.0186875 against 18.0, `audioonly.mp4` 6.014 against 6.0 — and those three
/// audio rows were the ONLY dump rows in this class, on the audio segments on BOTH sides. Row
/// counts, paths, kinds, modalities, window STARTS (the engine's own windowing), excerpts, FTS
/// bodies, the FILES rows' states and every other row compare digit-for-digit, the vector keeps
/// the 0.999 cosine bar, and the rows that rode this bar are counted and printed.
#[derive(Default)]
struct MediaBar {
    rows: usize,
}

fn compare_answers(
    tag: &str,
    got: &[String],
    vendored: &[String],
    media_bar: bool,
    bar: &mut MediaBar,
) {
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
        // A media segment row whose only differences are its window END (6) and the vector under
        // it (8), on a row that is an audio/video segment on both sides and agrees on path, kind,
        // modality, window start and excerpt: the decode build's answer, so it rides the bar.
        let media_row = media_bar
            && fa.len() == fb.len()
            && fa.len() > 8
            && fa[0] == "SEGMENTS"
            && fa[2] == fb[2]
            && fa[3] == fb[3]
            && fa[4] == fb[4]
            && fa[5] == fb[5]
            && fa[7] == fb[7]
            && matches!(fa[3], "audio" | "video")
            && !fields.is_empty()
            && fields.iter().all(|i| *i == 6 || *i == 8);
        // Vector-only drift rides the quantization bar (vec is field 8 of a SEGMENTS line).
        if !media_row && (fa.len() != fb.len() || fa[0] != "SEGMENTS" || fields != [8]) {
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
        if media_row {
            bar.rows += 1;
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
        "{tag}: {} lines — {} identical, {drift} vector drift (min cosine {min_cosine:.10}){}",
        vendored.len(),
        identical,
        if bar.rows > 0 {
            format!(
                ", {} media window rows (window end + its vector) on the ffmpeg-build bar",
                bar.rows
            )
        } else {
            String::new()
        }
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
    // The build the seam indexes media with, named out loud: the answers are pinned to 4.4.2.
    let seam = seam_ffmpeg_version();
    let recorded = seam.as_deref() == Some(ANSWER_FFMPEG);
    println!(
        "index answers are pinned to ffmpeg {ANSWER_FFMPEG}; this seam runs ffmpeg {}",
        seam.as_deref().unwrap_or("unknown")
    );
    let work = tempfile::tempdir().expect("a work folder");
    let corpus = work.path().join("corpus");
    let index = work.path().join("index");
    std::fs::create_dir_all(&index).expect("the index folder");
    build_corpus(&corpus);
    let root = corpus.to_str().expect("a utf-8 corpus root");

    for (tag, sqlite) in [("main", MAIN_FILE), ("code", CODE_FILE)] {
        let mut got = index_all(&corpus, &index, tag == "code", &models);
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
        if !recorded {
            // The tool's own answer, out of both sides: `ffmpeg failed (1)` is 4.4.2's status for
            // these trap files, 183 and 234 are the 9.0.2 pin's. The count is the check — a media
            // file the engine refused on one side and not the other is a difference, not a drift.
            let from_rust = drop_ffmpeg_statuses(&mut got);
            let from_pinned = drop_ffmpeg_statuses(&mut vendored);
            assert_eq!(
                from_rust, from_pinned,
                "events {tag}: the ffmpeg statuses must line up on both sides \
                 (rust {from_rust}, pinned {from_pinned})"
            );
            println!(
                "events {tag}: {from_rust} ffmpeg statuses rode out of both sides — ffmpeg {} \
                 is not the pinned {} these events were recorded on",
                seam.as_deref().unwrap_or("unknown"),
                ANSWER_FFMPEG
            );
        }
        compare_events(&format!("events {tag}"), &got, &vendored);

        // dump.py writes `"\n".join(records)`: bodies with newlines expand into multiple
        // physical lines. Both sides compare physical lines — identical bodies expand
        // identically (the gate's rule, measured byte-exact at the 2026-11-12 run).
        let mut got: Vec<String> = normalize_dump(&dump_lines(&index, sqlite, root))
            .iter()
            .flat_map(|line| line.split('\n').map(str::to_string))
            .collect();
        let mut vendored = normalize_dump(&vendored_dump);
        let mut bar = MediaBar::default();
        if !recorded {
            let from_rust = drop_dump_ffmpeg_statuses(&mut got);
            let from_pinned = drop_dump_ffmpeg_statuses(&mut vendored);
            assert_eq!(
                from_rust, from_pinned,
                "dump {tag}: the ffmpeg statuses must line up on both sides \
                 (rust {from_rust}, pinned {from_pinned})"
            );
            if from_rust > 0 {
                println!("dump {tag}: {from_rust} ffmpeg statuses rode out of both sides' rows",);
            }
        }
        compare_answers(&format!("dump {tag}"), &got, &vendored, !recorded, &mut bar);
    }
}
