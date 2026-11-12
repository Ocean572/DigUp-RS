//! `engine/search.py`'s `threading.RLock` in Rust: `std::sync::ReentrantLock` is
//! unstable in this toolchain (measured on rustc 1.99.1), and Python's RLock is
//! what makes `_call` → `_spawn_locked` re-enter the same lock. Owner + depth
//! under a mutex, a condvar for the other callers.

use std::sync::{Arc, Condvar, Mutex};
use std::thread;

#[derive(Default)]
struct State {
    owner: Option<thread::ThreadId>,
    depth: usize,
}

pub struct RLock {
    inner: Mutex<State>,
    ready: Condvar,
}

pub struct Guard {
    lock: Arc<RLock>,
}

impl RLock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(State::default()),
            ready: Condvar::new(),
        })
    }

    /// Acquire, recursively: the owner taking it again just deepens.
    pub fn lock(self: &Arc<Self>) -> Guard {
        let me = thread::current().id();
        let mut state = self.inner.lock().expect("the service lock");
        while state.depth > 0 && state.owner != Some(me) {
            state = self.ready.wait(state).expect("the service lock");
        }
        state.owner = Some(me);
        state.depth += 1;
        drop(state);
        Guard { lock: self.clone() }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut state = self.lock.inner.lock().expect("the service lock");
        state.depth -= 1;
        if state.depth == 0 {
            state.owner = None;
            self.lock.ready.notify_all();
        }
    }
}
