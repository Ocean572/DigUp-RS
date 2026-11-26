//! The crawl, pinned to `Crawler.swift`'s behavior on Linux: what lands in `candidates`, and
//! the reason counts beside it (`skipped`, `skippedFolders`, `datasets`, `leftOut`). The
//! reason strings are the app's, so they are compared as written.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use digup_core::crawler::{self, CrawlProgress};
use digup_core::model::FileKind;
use digup_core::options::IndexOptions;

fn temp_tree(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn touch(root: &Path, name: &str, body: &[u8]) {
    let file = root.join(name);
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::fs::write(file, body).unwrap();
}

fn symlink(target: &str, link: &Path) {
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(target, link).unwrap();
}

fn names(candidates: &[digup_core::model::Candidate]) -> BTreeSet<String> {
    candidates
        .iter()
        .map(|file| {
            file.path
                .strip_prefix("/")
                .unwrap_or(&file.path)
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

fn count(report: &digup_core::crawler::CrawlReport, map: &str, reason: &str) -> i64 {
    let one = |map: &HashMap<String, i64>| *map.get(reason).unwrap_or(&0);
    match map {
        "skipped" => one(&report.skipped),
        "folders" => one(&report.skipped_folders),
        _ => unreachable!(),
    }
}

/// F2 — the walk runs on STANDARDIZED roots (`Crawler.swift:60, 65`:
/// `roots.map { $0.standardizedFileURL }`), so every string the crawl REPORTS is
/// canonical: the progress lines' `root:`, the `leftOut` keys, the candidate paths, and
/// the dataset roots exclusion. A root spelled `/tmp/x/./` (the operator's
/// `DIGUP_SERVER_APP_TARGETS` strings carry exactly such spellings) walks the SAME
/// folder Swift walks and reports `/tmp/x` everywhere. The audit-base walked the raw
/// string: progress roots said `/tmp/x/./`, candidates landed `/tmp/x/./clip_….jpg`,
/// the `leftOut` key was the messy root, and the root's own 300 photos became a
/// "dataset" — the roots exclusion (`!roots.contains(folder)`) never matched, because
/// the raw root string never equals the walked folder's path. Swift EXCLUDES the roots
/// from the dataset scan: the root's photos are indexed.
#[test]
fn a_messy_root_is_reported_canonical_everywhere() {
    let root = temp_tree("digup-crawler-standardize");
    for i in 0..300 {
        touch(&root, &format!("clip_{i:06}.jpg"), b"x");
    }
    touch(&root, "thing.xyz", b"x");
    let messy = PathBuf::from(format!("{}/./", root.display()));
    let canonical = root.to_string_lossy().to_string();

    let mut seen_roots: Vec<String> = Vec::new();
    let report = crawler::crawl(&[messy], &IndexOptions::default(), &mut |progress| {
        if let Some(root) = &progress.root {
            seen_roots.push(root.clone());
        }
    });

    assert!(!seen_roots.is_empty(), "the walk reports its roots");
    for seen in &seen_roots {
        assert_eq!(
            seen, &canonical,
            "progress `root:` is the standardized root"
        );
    }
    assert!(
        report.datasets.is_empty(),
        "the root is never its own dataset — roots are excluded (`Crawler.swift`)"
    );
    assert_eq!(report.skipped.get("dataset"), None, "no dataset skips");
    assert_eq!(
        report.left_out.len(),
        1,
        "one key, the canonical root: {:?}",
        report.left_out.keys()
    );
    let left_out = report
        .left_out
        .get(&canonical)
        .expect("the leftovers under the canonical root");
    assert_eq!(left_out.get("other type"), Some(&1));
    assert_eq!(
        report
            .candidates
            .iter()
            .filter(|f| f.kind == FileKind::Image)
            .count(),
        300,
        "the root's 300 photos are indexed, not dataset-skipped"
    );
    for file in &report.candidates {
        assert!(
            file.path.starts_with(&canonical) && !file.path.contains("/./"),
            "candidate rides the canonical root: {}",
            file.path
        );
    }
}

fn crawl(root: &Path, options: &IndexOptions) -> digup_core::crawler::CrawlReport {
    crawler::crawl(&[root.to_path_buf()], options, &mut |_: &CrawlProgress| {})
}

#[test]
fn the_crawl_applies_the_rules_and_reports_what_it_leaves_out() {
    let root = temp_tree("digup-crawler-walk");
    touch(&root, "notes.txt", b"hello");
    touch(&root, "photo.jpg", b"\xff\xd8\xff\xe0");
    touch(&root, "Screenshot 2026-01-01 at 10.10.23.png", b"x");
    touch(&root, "Screenshots/pic.png", b"x");
    touch(&root, "thing.xyz", b"x");
    touch(&root, "node_modules/keep.txt", b"x");
    touch(&root, ".hidden/secret.txt", b"x");
    touch(&root, "Stuff.noindex/a.txt", b"x");
    touch(&root, "Photos.photoslibrary/Masters/a.jpg", b"x");
    touch(&root, "App.app/binary", b"x");
    touch(&root, "proj/package.json", b"{}");
    touch(&root, "proj/code.png", b"x");
    touch(&root, "proj/Screenshot one.png", b"x");
    touch(&root, "proj/sub/deep.png", b"x");
    // A bundle NAME spelled with capitals sitting in the scanned root as a plain file
    // (`Foo.XcodeProj` is Xcode's own spelling on a zip-restored tree): `isProjectRoot`
    // compares the RAW `pathExtension` against the lowercase markers (Rules.swift:179),
    // so the engine does NOT mark the root and indexes `asset.png`. The audit-base
    // lowercased — the root became a project root and `asset.png` landed "inside a code
    // project" where the engine keeps it (F8). (A FOLDER of that spelling is shadowed by
    // the package rule — `isPackage` lowercases in Swift too, Files+Portable.swift:63.)
    touch(&root, "xcode/Foo.XcodeProj", b"x");
    touch(&root, "xcode/asset.png", b"x");
    touch(
        &root,
        "cache/CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    touch(&root, "cache/blob.jpg", b"x");
    symlink("photo.jpg", &root.join("link.jpg"));
    symlink("nowhere.jpg", &root.join("broken.jpg"));
    symlink("proj", &root.join("linkdir"));
    for i in 0..300 {
        touch(&root, &format!("clips/clip_{i:06}.jpg"), b"x");
    }

    let report = crawl(&root, &IndexOptions::default());

    let found = names(&report.candidates);
    for name in [
        "notes.txt",
        "photo.jpg",
        "Screenshot 2026-01-01 at 10.10.23.png",
        "pic.png",
        "Screenshot one.png",
        // F8: `Foo.XcodeProj` is not a project root (the raw-extension compare), so its
        // asset is indexed — the audit-base lowercased and left it out.
        "asset.png",
    ] {
        assert!(found.contains(name), "{name} should be indexed: {found:?}");
    }
    for name in [
        "keep.txt",   // developer folder
        "secret.txt", // hidden
        "a.txt",      // .noindex
        "a.jpg",      // Photos library
        "binary",     // app or bundle
        "code.png",   // inside a code project
        "deep.png",   // inside a code project, deeper
        "blob.jpg",   // cache folder
        "link.jpg",   // not a regular file
        "broken.jpg",
        "clip_000000.jpg", // dataset
    ] {
        assert!(!found.contains(name), "{name} should be left out");
    }
    // Inside a code project, screenshots do count.
    assert!(found.contains("Screenshot one.png"));

    // The kinds the crawl settled on.
    let kinds: HashMap<String, FileKind> = report
        .candidates
        .iter()
        .map(|file| {
            (
                file.path.rsplit('/').next().unwrap_or("").to_string(),
                file.kind,
            )
        })
        .collect();
    assert_eq!(kinds.get("pic.png"), Some(&FileKind::Screenshot));
    assert_eq!(
        kinds.get("Screenshot one.png"),
        Some(&FileKind::Screenshot),
        "a screenshot keeps counting inside a code project"
    );
    assert_eq!(kinds.get("photo.jpg"), Some(&FileKind::Image));
    assert_eq!(kinds.get("notes.txt"), Some(&FileKind::Doc));

    // The reason counts, exactly as `skippedLine` will print them.
    // `thing.xyz` classifies `.ignore`; the FOLDER symlink `linkdir` joins it, because
    // Foundation reads URL resource values with `lstat` and a symlink is NOT a directory —
    // measured on swift-corelibs (`crawlprobe.swift`, 2026-11-14, `digup-builder:fixes3`)
    // AND Apple's Foundation on a Mac (`probe3.swift`, 2026-11-15: a symlink-to-a-folder
    // answers `isDirectory = false`). So the link takes the file branch on Linux and macOS
    // alike, its bare name classifies `.ignore`, and the engine counts it in "other type".
    // `Foo.XcodeProj` classifies `.ignore` too (classify lowercases, Rules.swift:78). Count 3
    // on every platform; the audit-base followed the link (`std::fs::metadata`), sent linkdir
    // to the folder branch, and reported NOTHING for it.
    assert_eq!(
        count(&report, "skipped", "other type"),
        3,
        "thing.xyz + linkdir + Foo.XcodeProj"
    );
    assert_eq!(count(&report, "skipped", "link or special file"), 2);
    assert_eq!(count(&report, "skipped", "app or bundle"), 1);
    assert_eq!(count(&report, "skipped", "Photos library"), 1);
    assert_eq!(count(&report, "skipped", "inside a code project"), 2);
    assert_eq!(count(&report, "skipped", "dataset"), 300);
    assert_eq!(count(&report, "folders", "developer folder"), 1);
    assert_eq!(count(&report, "folders", ".noindex folder"), 1);
    assert_eq!(count(&report, "folders", "cache folder"), 1);
    // The hidden folder and the symlinked folder are never listed as skipped folders
    // (the link is not a folder to the visit on the portable platforms — F1; on the
    // engine's macOS/Windows it is a folder the enumerator lists without descending,
    // and no skip reason applies there either: the count is 3 on every platform).
    assert_eq!(count(&report, "folders", "cache folder"), 1);
    assert_eq!(report.skipped_folders.len(), 3);

    // The dataset folder is reported with its file count, and `leftOut` splits the same
    // counts under the root they were found in.
    assert_eq!(
        report
            .datasets
            .get(&root.join("clips").to_string_lossy().into_owned()),
        Some(&300)
    );
    let left_out = report
        .left_out
        .get(&root.to_string_lossy().into_owned())
        .expect("the root's own leftovers");
    assert_eq!(left_out.get("dataset"), Some(&300));
    assert_eq!(left_out.get("folders developer folder"), Some(&1));
    assert_eq!(left_out.get("inside a code project"), Some(&2));

    // `--include-datasets` indexes the dataset folder.
    let with_data = IndexOptions {
        include_datasets: true,
        ..IndexOptions::default()
    };
    let with = crawl(&root, &with_data);
    assert_eq!(with.datasets.len(), 0);
    assert!(names(&with.candidates).contains("clip_000000.jpg"));

    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #1 — the dataset drop is scoped to the kind that triggered it. Three hundred
/// machine-named wallpapers make the folder a training pile FOR IMAGES; the song and the
/// party clip parked beside it are the user's, not the pile's, and their rows live. The
/// old filter keyed on the folder and swept every media kind: 302 files left out where 300
/// are the dataset. (Under `--include-datasets` nothing is dropped, as ever.)
#[test]
fn dataset_filtering_stops_at_the_media_kind_that_triggered_it() {
    let root = temp_tree("digup-crawler-dataset-kinds");
    for i in 1..=300 {
        touch(&root, &format!("collection/wall{i:03}.jpg"), b"x");
    }
    touch(&root, "collection/song.m4a", b"x");
    touch(&root, "collection/party.mp4", b"x");

    let report = crawl(&root, &IndexOptions::default());
    let collection = root.join("collection").to_string_lossy().into_owned();
    assert_eq!(report.datasets.get(&collection), Some(&300));
    assert_eq!(count(&report, "skipped", "dataset"), 300);
    let kept = names(&report.candidates);
    assert!(
        kept.contains("song.m4a") && kept.contains("party.mp4"),
        "the stragglers of the kinds that did NOT trigger the folder keep their rows: {kept:?}"
    );
    assert!(!kept.contains("wall001.jpg") && !kept.contains("wall300.jpg"));

    let with_data = IndexOptions {
        include_datasets: true,
        ..IndexOptions::default()
    };
    let with = crawl(&root, &with_data);
    assert!(with.datasets.is_empty());
    assert!(
        with.candidates.len() == 302,
        "300 + 2, all indexed: {}",
        with.candidates.len()
    );

    // Issue #1b, engine side: the app now sends the nested pick too (`folders()` keeps
    // it). A folder the person chose is never a dataset — the engine's own exemption —
    // and the overlapping walks hand in every file EXACTLY once.
    let collection = root.join("collection");
    let both = crawler::crawl(
        &[root.clone(), collection],
        &IndexOptions::default(),
        &mut |_| {},
    );
    assert!(
        both.datasets.is_empty(),
        "the chosen collection is not a dataset"
    );
    assert_eq!(count(&both, "skipped", "dataset"), 0);
    assert_eq!(
        both.candidates.len(),
        302,
        "all 302, each exactly once: {}",
        names(&both.candidates).len()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_chosen_folder_is_walked_whatever_markers_it_holds() {
    let root = temp_tree("digup-crawler-chosen");
    touch(&root, "node_modules/keep.txt", b"x");
    touch(&root, "proj/keep.jpg", b"x");
    touch(&root, "proj/package.json", b"{}");

    // A root that is itself a skipped folder's name is the user's choice: walked whole, and
    // the code-project rule does not apply to the root itself (its files are not "inside").
    let node = root.join("node_modules");
    let report = crawl(&node, &IndexOptions::default());
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.skipped_folders.len(), 0);

    // Inside a chosen project folder, a plain picture is inside a code project.
    let report = crawl(&root, &IndexOptions::default());
    assert_eq!(count(&report, "skipped", "inside a code project"), 1);
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #3 — overlapping roots: the parent AND the project itself chosen. The project rule
/// applies to project folders found INSIDE a chosen root, not to the chosen root itself
/// ("a folder chosen on purpose is read whole, whatever markers it holds", Crawler.swift).
/// Selecting the project ALONE indexes its pictures; selecting parent+project must index
/// them too, in either root order; selecting only the parent keeps excluding them (the pin
/// above). Every report string — attribution included — rides the deepest chosen root:
/// the more specific instruction.
#[test]
fn an_explicitly_selected_project_root_is_read_whole_inside_another_root() {
    let root = temp_tree("digup-crawler-project-roots");
    touch(&root, "notes.txt", b"x");
    touch(&root, "other/pic3.jpg", b"x");
    touch(&root, "proj/package.json", b"{}");
    touch(&root, "proj/pic.jpg", b"x");
    touch(&root, "proj/sub/pic2.jpg", b"x");
    let proj = root.join("proj");

    // Parent only: the project's two pictures are inside a code project (the old pin).
    let report = crawler::crawl(
        std::slice::from_ref(&root),
        &IndexOptions::default(),
        &mut |_| {},
    );
    assert_eq!(count(&report, "skipped", "inside a code project"), 2);
    assert_eq!(
        names(&report.candidates),
        ["notes.txt".to_string(), "pic3.jpg".to_string()]
            .into_iter()
            .collect::<BTreeSet<String>>()
    );

    // Project alone: its pictures index, nothing is inside it (the old pin, replayed).
    let report = crawler::crawl(
        std::slice::from_ref(&proj),
        &IndexOptions::default(),
        &mut |_| {},
    );
    assert_eq!(count(&report, "skipped", "inside a code project"), 0);
    assert_eq!(
        names(&report.candidates),
        ["pic.jpg".to_string(), "pic2.jpg".to_string()]
            .into_iter()
            .collect::<BTreeSet<String>>()
    );

    // Both roots, in BOTH orders: the pictures index; the project marker's own skip rides
    // the project root (the deepest chosen root containing it).
    for order in [[root.clone(), proj.clone()], [proj.clone(), root.clone()]] {
        let report = crawler::crawl(&order, &IndexOptions::default(), &mut |_| {});
        assert_eq!(
            count(&report, "skipped", "inside a code project"),
            0,
            "the chosen project is read whole in root order {order:?}"
        );
        assert_eq!(
            names(&report.candidates),
            [
                "notes.txt".to_string(),
                "pic.jpg".to_string(),
                "pic2.jpg".to_string(),
                "pic3.jpg".to_string()
            ]
            .into_iter()
            .collect::<BTreeSet<String>>(),
            "no file is indexed twice and none is lost, order {order:?}"
        );
        let under_proj = report
            .left_out
            .get(&proj.to_string_lossy().into_owned())
            .unwrap_or_else(|| panic!("the project root reports its own leftovers, {order:?}"));
        assert_eq!(
            under_proj.get("code or data file"),
            Some(&1),
            "package.json is attributed to the project root, order {order:?}"
        );
        assert!(
            !report
                .left_out
                .contains_key(&root.to_string_lossy().into_owned()),
            "the parent has nothing left out in this tree, order {order:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_cache_directory_root_is_skipped_whole() {
    let root = temp_tree("digup-crawler-cache-root");
    touch(
        &root,
        "CACHEDIR.TAG",
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    );
    touch(&root, "blob.jpg", b"x");
    let report = crawl(&root, &IndexOptions::default());
    assert!(report.candidates.is_empty());
    assert_eq!(count(&report, "folders", "cache folder"), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn excluded_folders_apply_inside_a_root() {
    let root = temp_tree("digup-crawler-excluded");
    touch(&root, "a/keep.txt", b"x");
    touch(&root, "b/drop.txt", b"x");
    let options = IndexOptions {
        excluded_folders: vec![root.join("b").to_string_lossy().into_owned()],
        ..IndexOptions::default()
    };
    let report = crawl(&root, &options);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(count(&report, "folders", "excluded folder"), 1);

    // A folder chosen *inside* an excluded one is walked whole (`FolderSelection`).
    let report = crawl(&root.join("b"), &options);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.skipped_folders.len(), 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// `git init` + a commit, with a fixed identity so the test never needs a configured one.
fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.email=digup@test",
            "-c",
            "user.name=digup",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    assert!(
        matches!(status, Ok(status) if status.success()),
        "git {args:?} failed"
    );
}

#[test]
fn the_code_index_walks_only_what_git_keeps() {
    if Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| !status.success())
        .unwrap_or(true)
    {
        eprintln!("skipped: git is not installed");
        return;
    }
    let root = temp_tree("digup-crawler-code");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    touch(&repo, "src/main.rs", b"fn main() {}");
    touch(&repo, "README.md", b"# hi");
    touch(&repo, "note.ipynb", b"{}");
    touch(&repo, "untracked.rs", b"fn extra() {}");
    touch(&repo, "ignored.rs", b"fn gone() {}");
    touch(&repo, "notes.txt", b"text");
    touch(&repo, "data.json", b"{}");
    touch(&repo, "id_rsa", b"private");
    touch(&repo, "min.js", b"x");
    touch(&repo, "app.min.js", b"x");
    touch(&repo, "vendor/lib.rs", b"fn vendored() {}");
    touch(&repo, "huge.rs", &vec![b'a'; 2 << 20]);
    std::fs::write(repo.join(".gitignore"), b"ignored.rs\n").unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "fixture"]);

    let options = IndexOptions {
        code: true,
        ..IndexOptions::default()
    };
    let report = crawl(&repo, &options);
    let found = names(&report.candidates);
    for name in [
        "main.rs",
        "README.md",
        "note.ipynb",
        "untracked.rs",
        "notes.txt",
    ] {
        assert!(
            found.contains(name),
            "{name} is code search's to read: {found:?}"
        );
    }
    // `min.js` is plain JavaScript to `classifyCode`: its check is on names *ending* ".min.js".
    assert!(
        found.contains("min.js"),
        "plain JS is code search's to read"
    );
    for name in [
        "ignored.rs",
        "data.json",
        "id_rsa",
        "app.min.js",
        "lib.rs",
        "huge.rs",
    ] {
        assert!(
            !found.contains(name),
            "{name} should be left out: {found:?}"
        );
    }
    assert_eq!(count(&report, "skipped", "ignored by git"), 1);
    assert_eq!(count(&report, "skipped", "data file"), 1);
    assert_eq!(count(&report, "skipped", "key or password file"), 1);
    assert_eq!(count(&report, "skipped", "minified"), 1);
    assert_eq!(
        count(&report, "skipped", "too big to be written by hand"),
        1
    );
    assert_eq!(count(&report, "folders", "vendored or built code"), 1);

    // A worktree's (or a submodule's) folder keeps itself out of the walk: its ".git" is a
    // file, not a folder (`Rules.codeSkipReason`).
    touch(&repo, "wt/.git", b"gitdir: ../.git/worktrees/wt\n");
    touch(&repo, "wt/extra.rs", b"fn side() {}");
    let report = crawl(&repo, &options);
    assert_eq!(count(&report, "folders", "submodule or worktree"), 1);
    // The code index never reports datasets, and never calls pictures in.
    assert!(report.datasets.is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn code_outside_a_repo_counts_and_progress_counts_entries() {
    let root = temp_tree("digup-crawler-nogit");
    touch(&root, "src/a.rs", b"fn a() {}");
    touch(&root, "src/b.rs", b"fn b() {}");
    touch(&root, "data.json", b"{}");
    let options = IndexOptions {
        code: true,
        ..IndexOptions::default()
    };
    let mut seen: Vec<CrawlProgress> = Vec::new();
    let report = crawler::crawl(
        std::slice::from_ref(&root),
        &options,
        &mut |progress: &CrawlProgress| seen.push(progress.clone()),
    );
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(count(&report, "skipped", "data file"), 1);
    // The first line names the root, the last one closes it (root: None), and the running
    // count only ever grows.
    assert_eq!(
        seen.first().and_then(|line| line.root.as_deref()),
        Some(root.to_str().unwrap())
    );
    assert_eq!(seen.last().and_then(|line| line.root.as_ref()), None);
    assert!(seen
        .windows(2)
        .all(|pair| pair[0].checked <= pair[1].checked));
    let _ = std::fs::remove_dir_all(&root);
}

/// Issue #2 — the walk's failures are REPORTED, not swallowed. Swift's enumerator carried
/// `errorHandler: { _, _ in true }` and told the sync nothing, so a subfolder that was
/// temporarily unreadable (permissions, a remount blip) looked exactly like an empty folder —
/// and the sync took "not seen" for "deleted" and dropped rows whose files were on disk. The
/// crawl now records every path the walk failed on (`scan_failures`); the sync keeps rows
/// under them. The files inside the sealed folder stay unfound — there is nothing to list.
#[cfg(unix)]
#[test]
fn an_unreadable_folder_is_reported_as_a_scan_failure() {
    use std::os::unix::fs::PermissionsExt;
    let seal = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .expect("change the mode");
    };
    let root = temp_tree("digup-crawler-scanfail");
    touch(&root, "read/keep.txt", b"x");
    touch(&root, "sealed/photo.jpg", b"x");
    let sealed = root.join("sealed");
    seal(&sealed, 0o000);
    let report = crawler::crawl(
        std::slice::from_ref(&root),
        &IndexOptions::default(),
        &mut |_| {},
    );
    let sealed = sealed.to_string_lossy().into_owned();
    assert!(
        report.scan_failures.contains(&sealed),
        "the folder the walk could not read is reported: {:?}",
        report.scan_failures
    );
    assert!(names(&report.candidates).contains("keep.txt"));
    assert!(!names(&report.candidates).contains("photo.jpg"));
    seal(&PathBuf::from(&sealed), 0o755);

    // A root that cannot be listed at all: the depth-0 failure names the ROOT, and the
    // crawl returns no candidates — the sync must keep every row of that root.
    seal(&root, 0o000);
    let report = crawler::crawl(
        std::slice::from_ref(&root),
        &IndexOptions::default(),
        &mut |_| {},
    );
    assert!(report.candidates.is_empty());
    assert!(report
        .scan_failures
        .contains(&root.to_string_lossy().into_owned()));
    seal(&root, 0o755);
    let _ = std::fs::remove_dir_all(&root);
}
