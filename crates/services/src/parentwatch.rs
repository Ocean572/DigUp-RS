//! `ParentWatch` (ParentWatch.swift, non-macOS branch): a service exits when its parent
//! becomes PID 1 — the app that spawned it is gone, and an orphaned helper would keep the
//! index lock and the model loaded for no one. Images run `tini` as PID 1, so a manual
//! `docker run` needs `--init`; helpers are spawned with argv arrays, never through a shell
//! wrapper (the shell would be the parent, and it exits at once).

use std::time::Duration;

/// The Linux watch: poll every second; when the parent changed (reparented to init),
/// `_exit(0)` — no destructors, no WAL surgery, the engine's rule.
#[cfg(unix)]
pub fn start() {
    let parent = unsafe { libc::getppid() };
    if parent == 1 {
        unsafe { libc::_exit(0) }; // init took it in: the app is gone already
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if unsafe { libc::getppid() } != parent {
            unsafe { libc::_exit(0) };
        }
    });
}

/// The Windows watch. There is no getppid, and NT has no reparenting: the
/// inherited parent id STAYS after the parent dies, so "parent became PID 1"
/// has no analog — the watch asks the engine's real question instead: is the
/// parent process gone? The id comes from `NtQueryInformationProcess`
/// (`ProcessBasicInformation`), the liveness from the same `OpenProcess`
/// probe `is_gone` runs, and the exit stays the engine's `_exit(0)`.
#[cfg(windows)]
pub fn start() {
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    fn parent_id() -> Option<u32> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, std::process::id());
            if handle.is_null() {
                return None;
            }
            let mut info: PROCESS_BASIC_INFORMATION = core::mem::zeroed();
            let mut returned: u32 = 0;
            let status = NtQueryInformationProcess(
                handle,
                ProcessBasicInformation,
                &mut info as *mut _ as *mut core::ffi::c_void,
                core::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                &mut returned as *mut u32,
            );
            CloseHandle(handle);
            if status == 0 && info.InheritedFromUniqueProcessId != 0 {
                Some(info.InheritedFromUniqueProcessId as u32)
            } else {
                None
            }
        }
    }
    // NT answers no parent (spawned by a system thread, query refused):
    // there is no one to watch, so the watch stays home.
    let Some(parent) = parent_id() else {
        return;
    };
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if crate::index_service::is_gone(parent as i32) {
            unsafe { libc::_exit(0) }; // the app that spawned it is gone
        }
    });
}
