//! Package requester provides commands to run load tests and display results.

pub mod print;
pub mod report;
#[allow(clippy::module_inception)]
pub mod requester;

// Go selects between now_other.go and now_windows.go with build constraints
// (`// +build !windows` and the `_windows` filename suffix respectively).
#[cfg(not(windows))]
pub mod now_other;
#[cfg(windows)]
pub mod now_windows;

#[cfg(not(windows))]
pub use now_other::{init_clock, now};
#[cfg(windows)]
pub use now_windows::{init_clock, now};

pub use report::{Bucket, LatencyDistribution, Report};
pub use requester::{ResultRecord, Work};

#[cfg(test)]
#[path = "requester_test.rs"]
mod requester_test;

#[cfg(test)]
#[path = "print_test.rs"]
mod print_test;

#[cfg(test)]
#[path = "report_test.rs"]
mod report_test;
