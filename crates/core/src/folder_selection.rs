//! `FolderSelection` (FolderSelection.swift): which folders DigUp searches — the
//! chosen folders, minus the folders skipped inside them. A skip applies inside
//! the chosen folders that contain it, and a chosen folder is searched whole
//! even inside a skipped one. This module carries the rule the engine's filters
//! run (`is_searched`, `reaches`, `is_inside`); the folder-list mechanics
//! (`mark`, `setFolder`, `folders`, `effectiveSkips`, `searchedAs`) land with
//! the shell port, which is what shows the checkboxes.

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
    let cut = if from.ends_with('/') {
        from.len()
    } else {
        from.len() + 1
    };
    !path[cut..].split('/').any(|part| {
        part.starts_with('.') || part.ends_with(".noindex") || SKIPPED_FOLDER_NAMES.contains(&part)
    })
}

/// `path` is inside `folder` (`isInside(_:_:)`, :104).
pub fn is_inside(path: &str, folder: &str) -> bool {
    let bound = if folder.ends_with('/') {
        folder.to_string()
    } else {
        format!("{folder}/")
    };
    path.starts_with(&bound)
}
