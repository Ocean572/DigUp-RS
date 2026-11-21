//! `FolderSelection` (FolderSelection.swift): which folders DigUp searches — the
//! chosen folders, minus the folders skipped inside them. A skip applies inside
//! the chosen folders that contain it, and a chosen folder is searched whole
//! even inside a skipped one. This module carries the rule the engine's filters
//! run (`is_searched`, `reaches`, `is_inside`); the folder-list mechanics
//! (`mark`, `setFolder`, `folders`, `effectiveSkips`, `searchedAs`) land with
//! the shell port, which is what shows the checkboxes.

use crate::paths::{bound, is_inside};
use crate::rules::SKIPPED_FOLDER_NAMES;

/// Whether `path` (a folder or a file) is searched by `folders` minus `skipped`
/// (`isSearched(_:folders:skipped:)`, FolderSelection.swift:34).
pub fn is_searched(path: &str, folders: &[String], skipped: &[String]) -> bool {
    folders.iter().any(|folder| {
        (path == folder.as_str() || reaches(path, folder))
            && !skipped.iter().any(|skip| {
                is_inside(skip, folder) && (path == skip.as_str() || is_inside(path, skip))
            })
    })
}

/// Whether a walk of `folder` gets to `path`: inside it, with no hidden,
/// `.noindex` or developer folder on the way (`reaches(_:from:)`, :43). The
/// crawl never enters those from above (`Rules::is_walked`); chosen on their own
/// they're read whole.
pub fn reaches(path: &str, from: &str) -> bool {
    if !is_inside(path, from) {
        return false;
    }
    // The cut is the bound's length: `paths::bound` is the engine's one spelling of the
    // trailing-slash rule (FolderSelection.swift:107), and `is_inside` above already proved
    // `path` starts with it — so the remaining components are exactly `path[cut..]`.
    let cut = bound(from).len();
    !path[cut..].split('/').any(|part| {
        part.starts_with('.') || part.ends_with(".noindex") || SKIPPED_FOLDER_NAMES.contains(&part)
    })
}

/// `FolderSelection.isWindows` (#189): the path rules below take it as a parameter so
/// a test on one OS measures the other OS's rules; the engine's own callers pass this.
pub const IS_WINDOWS: bool = cfg!(windows);

/// When `path` is inside a git worktree (where `.git` is a file pointing at
/// `<main>/.git/worktrees/<name>`), the corresponding path inside the main repository
/// (`worktreeMainPath`, FolderSelection.swift #178/#189). The code crawler skips worktrees
/// because they duplicate the main repository (`Rules.codeSkipReason`, rules.rs:343), so
/// scoping a search to a worktree (`--here`) must match the main repository's indexed
/// paths too. The walk up the tree mirrors Swift's: the `.git` FILE (a directory `.git`
/// is an ordinary repo — nil), its first line's `gitdir:`, the split at
/// `.git/worktrees/`, and `path`'s suffix under the worktree appended to the main repo.
pub fn worktree_main_path(path: &str) -> Option<String> {
    let mut current = path.to_string();
    while current.chars().count() > 1 {
        let dot_git = format!("{}{}", bound(&current), ".git"); // worktree/.git, not worktree.git
        match std::fs::metadata(&dot_git) {
            Ok(meta) if meta.is_file() => {
                let contents = std::fs::read_to_string(&dot_git).ok()?;
                let first_line = contents.split(['\r', '\n']).next().unwrap_or("");
                let raw = first_line.strip_prefix("gitdir:")?.trim();
                let git_dir = git_dir_path(raw, &current);
                let main_repo = git_dir.split("/.git/worktrees/").next()?;
                if main_repo == git_dir {
                    return None; // no `.git/worktrees/` in it
                }
                let suffix: String = path.chars().skip(current.chars().count()).collect();
                return Some(format!("{main_repo}{suffix}"));
            }
            Ok(_) => return None, // a directory `.git`: an ordinary repository, not a worktree
            Err(_) => {}          // nothing there: walk up
        }
        let parent = parent_path(&current);
        if parent == current {
            break;
        }
        current = parent;
    }
    None
}

/// `NSString.deletingLastPathComponent` in slash form: drop the last component and the
/// separator; `/` and `C:/` are their own parent (the loop above stops on that).
fn parent_path(path: &str) -> String {
    let head = path.trim_end_matches('/');
    match head.rfind('/') {
        Some(cut) => head[..cut].to_string(),
        None => String::new(),
    }
}

/// All canonical path spellings for `paths` (folders or files), including symlink/realPath
/// resolutions and git worktree → main repository mappings (`searchTargets`, #178). Every
/// spelling a scope can arrive as — the given one, the real path, the main repo, its real
/// path — joins the target list, deduped in first-seen order.
pub fn search_targets(paths: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in paths {
        if raw.is_empty() {
            continue;
        }
        let trimmed: String = if raw.chars().count() > 1 && raw.ends_with('/') {
            raw[..raw.rfind('/').unwrap()].to_string()
        } else {
            raw.clone()
        };
        let real = crate::paths::real_path(&trimmed);
        out.push(trimmed.clone());
        out.push(real.clone());
        for candidate in [trimmed, real] {
            if let Some(main) = worktree_main_path(&candidate) {
                out.push(main.clone());
                out.push(crate::paths::real_path(&main));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|path| seen.insert(path.clone()));
    out
}

/// Whether a path as git or a shell writes it is absolute: `/…`, and on Windows also
/// `C:/…`, `C:\…` and `\\server\share\…` (`isAbsolutePath`, #189 — Git for Windows
/// writes a worktree's `gitdir:` as `C:/Users/…/.git/worktrees/<name>`).
pub fn is_absolute_path(path: &str, windows: bool) -> bool {
    if path.starts_with('/') {
        return true;
    }
    if !windows {
        return false;
    }
    if path.starts_with(r"\\") {
        return true;
    }
    let chars: Vec<char> = path.chars().take(3).collect();
    chars.len() == 3
        && chars[0].is_ascii_alphabetic()
        && chars[1] == ':'
        && (chars[2] == '/' || chars[2] == '\\')
}

/// A worktree's `gitdir:` value as a path in the same form the index stores, resolved
/// against the worktree folder when it's relative (`git worktree add --relative-paths`),
/// through the standardizer so `C:\…` and `C:/…` come out the way the crawler's paths
/// do (`gitDirPath`, #189).
pub fn git_dir_path(raw: &str, worktree: &str) -> String {
    if is_absolute_path(raw, IS_WINDOWS) {
        return crate::paths::standardize(raw);
    }
    crate::paths::standardize(&std::path::Path::new(worktree).join(raw).to_string_lossy())
}

/// The path with its drive letter in capitals (`c:/x` → `C:/x`, `/c:/x` → `/C:/x`) on
/// Windows, as the crawler's paths have it, so a scope from a shell that reports `c:\…`
/// still finds the index's rows; unchanged elsewhere (`upperDrive`, #189).
pub fn upper_drive(path: &str, windows: bool) -> String {
    if !windows {
        return path.to_string();
    }
    let chars: Vec<char> = path.chars().take(3).collect();
    let at = usize::from(chars.first() == Some(&'/'));
    if chars.len() <= at + 1 || !chars[at].is_ascii_alphabetic() || chars[at + 1] != ':' {
        return path.to_string();
    }
    let head: String = path.chars().take(at).collect();
    format!("{head}{}{}", chars[at].to_uppercase(), &path[at + 1..])
}

/// Whether two paths name the same file: case-insensitively on Windows, whose file
/// systems ignore case (a shell can report the working folder as `c:\…` while the index
/// holds `C:/…`) (`samePath`, #189).
pub fn same_path(a: &str, b: &str, ignoring_case: bool) -> bool {
    if ignoring_case {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// Whether `path` is equal to or inside any of `targets` (case-insensitively on
/// Windows, as `same_path`) (`matches`, #178/#189).
pub fn matches(path: &str, targets: &[String], ignoring_case: bool) -> bool {
    let path = if ignoring_case {
        path.to_lowercase()
    } else {
        path.to_string()
    };
    targets.iter().any(|target| {
        let target = if ignoring_case {
            target.to_lowercase()
        } else {
            target.clone()
        };
        path == target || is_inside(&path, &target)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Git for Windows writes a worktree's gitdir as C:/Users/…: absolute, not relative
    // to the worktree (#189's windowsDrivePathsAreAbsolute, the flag parameter making
    // it measurable from one OS).
    #[test]
    fn windows_drive_paths_are_absolute() {
        assert!(is_absolute_path("/home/me/repo/.git/worktrees/wt", false));
        assert!(!is_absolute_path(
            "C:/Users/me/repo/.git/worktrees/wt",
            false
        ));
        assert!(is_absolute_path("C:/Users/me/repo/.git/worktrees/wt", true));
        assert!(is_absolute_path(r"d:\src\repo\.git\worktrees\wt", true));
        assert!(is_absolute_path(r"\\server\share\repo", true));
        assert!(!is_absolute_path("../repo/.git/worktrees/wt", true));
        assert!(!is_absolute_path("C:relative", true));
    }

    // Relative to the worktree folder: up out of feature/ and worktrees/, into billing/
    // (#189's relativeGitDirResolvesAgainstTheWorktree; the Linux shape — on Windows
    // Foundation and std::path join agree, `git_dir_path` takes no flag for it).
    #[test]
    fn relative_git_dir_resolves_against_the_worktree() {
        assert_eq!(
            git_dir_path(
                "../../billing/.git/worktrees/feature",
                "/src/worktrees/feature"
            ),
            "/src/billing/.git/worktrees/feature"
        );
        // Absolute stays absolute, standardize's way.
        assert_eq!(
            git_dir_path("/home/me/repo/.git/worktrees/wt", "/src/worktrees/feature"),
            "/home/me/repo/.git/worktrees/wt"
        );
    }

    // Windows file systems ignore case: a shell's c:\… is the index's C:/… (#189's
    // windowsScopesIgnoreCase).
    #[test]
    fn windows_scopes_ignore_case() {
        assert!(matches(
            "C:/Users/me/repo/a.swift",
            &["c:/users/me/repo".to_string()],
            true
        ));
        assert!(!matches(
            "C:/Users/me/repo/a.swift",
            &["c:/users/me/repo".to_string()],
            false
        ));
        assert!(!matches(
            "C:/Users/me/repo2/a.swift",
            &["c:/users/me/repo".to_string()],
            true
        ));
        assert!(same_path("C:/A/b.txt", "c:/a/B.TXT", true));
        assert!(!same_path("/A/b.txt", "/a/b.txt", false));
    }

    #[test]
    fn drive_letters_are_capitalized_on_windows_only() {
        assert_eq!(upper_drive("c:/Users/me", true), "C:/Users/me");
        assert_eq!(upper_drive("/d:/src", true), "/D:/src");
        assert_eq!(upper_drive("C:/Users", true), "C:/Users");
        assert_eq!(upper_drive("c:/Users/me", false), "c:/Users/me");
        assert_eq!(upper_drive("/home/me", true), "/home/me");
    }
}
