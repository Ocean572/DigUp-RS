//! `DispatchSemaphore(value: 2)` in the port's toolchain (rustc 1.99.1:
//! `std::sync::Semaphore` is not in `sync` — the stabilization was pulled back),
//! the same shape the search daemon's render slots use.

use std::sync::{Condvar, Mutex};

pub struct Slots {
    inner: Mutex<i32>,
    ready: Condvar,
}

impl Slots {
    pub fn new(count: i32) -> Self {
        Self {
            inner: Mutex::new(count),
            ready: Condvar::new(),
        }
    }

    pub fn acquire(&self) {
        let mut count = self.inner.lock().expect("the render slots");
        while *count == 0 {
            count = self.ready.wait(count).expect("the render slots");
        }
        *count -= 1;
    }

    pub fn release(&self) {
        *self.inner.lock().expect("the render slots") += 1;
        self.ready.notify_one();
    }
}
