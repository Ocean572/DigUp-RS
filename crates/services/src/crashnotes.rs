//! `CrashNotes` (CrashNotes.swift): notes the file the helper dies on when a fatal
//! signal beats the watchdog. Swift's allocation trap — the "indexer: exited (5)" of
//! issue #182 — fires on the first allocation the system refuses, inside a hundredth
//! of a second of a 45 MP RAW's reading, faster than a watchdog tick could note the
//! file. The file then stays pending, every restart takes it first, and the queue
//! never moves. These handlers write a `StoppedFile` note and re-raise, so the next
//! helper marks that file and indexing goes on. The note carries no path (the crash
//! beats `Indexer::now_reading`, and reading runs ahead of the queue's head in media
//! batches): the app that watched the helper names the file for the next one
//! (`DIGUP_STOPPED_FILE`, from the `file` events), and the head-mark is the fallback.
//!
//! The handler runs off install-time copies with syscalls alone (`open`, `write`):
//! malloc and the allocator can deadlock a process that was mid-allocation when the
//! signal arrived, and the helper's startup consumes any note left beside the index,
//! so the file is made at the crash itself, by path. Installing plain handlers
//! replaces the runtime's own signal handling: its crash print is lost,
//! deliberately — the note beside the index is what unblocks a queue. Re-raising at
//! `SIG_DFL` keeps what the system can build from the signal itself. One consequence
//! of handling SIGTRAP process-wide: a debugger or profiler that stops the helper on
//! a trap leaves a note here, and the app marks a file unreadable on the strength of
//! it — attach one to the helper knowing that.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::Path;

use digup_core::model::StoppedFile;

/// The note, copied at install: bytes + C path, leaked so the handler borrows
/// 'static storage. Plain reads of these statics are signal-safe (no locks, no
/// allocation; the pointers are published before any handler is installed).
static mut NOTE_BYTES: *const u8 = std::ptr::null();
static mut NOTE_LEN: usize = 0;
static mut NOTE_PATH: *const c_char = std::ptr::null();

/// `CrashNotes.install(in:)`: the Linux list — SIGTRAP is the runtime's trap;
/// SIGSEGV/SIGBUS the access faults; SIGABRT/SIGILL/SIGFPE the rest. (The Windows C
/// runtime has no SIGBUS; that port's faults arrive as SIGSEGV — out of scope on
/// this target.)
pub fn install(folder: &Path) {
    let note = StoppedFile {
        path: None,
        why: "crashed on it (a fatal signal — exit 5 among them) while reading it".to_string(),
    };
    let bytes = note.bytes();
    let path = CString::new(
        StoppedFile::location(folder)
            .to_string_lossy()
            .as_bytes()
            .to_vec(),
    )
    .ok();
    let (Some(path), true) = (path, !bytes.is_empty()) else {
        return;
    };
    let bytes: &[u8] = Box::leak(bytes.into_boxed_slice());
    let leaked: &'static CString = Box::leak(Box::new(path));
    let path: *const c_char = leaked.as_ptr();
    unsafe {
        NOTE_BYTES = bytes.as_ptr();
        NOTE_LEN = bytes.len();
        NOTE_PATH = path;
    }
    for sig in crash_signals() {
        unsafe {
            libc::signal(
                sig,
                crash_note_and_reraise as *const () as libc::sighandler_t,
            )
        };
    }
}

/// The fatal-signal set the crash handler is installed for. unix: the engine's
/// six (a bus error and a trap are real faults there). Windows: the CRT's fatal
/// four — the windows libc defines no SIGBUS and no SIGTRAP at all (measured by
/// the gnu cross-check 2026-11-16), and the access violation that stands in for
/// a bus error arrives as a SIGSEGV through the CRT's own mapping.
#[cfg(not(windows))]
fn crash_signals() -> [libc::c_int; 6] {
    [
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGFPE,
        libc::SIGILL,
        libc::SIGSEGV,
        libc::SIGTRAP,
    ]
}

#[cfg(windows)]
fn crash_signals() -> [libc::c_int; 4] {
    [libc::SIGABRT, libc::SIGFPE, libc::SIGILL, libc::SIGSEGV]
}

/// Runs on the crashing thread. Everything it touches was made at install; `open`
/// and `write` are async-signal-safe, so this notes the crash and then lets the
/// default handler take the process: the app sees the same exit it saw before, with
/// a note beside the index for the next helper to mark the file.
extern "C" fn crash_note_and_reraise(sig: c_int) {
    unsafe {
        let (path, bytes, len) = (NOTE_PATH, NOTE_BYTES, NOTE_LEN);
        if !path.is_null() && !bytes.is_null() {
            let fd = libc::open(
                path,
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                // `open`'s mode is variadic: pass it as the C-promoted `unsigned int` (the
                // Linux `mode_t`), which macOS's `mode_t` (`c_int`) would not be.
                0o644 as libc::c_uint,
            );
            if fd >= 0 {
                // unix's write takes a size_t count, the windows CRT's a
                // c_uint: `as _` lets each target's own parameter type win.
                let _ = libc::write(fd, bytes as *const libc::c_void, len as _);
                let _ = libc::close(fd);
            }
        }
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}
