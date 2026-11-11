//! `ParentWatch` (ParentWatch.swift, non-macOS branch): a service exits when its parent
//! becomes PID 1 — the app that spawned it is gone, and an orphaned helper would keep the
//! index lock and the model loaded for no one. Images run `tini` as PID 1, so a manual
//! `docker run` needs `--init`; helpers are spawned with argv arrays, never through a shell
//! wrapper (the shell would be the parent, and it exits at once).

use std::time::Duration;

/// The Linux watch: poll every second; when the parent changed (reparented to init),
/// `_exit(0)` — no destructors, no WAL surgery, the engine's rule.
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
