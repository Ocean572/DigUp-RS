//! The crawl (`Crawler.swift`): walks the chosen folders applying `Rules`, reading metadata
//! only, never file contents (except the 600-byte sniff that tells a real MPEG-TS video from
//! a TypeScript file). For the code index it walks the code folders instead: code files only,
//! in a git repo only those git keeps, and none in vendored or built folders.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::model::{Candidate, FileKind};
use crate::options::IndexOptions;
use crate::paths;
use crate::rules::{self, Classification};

/// What the crawl found: the files to index and, by reason, what it left out (`CrawlReport`).
#[derive(Debug, Default)]
pub struct CrawlReport {
    pub candidates: Vec<Candidate>,
    /// reason → files
    pub skipped: HashMap<String, i64>,
    /// reason → folders not walked into
    pub skipped_folders: HashMap<String, i64>,
    /// folder → files skipped as a dataset
    pub datasets: HashMap<String, i64>,
    /// root → (reason → files), the same counts as `skipped` split by folder
    pub left_out: HashMap<String, HashMap<String, i64>>,
    /// Paths the walk could not read (issue #2: a sealed subfolder, an ejected mount below the
    /// root). The Swift enumerator's `errorHandler: { _, _ in true }` kept walking and reported
    /// nothing, so an unreadable folder looked like an EMPTY one and the sync took "not seen"
    /// for "deleted" and dropped rows whose files were on disk. The paths ride to the sync,
    /// which keeps rows under them like the vanished roots; they are NOT relayed on the wire.
    pub scan_failures: HashSet<String>,
}

impl CrawlReport {
    fn left_out(&mut self, reason: &str, under: &str) {
        *self.skipped.entry(reason.to_string()).or_default() += 1;
        *self
            .left_out
            .entry(under.to_string())
            .or_default()
            .entry(reason.to_string())
            .or_default() += 1;
    }
}

/// Entries checked during a folder walk, cumulative across roots and not an indexing total
/// (`CrawlProgress`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlProgress {
    pub checked: i64,
    pub root: Option<String>,
}

/// `FileSystem.info`: the file's own facts, without following a link. `dataless` (an offline
/// cloud placeholder) is the Apple rule: macOS reads it from `st_flags`; on Linux there are no
/// dataless placeholders, so it is always `false`.
pub struct FileInfo {
    pub is_regular: bool,
    pub size: i64,
    pub mtime: f64,
    pub inode: u64,
    pub device: u64,
    pub dataless: bool,
}

pub fn file_info(path: &str) -> Option<FileInfo> {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        // lstat, not stat: Foundation reports the link itself, and a link is "not regular".
        let meta = std::fs::symlink_metadata(path).ok()?;
        const S_IFMT: u32 = 0o170_000;
        const S_IFREG: u32 = 0o100_000;
        const UF_DATAVAULT: u32 = 0o10; // st_flags: an iCloud dataless (offline) placeholder
        Some(FileInfo {
            is_regular: meta.st_mode() & S_IFMT == S_IFREG,
            size: meta.st_size() as i64,
            mtime: meta.st_mtime() as f64 + meta.st_mtime_nsec() as f64 / 1e9,
            inode: meta.st_ino() as u64,
            device: meta.st_dev() as u64,
            dataless: meta.st_flags() & UF_DATAVAULT != 0,
        })
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::fs::MetadataExt;
        // lstat, not stat: the engine reports the link itself, and a link is "not regular".
        let meta = std::fs::symlink_metadata(path).ok()?;
        const S_IFMT: u32 = 0o170_000;
        const S_IFREG: u32 = 0o100_000;
        Some(FileInfo {
            is_regular: meta.st_mode() & S_IFMT == S_IFREG,
            size: meta.st_size() as i64,
            mtime: meta.st_mtime() as f64 + meta.st_mtime_nsec() as f64 / 1e9,
            inode: meta.st_ino() as u64,
            device: meta.st_dev() as u64,
            dataless: false,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Windows and others: the portable facts. There is no dataless rule to read here (the
        // Apple rule is macOS-only), so `dataless` stays false, as on Linux.
        let meta = std::fs::symlink_metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_secs_f64())
            .unwrap_or(0.0);
        Some(FileInfo {
            is_regular: meta.is_file(),
            size: meta.len() as i64,
            mtime,
            inode: 0,
            device: 0,
            dataless: false,
        })
    }
}

/// `FileSystem.canList` (opendir on Linux): opening is enough — listing a cloud folder's top
/// level could take a while, at every sync.
pub fn can_list(path: &str) -> bool {
    std::fs::read_dir(path).is_ok()
}

/// `GitRepos`: the git repos a code crawl walks through, and the files each keeps. A folder
/// that isn't in a repo, or a machine without git, has no list: everything there counts.
struct GitRepos {
    /// Repo folder → the files git keeps in it (paths relative to it), None when git couldn't say.
    files: HashMap<String, Option<HashSet<String>>>,
    /// Folder → the repo it's in, as the walk found them.
    repo_of: HashMap<String, Option<String>>,
}

impl GitRepos {
    fn new() -> Self {
        Self {
            files: HashMap::new(),
            repo_of: HashMap::new(),
        }
    }

    /// A chosen folder: a repo itself, or inside one.
    fn enter_root(&mut self, path: &str) {
        let mut folder = path.to_string();
        loop {
            if Path::new(&folder).join(".git").exists() {
                self.files.insert(folder.clone(), git_files(&folder));
                self.repo_of.insert(path.to_string(), Some(folder));
                return;
            }
            let parent = paths::parent(&folder);
            if parent == folder || parent.len() <= 1 {
                break;
            }
            folder = parent;
        }
        self.repo_of.insert(path.to_string(), None);
    }

    /// A folder the walk goes into: a repo of its own when it has a .git folder.
    fn enter(&mut self, path: &str) {
        let git = Path::new(path).join(".git");
        if std::fs::metadata(&git)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            self.files.insert(path.to_string(), git_files(path));
            self.repo_of
                .insert(path.to_string(), Some(path.to_string()));
        }
    }

    /// The repo `folder` is in, from the closest folder the walk has seen.
    fn repo_of(&mut self, folder: &str) -> Option<String> {
        if let Some(known) = self.repo_of.get(folder) {
            return known.clone();
        }
        let parent = paths::parent(folder);
        let answer = if parent != folder && parent.len() > 1 {
            self.repo_of(&parent)
        } else {
            None
        };
        self.repo_of.insert(folder.to_string(), answer.clone());
        answer
    }

    /// Whether git ignores the file at `path` (in a repo whose files git listed).
    fn ignores(&mut self, path: &str) -> bool {
        let folder = paths::parent(path);
        let Some(repo) = self.repo_of(&folder) else {
            return false;
        };
        let Some(Some(kept)) = self.files.get(&repo).cloned() else {
            return false;
        };
        !kept.contains(&path[repo.len() + 1..])
    }
}

/// `GitFiles.list`: paths relative to the repo that git keeps (tracked plus untracked not
/// ignored), or None when git can't say. A repo that arrived unzipped can name programs in
/// its config (core.fsmonitor runs for ls-files): those settings are overridden and the
/// system's and user's config files are left out, so no config git reads here runs anything.
fn git_files(repo: &str) -> Option<HashSet<String>> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
            repo,
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect(),
    )
}

/// `Crawler.crawl`: walk the roots, apply the rules, report what was found and left out.
/// The roots are walked in order; `progress` hears each root's start, every quarter second
/// of entries, and the end.
pub fn crawl(
    roots: &[PathBuf],
    options: &IndexOptions,
    progress: &mut dyn FnMut(&CrawlProgress),
) -> CrawlReport {
    let home = paths::home().to_string_lossy().into_owned();
    let mut report = CrawlReport::default();
    let mut project_roots: HashSet<String> = HashSet::new();
    let mut repos = GitRepos::new();
    let mut found: Vec<Candidate> = Vec::new();
    // path → the chosen folder it was found under (a folder inside another root would be
    // walked twice).
    let mut found_root: HashMap<String, String> = HashMap::new();
    // Issue #3: nested chosen roots walk the SAME files twice, and every per-path report
    // is reported ONCE — the first walk to reach it, attributed to the deepest root (the
    // `under` computed below is the same string in both walks). Files and folder records
    // keep their own sets: a file's facts and a folder's skip are different claims.
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut seen_folders: HashSet<String> = HashSet::new();
    // `rootPaths = Set(roots.map { $0.standardizedFileURL.path })` (Crawler.swift:60) and
    // `for root in roots.map({ $0.standardizedFileURL })` (:65): the walk runs on the
    // STANDARDIZED roots — canonical strings for every join (candidate paths), every
    // `leftOut` key, every progress `root:`, and the dataset roots exclusion.
    let roots: Vec<PathBuf> = roots
        .iter()
        .map(|root| PathBuf::from(paths::standardize(&root.to_string_lossy())))
        .collect();
    let root_paths: HashSet<String> = roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    // Issue #3 — the deepest chosen root containing a path is ITS root: a folder chosen
    // inside another chosen folder is the more specific instruction, and every per-path
    // report (the `leftOut` attribution, the `found_root` entry, the project-rule boundary)
    // rides the deepest root. A single-root walk (every pinned fixture) has one root and
    // reports exactly as before. Sorted longest-first so the first match is the deepest.
    let mut sorted_roots: Vec<String> = root_paths.iter().cloned().collect();
    sorted_roots.sort_by_key(|root| std::cmp::Reverse(root.chars().count()));
    let mut checked: i64 = 0;

    for root in roots {
        let root = root.to_string_lossy().into_owned();
        progress(&CrawlProgress {
            checked,
            root: Some(root.clone()),
        });
        let mut last_update = Instant::now();
        if rules::is_cache_directory(&root) {
            *report
                .skipped_folders
                .entry("cache folder".to_string())
                .or_default() += 1;
            continue;
        }
        if options.code {
            repos.enter_root(&root);
        }
        // A skip applies inside the chosen folders that contain it: a folder chosen inside a
        // skipped one is walked whole (`FolderSelection`). The walk reports real paths.
        let real = paths::real_path(&root);
        let skips: Vec<String> = options
            .excluded_folders
            .iter()
            .filter(|folder| paths::is_inside(folder, &root) || paths::is_inside(folder, &real))
            .flat_map(|folder| [folder.clone(), paths::real_path(folder)])
            .collect();

        // The walk, as Foundation's enumerator does it on Linux: pre-order depth-first over
        // the root's *contents* (the root itself is never enumerated, so its own name is
        // never a skip reason), hidden entries not enumerated (their subtrees with them),
        // symlinked folders listed but not descended into, and a skipped folder pruned whole
        // (`skipDescendants`).
        let mut walker = walkdir::WalkDir::new(&root).follow_links(false).into_iter();
        while let Some(entry) = walker.next() {
            let entry = match entry {
                Ok(entry) => entry,
                // The enumerator's errorHandler: { _, _ in true } — walk on, report nothing…
                // except WHERE it failed (issue #2): the failed path rides to the sync, which
                // keeps its rows. A folder the walk could not read is not an empty folder.
                Err(err) => {
                    if let Some(path) = err.path() {
                        report
                            .scan_failures
                            .insert(paths::to_slashes(&path.to_string_lossy()));
                    }
                    continue;
                }
            };
            if entry.depth() == 0 {
                continue; // Foundation's enumerator starts with the root's contents
            }
            checked += 1;
            if last_update.elapsed().as_millis() >= 250 {
                progress(&CrawlProgress {
                    checked,
                    root: Some(root.clone()),
                });
                last_update = Instant::now();
            }
            // Windows: walkdir joins a slash-form root with `\` (PathBuf::push) — the
            // engine's ONE form is slashes (`to_slashes`; the identity on unix), the form
            // the scope ranges, counters, and Swift-era rows compare byte-wise.
            let path = paths::to_slashes(&entry.path().to_string_lossy());
            let name = paths::last_component(&path).to_string();
            // Issue #3: the root a path is ATTRIBUTED to is the deepest chosen root that
            // contains it — the more specific instruction. Every per-path report rides it:
            // the `leftOut` key, the `found_root` entry, the project-rule boundary below.
            // With one root (every pinned fixture) this is the root itself, byte-for-byte.
            let under: &str = sorted_roots
                .iter()
                .find(|root| path == root.as_str() || paths::is_inside(&path, root))
                .map(String::as_str)
                .unwrap_or(&root);
            if name.starts_with('.') {
                // Foundation's .skipsHiddenFiles: hidden entries are not enumerated, and
                // their whole subtree with them. walkdir still descends, so prune here.
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            // `values?.isDirectory` (Crawler.swift:96). Foundation reads URL resource values
            // with `lstat`, so a symlink is NOT a directory — measured on BOTH Foundation
            // families: Linux swift-corelibs 6.4 (2026-11-14 probe `/tmp/opencode/crawlprobe.swift`
            // in `digup-builder:fixes3`) AND Apple's Foundation on a Mac (`probe3.swift`,
            // 2026-11-15: a symlink-to-a-folder answers `isDirectory = false`,
            // `isSymbolicLink = true`). So a folder-behind-a-symlink goes to the FILE branch on
            // Linux and macOS alike: `Rules.classify` answers `.ignore` for its bare name →
            // `leftOut("other type")`; a link NAMED like a file (photo.jpg) classifies by its
            // extension, then `FileSystem.info` (lstat, `Files+Portable.swift:108-110`) fails
            // `isRegular` → `leftOut("link or special file")`. Windows is the one platform whose
            // walk reports the target's DIRECTORY attribute (`url.hasDirectoryPath`,
            // Crawler.swift:93-95, WIN32_FIND_DATAW) — unverified on this box, kept as its own
            // branch. The earlier "macOS follows the link" note was an unmeasured guess; following
            // it routed folder-symlinks into the folder branch (no `leftOut` row, and in a code
            // crawl `repos.enter`/`is_project_root` marked a linked folder through the link —
            // counts the engine never reports on Linux, and none on macOS either).
            let is_directory = if entry.file_type().is_symlink() {
                #[cfg(target_os = "windows")]
                let follows = std::fs::metadata(entry.path())
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false);
                #[cfg(not(target_os = "windows"))]
                let follows = false;
                follows
            } else {
                entry.file_type().is_dir()
            };
            // `url.hasDirectoryPath && packageExtensions`: a bundle as a real folder. A
            // symlink to one has no trailing slash in Foundation's listing, so it is not a
            // package there (and, being a folder to `isDirectory`, it takes the folder path).
            let is_package = entry.file_type().is_dir() && rules::is_package(&name);
            if is_directory && !is_package {
                let reason = rules::skip_reason(&path, &home, &skips).or_else(|| {
                    if options.code {
                        rules::code_skip_reason(&path)
                    } else {
                        None
                    }
                });
                if let Some(reason) = reason {
                    // Prune the folder's subtree. A symlinked folder has no subtree in this
                    // walk, and `skip_current_dir` there would skip the parent's rest.
                    if entry.file_type().is_dir() {
                        walker.skip_current_dir();
                    }
                    // The count is per unique folder: an overlapping root walks past the
                    // same skipped folder again, and the report must not double-count it.
                    if seen_folders.insert(path.clone()) {
                        *report.skipped_folders.entry(reason.clone()).or_default() += 1;
                        *report
                            .left_out
                            .entry(under.to_string())
                            .or_default()
                            .entry(format!("folders {}", reason))
                            .or_default() += 1;
                    }
                } else if options.code {
                    repos.enter(&path);
                } else if rules::is_project_root(&path) {
                    project_roots.insert(path.clone());
                }
                continue;
            }
            if is_package {
                walker.skip_current_dir(); // the enumerator never enters one; this walk would
                let library = matches!(
                    paths::extension(&name).to_lowercase().as_str(),
                    "photoslibrary" | "photolibrary"
                );
                if seen_folders.insert(path.clone()) {
                    report.left_out(
                        if library {
                            "Photos library"
                        } else {
                            "app or bundle"
                        },
                        under,
                    );
                }
                continue;
            }
            // Foundation skips hidden *files* by not enumerating them; the same filter here.
            if name.starts_with('.') {
                continue;
            }
            // Issue #3: a nested chosen root reaches the same files again; each file is
            // classified and reported once, by the first walk to it.
            if !seen_files.insert(path.clone()) {
                continue;
            }
            let classification = if options.code {
                rules::classify_code(&path, options)
            } else {
                rules::classify(&path, false, options)
            };
            match classification {
                Classification::Ignore => report.left_out("other type", under),
                Classification::Skip(reason) => report.left_out(&reason, under),
                Classification::Index(kind) => {
                    let Some(info) = file_info(&path) else {
                        report.left_out("link or special file", under);
                        continue;
                    };
                    if !info.is_regular {
                        report.left_out("link or special file", under);
                        continue;
                    }
                    if info.dataless {
                        report.left_out("in the cloud, not downloaded", under);
                        continue;
                    }
                    if options.code {
                        // Too big to be written by hand; a notebook's file holds its outputs too.
                        let limit = if paths::extension(&name).to_lowercase() == "ipynb" {
                            options.max_notebook_bytes
                        } else {
                            options.max_code_bytes
                        };
                        if info.size > limit {
                            report.left_out("too big to be written by hand", under);
                            continue;
                        }
                        if repos.ignores(&path) {
                            report.left_out("ignored by git", under);
                            continue;
                        }
                    }
                    if found_root.contains_key(&path) {
                        continue; // found under another root already
                    }
                    found_root.insert(path.clone(), under.to_string());
                    found.push(Candidate {
                        path,
                        kind,
                        size: info.size,
                        mtime: info.mtime,
                        inode: info.inode,
                        device: info.device,
                    });
                }
            }
        }
    }

    // Inside a code project only screenshots count (assets, fixtures and datasets don't).
    // Issue #3: the rule fires with the file's CONTAINING chosen root as its boundary, on
    // project markers STRICTLY inside that root. "A folder chosen on purpose is read whole,
    // whatever markers it holds" (Crawler.swift, the comment at the project registration):
    // the files of a chosen root are not "inside" it. Single-root walks — every pinned
    // fixture, parent-only roots and project-only roots alike — behave byte-for-byte as
    // before; the new case is parent+project: the project's own media indexes, exactly as
    // when the project is chosen alone.
    let mut in_project: HashMap<(String, String), bool> = HashMap::new();
    let mut is_in_project = |folder: String, root: String| -> bool {
        is_in_project_under(&mut in_project, &folder, &root, &project_roots)
    };
    let mut kept: Vec<Candidate> = Vec::new();
    for file in found {
        let folder = paths::parent(&file.path);
        let under = found_root.get(&file.path).cloned().unwrap_or_default();
        if !options.code
            && file.kind != FileKind::Screenshot
            && is_in_project(folder, under.clone())
        {
            report.left_out("inside a code project", &under);
        } else {
            kept.push(file);
        }
    }

    if !options.include_datasets && !options.code {
        let datasets = rules::dataset_folders(
            &kept
                .iter()
                .map(|file| {
                    let name = paths::last_component(&file.path).to_string();
                    (
                        paths::parent(&file.path),
                        paths::stem(&name).to_string(),
                        file.kind,
                    )
                })
                .collect::<Vec<_>>(),
            &root_paths,
            300,
        );
        if !datasets.counts.is_empty() {
            report.datasets = datasets.counts;
            // Issue #1: the drop is scoped to the kinds that TRIGGERED the folder. A 300-image
            // training pile is dataset media; the one video and the one song parked beside it
            // are the user's, and their rows live. (The old filter keyed on the folder and
            // swept every media kind: the stragglers died with the pile.)
            kept.retain(|file| {
                let skip = datasets
                    .kinds
                    .get(&paths::parent(&file.path))
                    .is_some_and(|kinds| kinds.contains(&file.kind));
                if skip {
                    report.left_out(
                        "dataset",
                        found_root.get(&file.path).map(String::as_str).unwrap_or(""),
                    );
                }
                !skip
            });
        }
    }
    report.candidates = kept;
    progress(&CrawlProgress {
        checked,
        root: None,
    });
    report
}

/// `isInProject` with the issue #3 root boundary: the walk climbs from `folder` up and
/// stops AT the file's containing chosen root (`folder == root` → false — "the code-project
/// rule does not apply to the root itself (its files are not 'inside')", `crawler.rs`'s own
/// chosen-folder pin); it fires only on project roots strictly inside that root.
/// (`parent.count > 1` bounds the recursion at "/" as Swift's does.) Memoized per
/// (folder, root) — the engine's `inProject` dictionary, keyed for the new argument.
fn is_in_project_under(
    memo: &mut HashMap<(String, String), bool>,
    folder: &str,
    root: &str,
    project_roots: &HashSet<String>,
) -> bool {
    if folder == root {
        return false; // a chosen root is read whole, whatever markers it holds
    }
    let key = (folder.to_string(), root.to_string());
    if let Some(known) = memo.get(&key) {
        return *known;
    }
    let parent = paths::parent(folder);
    let answer = project_roots.contains(folder)
        || (parent != folder
            && parent.len() > 1
            && is_in_project_under(memo, &parent, root, project_roots));
    memo.insert(key, answer);
    answer
}
