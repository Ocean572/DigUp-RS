//! Watching the chosen folders: a change where indexing reads asks the index helper to sync, two seconds after the last
//! one, ten at most (a camera import or an unzip is a burst of events; issue #7b: a stream of changes that never goes
//! quiet — a sync client, a recording — must not starve sync forever); in code search's folders a minute after the code goes quiet
//! (ten at most), as on the Mac, so the code index doesn't follow every save. `notify` uses ReadDirectoryChangesW on Windows, inotify on
//! Linux, FSEvents on a Mac. Changes where the crawl never goes are passed over (the engine's `Rules.isWalked`): hidden
//! folders (a leading dot, or Windows' hidden flag, which AppData has), developer folders, `.noindex` folders, skipped
//! folders, and DigUp's own index. With a home folder chosen, other apps' caches would ask for a sync every few seconds.
//! Issue #7a: a burst hands the helper its FOLDERS — the outermost directories that saw the
//! events — so a changed photo reconciles its collection, not the whole tree; a burst too
//! big to scope, and dropped (overflowed) events, keep the full-folder sync.

use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Folders the crawl never walks into, by name: the engine's `Rules.skippedFolderNames`.
const DEVELOPER_FOLDERS: [&str; 8] = [
    "node_modules",
    "bower_components",
    "venv",
    "site-packages",
    "__pycache__",
    "DerivedData",
    "Pods",
    "Carthage",
];

/// Issue #7a: how many folders a scoped sync is worth given: past that the burst is big
/// enough that the full crawl is the cheaper walk (issue #7a's own scoping — overflow stays
/// full, startup and manual rescans are full, so a missed scope edge is always repaired).
const MAX_SCOPED_DIRS: usize = 200;

enum WatchEvent {
    /// The system dropped events (an overflow): anything may have changed.
    Overflow,
    /// One change inside a folder the crawl walks.
    Path(PathBuf),
}

impl WatchEvent {
    fn path(self) -> Option<PathBuf> {
        match self {
            WatchEvent::Overflow => None,
            WatchEvent::Path(path) => Some(path),
        }
    }
}

pub struct FolderWatcher {
    _watcher: Option<notify::RecommendedWatcher>,
}

impl FolderWatcher {
    /// Watches `folders` (less `skipped` inside them); `changed` runs on the watcher's own thread after each burst of
    /// changes: once nothing has changed for `quiet`, or `longest` after the burst began. Dropping it stops.
    /// Issue #7a: `changed` receives the burst's folders — the outermost directories containing
    /// the events, so the helper reconciles the neighborhood the events touched (one walk of
    /// a folder covers everything inside it: a dir whose parent also changed folds into the
    /// parent) — and `full` says when the whole set is the job: dropped events, a
    /// too-wide burst, or a scope the collapse gave up on.
    pub fn start(
        folders: &[PathBuf],
        skipped: Vec<PathBuf>,
        ignore: Option<PathBuf>,
        quiet: Duration,
        longest: Duration,
        changed: impl Fn(&[PathBuf], bool) + Send + 'static,
    ) -> FolderWatcher {
        let (sender, receiver) = mpsc::channel::<WatchEvent>();
        let roots = folders.to_vec();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let Ok(event) = result else { return };
            if event.kind.is_access() {
                return;
            }
            let ours = |path: &PathBuf| {
                ignore
                    .as_ref()
                    .is_some_and(|folder| path.starts_with(folder))
            };
            // No paths: the system dropped events (an overflow), so anything may have changed.
            if event.paths.is_empty() {
                let _ = sender.send(WatchEvent::Overflow);
                return;
            }
            for path in event.paths {
                if !ours(&path) && walked(&path, &roots, &skipped) {
                    let _ = sender.send(WatchEvent::Path(path));
                }
            }
        });
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("digup-shell: can't watch folders: {error}");
                return FolderWatcher { _watcher: None };
            }
        };
        for folder in folders {
            if let Err(error) = watcher.watch(folder, RecursiveMode::Recursive) {
                eprintln!("digup-shell: can't watch {}: {error}", folder.display());
            }
        }
        std::thread::spawn(move || {
            let mut event = match receiver.recv() {
                Ok(event) => event,
                Err(_) => return,
            };
            loop {
                let began = std::time::Instant::now();
                let mut full = matches!(event, WatchEvent::Overflow);
                let mut paths: Vec<PathBuf> = event.path().into_iter().collect();
                while began.elapsed() < longest {
                    match receiver.recv_timeout(quiet) {
                        Ok(WatchEvent::Overflow) => full = true,
                        Ok(WatchEvent::Path(path)) => paths.push(path),
                        Err(_) => break,
                    }
                }
                let (dirs, full) = if full {
                    (Vec::new(), true)
                } else {
                    collapse_to_dirs(&paths)
                };
                changed(&dirs, full);
                event = match receiver.recv() {
                    Ok(event) => event,
                    Err(_) => return,
                };
            }
        });
        FolderWatcher {
            _watcher: Some(watcher),
        }
    }
}

/// The burst's folders: the directory each event happened in, folded to the outermost ones
/// (issue #7a — one walk of a folder covers everything inside it, so a dir whose parent also
/// changed folds into the parent; distant folders stay separate). More kept dirs than
/// `MAX_SCOPED_DIRS` answers `(empty, true)`: the whole crawl is then the cheaper walk.
fn collapse_to_dirs(paths: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            if !dirs.iter().any(|kept| kept == parent) {
                dirs.push(parent.to_path_buf());
            }
        }
    }
    dirs.sort_by_key(|dir| dir.components().count());
    let mut kept: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if kept.iter().any(|outer| dir.starts_with(outer)) {
            continue;
        }
        kept.push(dir);
        if kept.len() > MAX_SCOPED_DIRS {
            return (Vec::new(), true);
        }
    }
    (kept, false)
}

/// The crawl reads `path`: it's inside a chosen folder, and nothing on the way to it from there is skipped. A skipped
/// folder counts only inside the chosen folders that contain it (one chosen inside it is read whole).
fn walked(path: &Path, roots: &[PathBuf], skipped: &[PathBuf]) -> bool {
    roots.iter().any(|root| {
        let Ok(below) = path.strip_prefix(root) else {
            return false;
        };
        let mut folder = root.clone();
        below.components().all(|part| {
            folder.push(part);
            let name = part.as_os_str().to_string_lossy();
            !(name.starts_with('.')
                || DEVELOPER_FOLDERS.contains(&name.as_ref())
                || name.ends_with(".noindex")
                || skipped.contains(&folder)
                || hidden(&folder))
        })
    })
}

/// Windows' hidden flag; elsewhere a leading dot is the rule, checked above.
#[cfg(windows)]
fn hidden(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0)
}

#[cfg(not(windows))]
fn hidden(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{collapse_to_dirs, walked, MAX_SCOPED_DIRS};
    use std::path::PathBuf;

    #[test]
    fn walks_where_the_crawl_goes() {
        let home = PathBuf::from("/home/me");
        let roots = vec![home.clone(), home.join("Pictures/.private/shared")];
        let skipped = vec![home.join("Music"), home.join("Music/Mixes")];
        let read = |path: &str| walked(&home.join(path), &roots, &skipped);
        assert!(read("Documents/notes.md"));
        assert!(!read(".cache/thumbnails/x.png"));
        assert!(!read("code/app/node_modules/pkg/index.js"));
        assert!(!read("Archive.noindex/old.pdf"));
        assert!(!read("Music/song.mp3"));
        assert!(!read("Music/Mixes/one.mp3"));
        // A folder chosen inside a hidden or skipped one is read whole.
        assert!(read("Pictures/.private/shared/photo.jpg"));
        assert!(!walked(
            &PathBuf::from("/elsewhere/file.txt"),
            &roots,
            &skipped
        ));
    }

    #[test]
    fn a_burst_collapses_to_the_outermost_dirs() {
        let p = |s: &str| PathBuf::from(s);
        // Two sibling folders' files: the dirs that saw events stay SEPARATE scopes —
        // scoping is the neighborhood the events touched, never a wider walk (folding
        // siblings to their shared parent would scope MORE folders than the events name).
        let (dirs, full) = collapse_to_dirs(&[p("/home/me/a/x.png"), p("/home/me/b/y.png")]);
        assert!(!full);
        assert_eq!(dirs, vec![p("/home/me/a"), p("/home/me/b")]);
        // Distant folders stay separate — each is its own scope.
        let (dirs, full) = collapse_to_dirs(&[p("/home/me/a/x.png"), p("/mnt/nas/photos/z.jpg")]);
        assert!(!full);
        assert_eq!(dirs, vec![p("/home/me/a"), p("/mnt/nas/photos")]);
        // A folder and a deeper folder inside it: the outer walk covers the inner.
        let (dirs, full) = collapse_to_dirs(&[p("/a/b/one.txt"), p("/a/b/c/two.txt")]);
        assert!(!full);
        assert_eq!(dirs, vec![p("/a/b")]);
        // A burst spread wider than the scoped sync is worth: the whole crawl (issue #7a:
        // overflow stays full, and the full sync is the repair path by construction).
        let many: Vec<PathBuf> = (0..=MAX_SCOPED_DIRS)
            .map(|i| p(&format!("/root/d{i}/f.txt")))
            .collect();
        let (dirs, full) = collapse_to_dirs(&many);
        assert!(full, "a 201-folder burst scopes to nothing");
        assert!(dirs.is_empty());
    }
}
