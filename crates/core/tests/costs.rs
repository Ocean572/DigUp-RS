//! The `costs.json` writer (`MeasuredCosts.save`, MeasuredCosts.swift:135-139).
//!
//! The wire contract (field names, sorted keys, pretty layout) is pinned by the ⑩
//! cross-check diffs; what this file pins is the WAY the file lands: Swift writes with
//! Foundation's `writeReplacing` — `Data.write(options: .atomic)`, write beside the
//! destination, rename onto it — and a plain `fs::write` is not the same operation.

use digup_core::costs::{MeasuredCosts, Speed};

/// The writer is the index helper, and the watchdog ends that process with `_exit(75)`
/// when memory runs out — a mid-write death under a truncating write leaves a
/// half-written `costs.json`, and `load` answers "nothing measured yet" for a file it
/// cannot parse: the run silently loses every tally it earned. The atomic shape cannot
/// leave that state — the destination is only ever a complete older file or a complete
/// newer one.
///
/// The measurable half on a running system is the rename's permission rule: the rename
/// is governed by the DIRECTORY, the truncate by the FILE. `deploy/` bind-mounts the
/// index folder from the host, so a `costs.json` the container's uid cannot truncate, it
/// can replace. The pin: a read-only costs.json (444) is replaced by the next save, and
/// no scrap file is left beside it. The truncating writer fails this call outright.
#[cfg(unix)]
#[test]
fn the_costs_save_replaces_the_file_it_cannot_write_into() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("a temp folder");
    let folder = tmp.path();

    let first = Speed {
        embedding: 1.4,
        ocr: 2.0,
        ..Default::default()
    };
    MeasuredCosts::set_speed(first.clone(), folder, false);
    let saved = MeasuredCosts::load(folder);
    assert_eq!(saved.speed.as_ref(), Some(&first), "the first save lands");

    std::fs::set_permissions(
        MeasuredCosts::file(folder),
        std::fs::Permissions::from_mode(0o444),
    )
    .expect("the costs file becomes unwritable-in-place");

    let second = Speed {
        embedding: 0.7,
        ocr: 1.1,
        device: Some("gpu".to_string()),
        ..Default::default()
    };
    MeasuredCosts::set_speed(second.clone(), folder, false);
    let saved = MeasuredCosts::load(folder);
    assert_eq!(
        saved.speed.as_ref(),
        Some(&second),
        "the save REPLACES the file (rename rides the folder's permissions); the \
         truncating writer opens the destination for writing and dies on its mode"
    );

    let scraps: Vec<String> = std::fs::read_dir(folder)
        .expect("the folder reads")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("costs.json."))
        .collect();
    assert!(
        scraps.is_empty(),
        "the temp file is renamed onto the destination, not left beside it: {scraps:?}"
    );

    // Restore writability for the fixture's end (tempdir cleanup wants the tree writable).
    std::fs::set_permissions(
        MeasuredCosts::file(folder),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("the costs file becomes writable again");
}

/// `MeasuredCosts.merge` (the main index and the code index each add their own kinds)
/// accumulates onto what's saved, in the same file, with the same atomic save.
#[test]
fn merge_adds_onto_the_saved_tallies() {
    let tmp = tempfile::tempdir().expect("a temp folder");
    let folder = tmp.path();

    let mut first = MeasuredCosts::default();
    first.add("image", 10.0, 5.0);
    first.merge(folder);

    let mut second = MeasuredCosts::default();
    second.add("image", 6.0, 3.0);
    second.add("pdfPage", 30.0, 9.0);
    second.merge(folder);

    let saved = MeasuredCosts::load(folder);
    assert_eq!(
        saved.tallies.get("image").map(|t| (t.units, t.seconds)),
        Some((16.0, 8.0)),
        "the second merge adds onto the first"
    );
    assert_eq!(
        saved.tallies.get("pdfPage").map(|t| (t.units, t.seconds)),
        Some((30.0, 9.0)),
        "a kind the first run never saw arrives whole"
    );
}
