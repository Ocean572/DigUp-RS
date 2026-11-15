// ⑨ corpus factory: rebuilds the suite corpus byte-for-byte (the ⑩c harness reuses
// these exact functions through `#[path]`; `index.rs` keeps the pinned suite that runs
// them). The manifest (`fixtures/index-corpus-manifest.jsonl`) pins every file's SHA-256,
// size and mtime, and the rebuild must match before anything indexes.
//
// The `fixture!`/`media_fixture!` macros read through `CARGO_MANIFEST_DIR`, so the module
// compiles from `crates/runtime/tests` (the suite) and from a folder that carries the
// same relative layout (`tests/fixtures`, `../native/tests/fixtures`) — the ⑩c harness.
// Unix-only (utimes for the pinned mtimes).

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

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
pub const STAMP: i64 = 1731000000;

pub const MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/index-corpus-manifest.jsonl"
));

pub const PHOTOS: [(&str, &[u8]); 8] = [
    ("big.jpg", fixture!("index-photo-big.jpg")),
    ("big.png", fixture!("index-photo-big.png")),
    ("gray.jpg", fixture!("index-photo-gray.jpg")),
    ("gray.tif", fixture!("index-photo-gray.tif")),
    ("five-seven.png", fixture!("index-photo-five-seven.png")),
    ("empty.png", fixture!("index-photo-empty.png")),
    ("anim.gif", fixture!("index-photo-anim.gif")),
    ("exif-o1.jpg", fixture!("index-photo-exif-o1.jpg")),
];
pub const PDFS: [(&str, &[u8]); 11] = [
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
pub const MEDIA: [(&str, &[u8]); 13] = [
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
pub const REPO: [(&str, &[u8]); 9] = [
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
pub const DOCS: [(&str, &[u8]); 6] = [
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
pub const SHOTS: [(&str, &[u8]); 4] = [
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
pub fn generated_repo_files() -> Vec<(&'static str, Vec<u8>)> {
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

pub fn set_mtime(path: &Path, when: i64) {
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

pub fn sha256(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path).expect("the corpus file just written"));
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn collect_paths(root: &Path) -> Vec<PathBuf> {
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

pub fn write_corpus_file(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("create the folder");
    std::fs::write(path, bytes).expect("write the corpus file");
}

/// Rebuild the suite corpus, stamp the fixed mtimes, and prove byte-identity against the
/// manifest the Swift answers were measured over: every size, mtime and SHA-256 must match.
pub fn build_corpus(root: &Path) -> Vec<PathBuf> {
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

/// The whole indexing (sync → run → finish_long_files → finish), silent (the ⑨ suite's
/// `index_all` spells the same recipe with its event lines; the ⑩c harness and suite
/// need the effect only). Fully qualified paths so the module stays import-free for
/// its two homes: `crates/runtime/tests` and the searchcheck harness's `include!`.
// Shared test-module helper: `mod corpus` is compiled into several test
// targets (index.rs, search.rs, the corpus target itself); this target
// exercises others, so these helpers are dead HERE and live in siblings.
#[allow(dead_code)] // shared helpers: live in the sibling targets of `mod corpus`
pub fn build_index(
    corpus: &std::path::Path,
    index: &std::path::Path,
    code: bool,
    models: &::digup_runtime::LlamaModels,
) {
    let store = if code {
        ::digup_core::index_store::IndexStore::open(index, ::digup_core::index_store::CODE_FILE)
            .expect("open the code store")
    } else {
        ::digup_core::index_store::IndexStore::main(index).expect("open the main store")
    };
    let options = ::digup_core::options::IndexOptions {
        code,
        ..::digup_core::options::IndexOptions::default()
    };
    let models = models.clone();
    let make = move || -> ::digup_runtime::Result<Box<dyn ::digup_runtime::Embedder>> {
        Ok(Box::new(
            ::digup_runtime::LlamaEmbedder::new(&models, false, None, None).expect("the embedder"),
        ))
    };
    let mut indexer = ::digup_runtime::indexer::Indexer::new(store, options, Box::new(make))
        .expect("the indexer");
    indexer
        .sync(&[corpus.to_path_buf()], &mut |_| {})
        .expect("sync");
    indexer
        .run(None, None, &|| false, &mut |_| {})
        .expect("run");
    indexer
        .finish_long_files(&|| false, &mut |_| {})
        .expect("finish long files");
    indexer.finish();
}

/// Swift's JSONSerialization writes a whole-valued Double as an integer, serde_json keeps
/// `.0`; the measured gate parsed both and compared numbers by value. Same rule as ⑨.
#[allow(dead_code)] // shared helpers: live in the sibling targets of `mod corpus`
pub fn canonical_numbers(value: &mut ::serde_json::Value) {
    match value {
        ::serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                canonical_numbers(item);
            }
        }
        ::serde_json::Value::Array(items) => {
            for item in items {
                canonical_numbers(item);
            }
        }
        ::serde_json::Value::Number(number) => {
            if let Some(float) = number.as_f64() {
                *value = ::serde_json::Value::from(float);
            }
        }
        _ => {}
    }
}

/// The machine-bound root: answers carry the corpus root they measured at; the suite
/// indexes a temp folder — both sides print it `C` (the ⑨ normalization, shared).
#[allow(dead_code)] // shared helpers: live in the sibling targets of `mod corpus`
pub fn normalize_root(value: &mut ::serde_json::Value, root: &str) {
    match value {
        ::serde_json::Value::Object(map) => {
            for (_key, item) in map.iter_mut() {
                normalize_root(item, root);
            }
        }
        ::serde_json::Value::Array(items) => {
            for item in items {
                normalize_root(item, root);
            }
        }
        ::serde_json::Value::String(text) if text.contains(root) => *text = text.replace(root, "C"),
        _ => {}
    }
}
