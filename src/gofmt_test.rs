//! Tests for the `fmt` shim.
//!
//! The expected strings were produced by Go's `fmt.Sprintf`; a 200k-value
//! differential run showed no disagreement on the two verbs hey uses.

use super::*;

#[test]
fn sprintf_f_matches_go_for_finite_values() {
    assert_eq!(sprintf_f(0.0, 4, 4), "0.0000");
    assert_eq!(sprintf_f(0.075, 4, 4), "0.0750");
    assert_eq!(sprintf_f(1236.3967, 4, 4), "1236.3967");
    assert_eq!(sprintf_f(0.1618, 4, 4), "0.1618");
    // %4.3f is the histogram's Mark verb.
    assert_eq!(sprintf_f(0.0153, 4, 3), "0.015");
    assert_eq!(sprintf_f(0.0004, 4, 3), "0.000");
}

#[test]
fn sprintf_f_pads_non_finite_values_to_width() {
    // Go renders NaN as "NaN" and then right-aligns it in the field width, so
    // "%4.4f" yields a leading space. This is what the report prints when
    // every request failed.
    assert_eq!(sprintf_f(f64::NAN, 4, 4), " NaN");
    assert_eq!(sprintf_f(f64::INFINITY, 4, 4), "+Inf");
    assert_eq!(sprintf_f(f64::NEG_INFINITY, 4, 4), "-Inf");
    // Width 0 means no padding at all.
    assert_eq!(sprintf_f(f64::NAN, 0, 4), "NaN");
}

#[test]
fn sprintf_d_matches_go() {
    assert_eq!(sprintf_d(0), "0");
    assert_eq!(sprintf_d(200), "200");
    assert_eq!(sprintf_d(-7), "-7");
}
