//! Tests for the `flag` shim.
//!
//! hey's CLI is Go-flag shaped, not GNU shaped. These cases were all checked
//! against the real binary; the error strings are Go's verbatim.

use super::*;

fn fs() -> FlagSet {
    let mut f = FlagSet::new(Box::new(|| {}));
    f.string("m", "GET");
    f.string("h", "");
    f.string("T", "text/html");
    f.int("c", 50);
    f.int("n", 200);
    f.float64("q", 0.0);
    f.duration("z", GoDuration::ZERO);
    f.bool("h2", false);
    f.bool("disable-compression", false);
    f.var_slice("H");
    f
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn defaults_apply_when_nothing_is_passed() {
    let mut f = fs();
    f.try_parse(argv(&["http://x/"])).unwrap();
    assert_eq!(f.get_int("n"), 200);
    assert_eq!(f.get_int("c"), 50);
    assert_eq!(f.get_str("m"), "GET");
    assert_eq!(f.get_str("T"), "text/html");
    assert!(!f.get_bool("h2"));
    assert_eq!(f.narg(), 1);
    assert_eq!(f.args()[0], "http://x/");
}

#[test]
fn accepts_both_separated_and_equals_forms() {
    let mut f = fs();
    f.try_parse(argv(&["-n", "5", "-c=2", "http://x/"]))
        .unwrap();
    assert_eq!(f.get_int("n"), 5);
    assert_eq!(f.get_int("c"), 2);
}

#[test]
fn single_and_double_dash_are_equivalent() {
    // Go treats "-n" and "--n" identically; this is not GNU parsing.
    let mut f = fs();
    f.try_parse(argv(&["--n=5", "http://x/"])).unwrap();
    assert_eq!(f.get_int("n"), 5);
}

#[test]
fn bool_flags_never_consume_the_next_argument() {
    let mut f = fs();
    f.try_parse(argv(&["-h2", "http://x/"])).unwrap();
    assert!(f.get_bool("h2"));
    // The URL must still be a positional argument, not the flag's value.
    assert_eq!(f.args(), &["http://x/".to_string()]);

    let mut f = fs();
    f.try_parse(argv(&["-h2=false", "http://x/"])).unwrap();
    assert!(!f.get_bool("h2"));
}

#[test]
fn repeated_capital_h_accumulates() {
    // flag.Var(&hs, "H", "") -- headerSlice appends rather than replacing.
    let mut f = fs();
    f.try_parse(argv(&["-H", "A: 1", "-H", "B: 2", "http://x/"]))
        .unwrap();
    assert_eq!(
        f.get_slice("H"),
        vec!["A: 1".to_string(), "B: 2".to_string()]
    );
}

#[test]
fn parsing_stops_at_the_first_non_flag_argument() {
    // Anything after the URL is positional, even if it looks like a flag.
    let mut f = fs();
    f.try_parse(argv(&["-n", "5", "http://x/", "-c", "9"]))
        .unwrap();
    assert_eq!(f.get_int("n"), 5);
    assert_eq!(f.get_int("c"), 50, "-c after the URL must not be parsed");
    assert_eq!(f.narg(), 3);
}

#[test]
fn double_dash_terminates_flags() {
    let mut f = fs();
    f.try_parse(argv(&["-n", "5", "--", "-c", "9"])).unwrap();
    assert_eq!(f.get_int("c"), 50);
    assert_eq!(f.args(), &["-c".to_string(), "9".to_string()]);
}

#[test]
fn lowercase_h_is_a_string_flag_not_help() {
    // hey defines -h as a (deprecated) string flag, so it must not trigger
    // help. -help still does.
    let mut f = fs();
    let helped = f.try_parse(argv(&["-h", "foo", "http://x/"])).unwrap();
    assert!(!helped);
    assert_eq!(f.get_str("h"), "foo");

    let mut f = fs();
    assert!(f.try_parse(argv(&["-help"])).unwrap());
}

#[test]
fn error_messages_are_gos_verbatim() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["-n", "abc"],
            r#"invalid value "abc" for flag -n: parse error"#,
        ),
        (
            &["-q", "x"],
            r#"invalid value "x" for flag -q: parse error"#,
        ),
        // Go's durationValue discards ParseDuration's message for errParse.
        (
            &["-z", "3"],
            r#"invalid value "3" for flag -z: parse error"#,
        ),
        (
            &["-h2=maybe"],
            r#"invalid boolean value "maybe" for -h2: parse error"#,
        ),
        (&["-badflag"], "flag provided but not defined: -badflag"),
        (&["-n"], "flag needs an argument: -n"),
    ];
    for (args, want) in cases {
        let mut f = fs();
        let got = f.try_parse(argv(args)).unwrap_err();
        assert_eq!(&got, want, "for args {:?}", args);
    }
}

#[test]
fn ints_use_go_base_zero_parsing() {
    // strconv.ParseInt(s, 0, 64): 0x/0b/0o prefixes and underscores.
    let mut f = fs();
    f.try_parse(argv(&["-n", "0x10", "http://x/"])).unwrap();
    assert_eq!(f.get_int("n"), 16);

    let mut f = fs();
    f.try_parse(argv(&["-n", "-5", "http://x/"])).unwrap();
    assert_eq!(f.get_int("n"), -5);

    let mut f = fs();
    let e = f
        .try_parse(argv(&["-n", "99999999999999999999"]))
        .unwrap_err();
    assert!(e.ends_with("value out of range"), "got {}", e);
}

#[test]
fn bools_accept_go_spellings() {
    for (s, want) in [
        ("1", true),
        ("t", true),
        ("TRUE", true),
        ("True", true),
        ("0", false),
        ("f", false),
        ("FALSE", false),
        ("False", false),
    ] {
        let mut f = fs();
        f.try_parse(argv(&[&format!("-h2={}", s)])).unwrap();
        assert_eq!(f.get_bool("h2"), want, "for -h2={}", s);
    }
}

#[test]
fn durations_round_trip() {
    let mut f = fs();
    f.try_parse(argv(&["-z", "1h30m", "http://x/"])).unwrap();
    assert_eq!(f.get_dur("z"), GoDuration(90 * crate::gotime::MINUTE));
}
