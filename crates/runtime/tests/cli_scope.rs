//! The #178/#189 CLI-verbs port, pinned from the community suites: the two
//! `CodeTests` cases (`searchUnderScopesToFolderAndWorktreeMainRepo`,
//! `segmentLookupByIdAndFilePath`) replayed with a model-free `FakeEmbedder` — the
//! search legs run with `queryVector: nil`, so the engine's answers here are its
//! keyword-side facts (scope, worktree mapping, tie order, segment lookup) and the
//! harness needs no model files, exactly like the Swift original.
//!
//! The vectors the fake writes are all the same normalized row, so the per-query z
//! scale is degenerate (std 0): every semantic candidate falls below the 2.0σ gate
//! and the answers are decided by words — the shape the Swift FakeEmbedder gave.
//!
//! Unix-only (the suite's temp folders and `.git` fixtures, as the ⑨ suite is).

#![cfg(unix)]

use std::path::{Path, PathBuf};

use digup_core::index_store::{IndexStore, CODE_FILE};
use digup_core::options::IndexOptions;
use digup_core::searcher::{SearchOptions, Searcher};
use digup_runtime::indexer::Indexer;
use digup_runtime::worker::{Embedder, Embedding, WorkerInfo};

/// The engine's own vector width (vectors.rs): the fake writes real-shaped rows.
const DIM: usize = 768;

fn temp_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("digup-cli-scope-{name}"));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

fn write(folder: &Path, name: &str, body: &str) -> PathBuf {
    let path = folder.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, body).unwrap();
    path
}

/// `CodeTests.function` (CodeTests.swift): a function of `body` lines, each `width`
/// characters wide — `1 + body + 1` lines in total.
fn function(name: &str, body: i32, width: usize) -> String {
    let lines: Vec<String> = (1..=body)
        .map(|i| format!("    let v{i} = \"{}\"", "x".repeat(width)))
        .collect();
    format!("func {name}() {{\n{}\n}}\n", lines.join("\n"))
}

fn code_options() -> IndexOptions {
    IndexOptions {
        code: true,
        ..IndexOptions::default()
    }
}

/// Swift's `FakeEmbedder`: one L2-normalized row for every text, media encoders
/// unused by these fixtures. Deterministic: same bytes index the same rows.
struct FakeEmbedder {
    info: WorkerInfo,
}

impl FakeEmbedder {
    fn new() -> Self {
        Self {
            info: WorkerInfo {
                model: "fake-embed".to_string(),
                revision: "port-test-0001".to_string(),
                runtime: "rust-port".to_string(),
                dtype: "f32".to_string(),
                dim: DIM,
                text_only: true,
                load_seconds: Some(0.0),
            },
        }
    }
}

impl Embedder for FakeEmbedder {
    fn info(&self) -> &WorkerInfo {
        &self.info
    }

    fn embed_texts(&self, texts: &[String]) -> digup_runtime::Result<Vec<Vec<f32>>> {
        // (0.6, 0.8, 0, …): L2-normalized, identical for every input.
        let mut v = vec![0.0f32; DIM];
        v[0] = 0.6;
        v[1] = 0.8;
        Ok(texts.iter().map(|_| v.clone()).collect())
    }

    fn embed_images(
        &self,
        paths: &[String],
        _budget: i32,
    ) -> digup_runtime::Result<Vec<Embedding>> {
        Ok(paths
            .iter()
            .map(|_| Embedding::failed("fake embedder: no media"))
            .collect())
    }

    fn embed_audio(&self, paths: &[String]) -> digup_runtime::Result<Vec<Embedding>> {
        Ok(paths
            .iter()
            .map(|_| Embedding::failed("fake embedder: no media"))
            .collect())
    }

    fn close(&mut self) {}
}

/// `Indexer(store:options:makeEmbedder:)` + sync + run, silent.
fn index_code(index: &Path, roots: &[PathBuf]) {
    let store = IndexStore::open(index, CODE_FILE).expect("open the code store");
    let make = || -> digup_runtime::Result<Box<dyn Embedder>> { Ok(Box::new(FakeEmbedder::new())) };
    let mut indexer = Indexer::new(store, code_options(), Box::new(make)).expect("the indexer");
    indexer.sync(roots, &mut |_| {}).expect("sync");
    indexer
        .run(None, None, &|| false, &mut |_| {})
        .expect("run");
    indexer.finish();
}

/// `searchUnderScopesToFolderAndWorktreeMainRepo` (#178/#189, CodeTests): two repos
/// with the same code, the folder scope, and a git worktree whose `.git` file points
/// at `<main>/.git/worktrees/<name>` — the scope must land on the MAIN repo's rows.
#[test]
fn search_under_scopes_to_folder_and_worktree_main_repo() {
    let folder = temp_folder("scope");
    write(&folder, "billing/.git/HEAD", "ref: refs/heads/main\n");
    write(
        &folder,
        "billing/src/retry.py",
        &function("retryWithBackoff", 10, 40),
    );
    write(&folder, "shipping/.git/HEAD", "ref: refs/heads/main\n");
    write(
        &folder,
        "shipping/src/retry.py",
        &function("retryWithBackoff", 10, 40),
    );

    let index = folder.join("index");
    let billing_root = folder.join("billing");
    let shipping_root = folder.join("shipping");
    index_code(&index, &[billing_root.clone(), shipping_root.clone()]);

    let store = IndexStore::open(&index, CODE_FILE).expect("open the code store");
    let mut searcher = Searcher::new(&store, true).expect("the searcher");

    let unscoped = searcher
        .search(
            "retryWithBackoff",
            None,
            &SearchOptions {
                limit: 20,
                ..SearchOptions::default()
            },
        )
        .expect("search");
    assert_eq!(
        unscoped.len(),
        2,
        "both repos answer the symbol: {unscoped:?}"
    );

    let billing_roots = [billing_root.to_string_lossy().into_owned()];
    let billing_hits = searcher
        .search(
            "retryWithBackoff",
            None,
            &SearchOptions {
                limit: 20,
                under: Some(&billing_roots[..]),
                person_files: None,
                ..SearchOptions::default()
            },
        )
        .expect("scoped search");
    assert_eq!(billing_hits.len(), 1);
    assert!(billing_hits[0]
        .path
        .starts_with(&format!("{}/", billing_root.to_string_lossy())));

    let shipping_roots = [shipping_root.to_string_lossy().into_owned()];
    let shipping_hits = searcher
        .search(
            "retryWithBackoff",
            None,
            &SearchOptions {
                limit: 20,
                under: Some(&shipping_roots[..]),
                person_files: None,
                ..SearchOptions::default()
            },
        )
        .expect("scoped search");
    assert_eq!(shipping_hits.len(), 1);
    assert!(shipping_hits[0]
        .path
        .starts_with(&format!("{}/", shipping_root.to_string_lossy())));

    // A scope nothing lives under answers no hits — not an unscoped search.
    let empty_roots = [folder.join("nothing").to_string_lossy().into_owned()];
    let none = searcher
        .search(
            "retryWithBackoff",
            None,
            &SearchOptions {
                limit: 20,
                under: Some(&empty_roots[..]),
                person_files: None,
                ..SearchOptions::default()
            },
        )
        .expect("scoped search");
    assert!(none.is_empty());

    // A git worktree whose .git file points back to billing/.git/worktrees/billing-feature
    // maps to billing (#189: the gitdir is read as the crawler stores paths).
    let worktree = folder.join("worktrees/billing-feature");
    std::fs::create_dir_all(worktree.join("src")).expect("worktree src");
    std::fs::write(
        worktree.join(".git"),
        format!(
            "gitdir: {}/.git/worktrees/billing-feature\n",
            billing_root.to_string_lossy()
        ),
    )
    .expect("worktree .git");

    assert_eq!(
        digup_core::folder_selection::worktree_main_path(&worktree.to_string_lossy()),
        Some(digup_core::paths::real_path(
            &billing_root.to_string_lossy()
        ))
    );
    assert_eq!(
        digup_core::folder_selection::worktree_main_path(&worktree.join("src").to_string_lossy()),
        Some(format!(
            "{}/src",
            digup_core::paths::real_path(&billing_root.to_string_lossy())
        ))
    );

    let worktree_roots = [worktree.to_string_lossy().into_owned()];
    let worktree_hits = searcher
        .search(
            "retryWithBackoff",
            None,
            &SearchOptions {
                limit: 20,
                under: Some(&worktree_roots[..]),
                person_files: None,
                ..SearchOptions::default()
            },
        )
        .expect("worktree-scoped search");
    let billing_ids: Vec<i64> = billing_hits.iter().map(|hit| hit.segment_id).collect();
    let worktree_ids: Vec<i64> = worktree_hits.iter().map(|hit| hit.segment_id).collect();
    assert_eq!(
        worktree_ids, billing_ids,
        "--here in a worktree finds the main repo"
    );

    std::fs::remove_dir_all(&folder).ok();
}

/// `segmentLookupByIdAndFilePath` (#178, CodeTests): `digup show` reads a segment by
/// id and a whole file's content segments (no 'name' row), with the stored text,
/// ordered by location.
#[test]
fn segment_lookup_by_id_and_file_path() {
    let folder = temp_folder("segments");
    write(&folder, "billing/.git/HEAD", "ref: refs/heads/main\n");
    let file = write(
        &folder,
        "billing/retry.py",
        &[
            function("firstStretch", 22, 40),
            function("secondStretch", 22, 40),
        ]
        .join("\n"),
    );

    let index = folder.join("index");
    index_code(&index, &[folder.join("billing")]);

    let store = IndexStore::open(&index, CODE_FILE).expect("open the code store");
    let searcher = Searcher::new(&store, false).expect("the searcher");

    let segments = searcher
        .segments_of_file(&digup_core::paths::real_path(&file.to_string_lossy()))
        .expect("segments of file");
    assert_eq!(segments.len(), 2, "two content stretches: {segments:?}");
    assert_eq!(segments[0].loc, Some(1.0));
    assert_eq!(segments[1].loc, Some(26.0));
    assert!(segments[0]
        .text
        .as_deref()
        .is_some_and(|text| text.contains("func firstStretch()")));
    assert!(segments[1]
        .text
        .as_deref()
        .is_some_and(|text| text.contains("func secondStretch()")));

    let by_id = searcher
        .segment(segments[1].segment_id)
        .expect("segment lookup")
        .expect("the segment is there");
    assert_eq!(by_id, segments[1]);
    assert!(searcher.segment(-999).expect("lookup").is_none());

    std::fs::remove_dir_all(&folder).ok();
}
