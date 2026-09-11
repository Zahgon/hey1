//! Rust port of github.com/rakyll/hey.
//!
//! Layout mirrors the Go repository: `requester` is the load-generation
//! package (requester.go / report.go / print.go / now_*.go) and `main.rs` is
//! Go's `package main` from hey.go.
//!
//! Modules prefixed `go*` are shims standing in for Go standard-library
//! packages that have no direct Rust equivalent. They exist to keep the ported
//! code reading like the original and to preserve observable behaviour
//! (formatting, flag syntax, error strings) rather than to be general-purpose
//! libraries.

pub mod goflag;
pub mod gofmt;
pub mod gohttp;
pub mod golog;
pub mod gotemplate;
pub mod gotime;
pub mod gourl;
pub mod requester;
