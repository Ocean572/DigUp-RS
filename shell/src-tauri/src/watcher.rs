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
    /// too-wide burst, a scope the collapse gave up on, or `scope = false`, which asks for
    /// none of that work at all (the code watcher's sync is the cheap walk: it never scopes,
    /// so it never pays for a fold it discards).
    pub fn start(
        folders: &[PathBuf],
        skipped: Vec<PathBuf>,
        ignore: Option<PathBuf>,
        quiet: Duration,
        longest: Duration,
        scope: bool,
        changed: impl Fn(&[PathBuf], bool) + Send + 'static,
    ) -> FolderWatcher {
        let (sender, receiver) = mpsc::channel::<WatchEvent>();
        let roots = folders.to_vec();
        // The burst's folders are clamped to these same chosen folders (below): the watcher
        // thread needs the list the events were filtered by, and the notify closure takes its
        // own copy of it.
        let scope_roots = roots.clone();
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
                let (dirs, full) = if full || !scope {
                    (Vec::new(), true)
                } else {
                    collapse_to_dirs(&paths, &scope_roots)
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
/// So is a burst whose folder names are not valid UTF-8, and an event the chosen folders do
/// not contain: the plain `sync` verb carries no paths, so giving up on the scope is always
/// answerable, and it is answered by the full crawl — never by a narrower one.
fn collapse_to_dirs(paths: &[PathBuf], roots: &[PathBuf]) -> (Vec<PathBuf>, bool) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for path in paths {
        // The event's own folder, CLAMPED to the chosen folder it lives in: notify names the
        // watched folder itself for several events (`DELETE_SELF` on a deleted root,
        // `MOVE_SELF` on a renamed one, and the nameless ATTRIB/MODIFY spellings — a chmod
        // or touch of a root, a sync client stamping a folder's mtime, a download tool), so
        // `parent()` of such an event is the chosen root's PARENT. Handing the helper a scope
        // above the folders the user chose indexes the neighbour trees (rows its own scope
        // never embeds, so they sit in `files` forever), prunes the chosen root's rows by
        // rules the parent does not see (a code project, a dataset, a root-exempt folder —
        // add them, embed them, prune them, repeat), bypasses the ejected-drive keep (the
        // keep is keyed to the CHOSEN root), and for a root at the filesystem's top crawls
        // the whole disk. The deepest chosen root containing the event is the scope's floor.
        let Some(root) = roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
        else {
            return (Vec::new(), true);
        };
        let dir = match path.parent() {
            // The event's own folder, when that is at or inside the chosen folder the event
            // lives in (component-wise `starts_with`: `/a/bc` is not inside `/a/b`).
            Some(parent) if parent.starts_with(root) => parent.to_path_buf(),
            // The event NAMED the chosen folder itself — its scope is that folder, never its
            // parent.
            _ => root.clone(),
        };
        if !dirs.iter().any(|kept| kept == &dir) {
            dirs.push(dir.clone());
            // The cap belongs HERE, where the dirs are collected: checking it only in the
            // fold's second pass paid an O(n²) dedupe of a whole queue's worth of events
            // (inotify's queue is ~16k) before giving up on the scope.
            if dirs.len() > MAX_SCOPED_DIRS {
                return (Vec::new(), true);
            }
        }
    }
    // The scope rides a JSON array on the helper's stdin, and serde serializes a path by its
    // UTF-8 `str` — a folder named by an SMB/FTP/GBK restore (the common source on Linux)
    // makes the verb fail to build, and this tree would never sync again, every burst
    // answering the same error. A burst that touches one is the whole set's job.
    if dirs.iter().any(|dir| dir.to_str().is_none()) {
        return (Vec::new(), true);
    }
    dirs.sort_by_key(|dir| dir.components().count());
    let mut kept: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if kept.iter().any(|outer| dir.starts_with(outer)) {
            continue;
        }
        kept.push(dir);
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
        let roots = vec![p("/home/me"), p("/mnt/nas")];
        // Two sibling folders' files: the dirs that saw events stay SEPARATE scopes —
        // scoping is the neighborhood the events touched, never a wider walk (folding
        // siblings to their shared parent would scope MORE folders than the events name).
        let (dirs, full) =
            collapse_to_dirs(&[p("/home/me/a/x.png"), p("/home/me/b/y.png")], &roots);
        assert!(!full);
        assert_eq!(dirs, vec![p("/home/me/a"), p("/home/me/b")]);
        // Distant folders stay separate — each is its own scope.
        let (dirs, full) =
            collapse_to_dirs(&[p("/home/me/a/x.png"), p("/mnt/nas/photos/z.jpg")], &roots);
        assert!(!full);
        assert_eq!(dirs, vec![p("/home/me/a"), p("/mnt/nas/photos")]);
        // A folder and a deeper folder inside it: the outer walk covers the inner.
        let (dirs, full) = collapse_to_dirs(&[p("/a/b/one.txt"), p("/a/b/c/two.txt")], &[p("/a")]);
        assert!(!full);
        assert_eq!(dirs, vec![p("/a/b")]);
        // A burst spread wider than the scoped sync is worth: the whole crawl (issue #7a:
        // overflow stays full, and the full sync is the repair path by construction).
        let many: Vec<PathBuf> = (0..=MAX_SCOPED_DIRS)
            .map(|i| p(&format!("/root/d{i}/f.txt")))
            .collect();
        let (dirs, full) = collapse_to_dirs(&many, &[p("/root")]);
        assert!(full, "a 201-folder burst scopes to nothing");
        assert!(dirs.is_empty());
    }

    #[test]
    fn a_scope_never_leaves_the_chosen_folders() {
        let p = |s: &str| PathBuf::from(s);
        // notify names the WATCHED FOLDER ITSELF for several events (`DELETE_SELF`,
        // `MOVE_SELF`, the nameless ATTRIB/MODIFY spellings: a chmod of a root, a sync client
        // stamping its mtime). The review measured the leak: the parent of such an event is
        // the chosen root's PARENT, and a parent scope indexes the neighbour trees, prunes
        // the chosen root's own rows by rules the parent cannot see, bypasses the
        // ejected-drive keep, and for a root at the filesystem's top crawls the whole disk.
        // The clamp: an event naming the chosen folder scopes THAT folder.
        let roots = vec![p("/home/me/Pictures")];
        let (dirs, full) = collapse_to_dirs(&[p("/home/me/Pictures")], &roots);
        assert!(!full);
        assert_eq!(dirs, vec![p("/home/me/Pictures")], "never /home/me");
        // The same for a folder chosen at the filesystem's top (the bench shape): the scope
        // is the chosen folder, never `/`. The inner folder's event folds INTO the root
        // scope (the outermost-fold rule: one walk of `/data` covers `/data/photos`), so the
        // burst keeps the clamp's floor, not the child.
        let (dirs, full) = collapse_to_dirs(&[p("/data"), p("/data/photos/x.jpg")], &[p("/data")]);
        assert!(!full);
        assert_eq!(
            dirs,
            vec![p("/data")],
            "never `/`, and the child folds into it"
        );
        // A child folder ALONE (no event on the root) keeps the child as its scope.
        let (dirs, full) = collapse_to_dirs(&[p("/data/photos/x.jpg")], &[p("/data")]);
        assert!(!full);
        assert_eq!(dirs, vec![p("/data/photos")]);
        // A folder chosen INSIDE another chosen folder: the deepest instruction owns its
        // events (the crawler's own rule, issue #3) — the shared folder's event does not
        // scope the whole home tree.
        let roots = vec![p("/home/me"), p("/home/me/Pictures/.private/shared")];
        let (dirs, full) =
            collapse_to_dirs(&[p("/home/me/Pictures/.private/shared/photo.jpg")], &roots);
        assert!(!full);
        assert_eq!(dirs, vec![p("/home/me/Pictures/.private/shared")]);
        // The component boundary, at the shell edge too: a path that is not inside any chosen
        // folder (`/a/bc` beside the chosen `/a/b`) is not a scope — the burst is the whole
        // set's job, answered by a verb that names no paths at all.
        let (dirs, full) = collapse_to_dirs(&[p("/a/bc/x.jpg")], &[p("/a/b")]);
        assert!(full);
        assert!(dirs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_scope_that_cannot_be_written_is_the_whole_set() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;
        // A folder named by an SMB/FTP/GBK restore (the common source on Linux) is not valid
        // UTF-8, and serde serializes a path by its UTF-8 `str`: the scoped verb cannot be
        // built, and the tree would never sync again — every burst in it answering the same
        // error. The collapse gives up on the scope, and the plain `sync` line needs no paths.
        let mut raw_bytes: Vec<u8> = Vec::new();
        raw_bytes.extend_from_slice(b"/a/");
        raw_bytes.extend_from_slice(&[0xff, 0xfe]);
        raw_bytes.extend_from_slice(b"/x.jpg");
        let raw = OsString::from_vec(raw_bytes);
        let (dirs, full) = collapse_to_dirs(&[PathBuf::from(raw)], &[PathBuf::from("/a")]);
        assert!(full, "a non-UTF-8 scope is the whole set's job");
        assert!(dirs.is_empty());
    }
}
