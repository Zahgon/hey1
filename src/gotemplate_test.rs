//! Tests for the `text/template` subset.
//!
//! hey passes `-o` straight to `template.Must(...)`, so these cover the
//! constructs the built-in templates use plus the error text Go produces.

use super::*;
use std::collections::BTreeMap;

fn data() -> Value {
    let mut m = BTreeMap::new();
    m.insert("Num".to_string(), Value::Int(3));
    m.insert("Size".to_string(), Value::Int(0));
    m.insert("Avg".to_string(), Value::Float(0.075));
    m.insert("Name".to_string(), Value::Str("hey".to_string()));
    m.insert(
        "Lats".to_string(),
        Value::Slice(vec![Value::Float(0.001), Value::Float(0.5)]),
    );
    m.insert(
        "Codes".to_string(),
        Value::MapInt(BTreeMap::from([(500, Value::Int(3)), (200, Value::Int(2))])),
    );
    m.insert(
        "Errs".to_string(),
        Value::MapStr(BTreeMap::from([
            ("zeta".to_string(), Value::Int(2)),
            ("alpha".to_string(), Value::Int(7)),
        ])),
    );
    m.insert(
        "Dur".to_string(),
        Value::Duration(crate::gotime::GoDuration(161_800_000)),
    );
    Value::Struct(m)
}

fn render(src: &str) -> String {
    Template::parse(src).unwrap().execute(&data()).unwrap()
}

#[test]
fn renders_text_and_fields() {
    assert_eq!(render("a{{ .Num }}b"), "a3b");
    assert_eq!(render("{{ .Name }}"), "hey");
}

#[test]
fn calls_method_style_accessors_on_durations() {
    // The default template does `{{ formatNumber .Total.Seconds }}`.
    assert_eq!(render("{{ formatNumber .Dur.Seconds }}"), "0.1618");
}

#[test]
fn applies_heys_func_map() {
    assert_eq!(render("{{ formatNumber .Avg }}"), "0.0750");
    assert_eq!(render("{{ formatNumberInt .Num }}"), "3");
    assert_eq!(render("{{ jsonify .Lats }}"), "[0.001,0.5]");
    assert_eq!(render("{{ len .Lats }}"), "2");
    assert_eq!(render("{{ index .Lats 1 }}"), "0.5");
}

#[test]
fn if_else_uses_go_truthiness() {
    assert_eq!(render("{{ if gt .Num 0 }}Y{{ else }}N{{ end }}"), "Y");
    assert_eq!(render("{{ if gt .Size 0 }}Y{{ else }}N{{ end }}"), "N");
    // The zero value of each kind is false.
    assert_eq!(render("{{ if .Size }}Y{{ else }}N{{ end }}"), "N");
    assert_eq!(render("{{ if .Name }}Y{{ else }}N{{ end }}"), "Y");
    assert_eq!(
        render("{{ if gt (len .Errs) 0 }}Y{{ else }}N{{ end }}"),
        "Y"
    );
}

#[test]
fn ranges_maps_in_sorted_key_order() {
    // Go's text/template sorts map keys before ranging; the report's status
    // code and error distributions depend on it.
    assert_eq!(
        render("{{ range $c, $n := .Codes }}{{ $c }}={{ $n }} {{ end }}"),
        "200=2 500=3 "
    );
    assert_eq!(
        render("{{ range $e, $n := .Errs }}{{ $e }}={{ $n }} {{ end }}"),
        "alpha=7 zeta=2 "
    );
}

#[test]
fn ranges_slices_with_index_and_value() {
    assert_eq!(
        render("{{ range $i, $v := .Lats }}{{ $i }}:{{ formatNumber $v }} {{ end }}"),
        "0:0.0010 1:0.5000 "
    );
    // Dot is rebound to the element when no variables are declared.
    assert_eq!(
        render("{{ range .Lats }}[{{ formatNumber . }}]{{ end }}"),
        "[0.0010][0.5000]"
    );
}

#[test]
fn variable_assignment_persists_across_the_list() {
    // csvTmpl hoists .ConnLats & friends into variables up front.
    assert_eq!(
        render("{{ $x := .Lats }}{{ len $x }}|{{ formatNumber (index $x 0) }}"),
        "2|0.0010"
    );
}

#[test]
fn empty_range_emits_nothing() {
    let empty = Value::Struct(BTreeMap::from([
        ("L".to_string(), Value::Slice(vec![])),
        ("M".to_string(), Value::MapInt(BTreeMap::new())),
    ]));
    let t = Template::parse("a{{ range .L }}x{{ end }}b{{ range .M }}y{{ end }}c").unwrap();
    assert_eq!(t.execute(&empty).unwrap(), "abc");
}

#[test]
fn parse_errors_match_go_including_line_numbers() {
    // Verified against `hey -o ...`, which panics with these exact strings.
    let cases: &[(&str, &str)] = &[
        ("{{ .Bad", "1: unclosed action"),
        ("{{ end }}", "1: unexpected {{end}}"),
        (
            "{{ nosuchfunc }}",
            r#"1: function "nosuchfunc" not defined"#,
        ),
        ("{{ if .X }}", "1: unexpected EOF"),
        ("line1\nline2 {{ .Bad", "2: unclosed action"),
    ];
    for (src, want) in cases {
        assert_eq!(&Template::parse(src).unwrap_err(), want, "for {:?}", src);
    }
}

#[test]
fn go_style_value_printing() {
    assert_eq!(Value::Int(42).print(), "42");
    assert_eq!(Value::Str("x".into()).print(), "x");
    assert_eq!(Value::Bool(true).print(), "true");
    assert_eq!(Value::Float(f64::NAN).print(), "NaN");
    assert_eq!(Value::Float(f64::INFINITY).print(), "+Inf");
}

#[test]
fn execute_errors_match_go_including_column_and_type_name() {
    // Captured from `hey -o ...` against the Go binary: same line, column,
    // failing expression and Go type name.
    let cases: &[(&str, &str)] = &[
        (
            "{{ .NoSuchField }}",
            r#"template: tmpl:1:3: executing "tmpl" at <.NoSuchField>: can't evaluate field NoSuchField in type requester.Report"#,
        ),
        (
            "{{ .Dur.Nope }}",
            r#"template: tmpl:1:7: executing "tmpl" at <.Dur.Nope>: can't evaluate field Nope in type time.Duration"#,
        ),
        (
            "{{ index .Lats 99 }}",
            r#"template: tmpl:1:3: executing "tmpl" at <index .Lats 99>: error calling index: index out of range: 99"#,
        ),
        (
            "{{ if .Bad }}y{{ end }}",
            r#"template: tmpl:1:6: executing "tmpl" at <.Bad>: can't evaluate field Bad in type requester.Report"#,
        ),
        (
            "{{ .Lats.Nope }}",
            r#"template: tmpl:1:8: executing "tmpl" at <.Lats.Nope>: can't evaluate field Nope in type []float64"#,
        ),
        (
            "{{ .Codes.Nope }}",
            r#"template: tmpl:1:9: executing "tmpl" at <.Codes.Nope>: can't evaluate field Nope in type map[int]int"#,
        ),
    ];
    for (src, want) in cases {
        let got = Template::parse(src).unwrap().execute(&data()).unwrap_err();
        assert_eq!(&got, want, "for {:?}", src);
    }
}

#[test]
fn range_over_an_integer_matches_go_1_22_semantics() {
    // text/template gained range-over-int in Go 1.22; `{{ range .Num }}`
    // iterates Num times with dot bound to the index.
    assert_eq!(render("{{ range .Num }}x{{ end }}"), "xxx");
    assert_eq!(render("{{ range $i := .Num }}{{ $i }},{{ end }}"), "0,1,2,");
    // A zero count takes the else branch.
    assert_eq!(render("{{ range .Size }}x{{ else }}none{{ end }}"), "none");
}

#[test]
fn undefined_variable_is_reported() {
    let e = Template::parse("{{ $nope }}")
        .unwrap()
        .execute(&data())
        .unwrap_err();
    assert!(e.contains(r#"undefined variable "$nope""#), "got {}", e);
}
