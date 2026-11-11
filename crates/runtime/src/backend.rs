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

impl LlamaLog {
    const fn new() -> Self {
        Self {
            state: Mutex::new(LogState {
                file: None,
                level: ffi::ggml_log_level_GGML_LOG_LEVEL_WARN,
            }),
        }
    }

    /// `send(to:)`: nil sends the log to stderr.
    fn send(&self, url: Option<&Path>) {
        let mut state = self.state.lock().unwrap();
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
        let mut state = self.state.lock().unwrap();
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
        let mut state = self.state.lock().unwrap();
        if level != ffi::ggml_log_level_GGML_LOG_LEVEL_CONT {
            state.level = level;
        }
        if state.level < ffi::ggml_log_level_GGML_LOG_LEVEL_WARN {
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

unsafe extern "C" fn log_sink(level: ffi::ggml_log_level, text: *const c_char, _user: *mut c_void) {
    if text.is_null() {
        return;
    }
    LOG.write(level, &unsafe { CStr::from_ptr(text) }.to_string_lossy());
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
pub fn threads() -> i32 {
    let chosen = std::env::var(THREADS_ENV)
        .ok()
        .and_then(|value| value.parse::<i32>().ok());
    let logical = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(1);
    chosen.unwrap_or(logical.min(8)).max(1)
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
