//! Keeps the index in step with the chosen folders, then extracts and embeds what is pending
//! — `Sources/DigUpKit/Indexer.swift` in Rust.
//!
//! Model work goes to an `Embedder` (made on first use, freed by `close_worker`). Extraction
//! (stb pictures, PDFium pages, ffmpeg media — the `crates/native` seams the engine's portable
//! side runs on Linux) happens on a second thread one batch ahead, so reading files and
//! embedding them overlap (`process`): while the model embeds one job, the next one is read and
//! decoded (`prepare`), which is why `prepare` touches no database and no model.
//!
//! Long PDFs and documents get their first step (`pdf_step` pages, `doc_step` chunks) and stay
//! `partial` for `finish_long_files`. Per-file problems are recorded on the file; a dead worker
//! or a fingerprint mismatch stops the run (pending files stay pending, so the next run
//! resumes). `should_stop` is asked between batches: pausing, or new files that should go first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime};

use digup_core::costs::MeasuredCosts;
use digup_core::crawler::{CrawlProgress, CrawlReport, DirFacts};
use digup_core::doc;
use digup_core::extract::ExtractError;
use digup_core::index_store::IndexStore;
use digup_core::model::{FileKind, FileRecord, Modality, SegmentKind};
use digup_core::options::IndexOptions;
use digup_core::paths;
use digup_core::search_text::{has_prose, has_words};
use digup_core::uuid::uuid_string;
use digup_core::DirSnap;
use digup_native::{image, media, pdf};

use crate::prompts;
use crate::worker::{Embedder, Embedding};
use crate::{Error, Result};
use digup_core::code;

/// `IndexerError.fingerprintMismatch`: the index was built with another model or other
/// settings. The full text is the engine's (`Indexer.swift:9`), quoted verbatim by the CLI.
#[derive(Debug, thiserror::Error)]
#[error(
    "this index was built with different settings or model:\n  index: {index}\n  now:   {now}\nre-run with --reindex (vectors from different settings can't be compared)"
)]
pub struct FingerprintMismatch {
    pub index: String,
    pub now: String,
}

/// `Indexer.SyncSummary`.
#[derive(Debug, Default)]
pub struct SyncSummary {
    pub added: i64,
    pub changed: i64,
    pub moved: i64,
    pub removed: i64,
    pub unchanged: i64,
    pub crawl: CrawlReport,
    /// Files this sync queued for embedding (new or changed), so a watcher can index exactly
    /// those first.
    pub queued: Vec<i64>,
}

/// `Indexer.Event`: one file's result in the first pass.
#[derive(Debug)]
pub struct Event {
    pub done: usize,
    pub total: usize,
    pub path: String,
    pub kind: FileKind,
    pub state: String,
    pub seconds: f64,
    pub note: Option<String>,
}

/// `Indexer.Step`: one step of the later pass.
#[derive(Debug)]
pub struct Step {
    pub done: usize,
    pub total: usize,
    /// Long files not finished yet, this one included until its last step.
    pub files: i64,
    pub path: String,
    pub kind: FileKind,
    /// "partial" until the file's last step, then "done".
    pub state: String,
    pub seconds: f64,
    pub note: Option<String>,
}

/// The factory of the embedder, made on first use (its failure is the run's failure).
pub type MakeEmbedder = Box<dyn Fn() -> Result<Box<dyn Embedder>>>;
/// `resetOnModelChange`: hears about an emptied index (the app: a new version brought a new
/// model); None means a settings mismatch stops indexing.
type ResetOnModelChange = Option<Box<dyn Fn(&str, &str)>>;

/// `committed`: one job's results, with its place in the run and its seconds per file.
type Committed<'a> = &'a mut dyn FnMut(usize, &Job, &[(String, Option<String>)], f64);
/// `skip`: a job turned down when its turn comes (a long file finished sooner than its steps).
type Skip<'a> = &'a dyn Fn(&Job) -> bool;

/// `Indexer.Job`: files read and embedded together (images 8 at a time, other files one by
/// one), or one step of a long file (`step`: the pages of a PDF or the chunks of a document to
/// read, from 0). The first step comes with the file.
#[derive(Debug, Clone)]
pub struct Job {
    pub files: Vec<FileRecord>,
    pub step: Option<std::ops::Range<usize>>,
}

/// `Indexer.ReadyImage`.
struct ReadyImage {
    slot: usize,
    image: image::Prepared,
    ocr: String,
}

/// `Indexer.Outcome`.
#[derive(Debug, Clone)]
struct Outcome {
    state: String,
    detail: Option<String>,
}

/// `Indexer.Prepared`: a job read from disk and decoded, ready for the model. Its scratch
/// files live in `folder`.
struct Prepared {
    batch: Vec<FileRecord>,
    step: Option<std::ops::Range<usize>>,
    folder: PathBuf,
    /// Files already settled (skipped or failed while reading).
    outcomes: HashMap<usize, Outcome>,
    payload: Payload,
}

/// `Indexer.Payload`.
enum Payload {
    Nothing,
    Images {
        screenshot: bool,
        ready: Vec<ReadyImage>,
    },
    /// `read`: the pages read (from 0) of the PDF's `total`.
    Pdf {
        pages: Vec<pdf::Page>,
        read: std::ops::Range<usize>,
        total: usize,
        title: Option<String>,
    },
    /// `read`: the chunks in it (from 0) of the document's `total` (counted up to maxChunks+1).
    Doc {
        chunks: Vec<String>,
        read: std::ops::Range<usize>,
        total: usize,
        characters: usize,
    },
    Audio {
        pieces: Vec<media::MediaPiece>,
        duration: f64,
    },
    Video {
        frames: Vec<media::MediaPiece>,
        sound: Vec<media::MediaPiece>,
        duration: f64,
    },
    Code(code::File),
}

/// `Range.clamped(to: 0..<upper)` (`Indexer.swift`'s clamps): an empty range collapses onto
/// the lower bound, exactly as Swift's `clamped` does.
fn clamped(range: std::ops::Range<usize>, upper: usize) -> std::ops::Range<usize> {
    let lower = range.start;
    let hi = range.end.max(lower);
    lower..hi.min(upper).max(lower)
}

/// `Indexer.nowReading` (`Indexer.swift:383`): the file being read right now, and since when.
/// Frameworks have run away on some files (PDF text extraction growing by gigabytes a minute,
/// never returning), and nothing stops them from outside but ending the process, so the
/// watchdog has to know which file that was. One value for the whole process, since files are
/// read one at a time.
fn reading_cell() -> &'static Mutex<Option<(String, SystemTime)>> {
    static READING: OnceLock<Mutex<Option<(String, SystemTime)>>> = OnceLock::new();
    READING.get_or_init(|| Mutex::new(None))
}

pub fn now_reading() -> Option<(String, SystemTime)> {
    reading_cell().lock().expect("the reading lock").clone()
}

fn start_reading(path: &str) {
    *reading_cell().lock().expect("the reading lock") = Some((path.to_string(), SystemTime::now()));
}

fn end_reading() {
    *reading_cell().lock().expect("the reading lock") = None;
}

/// Keeps the index in step with the chosen folders, then extracts and embeds whatever is
/// pending. `make_embedder` starts the model on first use (its failure is the run's failure);
/// `close_worker` frees it again between runs.
pub struct Indexer {
    pub store: IndexStore,
    pub options: IndexOptions,
    make_embedder: MakeEmbedder,
    worker: Option<Box<dyn Embedder>>,
    /// One folder per process (`$TMPDIR/DigUp-<pid>`) and one in it per indexer: the helper
    /// runs two (the main index and the code index), and each clears its own away.
    scratch: PathBuf,
    /// When set, an index built with another model or other settings is emptied and embedded
    /// again, and this hears about it. When None, indexing stops with `FingerprintMismatch`.
    pub reset_on_model_change: ResetOnModelChange,
    /// The vector source of the embedders `make_embedder` makes, when it's known without
    /// starting one (llama.cpp's is): then `check_model` can notice a new model before anything
    /// needs embedding.
    pub embedder_source: Option<String>,
    /// The index folder, where what indexing costs on this machine is kept (`MeasuredCosts`)
    /// for estimates. None: nothing is kept.
    pub costs_folder: Option<PathBuf>,
    measured: MeasuredCosts,
    /// Names the scratch batch folders (`jobsPrepared`).
    jobs_prepared: usize,
}

impl Indexer {
    pub fn new(
        store: IndexStore,
        options: IndexOptions,
        make_embedder: MakeEmbedder,
    ) -> std::io::Result<Self> {
        let scratch = std::env::temp_dir()
            .join(format!("DigUp-{}", std::process::id()))
            .join(uuid_string());
        std::fs::create_dir_all(&scratch)?;
        Ok(Indexer {
            store,
            options,
            make_embedder,
            worker: None,
            scratch,
            reset_on_model_change: None,
            embedder_source: None,
            costs_folder: None,
            measured: MeasuredCosts::default(),
            jobs_prepared: 0,
        })
    }

    /// Frees the embedding model (about 1 GB) until the next file needs it.
    pub fn close_worker(&mut self) {
        if let Some(worker) = self.worker.as_mut() {
            worker.close();
        }
        self.worker = None;
    }

    /// Stops the worker and removes the scratch folder, for a process that's about to exit
    /// (the Drop won't run then).
    pub fn finish(&mut self) {
        self.close_worker();
        let _ = std::fs::remove_dir_all(&self.scratch);
    }

    pub fn worker_is_running(&self) -> bool {
        self.worker.is_some()
    }

    // MARK: Sync

    /// Crawls `roots` and reconciles the index: new files are queued, changed ones re-queued,
    /// moved ones re-pathed (no re-embedding), and files that are gone or newly excluded are
    pub fn sync(
        &self,
        roots: &[PathBuf],
        progress: &mut dyn FnMut(&CrawlProgress),
    ) -> digup_core::db::Result<SyncSummary> {
        self.sync_scoped(roots, roots, progress)
    }

    /// Issue #7a: reconcile only the `walk` folders (the watcher's burst of changed paths),
    /// treating them as the chosen folders they live under: the rules apply as in a full
    /// crawl, removals prune ONLY the rows under `walk`, and rows outside the scope stand
    /// untouched — they belong to a full sync (startup, manual rescans, watcher overflow,
    /// settings changes, all unchanged paths). Dataset detection fires for every folder the
    /// scope walks whole (a folder's own files are fully visible inside it); a folder whose
    /// line-crossing files sit OUTSIDE the scope defers its verdict to the next full sync.
    ///
    /// The issue #2 keeps are scoped to the level of the claim:
    /// - a chosen root that vanished (an ejected drive, a gone mount) keeps rows under it
    ///   exactly as the full sync does — those files are elsewhere, not deleted;
    /// - a scan failure at a path that EXISTS (a sealed folder, an unreachable mount below a
    ///   live root) keeps rows, same as the full sync: "not seen" is the walk's blindness;
    /// - a scan failure at a path that is GONE is a scope fired from remove events for a
    ///   folder that was deleted: its rows prune. The ejected-drive case is caught by the
    ///   root rule above, before this one can speak.
    pub fn sync_scoped(
        &self,
        roots: &[PathBuf],
        walk: &[PathBuf],
        progress: &mut dyn FnMut(&CrawlProgress),
    ) -> digup_core::db::Result<SyncSummary> {
        let root_paths: Vec<String> = roots
            .iter()
            .map(|root| paths::standardize(&root.to_string_lossy()))
            .collect();
        let mut walk_paths: Vec<String> = walk
            .iter()
            .map(|root| paths::standardize(&root.to_string_lossy()))
            .collect();
        // A scope is a set of folders INSIDE the user's chosen ones. A verb naming a folder
        // above them (`sync ["/"]`, `sync ["/home/secret"]`) is not a scope at all: crawling
        // it indexes trees the user never chose (rows its own scope never embeds, so they sit
        // in `files` forever) and pruning under it deletes rows for paths the sync has no
        // business touching. Answer it as a full sync — the roots, never less.
        let scoped = walk_paths.iter().all(|dir| {
            root_paths
                .iter()
                .any(|root| dir == root || paths::is_inside(dir, root))
        });
        let walk: Vec<PathBuf> = if scoped {
            walk.to_vec()
        } else {
            walk_paths = root_paths.clone();
            roots.to_vec()
        };
        let full = walk_paths == root_paths;
        // The crawl walks the scope under the rules of the CHOSEN folders: the root-level
        // exemptions (a chosen root is never a dataset; a chosen root's own files are not
        // "inside a code project") belong to the folders the user chose, not to the folders
        // the scope happens to name. Crawling the scope as its own root set indexes and
        // embeds what the full crawl excludes, and the next full crawl deletes it — add,
        // embed, prune, repeat, paying the model every burst.
        let crawl = digup_core::crawler::crawl_scoped(&walk, roots, &self.options, progress);
        let mut summary = SyncSummary::default();
        self.reconcile(
            &crawl,
            &root_paths,
            &walk_paths,
            full,
            &HashSet::new(),
            &mut summary,
        )?;
        summary.crawl = crawl;
        Ok(summary)
    }

    /// #110: the gated sync. When the store carries a dir snapshot recorded by the same
    /// rules, a steady-state sync pays: the prefilter (one readdir + one stat per folder —
    /// seconds-cheap), the rule walk entering ONLY the folders whose `(mtime, entries)` pair
    /// moved, the folders the snapshot never saw, and their ancestors, and the row-check (the
    /// file-level law) that stats the stored paths of rows in folders NOT entered — the
    /// content-edit-in-place edge (the parent folder's mtime is untouched by its files'
    /// content; the dir pair cannot see it) is caught there, never missed. The reconcile law
    /// — add/move/requeue/prune, the issue #7a/#2 keeps — is the byte-identical body both
    /// syncs share. Without a snapshot (fresh store, migrated store, rules change) the sync
    /// runs FULL first and records the snapshot: the gate only speaks after one honest full
    /// crawl. Measured (Phase-M, bench HDD, 1.7 M files): full walk 1262.53 s vs the row-stat
    /// proxy 57 µs/row warm — the gate is what makes sync a sync and not a re-crawl; the
    /// full crawl stays the honest base the snapshot is refreshed from.
    pub fn sync_gated(
        &self,
        roots: &[PathBuf],
        progress: &mut dyn FnMut(&CrawlProgress),
    ) -> digup_core::db::Result<SyncSummary> {
        let root_paths: Vec<String> = roots
            .iter()
            .map(|root| paths::standardize(&root.to_string_lossy()))
            .collect();
        // Dir rows are anchored rows: register the roots first (both paths — the fill's
        // `split_anchor` needs the root anchor to exist, and the empty-root case too).
        for root in &root_paths {
            self.store.ensure_anchor(root)?;
        }
        let fingerprint = self.rules_fingerprint();
        let snapshot = self.store.dir_snapshots()?;
        if snapshot.is_empty() || self.store.dirs_rules()?.as_deref() != Some(fingerprint.as_str())
        {
            // The honest base: full crawl, then fill the snapshot from the prefilter —
            // the same law, so the pairs the gate compares are pairs the walk produced.
            let mut summary = self.sync_scoped(roots, roots, progress)?;
            let facts = digup_core::crawler::dir_facts(roots, &self.options);
            self.record_snapshot(&facts)?;
            self.store.set_dirs_rules(Some(&fingerprint))?;
            summary.unchanged += 0; // the fill is plumbing; the summary stays the crawl's
            return Ok(summary);
        }
        let facts = digup_core::crawler::dir_facts(roots, &self.options);
        let mut active: HashSet<String> = root_paths.iter().cloned().collect();
        for (dir, fact) in &facts {
            let unchanged = snapshot
                .get(dir)
                .is_some_and(|(m, e)| (m - fact.mtime).abs() < 1e-6 && *e == fact.entries);
            if !unchanged {
                active.insert(dir.clone());
            }
        }
        // Ancestors of every active folder up to its root: the gate must let the walk
        // REACH a changed folder through its unchanged ancestors (the ancestors' own file
        // work is skipped by their own verdict — only the path down opens).
        let started: Vec<String> = active.iter().cloned().collect();
        for dir in started {
            let mut folder = dir;
            loop {
                if root_paths.contains(&folder) {
                    break;
                }
                let parent = paths::parent(&folder);
                if parent == folder {
                    break;
                }
                active.insert(parent.clone());
                folder = parent;
            }
        }
        let crawl =
            digup_core::crawler::crawl_gated(roots, roots, &self.options, &active, progress);
        let keep_under = Self::keep_prefixes(&root_paths, &crawl.scan_failures, true);
        // The move law's pairing, seen from the row side: an inode the gated walk DID
        // enumerate at a new path is a RENAME, not a delete — the row stands for
        // reconcile's by_inode pass (a folder rename mutates the parent's pair, so the
        // parent IS enumerated; the old spelling's ENOENT must not eat the row before
        // the pairing runs — measured while drafting the rename pin).
        let moved_inodes: HashSet<String> = crawl
            .candidates
            .iter()
            .map(|c| format!("{}:{}", c.device, c.inode))
            .collect();
        let mut summary = SyncSummary::default();
        let keep_rows = self.row_check(
            &root_paths,
            &active,
            &keep_under,
            &moved_inodes,
            &mut summary,
        )?;
        self.reconcile(
            &crawl,
            &root_paths,
            &root_paths,
            true,
            &keep_rows,
            &mut summary,
        )?;
        summary.crawl = crawl;
        // The snapshot refresh: every folder the prefilter visited rides its fresh pair.
        self.record_snapshot(&facts)?;
        self.store.set_dirs_rules(Some(&fingerprint))?;
        Ok(summary)
    }

    /// #110 row-check (the file-level law): stat the stored paths of rows in folders the
    /// gated walk did NOT enumerate. Content-edit-in-place (the Gemini inverse: a file's
    /// content moves, its parent folder's mtime does not) is caught HERE — the dir pair
    /// cannot see it. The answers ride the sync's own laws: size/mtime moved → requeue
    /// (the kind rides the row — the path is the same, the classification is the same);
    /// ENOENT under a live folder → delete (the folder WAS checked, the file is not
    /// there), EXCEPT when the row's `(device, inode)` rides a path the gated walk
    /// enumerated — a rename whose old spelling sits in an un-entered folder stands for
    /// reconcile's by_inode pass (the move law runs before this delete can eat it);
    /// the gone-root/scan-failure keeps (issue #2) apply; a file that is
    /// unreadable, dataless, or not a regular file → keep (the full crawl's classification
    /// owns those verdicts at the next full sync — the gate does not invent them).
    fn row_check(
        &self,
        root_paths: &[String],
        active: &HashSet<String>,
        keep_under: &[String],
        moved_inodes: &HashSet<String>,
        summary: &mut SyncSummary,
    ) -> digup_core::db::Result<HashSet<String>> {
        let rows = self.store.files_under(root_paths)?;
        let mut keep_rows = HashSet::new();
        for row in &rows {
            let dir = paths::parent(&row.path);
            if active.contains(&dir) {
                continue; // the gated walk enumerated this folder: the candidates answer
            }
            match std::fs::symlink_metadata(&row.path) {
                Ok(_) => {
                    let Some(info) = digup_core::crawler::file_info(&row.path) else {
                        keep_rows.insert(row.path.clone());
                        summary.unchanged += 1;
                        continue;
                    };
                    if !info.is_regular || info.dataless {
                        // A link/special, or a macOS cloud eviction: the full crawl's
                        // classification owns these verdicts. Keep.
                        keep_rows.insert(row.path.clone());
                        summary.unchanged += 1;
                        continue;
                    }
                    if row.size != info.size || (row.mtime - info.mtime).abs() > 0.001 {
                        let cand = digup_core::model::Candidate {
                            path: row.path.clone(),
                            kind: row.kind,
                            size: info.size,
                            mtime: info.mtime,
                            inode: info.inode,
                            device: info.device,
                        };
                        self.store.requeue_as(row.id, &cand)?;
                        // Seen-by-law: the folder WAS checked, the file IS there (it was
                        // requeued). The enumerate-only prune must not eat it.
                        keep_rows.insert(row.path.clone());
                        summary.changed += 1;
                        summary.queued.push(row.id);
                    } else {
                        keep_rows.insert(row.path.clone());
                        summary.unchanged += 1;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if moved_inodes.contains(&format!("{}:{}", row.device, row.inode)) {
                        // The inode rides a path the gated walk enumerated: this row is
                        // the OLD spelling of a rename. It stands for reconcile's
                        // by_inode pass, which re-paths it (the full sync's move law,
                        // untouched) — counting it here would double the verdict.
                        keep_rows.insert(row.path.clone());
                        continue;
                    }
                    if keep_under
                        .iter()
                        .any(|prefix| row.path.starts_with(prefix.as_str()))
                    {
                        keep_rows.insert(row.path.clone());
                        summary.unchanged += 1;
                    } else {
                        self.store.delete(row.id)?;
                        summary.removed += 1;
                    }
                }
                Err(_) => {
                    // Unreadable on the path (a sealed folder above): the walk's-blindness
                    // law of issue #2 — "not seen" is not "deleted". Keep.
                    keep_rows.insert(row.path.clone());
                    summary.unchanged += 1;
                }
            }
        }
        Ok(keep_rows)
    }

    /// The rules fingerprint the snapshot is valid for: every option that can move what the
    /// walk enumerates (skips, code mode, datasets, code limits, excluded extensions). A
    /// mismatch makes the next sync FULL and refills — the snapshot is only as honest as
    /// the rules that filled it.
    fn rules_fingerprint(&self) -> String {
        let mut folders = self.options.excluded_folders.clone();
        folders.sort();
        let mut exts: Vec<String> = self.options.excluded_extensions.iter().cloned().collect();
        exts.sort();
        format!(
            "code={} include_code={} include_datasets={} max_code={} max_nb={} stretch={} folders={} exts={}",
            self.options.code,
            self.options.include_code,
            self.options.include_datasets,
            self.options.max_code_bytes,
            self.options.max_notebook_bytes,
            self.options.code_stretch,
            folders.join(","),
            exts.join(",")
        )
    }

    fn record_snapshot(&self, facts: &HashMap<String, DirFacts>) -> digup_core::db::Result<()> {
        let snaps: Vec<DirSnap> = facts
            .iter()
            .map(|(path, f)| DirSnap {
                path: path.clone(),
                mtime: f.mtime,
                entries: f.entries,
            })
            .collect();
        self.store.record_dirs(&snaps)
    }

    /// The issue #2/#7a keeps, shared by reconcile and the row-check: vanished chosen roots
    /// (an ejected drive) and, at a full sync, the scan-failure paths keep their rows.
    fn keep_prefixes(
        root_paths: &[String],
        scan_failures: &HashSet<String>,
        full: bool,
    ) -> Vec<String> {
        root_paths
            .iter()
            .filter(|path| folder_is_gone(path))
            .map(|path| paths::bound(path))
            .chain(
                scan_failures
                    .iter()
                    .filter(|path| full || !folder_is_gone(path))
                    .map(|path| paths::bound(path)),
            )
            .collect()
    }

    /// The reconciliation of a crawl against the store — the issue #7a law VERBATIM (the
    /// body `sync_scoped` ran): match by path, by `(device, inode)` for renames, requeue
    /// what moved, prune what a successful scan did not see. `keep_rows` (#110) are the
    /// rows the row-check answered — kept or requeued — the prune law does not touch them:
    /// their folders WERE checked, file by file, and answered.
    fn reconcile(
        &self,
        crawl: &CrawlReport,
        root_paths: &[String],
        walk_paths: &[String],
        full: bool,
        keep_rows: &HashSet<String>,
        summary: &mut SyncSummary,
    ) -> digup_core::db::Result<()> {
        let existing = self.store.files_under(walk_paths)?;
        let mut by_path: HashMap<String, FileRecord> = HashMap::new();
        let mut by_inode: HashMap<String, FileRecord> = HashMap::new();
        for file in &existing {
            by_path.insert(file.path.clone(), file.clone());
            by_inode.insert(format!("{}:{}", file.device, file.inode), file.clone());
        }
        let seen: HashSet<String> = crawl.candidates.iter().map(|c| c.path.clone()).collect();
        let mut moved_from: HashSet<String> = HashSet::new();
        self.store.db.transaction(|| {
            for file in &crawl.candidates {
                if let Some(row) = by_path.get(&file.path) {
                    if row.size != file.size
                        || (row.mtime - file.mtime).abs() > 0.001
                        || row.kind != file.kind
                    {
                        self.store.requeue_as(row.id, file)?;
                        summary.changed += 1;
                        summary.queued.push(row.id);
                    } else {
                        summary.unchanged += 1;
                    }
                } else if let Some(row) = by_inode.get(&format!("{}:{}", file.device, file.inode)) {
                    if !seen.contains(&row.path)
                        && !moved_from.contains(&row.path)
                        && row.size == file.size
                        && (row.mtime - file.mtime).abs() < 0.001
                    {
                        self.store.move_file(row.id, &file.path)?;
                        moved_from.insert(row.path.clone());
                        summary.moved += 1;
                    } else {
                        summary.queued.push(self.store.insert(file)?);
                        summary.added += 1;
                    }
                } else {
                    // Issue #7a: a file that ENTERS the scope from outside. Its row lives
                    // above the scope, so the in-scope inode lookup misses and a SECOND row
                    // is inserted while the old path keeps standing inside the store's scope
                    // — search answers a path that no longer exists and the open fails, until
                    // a full sync cleans it up. The watcher fires both sides of a move it can
                    // see (pinned), so the trigger is the moves it cannot see: notify adds
                    // watches lazily for newly created folders, FSEvents coalesces on a Mac,
                    // and a dropped burst is a full sync by construction. The schema indexes
                    // `(device, inode)` for exactly this lookup. The full sync keeps the
                    // engine's pinned behavior unchanged: its scope is every chosen folder, so
                    // a row outside it is not the move of an indexed file.
                    let outside = if full {
                        None
                    } else {
                        self.store.file_by_inode(file.device, file.inode)?
                    };
                    match outside {
                        Some(row)
                            if !seen.contains(&row.path)
                                && !moved_from.contains(&row.path)
                                && row.size == file.size
                                && (row.mtime - file.mtime).abs() < 0.001 =>
                        {
                            self.store.move_file(row.id, &file.path)?;
                            moved_from.insert(row.path.clone());
                            summary.moved += 1;
                        }
                        _ => {
                            summary.queued.push(self.store.insert(file)?);
                            summary.added += 1;
                        }
                    }
                }
            }
            // A root that went away while it was being walked (a drive ejected mid-crawl)
            // looked emptier than it is: keep its rows rather than take that for "deleted".
            // The same rule for a folder the walk could not read at all (issue #2): its files
            // exist, they were only unlistable — "not seen" is the walk's failure, not a
            // delete. Real deletes still prune: a SUCCESSFUL scan that lacks the file saw the
            // folder's whole truth, and no failure is recorded for it. Issue #7a: in a scoped
            // sync a failure at a path that is GONE is the deleted folder the events named
            // (the scope is fired by filesystem events, not by a drive walk), so only
            // failures at paths that EXIST keep rows; vanished chosen roots keep theirs at
            // any sync — an ejected drive's files are elsewhere, not deleted. Issue #110:
            // rows the row-check ANSWERED (their folders were checked file-by-file) are
            // seen-by-law, not by enumeration — the prune law does not touch them.
            let keep_under = Self::keep_prefixes(root_paths, &crawl.scan_failures, full);
            for row in &existing {
                if !seen.contains(&row.path)
                    && !moved_from.contains(&row.path)
                    && !keep_rows.contains(&row.path)
                    && !keep_under
                        .iter()
                        .any(|prefix| row.path.starts_with(prefix.as_str()))
                {
                    self.store.delete(row.id)?;
                    summary.removed += 1;
                }
            }
            Ok(())
        })
    }

    /// Images and screenshots go to the model 8 at a time; other files one by one
    /// (`Indexer.batches`).
    pub fn batches(queue: &[FileRecord]) -> Vec<Vec<FileRecord>> {
        let mut batches: Vec<Vec<FileRecord>> = Vec::new();
        let mut index = 0;
        while index < queue.len() {
            let mut batch = vec![queue[index].clone()];
            if batch[0].kind == FileKind::Screenshot || batch[0].kind == FileKind::Image {
                while index + batch.len() < queue.len()
                    && batch.len() < 8
                    && queue[index + batch.len()].kind == batch[0].kind
                {
                    batch.push(queue[index + batch.len()].clone());
                }
            }
            let count = batch.len();
            batches.push(batch);
            index += count;
        }
        batches
    }

    // MARK: Model check

    /// Compares the index with the current model and settings (`embedder_source`), without
    /// starting the model; a change empties the index (see `reset_on_model_change`) or throws
    /// `FingerprintMismatch`. Without a known source the check waits for the first file to
    /// embed.
    pub fn check_model(&self) -> Result<()> {
        let Some(source) = &self.embedder_source else {
            return Ok(());
        };
        self.adopt(&format!("{} | {}", source, self.options.fingerprint()))
    }

    fn adopt(&self, fingerprint: &str) -> Result<()> {
        let indexed = self.store.meta("fingerprint").map_err(Error::from)?;
        let Some(indexed) = indexed else {
            return Ok(());
        };
        if indexed == fingerprint {
            return Ok(());
        }
        let Some(callback) = &self.reset_on_model_change else {
            return Err(Error::Indexer(FingerprintMismatch {
                index: indexed,
                now: fingerprint.to_string(),
            }));
        };
        // Everything is queued again: a run that's under way goes on with its batch, and later
        // runs take the rest.
        self.store.reset_all().map_err(Error::from)?;
        callback(&indexed, fingerprint);
        Ok(())
    }

    /// The embedder, started on first use; its vector source and the options' fingerprint are
    /// adopted together, before anything embeds.
    fn full_worker(&mut self) -> Result<()> {
        if self.worker.is_some() {
            return Ok(());
        }
        let mut started = (self.make_embedder)()?;
        let fingerprint = format!(
            "{} | {}",
            started.info().vector_source(),
            self.options.fingerprint()
        );
        if let Err(error) = self.adopt(&fingerprint) {
            started.close();
            return Err(error);
        }
        self.store
            .set_meta("fingerprint", Some(&fingerprint))
            .map_err(Error::from)?;
        self.store
            .set_meta("vector_source", Some(&started.info().vector_source()))
            .map_err(Error::from)?;
        self.worker = Some(started);
        Ok(())
    }

    // MARK: Run

    /// Processes pending files, screenshots first, newest first (`only`: just these files).
    /// Long PDFs and documents get their first step and stay `partial` for `finish_long_files`.
    pub fn run(
        &mut self,
        limit: Option<i64>,
        only: Option<&HashSet<i64>>,
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(&Event),
    ) -> Result<BTreeMap<String, i64>> {
        let queue = self.store.pending(limit, only).map_err(Error::from)?;
        let jobs: Vec<Job> = Self::batches(&queue)
            .into_iter()
            .map(|files| Job { files, step: None })
            .collect();
        let mut states: BTreeMap<String, i64> = BTreeMap::new();
        let mut done: usize = 0;
        let total = queue.len();
        self.process(
            &jobs,
            should_stop,
            &mut |_index, job, results, seconds| {
                for (offset, file) in job.files.iter().enumerate() {
                    let (state, note) = &results[offset];
                    *states.entry(state.clone()).or_insert(0) += 1;
                    on_event(&Event {
                        done: done + offset + 1,
                        total,
                        path: file.path.clone(),
                        kind: file.kind,
                        state: state.clone(),
                        seconds,
                        note: note.clone(),
                    });
                }
                done += job.files.len();
            },
            &|_job| false,
        )?;
        Ok(states)
    }

    /// The later pass: reads the rest of long PDFs and documents (`partial` after their first
    /// step), in indexing order, a step at a time. Each step is saved as one transaction, so
    /// stopping between steps loses nothing, and the next pass goes on from there. It runs
    /// once nothing is pending.
    pub fn finish_long_files(
        &mut self,
        should_stop: &dyn Fn() -> bool,
        on_step: &mut dyn FnMut(&Step),
    ) -> Result<BTreeMap<String, i64>> {
        let mut jobs: Vec<Job> = Vec::new();
        for file in self.store.unfinished().map_err(Error::from)? {
            let info = self.store.info_of(file.id).map_err(Error::from)?;
            let size = match file.kind {
                FileKind::Pdf => self.options.pdf_step,
                _ => self.options.doc_step,
            } as usize;
            // Where it stopped (after the last page or chunk stored, should its info not say;
            // nothing stored: the start).
            let start = match info
                .get("read_to")
                .and_then(|text| text.parse::<usize>().ok())
            {
                Some(value) => value,
                None => self
                    .store
                    .last_place(file.id)
                    .map_err(Error::from)?
                    .unwrap_or(0)
                    .max(0) as usize,
            };
            // At least one step, which settles a file whose length was only guessed (documents
            // cut before schema 5).
            let end = info
                .get("read_end")
                .and_then(|text| text.parse::<usize>().ok())
                .unwrap_or(0)
                .max(start + 1);
            let mut start = start;
            while start < end {
                jobs.push(Job {
                    files: vec![file.clone()],
                    step: Some(start..(start + size).min(end)),
                });
                start += size;
            }
        }
        // Progress counts pages and chunks: what's read once each job is through.
        let mut through: Vec<usize> = Vec::new();
        for job in &jobs {
            through.push(
                through.last().unwrap_or(&0) + job.step.as_ref().map(|s| s.len()).unwrap_or(0),
            );
        }
        let mut files: i64 = jobs
            .iter()
            .map(|j| j.files[0].id)
            .collect::<HashSet<i64>>()
            .len() as i64;
        let mut states: BTreeMap<String, i64> = BTreeMap::new();
        // committed inserts into it, skip reads it: the two closures share it through a cell.
        let finished: std::rc::Rc<std::cell::RefCell<HashSet<i64>>> =
            std::rc::Rc::new(std::cell::RefCell::new(HashSet::new()));
        let total = through.last().copied().unwrap_or(0);
        self.process(
            &jobs,
            should_stop,
            &mut |index, job, results, seconds| {
                let file = &job.files[0];
                let (state, note) = &results[0];
                if state != "partial" && finished.borrow_mut().insert(file.id) {
                    *states.entry(state.clone()).or_insert(0) += 1;
                    files -= 1;
                }
                on_step(&Step {
                    done: through[index],
                    total,
                    files,
                    path: file.path.clone(),
                    kind: file.kind,
                    state: state.clone(),
                    seconds,
                    note: note.clone(),
                });
            },
            &{
                let finished = std::rc::Rc::clone(&finished);
                move |job: &Job| finished.borrow().contains(&job.files[0].id)
            },
        )?;
        Ok(states)
    }

    /// Runs `jobs` in order until `should_stop` (asked between jobs), passing each one's
    /// results to `committed` (with its place in `jobs`). Jobs that `skip` turns down when
    /// their turn comes are left out.
    ///
    /// Two stages overlap: while the model embeds one job, the next one is read and decoded on
    /// another thread (images scaled, PDF pages rendered, audio and video decoded).
    fn process(
        &mut self,
        jobs: &[Job],
        should_stop: &dyn Fn() -> bool,
        committed: Committed<'_>,
        skip: Skip<'_>,
    ) -> Result<()> {
        let mut ahead: Option<JoinHandle<Prepared>> = None;
        let outcome = self.process_loop(jobs, should_stop, committed, skip, &mut ahead);
        // A job prepared for a run that stopped early: throw its files away (the Swift `defer`
        // joins the preparation, then removes the folder), and merge what this run measured.
        if let Some(handle) = ahead.take() {
            if let Ok(prepared) = handle.join() {
                let _ = std::fs::remove_dir_all(&prepared.folder);
            }
        }
        if let Some(folder) = &self.costs_folder {
            if !self.measured.tallies.is_empty() {
                self.measured.merge(folder);
                self.measured = MeasuredCosts::default();
            }
        }
        outcome
    }

    fn process_loop(
        &mut self,
        jobs: &[Job],
        should_stop: &dyn Fn() -> bool,
        committed: Committed<'_>,
        skip: Skip<'_>,
        ahead: &mut Option<JoinHandle<Prepared>>,
    ) -> Result<()> {
        for (index, job) in jobs.iter().enumerate() {
            if should_stop() {
                break;
            }
            let started = Instant::now();
            let prepared = match ahead.take() {
                Some(handle) => handle.join().expect("the preparation thread"),
                None => self.prepare(job),
            };
            self.jobs_prepared += 1;
            let next = jobs.get(index + 1);
            *ahead = next.map(|job| {
                let options = self.options.clone();
                let folder = self.scratch.join(format!("batch-{}", self.jobs_prepared));
                let job = job.clone();
                std::thread::spawn(move || Self::prepare_static(&job, &options, &folder))
            });
            if skip(job) {
                let _ = std::fs::remove_dir_all(&prepared.folder);
                continue;
            }
            let model_was_running = self.worker.is_some();
            let results = match self.commit(&prepared) {
                Ok(results) => results,
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&prepared.folder);
                    return Err(error);
                }
            };
            let seconds = started.elapsed().as_secs_f64();
            if model_was_running {
                self.measure(&prepared, seconds); // else it's seconds of the model starting
            }
            committed(index, job, &results, seconds / job.files.len() as f64);
            let _ = std::fs::remove_dir_all(&prepared.folder);
        }
        Ok(())
    }

    /// What a job read and how long it took (its reading and its model's work, which overlap),
    /// for `MeasuredCosts`.
    fn measure(&mut self, prepared: &Prepared, seconds: f64) {
        match &prepared.payload {
            Payload::Nothing => {}
            Payload::Images { screenshot, ready } => {
                self.measured.add(
                    if *screenshot { "screenshot" } else { "image" },
                    ready.len() as f64,
                    seconds,
                );
            }
            Payload::Pdf { pages, .. } => self.measured.add("pdfPage", pages.len() as f64, seconds),
            Payload::Doc { chunks, .. } => {
                // Chunks with a vector: a data dump's go to keyword search only and take next
                // to nothing, which isn't what a chunk of prose costs.
                let worded = chunks.iter().filter(|chunk| has_prose(chunk)).count();
                if worded > 0 {
                    self.measured.add("docChunk", worded as f64, seconds);
                }
            }
            Payload::Audio { duration, .. } => self.measured.add("audioSecond", *duration, seconds),
            Payload::Video { duration, .. } => self.measured.add("videoSecond", *duration, seconds),
            Payload::Code(file) => {
                self.measured
                    .add("codeStretch", file.pieces.len() as f64, seconds)
            }
        }
    }

    // MARK: Prepare (any thread: files only, no database, no model)

    /// The scheduled preparation: the same work on the preparation queue, naming the scratch
    /// folder after the batch count.
    fn prepare(&self, job: &Job) -> Prepared {
        let folder = self.scratch.join(format!("batch-{}", self.jobs_prepared));
        Self::prepare_static(job, &self.options, &folder)
    }

    /// A job read from disk and decoded, ready for the model. Its scratch files live in
    /// `folder`. Files that fail to read settle as outcomes, not as panics.
    fn prepare_static(job: &Job, options: &IndexOptions, folder: &Path) -> Prepared {
        let batch = job.files.clone();
        let mut prepared = Prepared {
            batch,
            step: job.step.clone(),
            folder: folder.to_path_buf(),
            outcomes: HashMap::new(),
            payload: Payload::Nothing,
        };
        let url = PathBuf::from(&prepared.batch[0].path);
        start_reading(&prepared.batch[0].path);
        let _guard = ReadingGuard;
        let _ = std::fs::create_dir_all(folder);
        let outcome = match prepared.batch[0].kind {
            FileKind::Image | FileKind::Screenshot => {
                let screenshot = prepared.batch[0].kind == FileKind::Screenshot;
                let mut ready: Vec<ReadyImage> = Vec::new();
                for (slot, file) in prepared.batch.iter().enumerate() {
                    let url = Path::new(&file.path);
                    start_reading(&file.path);
                    match image::prepare(
                        url,
                        if screenshot { 1536 } else { 1024 },
                        options.min_image_side,
                        folder,
                        screenshot,
                    ) {
                        Ok(image) => {
                            // No text recognition on this platform (`OCR.isAvailable == false`):
                            // screenshots have their picture vector and their name.
                            ready.push(ReadyImage {
                                slot,
                                image,
                                ocr: String::new(),
                            });
                        }
                        Err(error) => {
                            let state = if matches!(error, ExtractError::TooSmall { .. }) {
                                "skipped"
                            } else {
                                "failed"
                            };
                            prepared.outcomes.insert(
                                slot,
                                Outcome {
                                    state: state.to_string(),
                                    detail: Some(format!("{error}")),
                                },
                            );
                        }
                    }
                }
                prepared.payload = Payload::Images { screenshot, ready };
                Ok(())
            }
            FileKind::Pdf => {
                // Long PDFs (books) get their first pages now and the rest after everything
                // else. The step range is PLANNED FROM THE DOCUMENT'S REAL TOTAL inside the
                // one read that reads the pages (Swift's `range: { total in … }`,
                // Indexer.swift:434-438): the audit-base opened the PDF twice per prepare
                // — `pages_of` for the count, then the pages — and a book re-prepares for
                // every step, so the extra full parse recurs per step.
                let step = prepared.step.clone();
                let big = options.big_pdf_pages;
                let pdf_step = options.pdf_step as usize;
                let max_pages = options.max_pages as usize;
                let mut wanted = 0..0usize;
                match pdf::pages_with_range(
                    &url,
                    |total| {
                        let raw = step.clone().unwrap_or(if (total as i64) > big {
                            0..pdf_step
                        } else {
                            0..max_pages
                        });
                        wanted = clamped(raw, max_pages);
                        wanted.clone()
                    },
                    options.min_page_text as usize,
                    1400,
                    folder,
                ) {
                    Ok((pages, total, title)) => {
                        let read = clamped(wanted, total.min(options.max_pages as usize));
                        prepared.payload = Payload::Pdf {
                            pages,
                            read,
                            total,
                            title,
                        };
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            FileKind::Doc => (|| -> std::result::Result<(), ExtractError> {
                // The chunks are cut the same way every time, so a later step goes on where
                // the last one stopped.
                let text = doc::text_of(&url)?;
                let chunks = doc::chunks(&text, 1800, 200, options.max_chunks + 1);
                let wanted = prepared
                    .step
                    .clone()
                    .unwrap_or(0..options.doc_step as usize);
                let read = clamped(wanted, chunks.len().min(options.max_chunks as usize));
                prepared.payload = Payload::Doc {
                    chunks: chunks[read.clone()].to_vec(),
                    read,
                    total: chunks.len(),
                    // Swift's `text.count` (Indexer.swift:446) counts grapheme clusters, and
                    // the number lands in `files.info` as "chars": a Devanagari / Tamil /
                    // Thai text file indexes half the characters the engine says it has.
                    characters: digup_core::xmlscan::grapheme_len(&text),
                };
                Ok(())
            })(),
            FileKind::Audio => (|| -> std::result::Result<(), ExtractError> {
                let (pieces, duration) =
                    media::windows(&url, options.audio_window, options.audio_hop, 2.0, folder)?;
                prepared.payload = Payload::Audio { pieces, duration };
                Ok(())
            })(),
            FileKind::Video => (|| -> std::result::Result<(), ExtractError> {
                let (frames, duration, has_audio) =
                    media::keyframes(&url, options.frame_interval, 1024, 1200, folder)?;
                let sound = if has_audio {
                    media::windows(&url, options.audio_window, options.audio_hop, 2.0, folder)?.0
                } else {
                    Vec::new()
                };
                prepared.payload = Payload::Video {
                    frames,
                    sound,
                    duration,
                };
                Ok(())
            })(),
            FileKind::Code => (|| -> std::result::Result<(), ExtractError> {
                prepared.payload = Payload::Code(code::read(&url, options)?);
                Ok(())
            })(),
        };
        if let Err(error) = outcome {
            if matches!(error, ExtractError::LeftOut { .. }) {
                prepared.outcomes.insert(
                    0,
                    Outcome {
                        state: "skipped".to_string(),
                        detail: Some(format!("{error}")),
                    },
                );
            } else {
                for slot in 0..prepared.batch.len() {
                    prepared.outcomes.entry(slot).or_insert(Outcome {
                        state: "failed".to_string(),
                        detail: Some(format!("{error}")),
                    });
                }
            }
            prepared.payload = Payload::Nothing;
        }
        prepared
    }

    // MARK: Commit (the indexing thread: the model and the database)

    fn commit(&mut self, prepared: &Prepared) -> Result<Vec<(String, Option<String>)>> {
        let batch = &prepared.batch;
        let later = prepared.step.is_some();
        if let (Some(step), Some(outcome)) = (prepared.step.clone(), prepared.outcomes.get(&0)) {
            return Ok(vec![self.stop_reading(
                &batch[0],
                step.start,
                outcome.detail.as_deref().unwrap_or(&outcome.state),
            )?]);
        }
        let mut results: Vec<(String, Option<String>)> =
            vec![("failed".to_string(), None); batch.len()];
        for (slot, outcome) in &prepared.outcomes {
            self.store
                .finish(
                    batch[*slot].id,
                    &outcome.state,
                    outcome.detail.as_deref(),
                    HashMap::new(),
                )
                .map_err(Error::from)?;
            results[*slot] = (outcome.state.clone(), outcome.detail.clone());
        }
        let body = match &prepared.payload {
            Payload::Nothing => Ok(()),
            Payload::Images { screenshot, ready } => {
                self.images(batch, *screenshot, ready, &mut results)
            }
            Payload::Pdf {
                pages,
                read,
                total,
                title,
            } => self
                .pdf(&batch[0], pages, read.clone(), *total, title.clone(), later)
                .map(|result| results[0] = result),
            Payload::Doc {
                chunks,
                read,
                total,
                characters,
            } => self
                .doc(&batch[0], chunks, read.clone(), *total, *characters, later)
                .map(|result| results[0] = result),
            Payload::Audio { pieces, duration } => self
                .audio(&batch[0], pieces, *duration)
                .map(|result| results[0] = result),
            Payload::Video {
                frames,
                sound,
                duration,
            } => self
                .video(&batch[0], frames, sound, *duration)
                .map(|result| results[0] = result),
            Payload::Code(code) => self.code(&batch[0], code).map(|result| results[0] = result),
        };
        if let Err(error) = body {
            // A dead worker, a model mismatch: the run stops (pending files stay pending).
            match &error {
                Error::Worker(_) | Error::NotFound(_) | Error::Llama(_) | Error::Indexer(_) => {
                    return Err(error)
                }
                _ => {}
            }
            if let Some(step) = &prepared.step {
                return Ok(vec![self.stop_reading(
                    &batch[0],
                    step.start,
                    &format!("{error}"),
                )?]);
            }
            for (slot, file) in batch.iter().enumerate() {
                if !prepared.outcomes.contains_key(&slot) {
                    self.store
                        .finish(file.id, "failed", Some(&format!("{error}")), HashMap::new())
                        .map_err(Error::from)?;
                    results[slot] = ("failed".to_string(), Some(format!("{error}")));
                }
            }
        }
        Ok(results)
    }

    /// A long file that can't be read any further: it keeps the pages or chunks read so far,
    /// which stay searchable, and counts as done rather than failed or waiting to be tried
    /// again and again.
    fn stop_reading(
        &self,
        file: &FileRecord,
        place: usize,
        problem: &str,
    ) -> digup_core::db::Result<(String, Option<String>)> {
        let mut info = self.store.info_of(file.id)?;
        info.remove("read_to");
        info.remove("read_end");
        let unit = if file.kind == FileKind::Pdf {
            "page"
        } else {
            "chunk"
        };
        self.store.finish(
            file.id,
            "done",
            Some(&format!("read to {unit} {place}, not after: {problem}")),
            info,
        )?;
        Ok((
            "done".to_string(),
            Some(format!("stopped before {unit} {}: {problem}", place + 1)),
        ))
    }

    fn images(
        &mut self,
        batch: &[FileRecord],
        screenshot: bool,
        ready: &[ReadyImage],
        results: &mut [(String, Option<String>)],
    ) -> Result<()> {
        if ready.is_empty() {
            return Ok(());
        }
        self.full_worker()?;
        let paths: Vec<String> = ready.iter().map(|item| item.image.path.clone()).collect();
        let budget = if screenshot {
            self.options.screenshot_budget
        } else {
            self.options.photo_budget
        };
        let vectors = self
            .worker
            .as_ref()
            .expect("the embedder just started")
            .embed_images(&paths, budget as i32)?;
        self.store.db.transaction(|| {
            for (position, item) in ready.iter().enumerate() {
                let file = &batch[item.slot];
                let embedding = &vectors[position];
                let Some(vector) = &embedding.vector else {
                    self.store.finish(
                        file.id,
                        "failed",
                        embedding.error.as_deref(),
                        HashMap::new(),
                    )?;
                    results[item.slot] = ("failed".to_string(), embedding.error.clone());
                    continue;
                };
                self.store.add_segment(
                    file.id,
                    SegmentKind::Image,
                    Modality::Image,
                    None,
                    None,
                    None,
                    None,
                    Some(vector),
                )?;
                // The words on screen are for keyword search: codes, names and errors typed
                // exactly. Meaning comes from the picture alone (Indexer.swift:555).
                if !item.ocr.is_empty() {
                    let excerpt = doc::excerpt(&item.ocr, 240);
                    self.store.add_segment(
                        file.id,
                        SegmentKind::Ocr,
                        Modality::Text,
                        None,
                        None,
                        Some(&excerpt),
                        Some(&item.ocr),
                        None,
                    )?;
                }
                let mut info = HashMap::new();
                info.insert("width".to_string(), item.image.width.to_string());
                info.insert("height".to_string(), item.image.height.to_string());
                if let Some(taken) = &item.image.taken {
                    info.insert("taken".to_string(), taken.clone());
                }
                // Swift counts graphemes for both (`item.ocr.count`, Indexer.swift:563, 565):
                // an OCR line of marks-on-letters says half the characters the engine says.
                let ocr_chars = digup_core::xmlscan::grapheme_len(&item.ocr);
                if screenshot {
                    info.insert("ocr_chars".to_string(), ocr_chars.to_string());
                }
                // The ocr line is the event's note; the file's detail stays nil — Swift's
                // `finish(file.id, state: "done", info: info)` carries no detail (Indexer.swift:565).
                let note = if screenshot {
                    Some(ocr_note(&item.ocr))
                } else {
                    None
                };
                self.store.finish(file.id, "done", None, info)?;
                results[item.slot] = ("done".to_string(), note);
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Pages `read` of a PDF (all of a short one, a step of a long one). A long one stays
    /// `partial` with `read_to` and `read_end` (pages, from 0) in its info until its last step.
    #[allow(clippy::type_complexity)]
    fn pdf(
        &mut self,
        file: &FileRecord,
        pages: &[pdf::Page],
        read: std::ops::Range<usize>,
        total: usize,
        title: Option<String>,
        later: bool,
    ) -> Result<(String, Option<String>)> {
        let max_pages = self.options.max_pages as usize;
        let end = total.min(max_pages);
        let unread = read.end < end;
        if pages.is_empty() && !later && !unread {
            self.store
                .finish(file.id, "skipped", Some("no pages"), HashMap::new())
                .map_err(Error::from)?;
            return Ok(("skipped".to_string(), Some("no pages".to_string())));
        }
        let title = title.unwrap_or_else(|| {
            Path::new(&file.path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let text_pages: Vec<&pdf::Page> = pages
            .iter()
            .filter(|page| page.image_path.is_none())
            .collect();
        // A page that's mostly digits (a table, a data listing) gets keyword search only.
        let word_pages: Vec<&pdf::Page> = text_pages
            .iter()
            .copied()
            .filter(|page| has_prose(&page.text))
            .collect();
        let image_pages: Vec<&pdf::Page> = pages
            .iter()
            .filter(|page| page.image_path.is_some())
            .collect();
        let garbled_pages: Vec<&pdf::Page> =
            pages.iter().filter(|page| page.garbled.is_some()).collect();
        let mut text_vectors: HashMap<usize, Vec<f32>> = HashMap::new();
        let mut image_vectors: Vec<Embedding> = Vec::new();
        let mut garbled_vectors: Vec<Vec<f32>> = Vec::new();
        if !word_pages.is_empty() || !image_pages.is_empty() {
            self.full_worker()?;
            let worker = self.worker.as_ref().expect("the embedder just started");
            let vectors = worker.embed_texts(
                &word_pages
                    .iter()
                    .map(|page| prompts::document(Some(&title), &grapheme_prefix(&page.text, 4000)))
                    .collect::<Vec<String>>(),
            )?;
            for (page, vector) in word_pages.iter().zip(vectors) {
                text_vectors.insert(page.number, vector);
            }
            image_vectors = worker.embed_images(
                &image_pages
                    .iter()
                    .filter_map(|page| page.image_path.clone())
                    .collect::<Vec<String>>(),
                self.options.page_budget as i32,
            )?;
            garbled_vectors = worker.embed_texts(
                &garbled_pages
                    .iter()
                    .map(|page| {
                        prompts::document(
                            Some(&title),
                            &grapheme_prefix(page.garbled.as_deref().unwrap_or(""), 4000),
                        )
                    })
                    .collect::<Vec<String>>(),
            )?;
        }
        self.store.db.transaction(|| {
            // Meaning only: the garbled words are never shown or matched against what you type.
            for (page, vector) in garbled_pages.iter().zip(garbled_vectors.iter()) {
                self.store.add_segment(
                    file.id,
                    SegmentKind::Page,
                    Modality::Text,
                    Some(page.number as f64),
                    None,
                    None,
                    None,
                    Some(vector),
                )?;
            }
            for page in &text_pages {
                let excerpt = doc::excerpt(&page.text, 240);
                self.store.add_segment(
                    file.id,
                    SegmentKind::Page,
                    Modality::Text,
                    Some(page.number as f64),
                    None,
                    Some(&excerpt),
                    Some(&page.text),
                    text_vectors.get(&page.number).map(|v| v.as_slice()),
                )?;
            }
            for (index, page) in image_pages.iter().enumerate() {
                let embedding = &image_vectors[index];
                let image_excerpt = if page.text.is_empty() {
                    None
                } else {
                    Some(doc::excerpt(&page.text, 240))
                };
                self.store.add_segment(
                    file.id,
                    SegmentKind::PageImage,
                    Modality::Image,
                    Some(page.number as f64),
                    None,
                    image_excerpt.as_deref(),
                    Some(&page.text),
                    embedding.vector.as_deref(),
                )?;
            }
            let mut info: HashMap<String, String> = if later {
                self.store.info_of(file.id)?
            } else {
                HashMap::new()
            };
            info.insert("pages".to_string(), total.to_string());
            let scanned: usize = info
                .get("scanned_pages")
                .and_then(|text| text.parse::<usize>().ok())
                .unwrap_or(0)
                + image_pages.len();
            info.insert("scanned_pages".to_string(), scanned.to_string());
            if unread {
                info.insert("read_to".to_string(), read.end.to_string());
                info.insert("read_end".to_string(), end.to_string());
            } else {
                info.remove("read_to");
                info.remove("read_end");
            }
            let detail = if unread {
                Some(format!("read to page {} of {total}", read.end))
            } else if end < total {
                Some(format!("the first {end} of {total} pages"))
            } else {
                None
            };
            self.store.finish(
                file.id,
                if unread { "partial" } else { "done" },
                detail.as_deref(),
                info,
            )?;
            Ok(())
        })?;
        let ocr = pages.iter().filter(|page| page.ocr).count();
        let span = if later {
            format!("pages {}–{} of {total}", read.start + 1, read.end)
        } else {
            format!("{}/{} pages", pages.len(), total)
        };
        Ok((
            if unread {
                "partial".to_string()
            } else {
                "done".to_string()
            },
            Some(format!(
                "{span}, {} as images{}",
                image_pages.len(),
                if ocr > 0 {
                    format!(", {ocr} read by OCR")
                } else {
                    String::new()
                }
            )),
        ))
    }

    /// Chunks `read` of a document (all of a short one, a step of a long one), the way `pdf`
    /// reads pages.
    fn doc(
        &mut self,
        file: &FileRecord,
        chunks: &[String],
        read: std::ops::Range<usize>,
        total: usize,
        characters: usize,
        later: bool,
    ) -> Result<(String, Option<String>)> {
        if total == 0 {
            self.store
                .finish(file.id, "skipped", Some("no text"), HashMap::new())
                .map_err(Error::from)?;
            return Ok(("skipped".to_string(), Some("no text".to_string())));
        }
        let title = Path::new(&file.path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Text that's mostly digits (a data dump saved as .txt) gets keyword search only.
        let worded: Vec<usize> = chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| has_prose(chunk))
            .map(|(index, _)| index)
            .collect();
        let vectors = if worded.is_empty() {
            Vec::new()
        } else {
            self.full_worker()?;
            self.worker
                .as_ref()
                .expect("the embedder just started")
                .embed_texts(
                    &worded
                        .iter()
                        .map(|index| prompts::document(Some(&title), &chunks[*index]))
                        .collect::<Vec<String>>(),
                )?
        };
        let vector: HashMap<usize, Vec<f32>> = worded.iter().copied().zip(vectors).collect();
        let end = total.min(self.options.max_chunks as usize);
        let unread = read.end < end;
        self.store.db.transaction(|| {
            for (offset, chunk) in chunks.iter().enumerate() {
                let excerpt = doc::excerpt(chunk, 240);
                self.store.add_segment(
                    file.id,
                    SegmentKind::Chunk,
                    Modality::Text,
                    Some((read.start + offset + 1) as f64),
                    None,
                    Some(&excerpt),
                    Some(chunk),
                    vector.get(&offset).map(|v| v.as_slice()),
                )?;
            }
            let mut info: HashMap<String, String> = HashMap::new();
            info.insert("chunks".to_string(), end.to_string());
            info.insert("chars".to_string(), characters.to_string());
            if unread {
                info.insert("read_to".to_string(), read.end.to_string());
                info.insert("read_end".to_string(), end.to_string());
            }
            let detail = if unread {
                Some(format!("read to chunk {} of {end}", read.end))
            } else if end < total {
                Some(format!("the first {end} chunks"))
            } else {
                None
            };
            self.store.finish(
                file.id,
                if unread { "partial" } else { "done" },
                detail.as_deref(),
                info,
            )?;
            Ok(())
        })?;
        let span = if later {
            format!("chunks {}–{} of {end}", read.start + 1, read.end)
        } else if unread {
            format!("{} of {end} chunks", chunks.len())
        } else {
            format!("{} chunks", chunks.len())
        };
        Ok((
            if unread {
                "partial".to_string()
            } else {
                "done".to_string()
            },
            Some(format!(
                "{span}{}",
                if worded.len() < chunks.len() {
                    format!(", {} for keywords only", chunks.len() - worded.len())
                } else {
                    String::new()
                }
            )),
        ))
    }

    /// A code file's stretches of lines (a notebook's of cells), each embedded with the file's
    /// path in its repo as its title, and kept with its lines for keyword search and for
    /// opening at the place.
    fn code(&mut self, file: &FileRecord, code: &code::File) -> Result<(String, Option<String>)> {
        if code.pieces.is_empty() {
            self.store
                .finish(file.id, "skipped", Some("no code"), HashMap::new())
                .map_err(Error::from)?;
            return Ok(("skipped".to_string(), Some("no code".to_string())));
        }
        // Lines that are mostly numbers and symbols (a table of constants) get keyword search only.
        let worded: Vec<usize> = code
            .pieces
            .iter()
            .enumerate()
            .filter(|(_, piece)| has_words(&piece.text))
            .map(|(index, _)| index)
            .collect();
        let vectors = if worded.is_empty() {
            Vec::new()
        } else {
            self.full_worker()?;
            self.worker
                .as_ref()
                .expect("the embedder just started")
                .embed_texts(
                    &worded
                        .iter()
                        .map(|index| {
                            prompts::document(Some(&code.title), &code.pieces[*index].text)
                        })
                        .collect::<Vec<String>>(),
                )?
        };
        let vector: HashMap<usize, Vec<f32>> = worded.iter().copied().zip(vectors).collect();
        self.store.db.transaction(|| {
            for (offset, piece) in code.pieces.iter().enumerate() {
                let summary = code::summary(&piece.text);
                self.store.add_segment(
                    file.id,
                    if code.cells.is_none() {
                        SegmentKind::Lines
                    } else {
                        SegmentKind::Cells
                    },
                    Modality::Text,
                    Some(piece.first as f64),
                    Some(piece.last as f64),
                    Some(&summary),
                    Some(&piece.text),
                    vector.get(&offset).map(|v| v.as_slice()),
                )?;
            }
            let mut info: HashMap<String, String> = HashMap::new();
            info.insert("lines".to_string(), code.lines.to_string());
            if let Some(cells) = code.cells {
                info.insert("cells".to_string(), cells.to_string());
            }
            if let Some(repo) = &code.repo {
                info.insert("repo".to_string(), repo.clone());
            }
            self.store.finish(file.id, "done", None, info)?;
            Ok(())
        })?;
        Ok((
            "done".to_string(),
            Some(format!(
                "{} stretches, {}{}",
                code.pieces.len(),
                match code.cells {
                    Some(cells) => format!("{cells} cells"),
                    None => format!("{} lines", code.lines),
                },
                if worded.len() < code.pieces.len() {
                    format!(", {} for keywords only", code.pieces.len() - worded.len())
                } else {
                    String::new()
                }
            )),
        ))
    }

    fn audio(
        &mut self,
        file: &FileRecord,
        pieces: &[media::MediaPiece],
        duration: f64,
    ) -> Result<(String, Option<String>)> {
        if pieces.is_empty() {
            self.store
                .finish(
                    file.id,
                    "skipped",
                    Some("silent or shorter than 2 s"),
                    HashMap::new(),
                )
                .map_err(Error::from)?;
            return Ok((
                "skipped".to_string(),
                Some("silent or too short".to_string()),
            ));
        }
        self.full_worker()?;
        let vectors = self
            .worker
            .as_ref()
            .expect("the embedder just started")
            .embed_audio(
                &pieces
                    .iter()
                    .map(|piece| piece.path.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
            )?;
        self.store.db.transaction(|| {
            for (piece, embedding) in pieces.iter().zip(vectors.iter()) {
                self.store.add_segment(
                    file.id,
                    SegmentKind::Audio,
                    Modality::Audio,
                    Some(piece.start),
                    Some(piece.end),
                    None,
                    None,
                    embedding.vector.as_deref(),
                )?;
            }
            let mut info: HashMap<String, String> = HashMap::new();
            info.insert("duration".to_string(), format!("{duration:.1}"));
            self.store.finish(file.id, "done", None, info)?;
            Ok(())
        })?;
        Ok((
            "done".to_string(),
            Some(format!("{} windows, {} s", pieces.len(), duration.trunc())),
        ))
    }

    fn video(
        &mut self,
        file: &FileRecord,
        frames: &[media::MediaPiece],
        sound: &[media::MediaPiece],
        duration: f64,
    ) -> Result<(String, Option<String>)> {
        if frames.is_empty() && sound.is_empty() {
            self.store
                .finish(
                    file.id,
                    "skipped",
                    Some("no frames or sound"),
                    HashMap::new(),
                )
                .map_err(Error::from)?;
            return Ok((
                "skipped".to_string(),
                Some("no frames or sound".to_string()),
            ));
        }
        self.full_worker()?;
        let frame_vectors = self
            .worker
            .as_ref()
            .expect("the embedder just started")
            .embed_images(
                &frames
                    .iter()
                    .map(|piece| piece.path.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
                self.options.frame_budget as i32,
            )?;
        let sound_vectors = self
            .worker
            .as_ref()
            .expect("the embedder is running")
            .embed_audio(
                &sound
                    .iter()
                    .map(|piece| piece.path.to_string_lossy().into_owned())
                    .collect::<Vec<String>>(),
            )?;
        self.store.db.transaction(|| {
            for (frame, embedding) in frames.iter().zip(frame_vectors.iter()) {
                self.store.add_segment(
                    file.id,
                    SegmentKind::Frame,
                    Modality::Image,
                    Some(frame.start),
                    Some(frame.end),
                    None,
                    None,
                    embedding.vector.as_deref(),
                )?;
            }
            for (piece, embedding) in sound.iter().zip(sound_vectors.iter()) {
                self.store.add_segment(
                    file.id,
                    SegmentKind::Audio,
                    Modality::Audio,
                    Some(piece.start),
                    Some(piece.end),
                    None,
                    None,
                    embedding.vector.as_deref(),
                )?;
            }
            let mut info: HashMap<String, String> = HashMap::new();
            info.insert("duration".to_string(), format!("{duration:.1}"));
            info.insert("frames".to_string(), frames.len().to_string());
            info.insert("sound_windows".to_string(), sound.len().to_string());
            self.store.finish(file.id, "done", None, info)?;
            Ok(())
        })?;
        Ok((
            "done".to_string(),
            Some(format!(
                "{} frames, {} sound windows, {} s",
                frames.len(),
                sound.len(),
                duration.trunc()
            )),
        ))
    }
}

/// The screenshot event's note (`Indexer.swift:565`: `"ocr \(item.ocr.count) chars"`) —
/// the character count is grapheme clusters, as Swift's `String.count` is.
fn ocr_note(ocr: &str) -> String {
    format!("ocr {} chars", digup_core::xmlscan::grapheme_len(ocr))
}

/// `String.prefix(4000)` — Swift counts grapheme clusters, so combining marks ride with their
/// base letter (`unicode-segmentation`, the same rule the search text side follows).
fn grapheme_prefix(text: &str, count: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    text.graphemes(true).take(count).collect()
}

/// Issue #7a: is this path CONFIRMED gone — `NotFound`, or `NotADirectory` from its own stat?
/// `Path::exists` answers false for EVERY stat error, and that is the answer that deletes an
/// index: a network mount in the middle of a remount answers `ESTALE` after a server restart,
/// `EIO`, `ENOTCONN`, `EACCES` — a live folder, read by `exists()` as "the deleted folder the
/// events named", with every row and vector under it pruned. A keep is free and the next sync
/// rechecks; a delete is not, so an inconclusive stat keeps. `NotADirectory` rides the same
/// law: a folder under a parent that stopped being one is a gone folder's other spelling.
fn folder_is_gone(path: &str) -> bool {
    match std::fs::symlink_metadata(Path::new(path)) {
        Ok(_) => false,
        Err(error) => matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ),
    }
}

/// The Swift `defer { reading = nil }` of `prepare`: the reading stamp clears on every exit
/// path, including panics.
struct ReadingGuard;

impl Drop for ReadingGuard {
    fn drop(&mut self) {
        end_reading();
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.close_worker();
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::{Job, Payload};
    use digup_core::options::IndexOptions;
    use digup_core::{FileKind, FileRecord};

    fn record(path: &str, kind: FileKind) -> FileRecord {
        FileRecord {
            id: 1,
            path: path.to_string(),
            kind,
            size: 1_000,
            mtime: 1.0,
            inode: 7,
            device: 3,
            state: "pending".to_string(),
            detail: None,
        }
    }

    /// `characters: text.count` (Indexer.swift:446) — Foundation counts grapheme clusters,
    /// and the number is a wire fact: it lands in `files.info` as "chars" (Indexer.swift:
    /// 650), the doc's progress lines and its cost forecast read it, and the code index's
    /// "big file" rule does. A Devanagari / Tamil / Thai text file (base letter + matra
    /// pairs) has HALF the characters the scalar count reports: 40 pairs = 40 characters,
    /// 80 scalars.
    #[test]
    fn doc_characters_count_grapheme_clusters() {
        let devanagari = "\u{915}\u{93F}".repeat(40); // 40 clusters, 80 scalars

        let dir = std::env::temp_dir().join(format!("digup-indexer-chars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch folder");
        let file = dir.join("note.txt");
        std::fs::write(&file, format!("{devanagari}\n")).expect("the doc");

        let job = Job {
            files: vec![record(&file.to_string_lossy(), FileKind::Doc)],
            step: None,
        };
        let prepared =
            super::Indexer::prepare_static(&job, &IndexOptions::default(), &dir.join("batch-0"));
        // The file is 40 base+matra pairs and a newline: 41 characters, 81 scalars.
        match prepared.payload {
            Payload::Doc { characters, .. } => assert_eq!(
                characters, 41,
                "the engine's character count (40 clusters + the newline), not the scalar \
                 count (81)"
            ),
            _ => panic!("a doc job prepares a doc payload"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The screenshot note (`Indexer.swift:565`: `"ocr \(item.ocr.count) chars"`) counts
    /// graphemes too — an OCR line read off a Hindi / Thai screenshot says half the
    /// characters the engine says, in the event line the UI shows.
    #[test]
    fn the_ocr_note_counts_grapheme_clusters() {
        let ocr = "\u{915}\u{93F}".repeat(12); // 12 clusters, 24 scalars
        assert_eq!(super::ocr_note(&ocr), "ocr 12 chars");
        assert_eq!(super::ocr_note(""), "ocr 0 chars");
        assert_eq!(super::ocr_note("hello"), "ocr 5 chars");
    }

    /// The keep/prune law's new hinge (issue #7a review): what counts as a GONE folder.
    #[cfg(unix)]
    #[test]
    fn a_stat_error_that_is_not_a_delete_is_not_a_gone_folder() {
        use std::os::unix::fs::PermissionsExt;
        let work = std::env::temp_dir().join("digup-gone-law");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("file.txt"), b"hello").unwrap();
        std::fs::create_dir_all(work.join("sealed")).unwrap();
        std::fs::set_permissions(work.join("sealed"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        // A folder that exists is not gone. A SEALED one is not gone either: its own stat
        // answers, and only the listing of its contents fails — which is the walk's blindness
        // the #2 law keeps rows for. `Path::exists` says "false" for both.
        assert!(!super::folder_is_gone(
            &work.join("sealed").to_string_lossy()
        ));
        assert!(!super::folder_is_gone(
            &work.join("file.txt").to_string_lossy()
        ));
        // The two spellings of GONE the engine acts on, and only these.
        assert!(super::folder_is_gone(
            &work.join("missing").to_string_lossy()
        ));
        assert!(super::folder_is_gone(
            &work.join("file.txt/child").to_string_lossy()
        ));
        std::fs::set_permissions(work.join("sealed"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let _ = std::fs::remove_dir_all(&work);
    }
}
