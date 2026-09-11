//! Tests for the `time` shim.
//!
//! Every expectation here was captured from the Go binary or from Go's
//! documented semantics, so a regression shows up as a diff against Go rather
//! than against an invented baseline.

use super::*;

#[test]
fn parse_duration_accepts_the_forms_go_accepts() {
    // Values exercised against the real hey binary via `-z`.
    assert_eq!(parse_duration("10s").unwrap(), GoDuration(10 * SECOND));
    assert_eq!(parse_duration("3m").unwrap(), GoDuration(3 * MINUTE));
    assert_eq!(
        parse_duration("1h30m").unwrap(),
        GoDuration(HOUR + 30 * MINUTE)
    );
    assert_eq!(
        parse_duration("1.5s").unwrap(),
        GoDuration(1_500 * MILLISECOND)
    );
    assert_eq!(
        parse_duration("100ms").unwrap(),
        GoDuration(100 * MILLISECOND)
    );
    assert_eq!(
        parse_duration("300us").unwrap(),
        GoDuration(300 * MICROSECOND)
    );
    assert_eq!(
        parse_duration("300\u{00b5}s").unwrap(),
        GoDuration(300 * MICROSECOND)
    );
    assert_eq!(parse_duration("7ns").unwrap(), GoDuration(7));
    // Go keeps "0" as a special case meaning 0s, with no unit required.
    assert_eq!(parse_duration("0").unwrap(), GoDuration(0));
    assert_eq!(parse_duration("-5s").unwrap(), GoDuration(-5 * SECOND));
    assert_eq!(parse_duration("+5s").unwrap(), GoDuration(5 * SECOND));
    assert_eq!(
        parse_duration("2h45m30s").unwrap(),
        GoDuration(2 * HOUR + 45 * MINUTE + 30 * SECOND)
    );
}

#[test]
fn parse_duration_rejects_what_go_rejects() {
    // `hey -z 3` really does fail: a bare number has no unit.
    for bad in ["3", "1x", "abc", "", "10S", "1.5.5s", "s", "."] {
        assert!(
            parse_duration(bad).is_err(),
            "expected {:?} to be rejected",
            bad
        );
    }
}

#[test]
fn duration_seconds_matches_gos_split_arithmetic() {
    assert_eq!(GoDuration(0).seconds(), 0.0);
    assert_eq!(GoDuration(SECOND).seconds(), 1.0);
    assert_eq!(GoDuration(161_800_000).seconds(), 0.1618);
    assert_eq!(GoDuration(1_500 * MILLISECOND).seconds(), 1.5);
    // Go splits whole seconds from the remainder rather than dividing by 1e9,
    // which keeps precision for large values.
    assert_eq!(GoDuration(3 * HOUR + 250 * MILLISECOND).seconds(), 10800.25);
}

#[test]
fn durations_are_signed_like_gos() {
    // std::time::Duration cannot represent this; Go's can, and requester.go
    // relies on it when a trace hook never fired.
    let d = GoDuration(5) - GoDuration(9);
    assert_eq!(d, GoDuration(-4));
    assert!(d.seconds() < 0.0);
    // Converting a negative duration for sleeping must clamp, not panic.
    assert_eq!(d.to_std(), std::time::Duration::ZERO);
}

#[test]
fn duration_display_matches_go() {
    assert_eq!(GoDuration(0).to_string(), "0s");
    assert_eq!(GoDuration(SECOND).to_string(), "1s");
    assert_eq!(GoDuration(90 * SECOND).to_string(), "1m30s");
    assert_eq!(GoDuration(100 * MILLISECOND).to_string(), "100ms");
    assert_eq!(GoDuration(-5 * SECOND).to_string(), "-5s");
    assert_eq!(GoDuration(HOUR + MINUTE + SECOND).to_string(), "1h1m1s");
}
