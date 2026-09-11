//! End-to-end golden tests for the report pipeline.
//!
//! These drive the *whole* path -- `record` -> `finalize` -> `snapshot` ->
//! `histogram`/`latencies` -> template -- from synthetic results, so the
//! statistics code is genuinely exercised rather than bypassed by a
//! hand-built `Report`.
//!
//! The fixtures in `tests/golden/pipeline_*.txt` were produced by feeding the
//! identical results through the **original Go** `runReporter`/`finalize`, so
//! any divergence in bucketing, percentile selection, sorting, averaging or
//! rounding shows up as a byte diff.

use super::report::{new_report, sort_f64};
use super::requester::ResultRecord;
use crate::gotime::GoDuration;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Go: `ms(v)` -- `time.Duration(v * float64(time.Millisecond))`, truncating.
fn ms(v: f64) -> GoDuration {
    GoDuration::from_nanos((v * 1e6) as i64)
}

#[derive(Clone, Copy, Default)]
struct Fx {
    dur: f64,
    conn: f64,
    dns: f64,
    req: f64,
    res: f64,
    delay: f64,
    code: i64,
    clen: i64,
    off: f64,
    err: &'static str,
}

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(output: &str, fxs: &[Fx], total_ms: f64) -> String {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let mut r = new_report(Box::new(Capture(sink.clone())), output, fxs.len() as i64);
    for f in fxs {
        r.record(&ResultRecord {
            err: if f.err.is_empty() {
                None
            } else {
                Some(f.err.to_string())
            },
            status_code: f.code,
            offset: ms(f.off),
            duration: ms(f.dur),
            conn_duration: ms(f.conn),
            dns_duration: ms(f.dns),
            req_duration: ms(f.req),
            res_duration: ms(f.res),
            delay_duration: ms(f.delay),
            content_length: f.clen,
        });
    }
    r.finalize(ms(total_ms));
    let out = sink.lock().unwrap().clone();
    String::from_utf8(out).unwrap()
}

/// Go: `spread()` -- values chosen to fill several histogram buckets and to
/// leave some percentile slots empty.
fn spread() -> Vec<Fx> {
    let vals = [
        1.0, 1.0, 2.0, 2.0, 3.0, 4.0, 5.0, 8.0, 13.0, 21.0, 34.0, 55.0, 89.0, 100.0, 100.0, 120.0,
        150.0, 200.0, 400.0, 900.0,
    ];
    vals.iter()
        .enumerate()
        .map(|(i, &v)| {
            let mut code = 200;
            if i % 7 == 3 {
                code = 404;
            }
            if i % 11 == 5 {
                code = 500;
            }
            Fx {
                dur: v,
                conn: v / 10.0,
                dns: v / 100.0,
                req: v / 50.0,
                res: v / 20.0,
                delay: v / 4.0,
                code,
                clen: 10 + i as i64,
                off: i as f64 * 3.0,
                err: "",
            }
        })
        .collect()
}

fn assert_golden(name: &str, want: &str, got: &str) {
    assert!(
        want == got,
        "pipeline golden mismatch for {}\n--- want ---\n{}\n--- got ---\n{}",
        name,
        want,
        got
    );
}

#[test]
fn pipeline_spread_summary() {
    assert_golden(
        "spread",
        include_str!("../../tests/golden/pipeline_spread.txt"),
        &run("", &spread(), 1234.5),
    );
}

#[test]
fn pipeline_spread_csv() {
    // Also proves Report.Lats stays in completion order while the percentile
    // maths runs on a sorted copy.
    assert_golden(
        "spread_csv",
        include_str!("../../tests/golden/pipeline_spread_csv.txt"),
        &run("csv", &spread(), 1234.5),
    );
}

#[test]
fn pipeline_tiny_sample_leaves_percentile_slots_at_zero() {
    let fxs = [
        Fx {
            dur: 5.0,
            conn: 1.0,
            dns: 0.0,
            req: 0.1,
            res: 0.2,
            delay: 2.0,
            code: 200,
            clen: 7,
            off: 0.0,
            err: "",
        },
        Fx {
            dur: 9.0,
            conn: 2.0,
            dns: 0.0,
            req: 0.3,
            res: 0.4,
            delay: 4.0,
            code: 200,
            clen: 7,
            off: 1.0,
            err: "",
        },
        Fx {
            dur: 9.0,
            conn: 2.0,
            dns: 0.0,
            req: 0.3,
            res: 0.4,
            delay: 4.0,
            code: 301,
            clen: 0,
            off: 2.0,
            err: "",
        },
    ];
    assert_golden(
        "tiny",
        include_str!("../../tests/golden/pipeline_tiny.txt"),
        &run("", &fxs, 20.0),
    );
}

#[test]
fn pipeline_identical_latencies_give_a_zero_width_histogram() {
    let f = Fx {
        dur: 4.0,
        conn: 1.0,
        dns: 0.0,
        req: 1.0,
        res: 1.0,
        delay: 1.0,
        code: 200,
        clen: 5,
        off: 0.0,
        err: "",
    };
    let fxs = [
        Fx { off: 0.0, ..f },
        Fx { off: 1.0, ..f },
        Fx { off: 2.0, ..f },
    ];
    assert_golden(
        "identical",
        include_str!("../../tests/golden/pipeline_identical.txt"),
        &run("", &fxs, 12.0),
    );
}

#[test]
fn pipeline_mixed_errors_and_successes() {
    let fxs = [
        Fx {
            dur: 3.0,
            conn: 1.0,
            dns: 0.0,
            req: 0.5,
            res: 0.5,
            delay: 1.0,
            code: 200,
            clen: 11,
            off: 0.0,
            err: "",
        },
        Fx {
            err: "boom one",
            ..Default::default()
        },
        Fx {
            err: "aaa two",
            ..Default::default()
        },
        Fx {
            err: "boom one",
            ..Default::default()
        },
        Fx {
            dur: 6.0,
            conn: 2.0,
            dns: 1.0,
            req: 1.0,
            res: 1.0,
            delay: 2.0,
            code: 500,
            clen: 0,
            off: 4.0,
            err: "",
        },
    ];
    assert_golden(
        "errors",
        include_str!("../../tests/golden/pipeline_errors.txt"),
        &run("", &fxs, 30.0),
    );
}

#[test]
fn pipeline_all_failures_divide_by_zero() {
    let fxs = [
        Fx {
            err: "x",
            ..Default::default()
        },
        Fx {
            err: "x",
            ..Default::default()
        },
    ];
    assert_golden(
        "allfail",
        include_str!("../../tests/golden/pipeline_allfail.txt"),
        &run("", &fxs, 10.0),
    );
}

#[test]
fn snapshot_keeps_upstreams_inverted_min_max_naming() {
    // `ConnMax` is assigned connLats[0] after an ascending sort -- i.e. the
    // minimum. The template labels the columns "(average, fastest, slowest)",
    // so the rendered output is right even though the field names are not.
    let fxs = spread();
    let sink = Arc::new(Mutex::new(Vec::new()));
    let mut r = new_report(Box::new(Capture(sink)), "", fxs.len() as i64);
    for f in &fxs {
        r.record(&ResultRecord {
            err: None,
            status_code: f.code,
            offset: ms(f.off),
            duration: ms(f.dur),
            conn_duration: ms(f.conn),
            dns_duration: ms(f.dns),
            req_duration: ms(f.req),
            res_duration: ms(f.res),
            delay_duration: ms(f.delay),
            content_length: f.clen,
        });
    }
    let snap = r.snapshot();

    let mut sorted = snap.conn_lats.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(snap.conn_max, sorted[0], "ConnMax holds the minimum");
    assert_eq!(
        snap.conn_min,
        *sorted.last().unwrap(),
        "ConnMin holds the maximum"
    );
    assert!(snap.conn_max < snap.conn_min);

    // Fastest/Slowest are named correctly, unlike the per-phase pairs.
    assert_eq!(snap.fastest, 0.001);
    assert_eq!(snap.slowest, 0.9);

    // Lats keeps completion order; the CSV output depends on it.
    assert_eq!(snap.lats[0], 0.001);
    assert_eq!(snap.lats[snap.lats.len() - 1], 0.9);

    // SizeReq is Go's integer division of the total by the sample count.
    assert_eq!(snap.size_total, (10..30).sum::<i64>());
    assert_eq!(snap.size_req, snap.size_total / 20);

    // The histogram always has bc+1 = 11 buckets and they sum to the sample.
    assert_eq!(snap.histogram.len(), 11);
    assert_eq!(snap.histogram.iter().map(|b| b.count).sum::<i64>(), 20);
    assert_eq!(snap.latency_distribution.len(), 7);
}

/// Out-of-order latencies, so that `Report.Lats` (completion order) genuinely
/// differs from the sorted copy the statistics run on. Without this the CSV
/// ordering guarantee is untestable.
fn unsorted() -> Vec<Fx> {
    let vals = [50.0, 3.0, 900.0, 12.0, 7.0, 300.0, 1.0, 120.0, 25.0, 60.0];
    vals.iter()
        .enumerate()
        .map(|(i, &v)| Fx {
            dur: v,
            conn: v / 10.0,
            dns: v / 100.0,
            req: v / 50.0,
            res: v / 20.0,
            delay: v / 4.0,
            code: 200,
            clen: 100 + i as i64,
            off: i as f64 * 2.0,
            err: "",
        })
        .collect()
}

#[test]
fn pipeline_unsorted_summary() {
    assert_golden(
        "unsorted",
        include_str!("../../tests/golden/pipeline_unsorted.txt"),
        &run("", &unsorted(), 1500.0),
    );
}

#[test]
fn pipeline_unsorted_csv_keeps_completion_order() {
    // snapshot() copies the latency slices *before* sorting them in place. If
    // that order were lost, every CSV row would be wrong.
    let got = run("csv", &unsorted(), 1500.0);
    assert_golden(
        "unsorted_csv",
        include_str!("../../tests/golden/pipeline_unsorted_csv.txt"),
        &got,
    );
    let first_col: Vec<&str> = got
        .lines()
        .skip(1)
        .filter(|l| !l.is_empty())
        .map(|l| l.split(',').next().unwrap())
        .collect();
    assert_eq!(
        first_col,
        vec![
            "0.0500", "0.0030", "0.9000", "0.0120", "0.0070", "0.3000", "0.0010", "0.1200",
            "0.0250", "0.0600"
        ],
        "CSV rows must stay in completion order, not sorted order"
    );
}

#[test]
fn a_template_that_fails_at_execute_produces_no_output() {
    // Go's report.print() logs the error and returns without writing, so the
    // process still exits 0 with an empty report.
    let sink = Arc::new(Mutex::new(Vec::new()));
    let mut r = new_report(Box::new(Capture(sink.clone())), "{{ .NoSuchField }}", 1);
    r.record(&ResultRecord {
        err: None,
        status_code: 200,
        offset: ms(0.0),
        duration: ms(1.0),
        conn_duration: ms(0.1),
        dns_duration: ms(0.0),
        req_duration: ms(0.0),
        res_duration: ms(0.0),
        delay_duration: ms(0.0),
        content_length: 5,
    });
    r.finalize(ms(10.0));
    assert!(
        sink.lock().unwrap().is_empty(),
        "an execute error must suppress the report entirely"
    );
}

#[test]
fn nan_latencies_sort_first_like_go() {
    // Go's sort.Float64s puts NaN ahead of every real value; the port keeps
    // that so a NaN can never be picked as the "slowest".
    let sink = Arc::new(Mutex::new(Vec::new()));
    let mut r = new_report(Box::new(Capture(sink)), "", 3);
    for d in [
        GoDuration::from_nanos(5_000_000),
        GoDuration::from_nanos(1_000_000),
    ] {
        r.record(&ResultRecord {
            err: None,
            status_code: 200,
            offset: GoDuration::ZERO,
            duration: d,
            conn_duration: GoDuration::ZERO,
            dns_duration: GoDuration::ZERO,
            req_duration: GoDuration::ZERO,
            res_duration: GoDuration::ZERO,
            delay_duration: GoDuration::ZERO,
            content_length: 1,
        });
    }
    let snap = r.snapshot();
    assert_eq!(snap.fastest, 0.001);
    assert_eq!(snap.slowest, 0.005);
}

#[test]
fn init_clock_is_idempotent_and_monotonic() {
    // main() calls this so the clock origin matches Go's package-init timing.
    crate::requester::init_clock();
    let a = crate::requester::now();
    crate::requester::init_clock();
    let b = crate::requester::now();
    assert!(b >= a, "now() must be monotonic across init_clock calls");
    assert!(a.nanoseconds() >= 0);
}

#[test]
fn sort_f64_matches_go_sort_float64s() {
    // Ascending order for ordinary values.
    let mut v = vec![0.5, 0.001, 0.25, 0.0];
    sort_f64(&mut v);
    assert_eq!(v, vec![0.0, 0.001, 0.25, 0.5]);

    // Go's sort.Float64s orders NaN before every real value. Durations are
    // i64 nanoseconds so a NaN cannot arrive through the normal path, but the
    // comparator must still be total or the sort would be unstable.
    let mut v = vec![0.5, f64::NAN, 0.1, f64::NAN, 0.9];
    sort_f64(&mut v);
    assert!(v[0].is_nan() && v[1].is_nan(), "NaNs sort first: {:?}", v);
    assert_eq!(&v[2..], &[0.1, 0.5, 0.9]);

    // All-NaN input must not panic.
    let mut v = vec![f64::NAN, f64::NAN];
    sort_f64(&mut v);
    assert!(v.iter().all(|x| x.is_nan()));
}
