//! ffmpeg and ffprobe, the way the portable platform reaches them (`Media+Portable.swift`):
//! looked for in `$DIGUP_FFMPEG`, then beside this executable, then on PATH. A tool runs with an
//! argv array (never a shell), and its standard output is handed to the reader as it comes — a long
//! decode never sits in memory whole, and a reader that has had enough ends the tool with it.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use digup_core::extract::ExtractError;

/// `FFmpeg.find(_:)` (`Media+Portable.swift:9-18`): `$DIGUP_FFMPEG` (a FOLDER), the folder
/// this executable sits in, then PATH — the workspace's one lookup,
/// `digup_core::paths::find_program` (`.exe` on Windows, `;` PATH separator there, `/`-style
/// with the engine's default PATH elsewhere, cached once per process as the engine's
/// `static let` does). Two twins of this lookup drifted once already: core's copy lacked
/// Windows' `.exe` suffix and `;` split, so the estimate's `duration_of` answered 0 for every
/// media file on Windows. `crates/native` depends on `crates/core`, so the one copy lives here.
pub fn find(name: &str) -> Option<String> {
    digup_core::paths::find_program(name)
}

/// A tool ran, and why it did not: the engine's own refusal (`ffmpeg isn't installed …`,
/// `ffmpeg failed (1)`), or the OS refusing to start it at all — the difference `decodeWithFFmpeg`
/// reads as "this ffmpeg can't read HEIC" rather than "can't decode image".
#[derive(Debug)]
pub enum RunError {
    Extract(ExtractError),
    Io(std::io::Error),
}

impl RunError {
    /// The media seam (`Media+Portable.swift`) throws `ExtractError` to its callers, so an IO
    /// failure becomes the engine's refusal here; `decodeWithFFmpeg` reads the distinction itself
    /// (an IO failure is "this ffmpeg can't read HEIC", not "can't decode image").
    pub fn extract(self) -> ExtractError {
        match self {
            RunError::Extract(error) => error,
            RunError::Io(error) => ExtractError::Unreadable {
                why: format!("can't run ffmpeg: {error}"),
            },
        }
    }
}

/// `FFmpeg.run(_:arguments:read:)`: run a tool, handing its standard output to `read` in 64 KB
/// pieces. When the reader has had enough, the tool is ended here — not left to block on a full
/// pipe forever with the reader gone past it.
pub fn run(
    tool: Option<&str>,
    arguments: &[String],
    read: &mut dyn FnMut(&[u8]) -> Result<(), ExtractError>,
) -> Result<(), RunError> {
    let Some(tool) = tool else {
        return Err(RunError::Extract(ExtractError::Unreadable {
            why: "ffmpeg isn't installed (audio and video need it)".into(),
        }));
    };
    let mut child = Command::new(tool)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(RunError::Io)?;
    let mut pipe = child.stdout.take().expect("the pipe the process was given");
    let mut buffer = [0u8; 1 << 16];
    loop {
        // `readData(ofLength:)` returns what is available and an empty read at the END —
        // and a read ERROR is a Foundation exception, which Swift cannot catch: the engine
        // CRASHES there, so Swift never faces "error vs EOF". The audit-base folded every
        // read error into EOF (`unwrap_or_default()`), which answers SUCCESS mid-stream: a
        // disk fault mid-audio ends the window stream with `run` returning Ok, and the
        // file indexes `done` with its windows truncated — a silent data loss the engine
        // does not have (it dies loudly instead). The port's established IO shape (the
        // spawn failure below): the file lands `failed` with the OS's text, and the index
        // run carries on. std's File::read already restarts EINTR, so what surfaces here
        // is a real fault. Killing the child first keeps the tool from sitting on a
        // reader that is gone.
        let read_any = match pipe.read(&mut buffer) {
            Ok(count) => count,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Io(error));
            }
        };
        if read_any == 0 {
            break; // the writer closed: the engine's empty read
        }
        // DEVIATION, on purpose: when the reader throws, Swift's `try read(data)` leaves
        // the tool RUNNING, writing into a pipe nobody reads — ffmpeg blocks on the full
        // pipe forever (an orphan per failed file on a long index). The port ends it:
        // kill, reap, and the reader's refusal propagates the same.
        if let Err(error) = read(&buffer[..read_any]) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Extract(error));
        }
    }
    // EOF on the writer's side; the reader drops with the scope. (The audit-base carried a
    // `let _ = pipe.read(&mut []);` here — a zero-byte read on a pipe answers 0 with no
    // state change: the line did nothing.)
    let status = child.wait().map_err(RunError::Io)?;
    // Foundation's `terminationStatus` is the signal number when a signal ended the process;
    // `ExitStatus::code()` says nothing in that case, so the signal stands in for it.
    // The cfg arms are spelled as two definitions on purpose: compiled for Windows the
    // closure body is the plain constant 1, and clippy refuses `unwrap_or_else` over a
    // constant (`unnecessary_lazy_evaluations`, measured on the mingw cross-check
    // 2026-11-17) — each platform names its own fallback.
    #[cfg(unix)]
    let code = status
        .code()
        .unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            ExitStatusExt::signal(&status).unwrap_or(1)
        });
    #[cfg(not(unix))]
    let code = status.code().unwrap_or(1);
    if code == 0 {
        return Ok(());
    }
    let name = Path::new(tool)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    Err(RunError::Extract(ExtractError::Unreadable {
        why: format!("{name} failed ({code})"),
    }))
}

/// `FFmpeg.output(_:arguments:)`: the whole standard output, as text.
pub fn output(tool: Option<&str>, arguments: &[String]) -> Result<String, RunError> {
    let mut data: Vec<u8> = Vec::new();
    run(tool, arguments, &mut |chunk: &[u8]| {
        data.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// The tools themselves, found once per process as the engine's `static let` does —
/// `paths::find_program` keeps the answer for the life of the process, so these are plain
/// calls, and `$DIGUP_FFMPEG` set after the first lookup is deliberately not re-read (the
/// engine's `static let ffmpeg` never re-reads it either).
pub fn ffmpeg() -> Option<String> {
    digup_core::paths::find_program("ffmpeg")
}

pub fn ffprobe() -> Option<String> {
    digup_core::paths::find_program("ffprobe")
}

/// The arguments of the picture decode: the first frame, 8-bit RGB, as PPM down the pipe.
pub fn picture_arguments(path: &Path) -> Vec<String> {
    ["-v", "error", "-nostdin", "-i"]
        .iter()
        .map(|argument| argument.to_string())
        .chain(std::iter::once(path.to_string_lossy().into_owned()))
        .chain(
            [
                "-frames:v",
                "1",
                "-pix_fmt",
                "rgb24",
                "-f",
                "image2pipe",
                "-c:v",
                "ppm",
                "-",
            ]
            .iter()
            .map(|argument| argument.to_string()),
        )
        .collect()
}
