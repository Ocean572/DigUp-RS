//! The program lookup the media paths depend on (`paths::find_program`, the engine's
//! `FFmpeg.find` + `FileSystem.executable`, Media+Portable.swift:9-18 and
//! Files+Portable.swift:146-168).
//!
//! One test, in sequence: the lookup is cached per process (the engine's `static let
//! ffmpeg, ffprobe`), so the phases use distinct tool names and each phase asserts the state
//! it is about. The env this test mutates is its own process's — an integration test is its
//! own binary.

use digup_core::paths::{find_program, program};
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn make_tool(folder: &Path, name: &str, executable: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = folder.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("a tool file");
    std::fs::set_permissions(
        &path,
        std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )
    .expect("its mode");
    path
}

/// On Windows the engine's `isExecutableFile(atPath:)` is the executable *extension*, which
/// `program` has already put in the candidate's name — so a `.txt` is the "not a program"
/// shape here, and every tool is spelled `.exe`.
#[cfg(windows)]
fn make_tool(folder: &Path, name: &str, executable: bool) -> PathBuf {
    let path = folder.join(if executable {
        format!("{name}.exe")
    } else {
        format!("{name}.txt")
    });
    std::fs::write(&path, "@echo off\r\n").expect("a tool file");
    path
}

#[test]
fn the_program_lookup_follows_the_engines_order() {
    let tmp = tempfile::tempdir().expect("a temp folder");
    let chosen = tmp.path().join("chosen");
    let on_path = tmp.path().join("path");
    std::fs::create_dir(&chosen).expect("$DIGUP_FFMPEG folder");
    std::fs::create_dir(&on_path).expect("a PATH folder");

    let in_chosen = make_tool(&chosen, "digup-probe-ffmpeg", true);
    let in_path = make_tool(&on_path, "digup-probe-ffprobe", true);
    let not_a_program = make_tool(&on_path, "digup-probe-nota", false);

    assert_eq!(
        program("ffmpeg"),
        if cfg!(windows) {
            "ffmpeg.exe".to_string()
        } else {
            "ffmpeg".to_string()
        },
        "`FileSystem.program`: the .exe suffix is Windows' (Files+Portable.swift:146-151)"
    );

    std::env::set_var("DIGUP_FFMPEG", &chosen);
    std::env::set_var("PATH", &on_path);

    // `$DIGUP_FFMPEG` (a folder) is the first place, as the engine's `find` lists it.
    assert_eq!(
        find_program("digup-probe-ffmpeg").as_deref(),
        Some(in_chosen.to_string_lossy().as_ref()),
        "$DIGUP_FFMPEG names the folder the tool is looked for in"
    );
    // Not there → PATH, split the platform's way (`;` on Windows, `:` elsewhere).
    assert_eq!(
        find_program("digup-probe-ffprobe").as_deref(),
        Some(in_path.to_string_lossy().as_ref()),
        "the PATH lookup reaches the tool"
    );
    assert_eq!(
        find_program("digup-probe-nota"),
        None,
        "a file the OS will not run is not a program"
    );
    let _ = not_a_program;

    // No PATH in the environment: the engine's own default list
    // (`/usr/local/bin:/usr/bin:/bin`, Files+Portable.swift:158-160) — the shape that
    // `find_program` needed on Windows is `.exe` + `;`, which is why this lookup is one
    // function for the whole workspace.
    #[cfg(unix)]
    {
        std::env::remove_var("PATH");
        assert!(
            find_program("sh").is_some(),
            "an environment with no PATH still finds /bin/sh, as `executable(_:)` does"
        );
    }

    // Found once: the tool is gone from the folder and the answer is unchanged — the engine's
    // `static let ffmpeg` reads `$DIGUP_FFMPEG` once too, and a helper that re-read it per
    // file would move a run's answers when the folder moves mid-run.
    std::fs::remove_file(&in_chosen).expect("the tool file is removed");
    assert_eq!(
        find_program("digup-probe-ffmpeg").as_deref(),
        Some(in_chosen.to_string_lossy().as_ref()),
        "the lookup is resolved once per process, like the engine's `static let`"
    );
}
