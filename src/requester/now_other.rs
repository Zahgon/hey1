//! Port of requester/now_other.go  (build tag: `// +build !windows`)

use crate::gotime::GoDuration;
use std::sync::OnceLock;
use std::time::Instant;

/// Go: `var startTime = time.Now()`
///
/// Package-level initialisation in Go runs before `main`; `OnceLock` gives the
/// same "sampled once, at first use" behaviour without a static initialiser.
fn start_time() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Go: `func now() time.Duration { return time.Since(startTime) }`
pub fn now() -> GoDuration {
    GoDuration::from_std(start_time().elapsed())
}

/// Called from `main` so the clock origin matches Go's package-init timing
/// rather than drifting to whenever the first request happens.
pub fn init_clock() {
    let _ = start_time();
}
