//! ffmpeg and ffprobe, the way the portable platform reaches them (`Media+Portable.swift`):
//! looked for in `$DIGUP_FFMPEG`, then beside this executable, then on PATH. A tool runs with an
//! argv array (never a shell), and its standard output is handed to the reader as it comes — a long
//! decode never sits in memory whole, and a reader that has had enough ends the tool with it.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use digup_core::extract::ExtractError;

/// `FileSystem.program(_:)`: the name as an OS program is spelled on this platform.
pub fn program(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// `FileSystem.executable(_:)`: `name` on PATH.
fn on_path(name: String) -> Option<String> {
    let (key, separator, default) = if cfg!(windows) {
        ("PATH", ';', "")
    } else {
        ("PATH", ':', "/usr/local/bin:/usr/bin:/bin")
    };
    let path = std::env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string());
    for folder in path.split(separator).filter(|folder| !folder.is_empty()) {
        let candidate = format!("{folder}/{name}");
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path)
            .map(|meta| meta.is_file())
            .unwrap_or(false)
    }
}

/// `FFmpeg.find(_:)`: `$DIGUP_FFMPEG`, beside this executable, then PATH.
pub fn find(name: &str) -> Option<String> {
    let name = program(name);
    let mut folders: Vec<String> = Vec::new();
    if let Ok(chosen) = std::env::var("DIGUP_FFMPEG") {
        if !chosen.is_empty() {
            folders.push(chosen);
        }
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(folder) = executable.parent() {
            folders.push(folder.to_string_lossy().into_owned());
        }
    }
    for folder in folders {
        let candidate = format!("{folder}/{name}");
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    on_path(name)
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
        // `readData(ofLength:)` returns what is available and an empty read at the end.
        let read_any: usize = pipe.read(&mut buffer).unwrap_or_default();
        if read_any == 0 {
            break;
        }
        if let Err(error) = read(&buffer[..read_any]) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Extract(error));
        }
    }
    let _ = pipe.read(&mut []); // closing the reader's side
    let status = child.wait().map_err(RunError::Io)?;
    // Foundation's `terminationStatus` is the signal number when a signal ended the process;
    // `ExitStatus::code()` says nothing in that case, so the signal stands in for it.
    let code = status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitStatusExt::signal(&status).unwrap_or(1)
        }
        #[cfg(not(unix))]
        {
            1
        }
    });
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

/// The tools themselves, found once per process as the engine's `static let` does.
pub fn ffmpeg() -> Option<String> {
    use std::sync::OnceLock;
    static FFMPEG: OnceLock<Option<String>> = OnceLock::new();
    FFMPEG.get_or_init(|| find("ffmpeg")).clone()
}

pub fn ffprobe() -> Option<String> {
    use std::sync::OnceLock;
    static FFPROBE: OnceLock<Option<String>> = OnceLock::new();
    FFPROBE.get_or_init(|| find("ffprobe")).clone()
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
