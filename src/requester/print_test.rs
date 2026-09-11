//! Byte-for-byte parity harness for the report renderer.
//!
//! There is no counterpart to this file in the Go repository. The fixtures in
//! `tests/golden/` were produced by executing the *original Go* templates via
//! `text/template` over hand-built `requester.Report` values, so a passing test
//! here means the Rust template engine, the `%4.4f` formatting, the map key
//! ordering and the NaN/Inf spellings all agree with Go exactly.

use crate::gotime::GoDuration;
use crate::requester::print::new_template;
use crate::requester::report::{Bucket, LatencyDistribution, Report};
use std::collections::BTreeMap;

fn render(output: &str, rep: &Report) -> String {
    // Mirrors report.print(): Execute into a buffer, write it, then a "\n".
    let mut s = new_template(output).execute(&rep.to_value()).unwrap();
    s.push('\n');
    s
}

fn full_fixture() -> Report {
    Report {
        avg_total: 1.5,
        fastest: 0.001,
        slowest: 0.5,
        average: 0.075,
        rps: 1236.3967,
        avg_conn: 0.0089,
        avg_dns: 0.00001,
        avg_req: 0.00002,
        avg_res: 0.0002,
        avg_delay: 0.0013,
        conn_max: 0.0001,
        conn_min: 0.147,
        dns_max: 0.0,
        dns_min: 0.0,
        req_max: 0.00001,
        req_min: 0.0004,
        res_max: 0.00002,
        res_min: 0.001,
        delay_max: 0.0002,
        delay_min: 0.003,
        lats: vec![0.001, 0.25, 0.5],
        conn_lats: vec![0.0001, 0.02, 0.147],
        dns_lats: vec![0.0, 0.0, 0.0],
        req_lats: vec![0.00001, 0.0002, 0.0004],
        res_lats: vec![0.00002, 0.0005, 0.001],
        delay_lats: vec![0.0002, 0.001, 0.003],
        offsets: vec![0.0001, 0.1, 0.2],
        status_codes: vec![200, 404, 200],
        total: GoDuration::from_nanos(161_800_000),
        error_dist: BTreeMap::from([
            ("zeta error".to_string(), 2),
            ("alpha error".to_string(), 7),
        ]),
        status_code_dist: BTreeMap::from([(200, 2), (404, 1), (500, 3)]),
        size_total: 4600,
        size_req: 23,
        num_res: 3,
        latency_distribution: vec![
            LatencyDistribution {
                percentage: 10,
                latency: 0.0011,
            },
            LatencyDistribution {
                percentage: 25,
                latency: 0.0014,
            },
            LatencyDistribution {
                percentage: 50,
                latency: 0.0017,
            },
            LatencyDistribution {
                percentage: 75,
                latency: 0.0022,
            },
            LatencyDistribution {
                percentage: 90,
                latency: 0.003,
            },
            LatencyDistribution {
                percentage: 0,
                latency: 0.0,
            },
            LatencyDistribution {
                percentage: 0,
                latency: 0.0,
            },
        ],
        histogram: vec![
            Bucket {
                mark: 0.0004,
                count: 1,
                frequency: 0.005,
            },
            Bucket {
                mark: 0.0153,
                count: 184,
                frequency: 0.92,
            },
            Bucket {
                mark: 0.0302,
                count: 0,
                frequency: 0.0,
            },
        ],
    }
}

fn assert_golden(name: &str, want: &str, got: &str) {
    if want != got {
        panic!(
            "golden mismatch for {}\n--- want ({} bytes) ---\n{:?}\n--- got ({} bytes) ---\n{:?}",
            name,
            want.len(),
            want,
            got.len(),
            got
        );
    }
}

#[test]
fn golden_default_template_full() {
    let want = include_str!("../../tests/golden/full.txt");
    assert_golden("full", want, &render("", &full_fixture()));
}

#[test]
fn golden_default_template_without_size() {
    // Exercises the `{{ if gt .SizeTotal 0 }}` branch and the empty ErrorDist
    // branch, both of which change the trailing blank lines.
    let mut rep = full_fixture();
    rep.size_total = 0;
    rep.size_req = 0;
    rep.error_dist = BTreeMap::new();
    let want = include_str!("../../tests/golden/nosize.txt");
    assert_golden("nosize", want, &render("", &rep));
}

#[test]
fn golden_default_template_empty() {
    // Every request failed: Go divides by zero and renders " NaN" / "+Inf".
    let rep = Report {
        avg_total: 0.0,
        fastest: 0.0,
        slowest: 0.0,
        average: f64::NAN,
        rps: f64::INFINITY,
        avg_conn: 0.0,
        avg_dns: 0.0,
        avg_req: 0.0,
        avg_res: 0.0,
        avg_delay: 0.0,
        conn_max: 0.0,
        conn_min: 0.0,
        dns_max: 0.0,
        dns_min: 0.0,
        req_max: 0.0,
        req_min: 0.0,
        res_max: 0.0,
        res_min: 0.0,
        delay_max: 0.0,
        delay_min: 0.0,
        lats: vec![],
        conn_lats: vec![],
        dns_lats: vec![],
        req_lats: vec![],
        res_lats: vec![],
        delay_lats: vec![],
        offsets: vec![],
        status_codes: vec![],
        total: GoDuration::ZERO,
        error_dist: BTreeMap::new(),
        status_code_dist: BTreeMap::new(),
        size_total: 0,
        size_req: 0,
        num_res: 0,
        latency_distribution: vec![],
        histogram: vec![],
    };
    let want = include_str!("../../tests/golden/empty.txt");
    assert_golden("empty", want, &render("", &rep));
}

#[test]
fn golden_csv_template() {
    let want = include_str!("../../tests/golden/csv.txt");
    assert_golden("csv", want, &render("csv", &full_fixture()));
}

#[test]
fn custom_template_is_honoured() {
    // `-o` is passed straight to text/template in Go, so an arbitrary template
    // must render rather than being rejected.
    let rep = full_fixture();
    let got = new_template("{{ .NumRes }} in {{ formatNumber .Average }}")
        .execute(&rep.to_value())
        .unwrap();
    assert_eq!(got, "3 in 0.0750");
}

#[test]
fn histogram_rejects_values_that_are_not_buckets() {
    use crate::gotemplate::Value;
    use crate::requester::print::histogram;
    // Defensive paths that stand in for Go's static typing on []Bucket.
    assert!(histogram(&Value::Int(1)).is_err());
    assert!(histogram(&Value::Slice(vec![Value::Int(1)])).is_err());
    assert!(histogram(&Value::Slice(vec![Value::Struct(Default::default())])).is_err());
    // Nil and empty render as the empty string, as Go's does for a nil slice.
    assert_eq!(histogram(&Value::Nil).unwrap(), "");
    assert_eq!(histogram(&Value::Slice(vec![])).unwrap(), "");
}

#[test]
fn histogram_bar_lengths_use_gos_integer_rounding() {
    use crate::gotemplate::Value;
    use crate::requester::print::histogram;
    use std::collections::BTreeMap;
    let bucket = |mark: f64, count: i64| {
        let mut m = BTreeMap::new();
        m.insert("Mark".to_string(), Value::Float(mark));
        m.insert("Count".to_string(), Value::Int(count));
        m.insert("Frequency".to_string(), Value::Float(0.0));
        Value::Struct(m)
    };
    // (count*40 + max/2) / max with truncating division: (40 + 1) / 3 = 13.
    let out = histogram(&Value::Slice(vec![bucket(0.5, 1), bucket(1.5, 3)])).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], format!("  0.500 [1]\t|{}", "\u{25a0}".repeat(13)));
    assert_eq!(lines[1], format!("  1.500 [3]\t|{}", "\u{25a0}".repeat(40)));
}
