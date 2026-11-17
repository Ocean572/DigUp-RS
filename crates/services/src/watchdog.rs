//! `Watchdog` (Watchdog.swift): ends the indexing helper when reading one file runs
//! away. Apple's PDF text extraction has grown without end on a page of some PDFs
//! (30 GB in 3.5 minutes, never returning), and nothing stops code like that from
//! outside but ending the process. Ten times a second the watchdog looks at the
//! helper's memory (resident bytes, as Linux counts them) and at how long the file
//! being read (`Indexer::now_reading`) has taken. Past `memory_limit`, or past
//! `time_limit` on one file, it writes down the file and why (`StoppedFile`) and
//! exits with `StoppedFile::EXIT_CODE`: the app starts a new helper, which marks that
//! file as one that couldn't be read and goes on. The ten-times-a-second rate
//! matters: the runaway reading of a 45 MP RAW climbs past gigabytes in well under a
//! second, and a watchdog that looks once a second loses it to an allocation trap,
//! which notes nothing (issue #182 — `CrashNotes` is that case's net).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use digup_core::model::StoppedFile;

/// Half the machine's memory, but at least 4 GB and at most 8 GB: indexing peaks at
/// 1–3 GB on the biggest files. `physicalMemory` is `/proc/meminfo`'s MemTotal (the
/// same reading Foundation answers on Linux); a machine whose memory can't be read
/// takes the floor, as Foundation's 0 does.
pub fn default_memory_limit() -> u64 {
    let physical = physical_memory().unwrap_or(0);
    (8 << 30).min((4 << 30).max(physical / 2))
}

/// Reading one file never takes this long: an hour of 4K video is minutes, a long
/// book is read 30 pages a step.
pub const DEFAULT_TIME_LIMIT: f64 = 15.0 * 60.0;

pub struct Watchdog {
    memory_limit: u64,
    time_limit: f64,
    note: PathBuf,
}

impl Watchdog {
    pub fn new(folder: &Path, memory_limit: u64, time_limit: f64) -> Self {
        Self {
            memory_limit,
            time_limit,
            note: StoppedFile::location(folder),
        }
    }

    /// The watching thread: `Thread.detachNewThread { start() }`'s shape — the main
    /// thread never waits on it, and nothing joins it: it ends the process.
    pub fn start(self) {
        let Watchdog {
            memory_limit,
            time_limit,
            note,
        } = self;
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(100));
            let reading = digup_runtime::indexer::now_reading();
            if let Some(footprint) = footprint() {
                if footprint > memory_limit {
                    let why = format!("needed more than {} of memory to read", size(memory_limit));
                    stop(&note, reading.as_ref().map(|(path, _)| path.as_str()), &why);
                }
            }
            if let Some((path, since)) = reading {
                if elapsed(since) > time_limit {
                    let limit = if time_limit >= 120.0 {
                        format!("{} minutes", (time_limit / 60.0) as i64)
                    } else {
                        format!("{} seconds", time_limit as i64)
                    };
                    stop(
                        &note,
                        Some(path.as_str()),
                        &format!("took more than {limit} to read"),
                    );
                }
            }
        });
    }
}

/// Notes the file, says it on stdout and stderr, and ends the process with 75 — the
/// code that says "a file was stopped: start a new helper".
fn stop(note: &Path, path: Option<&str>, why: &str) -> ! {
    StoppedFile {
        path: path.map(str::to_string),
        why: why.to_string(),
    }
    .write_to(note.parent().unwrap_or(Path::new(".")));
    let line = format!("watchdog: {why}: {}", path.unwrap_or("no file being read"));
    let event = serde_json::json!({"event": "error", "message": line});
    if let Some(json) = digup_core::jsonline::to_line(&event) {
        write_stdout(&format!("{json}\n"));
    }
    write_stderr(&format!("{line}\n"));
    unsafe { libc::_exit(StoppedFile::EXIT_CODE) }
}

/// `Watchdog.size`: whole GB when there are GB, whole MB below.
fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.0} GB", bytes as f64 / (1 << 30) as f64)
    } else {
        format!("{} MB", bytes >> 20)
    }
}

/// This process's memory as Activity Monitor counts it — resident memory here, the
/// statm `resident` field times the page size (Watchdog.footprint's Linux branch).
pub fn footprint() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * page_size())
}

/// `getpagesize()`, the page size the statm counts are in.
fn page_size() -> u64 {
    #[cfg(unix)]
    {
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if size > 0 {
            size as u64
        } else {
            4096
        }
    }
    #[cfg(windows)]
    {
        // Unreachable by construction: `footprint` reads /proc/self/statm,
        // which answers no file on windows, so the page count path ends at
        // None before this runs. The number keeps the unix fallback's value.
        4096
    }
}

/// `ProcessInfo.physicalMemory` on Linux: `/proc/meminfo`'s MemTotal, in kilobytes.
fn physical_memory() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            return rest.trim().trim_end_matches(" kB").trim().parse().ok();
        }
    }
    None
}

fn elapsed(since: SystemTime) -> f64 {
    SystemTime::now()
        .duration_since(since)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Events go straight to the file descriptor (print() would buffer in a pipe, and
/// the app may be gone: the write is best effort, as `emit`'s `try?` is).
fn write_stdout(text: &str) {
    use std::io::Write;
    let _ = std::io::stdout().write_all(text.as_bytes());
    let _ = std::io::stdout().flush();
}

fn write_stderr(text: &str) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(text.as_bytes());
}
