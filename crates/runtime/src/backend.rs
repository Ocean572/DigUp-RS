//! llama.cpp's process-wide setup, in the engine's shape (`LlamaEmbedder.swift:321-387`): the log
//! sink (warnings and errors only, to a file or stderr), the backend modules ggml loads at run
//! time, which GPU is there, and the choice to leave the GPU out when the processor measured faster.
//!
//! The environment names are part of the engine's contract: `DIGUP_DEVICE=cpu` (the helpers run on
//! what `speed --model` measured), `DIGUP_THREADS`, `DIGUP_BACKENDS` (where the ggml modules are).

use std::ffi::{c_char, c_void, CStr, CString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, Once};

use crate::ffi;

pub const DEVICE_ENV: &str = "DIGUP_DEVICE";
pub const THREADS_ENV: &str = "DIGUP_THREADS";
pub const BACKENDS_ENV: &str = "DIGUP_BACKENDS";

/// llama.cpp's log is process-wide: warnings and errors go to one file (or stderr), the rest is
/// dropped (it logs a lot at load time). `LlamaLog` in the engine.
pub struct LlamaLog {
    state: Mutex<LogState>,
}

struct LogState {
    file: Option<std::fs::File>,
    level: u32,
}

// bindgen's C-enum constant width follows the C compiler's enum-type choice: gcc/clang
// give the non-negative ggml_log_level `unsigned int` on Linux (u32), while MSVC's ABI
// makes every C enum `int` (i32) — the same source line compiled the Linux gate green
// and died on Windows with E0308 (Packages run, 2026-11-18). The port's LOG API is
// u32 everywhere, so the constants normalize to u32 on every platform; on Linux the
// cast is a no-op, which is what the allow below silences (clippy's -D warnings bar).
#[allow(clippy::unnecessary_cast)]
mod log_levels {
    use crate::ffi;
    pub const WARN: u32 = ffi::ggml_log_level_GGML_LOG_LEVEL_WARN as u32;
    // ERROR is the poison test's pin (mod tests): a normal build never writes one.
    #[allow(dead_code)]
    pub const ERROR: u32 = ffi::ggml_log_level_GGML_LOG_LEVEL_ERROR as u32;
    pub const CONT: u32 = ffi::ggml_log_level_GGML_LOG_LEVEL_CONT as u32;
}

impl LlamaLog {
    const fn new() -> Self {
        Self {
            state: Mutex::new(LogState {
                file: None,
                level: log_levels::WARN,
            }),
        }
    }

    /// `send(to:)`: nil sends the log to stderr.
    ///
    /// `unwrap_or_else(into_inner)` everywhere below, NOT `unwrap()`: `write` is called
    /// from INSIDE llama.cpp (`log_sink` is an `unsafe extern "C"` callback) — a panic
    /// there unwinds across the C boundary, which is undefined behavior (an abort with
    /// the model half-loaded, the run past the caller's `catch`). A poisoned lock is a
    /// *record* of a panic on some other thread; `LogState` is plain data (a `File` and
    /// a level), no multi-field invariant a panic could leave broken, so the right move
    /// is to take the data and carry on logging, exactly as an NSLock-based Swift
    /// `LlamaLog` carries on (Swift has no lock poisoning at all — the engine cannot
    /// poison, the port can).
    fn send(&self, url: Option<&Path>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        drop(state.file.take());
        let Some(url) = url else { return };
        if !url.exists() {
            let _ = std::fs::File::create(url);
        }
        if let Ok(file) = std::fs::OpenOptions::new().append(true).open(url) {
            state.file = Some(file);
        }
    }

    /// One of our own lines.
    fn line(&self, message: &str) {
        let text = format!("[llama.cpp worker] {message}\n");
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match state.file.as_mut() {
            Some(file) => {
                let _ = file.write_all(text.as_bytes());
                let _ = file.flush();
            }
            None => {
                let _ = std::io::stderr().write_all(text.as_bytes());
            }
        }
    }

    /// A line from llama.cpp itself: only WARN and above, with `CONT` continuing at its level.
    fn write(&self, level: u32, text: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if level != log_levels::CONT {
            state.level = level;
        }
        if state.level < log_levels::WARN {
            return;
        }
        match state.file.as_mut() {
            Some(file) => {
                let _ = file.write_all(text.as_bytes());
                let _ = file.flush();
            }
            None => {
                let _ = std::io::stderr().write_all(text.as_bytes());
            }
        }
    }
}

static LOG: LlamaLog = LlamaLog::new();

// `level as u32` normalizes the callback's enum width (c_uint on Linux, c_int on MSVC);
// the same width note as the log_levels module above.
#[allow(clippy::unnecessary_cast)]
unsafe extern "C" fn log_sink(level: ffi::ggml_log_level, text: *const c_char, _user: *mut c_void) {
    if text.is_null() {
        return;
    }
    LOG.write(
        level as u32,
        &unsafe { CStr::from_ptr(text) }.to_string_lossy(),
    );
}

/// Where the log goes (`LlamaEmbedder`'s `log:` argument; each helper passes `worker.log`).
pub fn set_log(path: Option<&Path>) {
    LOG.send(path);
}

/// One of our own lines, in the same place.
pub fn line(message: &str) {
    LOG.line(message);
}

/// The GPU left out: where this machine's speed check found its processor faster than its GPU
/// (`MeasuredCosts.Speed.device`; the helpers set it before the model loads), or `DIGUP_DEVICE=cpu`.
static CPU_ONLY: AtomicBool = AtomicBool::new(false);
/// Whether `DIGUP_DEVICE` has been consulted: the first read takes the environment's choice, and a
/// helper that set it first keeps its setting (the Swift code decides at process start).
static CPU_ONLY_READ: AtomicBool = AtomicBool::new(false);

pub fn set_cpu_only(value: bool) {
    CPU_ONLY.store(value, Ordering::Relaxed);
    CPU_ONLY_READ.store(true, Ordering::Relaxed);
}

/// `LlamaEmbedder.cpuOnly`.
pub fn cpu_only() -> bool {
    if !CPU_ONLY_READ.load(Ordering::Relaxed) {
        let from_env = std::env::var(DEVICE_ENV)
            .ok()
            .filter(|value| value == "cpu")
            .is_some();
        CPU_ONLY.store(from_env, Ordering::Relaxed);
        CPU_ONLY_READ.store(true, Ordering::Relaxed);
    }
    CPU_ONLY.load(Ordering::Relaxed)
}

static GPU_FOUND: AtomicU32 = AtomicU32::new(0);

fn is_gpu(device: ffi::ggml_backend_dev_t) -> bool {
    let kind = unsafe { ffi::ggml_backend_dev_type(device) };
    kind == ffi::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU
        || kind == ffi::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU
}

fn gpu_devices() -> bool {
    unsafe {
        (0..ffi::ggml_backend_dev_count()).any(|index| is_gpu(ffi::ggml_backend_dev_get(index)))
    }
}

/// A GPU llama.cpp can run on here (Metal on a Mac; CUDA or Vulkan elsewhere, when their backend
/// loaded and found one), before `cpuOnly` leaves it out.
pub fn has_gpu() -> bool {
    init();
    GPU_FOUND.load(Ordering::Relaxed) == 1 && !cpu_only()
}

/// The threads llama.cpp computes on (what runs on the CPU: all of it on a machine without a GPU):
/// every logical processor up to 8, where llama.cpp's default is 4; `DIGUP_THREADS` says otherwise.
/// Cached on the first call — the engine's `static let threads` (`LlamaEmbedder.swift:351-355`)
/// reads the environment ONCE per process, and this feeds `n_threads`/`n_threads_batch` at every
/// model-load and media-encoder creation: the audit-base re-read `DIGUP_THREADS` per call, so a
/// process that changed its environment mid-run (a helper that exports it, a test harness) got
/// the second model loaded on a different thread count than the first — the engine cannot, and
/// the vectors carry the difference (batch partitioning is thread-count-sensitive). Same
/// stance as the ffmpeg lookup (ffmpeg.rs:118-124: "the engine's `static let` never re-reads").
pub fn threads() -> i32 {
    static ONCE: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        let chosen = std::env::var(THREADS_ENV)
            .ok()
            .and_then(|value| value.parse::<i32>().ok());
        let logical = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(1);
        chosen.unwrap_or(logical.min(8)).max(1)
    })
}

/// The backend names ggml loaded, for logs and `digup status`.
pub fn backend_summary() -> String {
    let names: Vec<String> = unsafe {
        (0..ffi::ggml_backend_reg_count())
            .map(|index| {
                let registry = ffi::ggml_backend_reg_get(index);
                CStr::from_ptr(ffi::ggml_backend_reg_name(registry))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };
    names.join(", ")
}

static INIT: Once = Once::new();

/// llama.cpp's process-wide setup, once.
pub fn init() {
    INIT.call_once(|| unsafe {
        ffi::llama_log_set(Some(log_sink), std::ptr::null_mut());
        ffi::mtmd_helper_log_set(Some(log_sink), std::ptr::null_mut());
        if !cfg!(target_os = "macos") {
            load_backends();
        }
        ffi::llama_backend_init();
        if cfg!(target_os = "macos") {
            GPU_FOUND.store(u32::from(gpu_devices()), Ordering::Relaxed);
        }
    });
}

/// On Linux and Windows ggml's backends are shared libraries (`GGML_BACKEND_DL`,
/// scripts/build-llama.sh): the CPU variants always, CUDA or Vulkan when the machine has them.
/// They're looked for in `$DIGUP_BACKENDS`, then beside this executable, then wherever ggml itself
/// looks; whatever loads and works is what the model runs on.
unsafe fn load_backends() {
    let mut folders: Vec<PathBuf> = Vec::new();
    if let Some(chosen) = std::env::var_os(BACKENDS_ENV).filter(|value| !value.is_empty()) {
        folders.push(PathBuf::from(chosen));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(folder) = exe.parent() {
            folders.push(folder.to_path_buf());
        }
    }
    for folder in &folders {
        if ffi::ggml_backend_dev_count() == 0 {
            if let Ok(path) = CString::new(folder.to_string_lossy().into_owned()) {
                ffi::ggml_backend_load_all_from_path(path.as_ptr());
            }
        }
    }
    if ffi::ggml_backend_dev_count() == 0 {
        ffi::ggml_backend_load_all();
    }
    let found = gpu_devices();
    GPU_FOUND.store(u32::from(found), Ordering::Relaxed);
    if cpu_only() {
        // Unloaded rather than just unused: a GPU backend would still set itself up (Vulkan: seconds).
        for index in (0..ffi::ggml_backend_reg_count()).rev() {
            let registry = ffi::ggml_backend_reg_get(index);
            let has_gpu_device = (0..ffi::ggml_backend_reg_dev_count(registry))
                .any(|device| is_gpu(ffi::ggml_backend_reg_dev_get(registry, device)));
            if has_gpu_device {
                ffi::ggml_backend_unload(registry);
            }
        }
    }
    let suffix = if cpu_only() && found {
        " (the GPU left out: the processor was faster here)"
    } else {
        ""
    };
    LOG.line(&format!("backends: {}{suffix}", backend_summary()));
}

#[cfg(test)]
mod tests {
    use super::{LlamaLog, LOG};

    /// The FFI hygiene the fix is about: a panic on a thread that HOLDS the log's lock
    /// poisons it, and the audit-base `unwrap()` in `write`/`line` then panicked —
    /// inside the `unsafe extern "C"` `log_sink`, unwinding across the C boundary llama.cpp
    /// called through (UB; the engine's Swift `LlamaLog` has no poison state to reach).
    /// The pins: after the poison, the log must take its data and carry on — the mutex
    /// `into_inner` path — and not panic. (Measures the FIXED shape; the audit-base
    /// `unwrap()` aborts this test process, which no libtest run can report.)
    #[test]
    fn the_log_survives_a_poisoned_state_lock() {
        let poison = std::thread::spawn(|| {
            let _state = LOG.state.lock().expect("the log lock, fresh");
            panic!("the log's writer died holding the lock");
        });
        let _ = poison.join(); // the panic poisons; join swallows it
        assert!(
            LOG.state.lock().is_err(),
            "the poison landed: the lock is poisoned"
        );
        // The callback path (what llama.cpp calls) and our own line: both must log,
        // not panic.
        LOG.send(None);
        LOG.write(super::log_levels::ERROR, "logged after the poison\n");
        LlamaLog::new(); // shape check: the type is still constructible
    }
}
