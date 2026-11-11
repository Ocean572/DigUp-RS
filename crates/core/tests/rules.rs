//! What gets indexed, pinned to the engine's rules (`Rules.swift`, `Files+Portable.swift`):
//! the classification by extension, the machine-name and date detectors, the folder rules,
//! the code-index rules and the path pieces they lean on. The reason strings are part of the
//! contract (the apps print them verbatim), so they are compared as written.

use std::collections::HashSet;

use digup_core::model::FileKind;
use digup_core::options::IndexOptions;
use digup_core::rules::{self, Classification};

fn index(kind: FileKind) -> Classification {
    Classification::Index(kind)
}

fn skip(reason: &str) -> Classification {
    Classification::Skip(reason.to_string())
}

/// Writes a file whose first bytes are `head` and returns its path.
fn write_temp(folder: &std::path::Path, name: &str, head: &[u8], pad: usize) -> String {
    let file = folder.join(name);
    std::fs::write(&file, [head.to_vec(), vec![0u8; pad]].concat()).unwrap();
    file.to_string_lossy().into_owned()
}

/// A real MPEG-TS: 0x47 every 188 bytes.
fn mpeg_ts(folder: &std::path::Path, name: &str) -> String {
    let mut head = vec![0u8; 400];
    for (index, byte) in head.iter_mut().enumerate() {
        if index % 188 == 0 {
            *byte = 0x47;
        }
    }
    write_temp(folder, name, &head, 200)
}

#[test]
fn classify_follows_the_extension_tables() {
    let temp = std::env::temp_dir().join("digup-rules-classify");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let options = IndexOptions::default();
    let ts_video = mpeg_ts(&temp, "clip.ts");
    let plain_ts = write_temp(&temp, "module.ts", b"export const x = 1;\n", 500);

    let cases: Vec<(String, Classification)> = vec![
        ("/x/home/notes.txt".into(), index(FileKind::Doc)),
        ("/x/README.md".into(), index(FileKind::Doc)),
        ("/x/mail.emlx".into(), index(FileKind::Doc)),
        ("/x/book.docx".into(), index(FileKind::Doc)),
        ("/x/sheet.xlsx".into(), index(FileKind::Doc)),
        ("/x/packed.m4p".into(), skip("copy-protected")),
        (plain_ts.clone(), skip("code or data file")),
        (ts_video.clone(), index(FileKind::Video)),
        ("/x/module.py".into(), skip("code or data file")),
        ("/x/data.json".into(), skip("code or data file")),
        ("/x/report.PDF".into(), index(FileKind::Pdf)),
        ("/x/photo.JPG".into(), index(FileKind::Image)),
        ("/x/raw.NEF".into(), index(FileKind::Image)),
        (
            "/x/Screenshot 2026-01-01 at 10.10.23.png".into(),
            index(FileKind::Screenshot),
        ),
        (
            "/x/Screenshots/2026-02-10_10-10.png".into(),
            index(FileKind::Screenshot),
        ),
        (
            "/x/Screenshots/IMG_1234.png".into(),
            index(FileKind::Screenshot),
        ),
        ("/x/MOV_1234.png".into(), index(FileKind::Image)),
        ("/x/clip.mp4".into(), index(FileKind::Video)),
        ("/x/song.flac".into(), index(FileKind::Audio)),
        ("/x/thing.xyz".into(), Classification::Ignore),
        ("/x/no-extension".into(), Classification::Ignore),
    ];
    for (path, expected) in cases {
        assert_eq!(
            rules::classify(&path, false, &options),
            expected,
            "classify({path})"
        );
    }

    // Source files count as documents only when `includeCode` says so.
    let with_code = IndexOptions {
        include_code: true,
        ..IndexOptions::default()
    };
    assert_eq!(
        rules::classify("/x/module.py", false, &with_code),
        index(FileKind::Doc)
    );
    assert_eq!(
        rules::classify(&plain_ts, false, &with_code),
        index(FileKind::Doc)
    );

    // `--exclude-ext` wins over every other rule.
    let excluded = IndexOptions {
        excluded_extensions: IndexOptions::extensions("HEIC, .txt *.md"),
        ..IndexOptions::default()
    };
    assert!(excluded.excluded_extensions.contains("heic"));
    assert_eq!(
        rules::classify("/x/notes.txt", false, &excluded),
        skip("excluded type")
    );
    assert_eq!(
        rules::classify("/x/notes.md", false, &excluded),
        skip("excluded type")
    );
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn classify_code_reads_source_not_data() {
    let temp = std::env::temp_dir().join("digup-rules-classify-code");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let options = IndexOptions::default();
    let ts_video = mpeg_ts(&temp, "clip.ts");

    let cases: Vec<(String, Classification)> = vec![
        ("/repo/src/main.rs".into(), index(FileKind::Code)),
        ("/repo/README.md".into(), index(FileKind::Code)),
        ("/repo/nb.ipynb".into(), index(FileKind::Code)),
        ("/repo/Makefile".into(), index(FileKind::Code)),
        ("/repo/Dockerfile.dev".into(), index(FileKind::Code)),
        ("/repo/app.min.js".into(), skip("minified")),
        ("/repo/LICENSE".into(), skip("license")),
        ("/repo/id_rsa".into(), skip("key or password file")),
        ("/repo/server.key".into(), skip("key or password file")),
        ("/repo/.env".into(), skip("key or password file")),
        ("/repo/data.json".into(), skip("data file")),
        ("/repo/notes.csv".into(), skip("data file")),
        ("/repo/build.log".into(), skip("data file")),
        (ts_video, Classification::Ignore),
        ("/x/IMG_1234.jpg".into(), Classification::Ignore),
    ];
    for (path, expected) in cases {
        assert_eq!(
            rules::classify_code(&path, &options),
            expected,
            "classifyCode({path})"
        );
    }
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn machine_names_are_machine_named() {
    // `^[a-z]{0,16}[-_. ]?\d{3,}([-_.]\d+)*$` and `^[0-9a-f]{8,}([-_][0-9a-f]{4,})*$`.
    for name in [
        "clip_000123",
        "frame-0042",
        "clip-000123_2",
        "3f9a1c0e",
        "3f9a1c0e-4d5f",
        "a1b2c3d4e5f6",
        "00000000",
        "pic.010203",
        "x 90210",
        "shard_100-200-300",
        "2026-13-21", // not a date (month 13): the numbered pattern takes it
        "2026100",    // seven digits: no day fits the date rule
    ] {
        assert!(rules::looks_machine_named(name), "{name} is machine-made");
    }
    for name in [
        // Camera names, whatever the digits look like.
        "IMG_1234",
        "img_000123",
        "DSC_0123",
        "dsc0123456",
        "VID_1234567",
        "vid0001234567",
        "PXL_20260101",
        "GOPR0001",
        "DJI_0001",
        "Photo_1234",
        "whatsapp_000123",
        "Screenshot_20260101",
        // The dates phones write.
        "20261021_143022",
        "2026-10-21 14.30.22",
        "1999_12_31",
        "2026.01.05-12-34",
        "20260101",
        // Too few digits, too few hex characters, nothing to match.
        "clip_12",
        "n1",
        "ab12",
        "note",
        "3f9a1c0",
        "3f9a1c0e-4d",
        "20261031x",
        "",
    ] {
        assert!(
            !rules::looks_machine_named(name),
            "{name} is not machine-made"
        );
    }
}

#[test]
fn phone_dates_are_not_machine_names() {
    // The date rule: `^(19|20)\d{2}[-_.]?(0[1-9]|1[0-2])[-_.]?(0[1-9]|[12]\d|3[01])([-_. (]|$)`,
    // then the machine-name patterns. A name that starts with a date is the user's own.
    for name in [
        "20261021",
        "2026-10-21",
        "2026_10_21",
        "2026.10.21",
        "20261021 (1)",
        "1999-12-31",
    ] {
        assert!(
            !rules::looks_machine_named(name),
            "{name} starts with a date"
        );
    }
    // Month 13 and day 0 are not dates: the numbered pattern takes them.
    assert!(rules::looks_machine_named("2026-13-01"));
    // A day of 00 is not a date either, and the "x" spoils the numbered pattern.
    assert!(!rules::looks_machine_named("2026-10-00x"));
    // A year alone is not a date, and its digits run out: not a name either.
    assert!(!rules::looks_machine_named("2026notes"));
}

#[test]
fn folders_of_hundreds_of_machine_named_media_are_datasets() {
    let root = "/data/home".to_string();
    let dataset = "/data/home/clips".to_string();
    let photos: Vec<(String, String, FileKind)> = (0..300)
        .map(|i| (dataset.clone(), format!("clip_{i:06}"), FileKind::Video))
        .chain((0..40).map(|i| (root.clone(), format!("clip_{i:06}"), FileKind::Video)))
        .chain((0..299).map(|i| (dataset.clone(), format!("clip_{i:06}"), FileKind::Image)))
        .chain((0..300).map(|i| (dataset.clone(), format!("IMG_{i}"), FileKind::Image)))
        .collect();
    let roots: HashSet<String> = [root.clone()].into_iter().collect();
    let datasets = rules::dataset_folders(&photos, &roots, 300);
    // The 300 machine-named videos count; the 299 machine-named images are one short of the
    // minimum; the 300 camera-named images never qualify; the files right in the root are
    // the user's own choice and never a dataset.
    assert_eq!(datasets.get(&dataset), Some(&300));
    assert_eq!(datasets.get(&root), None);
    assert_eq!(datasets.len(), 1);

    // Under the minimum, nothing.
    let few: Vec<(String, String, FileKind)> = (0..299)
        .map(|i| (dataset.clone(), format!("clip_{i:06}"), FileKind::Video))
        .collect();
    assert!(rules::dataset_folders(&few, &roots, 300).is_empty());

    // A folder that is a third camera-named and a third machine-named stays: 0.7 is the line.
    let mixed: Vec<(String, String, FileKind)> = (0..100)
        .map(|i| (dataset.clone(), format!("clip_{i:06}"), FileKind::Video))
        .chain((0..100).map(|i| (dataset.clone(), format!("IMG_{i}"), FileKind::Video)))
        .chain((0..40).map(|i| (dataset.clone(), format!("vid_{i:06}"), FileKind::Video)))
        .collect();
    // 140 of 240 machine-named: below 0.7.
    assert!(rules::dataset_folders(&mixed, &roots, 300).is_empty());
}

#[test]
fn a_cache_directory_needs_the_tags_signature() {
    let temp = std::env::temp_dir().join("digup-rules-cache");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let tagged = temp.join("tagged");
    let untagged = temp.join("untagged");
    std::fs::create_dir_all(&tagged).unwrap();
    std::fs::create_dir_all(&untagged).unwrap();
    std::fs::write(
        tagged.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\nThis is a cache directory tag created by digup.",
    )
    .unwrap();
    std::fs::write(untagged.join("CACHEDIR.TAG"), b"Signature: nope\n").unwrap();

    assert!(rules::is_cache_directory(&tagged.to_string_lossy()));
    assert!(!rules::is_cache_directory(&untagged.to_string_lossy()));
    assert!(!rules::is_cache_directory(&temp.to_string_lossy()));

    let home = "/home/dev".to_string();
    let excluded = vec!["/home/dev/Downloads".to_string()];
    let reason = |path: &str| rules::skip_reason(path, &home, &excluded);
    assert_eq!(
        reason("/a/node_modules").as_deref(),
        Some("developer folder")
    );
    assert_eq!(
        reason("/a/site-packages").as_deref(),
        Some("developer folder")
    );
    assert_eq!(
        reason("/a/__pycache__").as_deref(),
        Some("developer folder")
    );
    assert_eq!(
        reason("/a/Stuff.noindex").as_deref(),
        Some(".noindex folder")
    );
    assert_eq!(
        reason(&tagged.to_string_lossy()).as_deref(),
        Some("cache folder")
    );
    assert_eq!(reason("/home/dev/Library").as_deref(), Some("~/Library"));
    assert_eq!(reason("/home/dev/Library/Fonts").as_deref(), None);
    assert_eq!(
        reason("/home/dev/Downloads").as_deref(),
        Some("excluded folder")
    );
    assert_eq!(
        reason("/home/dev/Downloads/sub").as_deref(),
        Some("excluded folder")
    );
    assert_eq!(reason("/home/dev/Downloaded").as_deref(), None);
    assert_eq!(reason("/home/dev/Documents").as_deref(), None);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn code_search_skips_vendored_and_worktree_folders() {
    let temp = std::env::temp_dir().join("digup-rules-code-skip");
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(temp.join("vendor")).unwrap();
    std::fs::create_dir_all(temp.join("target")).unwrap();
    std::fs::create_dir_all(temp.join("mlruns")).unwrap();
    std::fs::create_dir_all(temp.join("lightning_logs")).unwrap();
    std::fs::create_dir_all(temp.join("plain")).unwrap();
    std::fs::create_dir_all(temp.join("repo/.git")).unwrap();
    std::fs::create_dir_all(temp.join("worktree")).unwrap();
    std::fs::write(
        temp.join("worktree/.git"),
        b"gitdir: ../repo/.git/worktrees/w\n",
    )
    .unwrap();

    assert_eq!(
        rules::code_skip_reason(&temp.join("vendor").to_string_lossy()).as_deref(),
        Some("vendored or built code")
    );
    assert_eq!(
        rules::code_skip_reason(&temp.join("mlruns").to_string_lossy()).as_deref(),
        Some("vendored or built code")
    );
    assert_eq!(
        rules::code_skip_reason(&temp.join("lightning_logs").to_string_lossy()).as_deref(),
        Some("vendored or built code")
    );
    assert_eq!(
        rules::code_skip_reason(&temp.join("plain").to_string_lossy()),
        None
    );
    assert_eq!(
        rules::code_skip_reason(&temp.join("repo").to_string_lossy()),
        None
    );
    assert_eq!(
        rules::code_skip_reason(&temp.join("worktree").to_string_lossy()).as_deref(),
        Some("submodule or worktree")
    );
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn path_pieces_match_foundation_strings() {
    // `deletingLastPathComponent`: the walk's guards stop at "/" (its count is 1).
    assert_eq!(digup_core::paths::parent("/a/b/c"), "/a/b");
    assert_eq!(digup_core::paths::parent("/a/b"), "/a");
    assert_eq!(digup_core::paths::parent("/a"), "/");
    assert_eq!(digup_core::paths::parent("/"), "/");
    assert_eq!(digup_core::paths::parent("a/b"), "a");
    assert_eq!(digup_core::paths::parent("a"), "");

    // `standardizedFileURL.path`.
    assert_eq!(digup_core::paths::standardize("/a/./b/../c"), "/a/c");
    assert_eq!(digup_core::paths::standardize("/a//b/"), "/a/b");
    assert_eq!(digup_core::paths::standardize("/.."), "/");
    assert_eq!(digup_core::paths::standardize("/a/../../b"), "/b");
    assert_eq!(digup_core::paths::standardize("a/./b"), "a/b");
    assert_eq!(digup_core::paths::standardize("."), ".");

    // `FolderSelection.isInside` is a strict descendant test: a folder is not inside itself.
    assert!(digup_core::paths::is_inside("/a/b/c", "/a/b"));
    assert!(!digup_core::paths::is_inside("/a/b", "/a/b"));
    assert!(digup_core::paths::is_inside("/a/b/c", "/a/b/"));
    assert!(!digup_core::paths::is_inside("/a/bc", "/a/b"));
    assert!(!digup_core::paths::is_inside("/a", "/a/b"));

    // `pathExtension` / `deletingPathExtension` / `lastPathComponent`.
    assert_eq!(digup_core::paths::extension("photo.jpeg"), "jpeg");
    assert_eq!(digup_core::paths::extension("Photo.PNG"), "PNG");
    assert_eq!(digup_core::paths::extension(".hidden"), "");
    assert_eq!(digup_core::paths::extension("noext"), "");
    assert_eq!(digup_core::paths::stem("clip_001.mp4"), "clip_001");
    assert_eq!(digup_core::paths::stem(".hidden"), ".hidden");
    assert_eq!(digup_core::paths::last_component("/a/b/c.txt"), "c.txt");

    // `IndexOptions.extensions`: "HEIC, .txt *.md" → heic, txt, md.
    let set = IndexOptions::extensions("HEIC, .txt *.md  ,");
    assert_eq!(
        set,
        ["heic".to_string(), "txt".to_string(), "md".to_string()]
            .into_iter()
            .collect()
    );
}
