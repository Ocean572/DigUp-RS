//! First-index time from metadata alone (`Estimator.swift`): counts, PDF page counts and
//! media durations come from the file header (`PDFium`'s page count, `ffprobe`), so nothing
//! is decoded. The costs standing in are the M4 Pro's fixed `Costs` as scaled by
//! `MeasuredCosts` (`crate::costs::Costs` lives in services; the estimate multiplies it in).

use std::collections::BTreeMap;
use std::process::Command;

use crate::model::{Candidate, FileKind};
use crate::options::IndexOptions;
use crate::paths;

/// `CodeIndexSize`: the code index's size on disk, per character and per stretch.
pub const PER_CHARACTER: f64 = 2.4;
pub const PER_STRETCH: f64 = 1600.0;
/// Characters a stretch holds on average, its lines cut where the code breaks.
pub const CHARACTERS_PER_STRETCH: i64 = 1400;

/// `Estimate`: what the first pass will read, what it costs, and what waits behind it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Estimate {
    pub files: BTreeMap<FileKind, i64>,
    /// pages the first pass will read
    pub pdf_pages: i64,
    pub pdf_pages_total: i64,
    pub audio_seconds: f64,
    pub video_seconds: f64,
    /// chunks the first pass will read
    pub doc_chunks: i64,
    /// the first pass
    pub seconds: f64,
    /// long files' rest, once everything else is in
    pub long_files: i64,
    pub later_seconds: f64,
    pub code_stretches: i64,
    pub bytes: f64,
    /// the first pass's seconds by kind (a progress bar's time left)
    pub kind_seconds: BTreeMap<FileKind, f64>,
}

impl Estimate {
    /// `Estimate + Estimate`: the totals a multi-folder estimate reports.
    pub fn add(&mut self, other: &Estimate) {
        for (kind, count) in &other.files {
            *self.files.entry(*kind).or_default() += count;
        }
        self.pdf_pages += other.pdf_pages;
        self.pdf_pages_total += other.pdf_pages_total;
        self.audio_seconds += other.audio_seconds;
        self.video_seconds += other.video_seconds;
        self.doc_chunks += other.doc_chunks;
        self.seconds += other.seconds;
        self.long_files += other.long_files;
        self.later_seconds += other.later_seconds;
        self.code_stretches += other.code_stretches;
        self.bytes += other.bytes;
        for (kind, seconds) in &other.kind_seconds {
            *self.kind_seconds.entry(*kind).or_default() += seconds;
        }
    }
}

/// The seconds per unit the estimate multiplies (`Costs`' fields; services'
/// `MeasuredCosts::costs()` produces these calibrated to the machine).
#[derive(Debug, Clone, Copy)]
pub struct Costs {
    pub image: f64,
    pub screenshot: f64,
    pub pdf_page: f64,
    pub doc_chunk: f64,
    pub audio_second: f64,
    pub video_second: f64,
    pub code_stretch: f64,
}

impl Default for Costs {
    fn default() -> Self {
        // The M4 Pro's measured fixed costs (Estimator.swift:7-20).
        Self {
            image: 0.12,
            screenshot: 0.32,
            pdf_page: 0.09,
            doc_chunk: 0.045,
            audio_second: 0.0089,
            video_second: 0.019,
            code_stretch: 0.037,
        }
    }
}

impl Costs {
    /// Field access by the JSON kind names (`MeasuredCosts` scales fields by measured factors).
    pub fn set(&mut self, name: &str, value: f64) {
        match name {
            "image" => self.image = value,
            "screenshot" => self.screenshot = value,
            "pdfPage" => self.pdf_page = value,
            "docChunk" => self.doc_chunk = value,
            "audioSecond" => self.audio_second = value,
            "videoSecond" => self.video_second = value,
            "codeStretch" => self.code_stretch = value,
            _ => {}
        }
    }

    pub fn get(&self, name: &str) -> f64 {
        match name {
            "image" => self.image,
            "screenshot" => self.screenshot,
            "pdfPage" => self.pdf_page,
            "docChunk" => self.doc_chunk,
            "audioSecond" => self.audio_second,
            "videoSecond" => self.video_second,
            "codeStretch" => self.code_stretch,
            _ => 0.0,
        }
    }
}

/// `Estimator.estimate`: time from metadata alone, for a folder's candidates.
pub fn estimate(candidates: &[Candidate], options: &IndexOptions, costs: &Costs) -> Estimate {
    let mut estimate = Estimate::default();
    for file in candidates {
        *estimate.files.entry(file.kind).or_default() += 1;
        let before = estimate.seconds;
        match file.kind {
            FileKind::Image => estimate.seconds += costs.image,
            FileKind::Screenshot => estimate.seconds += costs.screenshot,
            FileKind::Pdf => {
                let total = pages_of(&file.path);
                let all = total.min(options.max_pages);
                let first = if total > options.big_pdf_pages {
                    options.pdf_step.min(all)
                } else {
                    all
                };
                estimate.pdf_pages_total += total;
                estimate.pdf_pages += first;
                estimate.seconds += first as f64 * costs.pdf_page;
                if all > first {
                    estimate.long_files += 1;
                    estimate.later_seconds += (all - first) as f64 * costs.pdf_page;
                }
            }
            FileKind::Doc => {
                let all = 1.max(options.max_chunks.min(file.size / 2000));
                let first = all.min(options.doc_step);
                estimate.doc_chunks += first;
                estimate.seconds += first as f64 * costs.doc_chunk;
                if all > first {
                    estimate.long_files += 1;
                    estimate.later_seconds += (all - first) as f64 * costs.doc_chunk;
                }
            }
            FileKind::Audio => {
                let seconds = duration_of(&file.path);
                estimate.audio_seconds += seconds;
                estimate.seconds += seconds * costs.audio_second;
            }
            FileKind::Video => {
                let seconds = duration_of(&file.path);
                estimate.video_seconds += seconds;
                estimate.seconds += seconds * costs.video_second;
            }
            FileKind::Code => {
                // A notebook's file is mostly its outputs, which aren't read: a guess at
                // its cells' share.
                let characters = if file.path.to_lowercase().ends_with(".ipynb") {
                    file.size / 8
                } else {
                    file.size
                };
                let stretches =
                    1.max((characters + CHARACTERS_PER_STRETCH - 1) / CHARACTERS_PER_STRETCH);
                estimate.code_stretches += stretches;
                estimate.seconds += stretches as f64 * costs.code_stretch;
                estimate.bytes +=
                    characters as f64 * PER_CHARACTER + stretches as f64 * PER_STRETCH;
            }
        }
        // `defer { kindSeconds[kind] += seconds - before }`: the whole per-kind share.
        *estimate.kind_seconds.entry(file.kind).or_default() += estimate.seconds - before;
    }
    estimate
}

/// `Estimator.estimates(bySubfolder)`: one estimate per subfolder one level down, and for
/// the files right in `folder` (under its own path). Subfolders are named under `folder` as
/// given, though the walk reports real paths (/private/var for /var).
pub fn estimates_by_subfolder(
    candidates: &[Candidate],
    folder: &str,
    options: &IndexOptions,
    costs: &Costs,
) -> BTreeMap<String, Estimate> {
    let prefix = if folder.ends_with('/') {
        folder.to_string()
    } else {
        format!("{folder}/")
    };
    let real = format!("{}/", paths::real_path(folder));
    let mut groups: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for file in candidates {
        let rest = file
            .path
            .strip_prefix(&prefix)
            .or_else(|| file.path.strip_prefix(&real));
        let key = match rest {
            Some(rest) => match rest.find('/') {
                Some(index) => format!("{}{}", prefix, &rest[..index]),
                None => folder.to_string(),
            },
            None => folder.to_string(),
        };
        groups.entry(key).or_default().push(file.clone());
    }
    groups
        .into_iter()
        .map(|(key, files)| (key, estimate(&files, options, costs)))
        .collect()
}

/// `PDFium.pages(of:)` on Linux: FPDF_GetPageCount, 1 when the document can't be read.
/// The engine opens the file with PDFium; the port answers through this seam. `crates/native`
/// installs the PDFium call at startup; until something installs it — a core-only test, a
/// helper built without the C library — the header scan stands in: the trailer's `/Count` is
/// the page tree's own number, `/Type /Page` tokens count the same objects, and an unreadable
/// file is 1 as PDFium's failure is. The seam is a C-ABI pointer so any crate that links
/// PDFium can hand one over (`crates/native::pdf::install_pages_hook`).
pub type PagesHook = unsafe extern "C" fn(*const libc::c_char) -> i64;
static PAGES_HOOK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Installs the count function (process start, once); `None` puts the header scan back.
pub fn set_pages_hook(hook: Option<PagesHook>) {
    let address = hook.map_or(0, |f| f as usize);
    PAGES_HOOK.store(address, std::sync::atomic::Ordering::Relaxed);
}

pub fn pages_of(path: &str) -> i64 {
    let address = PAGES_HOOK.load(std::sync::atomic::Ordering::Relaxed);
    if address != 0 {
        let hook = unsafe { std::mem::transmute::<usize, PagesHook>(address) };
        let Ok(name) = std::ffi::CString::new(path) else {
            return 1; // the engine's `try?` on a name C can't carry
        };
        return unsafe { hook(name.as_ptr()) };
    }
    let Ok(data) = read_tail(path) else {
        return 1;
    };
    if let Some(count) = trailer_page_count(&data) {
        return count;
    }
    if let Some(count) = page_object_count(&data) {
        return count;
    }
    1
}

/// The last 32 KiB is where a PDF's trailer/xref sits; small files come whole.
fn read_tail(path: &str) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let take = size.min(32 * 1024);
    file.seek(SeekFrom::End(-(take as i64)))?;
    let mut buffer = Vec::with_capacity(take as usize);
    file.read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// The trailer's `/Count N`: the page tree root's count, as PDFium reports it. Scans the
/// tail backward for `trailer` and reads the `/Count` of the last one.
fn trailer_page_count(tail: &[u8]) -> Option<i64> {
    let text = String::from_utf8_lossy(tail);
    let at = text.rfind("trailer")?;
    let after = &text[at..];
    let count_at = after.rfind("/Count")?;
    parse_number(&after[count_at + 6..])
}

fn parse_number(text: &str) -> Option<i64> {
    let digits: String = text
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// A header-less count of page objects: `/Type /Page` tokens (not `/Pages`), scanning the
/// whole file (xref-stream PDFs have no `trailer` keyword).
fn page_object_count(data: &[u8]) -> Option<i64> {
    let mut count = 0i64;
    let needle = b"/Type";
    let mut at = 0;
    while let Some(found) = find(&data[at..], needle) {
        at += found + needle.len();
        let mut rest = &data[at..];
        // one whitespace, then /Page (not /Pages), then a delimiter
        let Some(slashed) = rest.iter().position(|byte| *byte == b'/') else {
            continue;
        };
        rest = &rest[slashed..];
        if rest.starts_with(b"/Page") && !rest.starts_with(b"/Pages") {
            let after = &rest[b"/Page".len()..];
            if after
                .first()
                .map(|byte| !byte.is_ascii_alphanumeric())
                .unwrap_or(true)
            {
                count += 1;
            }
        }
    }
    if count > 0 {
        Some(count)
    } else {
        None
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `Media.duration(of:)`: ffprobe's format duration, 0 when it can't say (the engine's
/// `MediaInfo.duration(of:) ?? 0`). ffprobe sits in `$DIGUP_FFMPEG`, beside this
/// executable, or on PATH, as the engine's `FFmpeg.find` looks.
pub fn duration_of(path: &str) -> f64 {
    let Some(tool) = find_program("ffprobe") else {
        return 0.0;
    };
    let Ok(output) = Command::new(&tool)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            path,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return 0.0;
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match text.parse::<f64>() {
        Ok(seconds) if seconds.is_finite() && seconds > 0.0 => seconds,
        _ => 0.0,
    }
}

/// `FFmpeg.find` + `FileSystem.executable`: `$DIGUP_FFMPEG`, beside this executable, then PATH.
pub fn find_program(name: &str) -> Option<String> {
    let mut folders: Vec<std::path::PathBuf> = Vec::new();
    if let Some(chosen) = std::env::var("DIGUP_FFMPEG")
        .ok()
        .filter(|value| !value.is_empty())
    {
        folders.push(std::path::PathBuf::from(chosen));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(folder) = exe.parent() {
            folders.push(folder.to_path_buf());
        }
    }
    for folder in folders {
        let candidate = folder.join(name);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for folder in path.split(':') {
        let candidate = std::path::Path::new(folder).join(name);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}
