//! Port of requester/now_windows.go
//!
//! The Go original calls `QueryPerformanceCounter` through `syscall` because
//! Go's `time.Now()` historically had coarse resolution on Windows. Rust's
//! `Instant` is already backed by `QueryPerformanceCounter` on Windows, so the
//! same precision is obtained without the raw FFI and the `unsafe` block.

use crate::gotime::GoDuration;
use std::sync::OnceLock;
use std::time::Instant;

fn start_time() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Go: `func now() time.Duration` -- QPC-based high resolution timing.
pub fn now() -> GoDuration {
    GoDuration::from_std(start_time().elapsed())
}

pub fn init_clock() {
    let _ = start_time();
}
