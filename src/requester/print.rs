//! Port of requester/print.go
//!
//! > Hey supports two output formats: summary and CSV
//! >
//! > The summary output presents a number of statistics about the requests in a
//! > human-readable format, including:
//! > - general statistics: requests/second, total runtime, and average, fastest, and slowest requests.
//! > - a response time histogram.
//! > - a percentile latency distribution.
//! > - statistics (average, fastest, slowest) on the stages of the requests.
//! >
//! > The comma-separated CSV format is proceeded by a header, and consists of the following columns:
//! > 1. response-time:  Total time taken for request (in seconds)
//! > 2. DNS+dialup:     Time taken to establish the TCP connection (in seconds)
//! > 3. DNS:            Time taken to do the DNS lookup (in seconds)
//! > 4. Request-write:  Time taken to write full request (in seconds)
//! > 5. Response-delay: Time taken to first byte received (in seconds)
//! > 6. Response-read:  Time taken to read full response (in seconds)
//! > 7. status-code:    HTTP status code of the response (e.g. 200)
//! > 8. offset:         The time since the start of the benchmark when the request was started. (in seconds)
//!
//! The two template literals below are copied byte-for-byte out of print.go,
//! including the `%%` in the latency distribution -- `text/template` does not
//! collapse `%%` the way `fmt` does, so the Go binary really does print
//! "10%% in 0.0011 secs". That is reproduced here rather than fixed, because
//! the goal is behavioural parity with the original.

use crate::gofmt;
use crate::gotemplate::{Template, Value};
use crate::requester::report::BAR_CHAR;

/// Go: `func newTemplate(output string) *template.Template`
pub fn new_template(output: &str) -> Template {
    let output_tmpl = match output {
        "" => DEFAULT_TMPL,
        "csv" => CSV_TMPL,
        other => other,
    };
    Template::must(output_tmpl)
}

/// Go: `func jsonify(v interface{}) string` -- registered in `tmplFuncMap`
/// and dispatched from the template engine's function table.
///
/// Go: `func formatNumber(duration float64) string`
pub fn format_number(duration: f64) -> String {
    gofmt::sprintf_f(duration, 4, 4)
}

/// Go: `func formatNumberInt(duration int) string`
pub fn format_number_int(duration: i64) -> String {
    gofmt::sprintf_d(duration)
}

/// Go: `func histogram(buckets []Bucket) string`
///
/// Bar length uses Go's integer arithmetic -- `(count*40 + max/2) / max` with
/// truncating division -- so the rounding matches exactly.
pub fn histogram(buckets: &Value) -> Result<String, String> {
    let items = match buckets {
        Value::Slice(v) => v.clone(),
        Value::Nil => Vec::new(),
        other => return Err(format!("histogram: expected []Bucket, got {:?}", other)),
    };

    let mut max: i64 = 0;
    for b in &items {
        let v = bucket_count(b)?;
        if v > max {
            max = v;
        }
    }

    let mut res = String::new();
    for b in items.iter() {
        // Normalize bar lengths.
        let count = bucket_count(b)?;
        let mut bar_len: i64 = 0;
        if max > 0 {
            bar_len = (count * 40 + max / 2) / max;
        }
        let mark = bucket_mark(b)?;
        res.push_str("  ");
        res.push_str(&gofmt::sprintf_f(mark, 4, 3));
        res.push_str(" [");
        res.push_str(&count.to_string());
        res.push_str("]\t|");
        for _ in 0..bar_len.max(0) {
            res.push_str(BAR_CHAR);
        }
        res.push('\n');
    }
    Ok(res)
}

fn bucket_count(b: &Value) -> Result<i64, String> {
    match b {
        Value::Struct(m) => match m.get("Count") {
            Some(Value::Int(i)) => Ok(*i),
            _ => Err("histogram: Bucket.Count missing".to_string()),
        },
        _ => Err("histogram: not a Bucket".to_string()),
    }
}

fn bucket_mark(b: &Value) -> Result<f64, String> {
    match b {
        Value::Struct(m) => match m.get("Mark") {
            Some(Value::Float(f)) => Ok(*f),
            Some(Value::Int(i)) => Ok(*i as f64),
            _ => Err("histogram: Bucket.Mark missing".to_string()),
        },
        _ => Err("histogram: not a Bucket".to_string()),
    }
}

pub const DEFAULT_TMPL: &str = r###"
Summary:
  Total:	{{ formatNumber .Total.Seconds }} secs
  Slowest:	{{ formatNumber .Slowest }} secs
  Fastest:	{{ formatNumber .Fastest }} secs
  Average:	{{ formatNumber .Average }} secs
  Requests/sec:	{{ formatNumber .Rps }}
  {{ if gt .SizeTotal 0 }}
  Total data:	{{ .SizeTotal }} bytes
  Size/request:	{{ .SizeReq }} bytes{{ end }}

Response time histogram:
{{ histogram .Histogram }}

Latency distribution:{{ range .LatencyDistribution }}
  {{ .Percentage }}%% in {{ formatNumber .Latency }} secs{{ end }}

Details (average, fastest, slowest):
  DNS+dialup:	{{ formatNumber .AvgConn }} secs, {{ formatNumber .ConnMax }} secs, {{ formatNumber .ConnMin }} secs
  DNS-lookup:	{{ formatNumber .AvgDNS }} secs, {{ formatNumber .DnsMax }} secs, {{ formatNumber .DnsMin }} secs
  req write:	{{ formatNumber .AvgReq }} secs, {{ formatNumber .ReqMax }} secs, {{ formatNumber .ReqMin }} secs
  resp wait:	{{ formatNumber .AvgDelay }} secs, {{ formatNumber .DelayMax }} secs, {{ formatNumber .DelayMin }} secs
  resp read:	{{ formatNumber .AvgRes }} secs, {{ formatNumber .ResMax }} secs, {{ formatNumber .ResMin }} secs

Status code distribution:{{ range $code, $num := .StatusCodeDist }}
  [{{ $code }}]	{{ $num }} responses{{ end }}

{{ if gt (len .ErrorDist) 0 }}Error distribution:{{ range $err, $num := .ErrorDist }}
  [{{ $num }}]	{{ $err }}{{ end }}{{ end }}
"###;

pub const CSV_TMPL: &str = r###"{{ $connLats := .ConnLats }}{{ $dnsLats := .DnsLats }}{{ $dnsLats := .DnsLats }}{{ $reqLats := .ReqLats }}{{ $delayLats := .DelayLats }}{{ $resLats := .ResLats }}{{ $statusCodeLats := .StatusCodes }}{{ $offsets := .Offsets}}response-time,DNS+dialup,DNS,Request-write,Response-delay,Response-read,status-code,offset{{ range $i, $v := .Lats }}
{{ formatNumber $v }},{{ formatNumber (index $connLats $i) }},{{ formatNumber (index $dnsLats $i) }},{{ formatNumber (index $reqLats $i) }},{{ formatNumber (index $delayLats $i) }},{{ formatNumber (index $resLats $i) }},{{ formatNumberInt (index $statusCodeLats $i) }},{{ formatNumber (index $offsets $i) }}{{ end }}"###;
