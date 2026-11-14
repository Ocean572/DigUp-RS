//! Path operations as Swift's `NSString`/`URL` do them (`FolderSelection`, `Crawler`): the
//! crawl compares and reports paths, so the pieces (extension, stem, parent) and the
//! lexical standardization must behave exactly as Foundation does — `Path` alone does not
//! (`Path::parent` of "a/b" is "a", `deletingLastPathComponent` of "/a/b" is "/a", and the
//! crawl stops at "/" with `parent.count > 1`).

use std::path::{Path, PathBuf};

/// `URL.standardizedFileURL.path`: lexical cleanup of ".", ".." and duplicate slashes.
/// A ".." at the root is dropped (Foundation: `/..` standardizes to `/`).
pub fn standardize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{}", joined)
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// `NSString.deletingLastPathComponent`: "/a/b" → "/a", "/a" → "/", "/" → "/".
/// The crawl's recursion guards lean on this: it stops when the parent is "/" (count 1).
pub fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(index) => trimmed[..index].to_string(),
        // "a" → ""; "/" → "/" (Foundation keeps the root a root: `parent.count > 1` is the
        // crawl's recursion guard).
        None if path.starts_with('/') => "/".to_string(),
        None => String::new(),
    }
}

/// `FolderSelection.isInside`: is `path` a path under `folder`?
pub fn is_inside(path: &str, folder: &str) -> bool {
    path.starts_with(&format!("{}/", folder.trim_end_matches('/')))
}

/// `FolderSelection.realPath` (FolderSelection.swift:116-124): the realpath of a path, the
/// path itself when it can't be resolved (the walk reports real paths: /private/var where the
/// root says /var). The engine's two branches are the two system calls, and the port mirrors
/// the split: `realpath(3)` exists only in the libc crate's unix module (measured on this box
/// 2026-11-13: `libc::realpath` on `x86_64-pc-windows-msvc` → `error[E0425]: cannot find
/// function realpath in crate libc`, which is the whole `crates/core` Windows build dying at
/// this line, and `.github/workflows/rust-release.yml:45` gates the Gates matrix on
/// `windows-latest`), so Windows gets Foundation's branch —
/// `URL(fileURLWithPath:).resolvingSymlinksInPath()`, whose Rust shape is
/// `std::fs::canonicalize`. `canonicalize` answers a verbatim path (`\\?\C:\Users…`), which
/// Foundation's URL does not, so the prefix is dropped; the comparison these values feed
/// (`model.rs::path_ranges`, `crawler.rs`'s scope) is bytewise, and a `\\?\` spelling would
/// never match the root the app stored.
pub fn real_path(path: &str) -> String {
    #[cfg(unix)]
    {
        let Ok(c_path) = std::ffi::CString::new(path) else {
            return path.to_string();
        };
        unsafe {
            let resolved = libc::realpath(c_path.as_ptr(), std::ptr::null_mut());
            if resolved.is_null() {
                return path.to_string();
            }
            let text =
                String::from_utf8_lossy(std::ffi::CStr::from_ptr(resolved).to_bytes()).into_owned();
            libc::free(resolved as *mut libc::c_void);
            text
        }
    }
    #[cfg(not(unix))]
    {
        let resolved = std::fs::canonicalize(Path::new(path)).ok().map(|real| {
            let text = real.to_string_lossy().into_owned();
            match text.strip_prefix(r"\\?\") {
                Some(stripped) => stripped.to_string(),
                None => text,
            }
        });
        resolved.unwrap_or_else(|| path.to_string())
    }
}

/// `NSString.pathExtension` (empty for names without one, like ".hidden").
pub fn extension(name: &str) -> &str {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
}

/// `NSString.deletingPathExtension`: the name without its extension.
pub fn stem(name: &str) -> &str {
    Path::new(name)
        .file_stem()
        .and_then(|inner| inner.to_str())
        .unwrap_or(name)
}

/// `NSString.lastPathComponent`.
pub fn last_component(name: &str) -> &str {
    Path::new(name)
        .file_name()
        .and_then(|inner| inner.to_str())
        .unwrap_or(name)
}

/// `expandingTildeInPath`: a leading `~` or `~/` is the home folder.
pub fn expand_tilde(path: &str) -> PathBuf {
    let home = crate::paths::home();
    if path == "~" {
        return home;
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(path)
}

pub fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/root"))
}

#[cfg(test)]
mod tests {
    use super::real_path;

    /// The engine's fallback (`guard let real = realpath(path, nil) else { return path }`,
    /// FolderSelection.swift:120): a path the system cannot resolve keeps its own spelling,
    /// so the crawl's scope and the index's path ranges stay comparable.
    #[test]
    fn a_path_that_cannot_resolve_is_answered_as_given() {
        assert_eq!(
            real_path("/definitely-not-here/x"),
            "/definitely-not-here/x"
        );
    }

    /// The resolved spelling, not the one typed: the crawl reports `/private/var/folders/…`
    /// where the root says `/var/folders/…`, and `path_ranges` needs both spellings
    /// (IndexStore.swift:100-103). Compared against the target's own realpath so the
    /// platform's own `/tmp` → `/private/tmp` style rule is on both sides.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_answered_by_its_target() {
        let tmp = tempfile::tempdir().expect("a temp folder");
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).expect("the target folder");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("the symlink");
        assert_eq!(
            real_path(&link.to_string_lossy()),
            real_path(&target.to_string_lossy())
        );
    }
}
