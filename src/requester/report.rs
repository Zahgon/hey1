//! Port of requester/report.go

use crate::golog;
use crate::gotemplate::Value;
use crate::gotime::GoDuration;
use crate::requester::print::new_template;
use crate::requester::requester::ResultRecord;
use std::collections::BTreeMap;
use std::io::Write;

/// Go: `const barChar = "■"`
pub const BAR_CHAR: &str = "\u{25a0}";

/// Go: `const maxRes = 1000000` -- "We report for max 1M results."
pub const MAX_RES: usize = 1_000_000;

/// Go: `type report struct` (unexported accumulator).
pub struct Reporter {
    avg_total: f64,
    fastest: f64,
    slowest: f64,
    average: f64,
    rps: f64,

    avg_conn: f64,
    avg_dns: f64,
    avg_req: f64,
    avg_res: f64,
    avg_delay: f64,
    conn_lats: Vec<f64>,
    dns_lats: Vec<f64>,
    req_lats: Vec<f64>,
    res_lats: Vec<f64>,
    delay_lats: Vec<f64>,
    offsets: Vec<f64>,
    status_codes: Vec<i64>,

    total: GoDuration,

    error_dist: BTreeMap<String, i64>,
    lats: Vec<f64>,
    size_total: i64,
    num_res: i64,
    output: String,

    w: Box<dyn Write + Send>,
}

/// Go: `func newReport(w io.Writer, results chan *result, output string, n int) *report`
///
/// The `results` channel is passed separately in the Rust port (it is owned by
/// the reporter task), everything else matches field for field.
pub fn new_report(w: Box<dyn Write + Send>, output: &str, n: i64) -> Reporter {
    let cap = std::cmp::min(n.max(0) as usize, MAX_RES);
    Reporter {
        avg_total: 0.0,
        fastest: 0.0,
        slowest: 0.0,
        average: 0.0,
        rps: 0.0,
        avg_conn: 0.0,
        avg_dns: 0.0,
        avg_req: 0.0,
        avg_res: 0.0,
        avg_delay: 0.0,
        conn_lats: Vec::with_capacity(cap),
        dns_lats: Vec::with_capacity(cap),
        req_lats: Vec::with_capacity(cap),
        res_lats: Vec::with_capacity(cap),
        delay_lats: Vec::with_capacity(cap),
        offsets: Vec::new(),
        status_codes: Vec::with_capacity(cap),
        total: GoDuration::ZERO,
        error_dist: BTreeMap::new(),
        lats: Vec::with_capacity(cap),
        size_total: 0,
        num_res: 0,
        output: output.to_string(),
        w,
    }
}

impl Reporter {
    /// Go: the body of `runReporter`'s loop, for one received result.
    pub fn record(&mut self, res: &ResultRecord) {
        self.num_res += 1;
        if let Some(err) = &res.err {
            *self.error_dist.entry(err.clone()).or_insert(0) += 1;
        } else {
            self.avg_total += res.duration.seconds();
            self.avg_conn += res.conn_duration.seconds();
            self.avg_delay += res.delay_duration.seconds();
            self.avg_dns += res.dns_duration.seconds();
            self.avg_req += res.req_duration.seconds();
            self.avg_res += res.res_duration.seconds();
            if self.res_lats.len() < MAX_RES {
                self.lats.push(res.duration.seconds());
                self.conn_lats.push(res.conn_duration.seconds());
                self.dns_lats.push(res.dns_duration.seconds());
                self.req_lats.push(res.req_duration.seconds());
                self.delay_lats.push(res.delay_duration.seconds());
                self.res_lats.push(res.res_duration.seconds());
                self.status_codes.push(res.status_code);
                self.offsets.push(res.offset.seconds());
            }
            if res.content_length > 0 {
                self.size_total += res.content_length;
            }
        }
    }

    /// Go: `func (r *report) finalize(total time.Duration)`
    ///
    /// Note the divisions by `len(r.lats)`: when every request failed this is
    /// a divide-by-zero producing NaN, which Go prints as " NaN". Preserved.
    pub fn finalize(&mut self, total: GoDuration) {
        self.total = total;
        self.rps = self.num_res as f64 / self.total.seconds();
        let n = self.lats.len() as f64;
        self.average = self.avg_total / n;
        self.avg_conn /= n;
        self.avg_delay /= n;
        self.avg_dns /= n;
        self.avg_req /= n;
        self.avg_res /= n;
        self.print();
    }

    /// Go: `func (r *report) print()`
    fn print(&mut self) {
        let snapshot = self.snapshot();
        let buf = match new_template(&self.output).execute(&snapshot.to_value()) {
            Ok(s) => s,
            Err(e) => {
                golog::println_err(&format!("error: {}", e));
                return;
            }
        };
        self.printf(&buf);
        self.printf("\n");
    }

    /// Go: `func (r *report) printf(s string, v ...interface{})`
    fn printf(&mut self, s: &str) {
        let _ = self.w.write_all(s.as_bytes());
        let _ = self.w.flush();
    }

    /// Go: `func (r *report) snapshot() Report`
    ///
    /// Careful: Go copies the latency slices into the snapshot *before*
    /// sorting them in place, so `Report.Lats` (used by csvTmpl) stays in
    /// completion order while the percentile/histogram maths runs on sorted
    /// data. That ordering is load-bearing and is preserved here.
    pub fn snapshot(&mut self) -> Report {
        let mut snapshot = Report {
            avg_total: self.avg_total,
            fastest: 0.0,
            slowest: 0.0,
            average: self.average,
            rps: self.rps,
            avg_conn: self.avg_conn,
            avg_dns: self.avg_dns,
            avg_req: self.avg_req,
            avg_res: self.avg_res,
            avg_delay: self.avg_delay,
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
            lats: Vec::new(),
            conn_lats: Vec::new(),
            dns_lats: Vec::new(),
            req_lats: Vec::new(),
            res_lats: Vec::new(),
            delay_lats: Vec::new(),
            offsets: Vec::new(),
            status_codes: Vec::new(),
            total: self.total,
            error_dist: self.error_dist.clone(),
            status_code_dist: BTreeMap::new(),
            size_total: self.size_total,
            size_req: 0,
            num_res: self.num_res,
            latency_distribution: Vec::new(),
            histogram: Vec::new(),
        };

        if self.lats.is_empty() {
            return snapshot;
        }

        snapshot.size_req = self.size_total / self.lats.len() as i64;

        snapshot.lats = self.lats.clone();
        snapshot.conn_lats = self.conn_lats.clone();
        snapshot.dns_lats = self.dns_lats.clone();
        snapshot.req_lats = self.req_lats.clone();
        snapshot.res_lats = self.res_lats.clone();
        snapshot.delay_lats = self.delay_lats.clone();
        snapshot.status_codes = self.status_codes.clone();
        snapshot.offsets = self.offsets.clone();

        sort_f64(&mut self.lats);
        self.fastest = self.lats[0];
        self.slowest = self.lats[self.lats.len() - 1];

        sort_f64(&mut self.conn_lats);
        sort_f64(&mut self.dns_lats);
        sort_f64(&mut self.req_lats);
        sort_f64(&mut self.res_lats);
        sort_f64(&mut self.delay_lats);

        snapshot.histogram = self.histogram();
        snapshot.latency_distribution = self.latencies();

        snapshot.fastest = self.fastest;
        snapshot.slowest = self.slowest;
        // Upstream names these backwards -- `ConnMax` is assigned the *first*
        // element of the ascending sort (i.e. the minimum) and `ConnMin` the
        // last. The template prints them under "(average, fastest, slowest)",
        // so the rendered output is correct even though the field names are
        // inverted. Kept as-is for byte parity.
        snapshot.conn_max = self.conn_lats[0];
        snapshot.conn_min = self.conn_lats[self.conn_lats.len() - 1];
        snapshot.dns_max = self.dns_lats[0];
        snapshot.dns_min = self.dns_lats[self.dns_lats.len() - 1];
        snapshot.req_max = self.req_lats[0];
        snapshot.req_min = self.req_lats[self.req_lats.len() - 1];
        snapshot.delay_max = self.delay_lats[0];
        snapshot.delay_min = self.delay_lats[self.delay_lats.len() - 1];
        snapshot.res_max = self.res_lats[0];
        snapshot.res_min = self.res_lats[self.res_lats.len() - 1];

        let mut status_code_dist: BTreeMap<i64, i64> = BTreeMap::new();
        for code in &snapshot.status_codes {
            *status_code_dist.entry(*code).or_insert(0) += 1;
        }
        snapshot.status_code_dist = status_code_dist;

        snapshot
    }

    /// Go: `func (r *report) latencies() []LatencyDistribution`
    ///
    /// Percentile buckets that never got filled stay at the zero value, which
    /// is why the real hey prints trailing "0%% in 0.0000 secs" lines for
    /// small samples. Reproduced.
    fn latencies(&self) -> Vec<LatencyDistribution> {
        let pctls: [i64; 7] = [10, 25, 50, 75, 90, 95, 99];
        let mut data = vec![0.0f64; pctls.len()];
        let mut j = 0usize;
        let mut i = 0usize;
        while i < self.lats.len() && j < pctls.len() {
            let current = (i as i64) * 100 / self.lats.len() as i64;
            if current >= pctls[j] {
                data[j] = self.lats[i];
                j += 1;
            }
            i += 1;
        }
        let mut res = vec![LatencyDistribution::default(); pctls.len()];
        for i in 0..pctls.len() {
            if data[i] > 0.0 {
                res[i] = LatencyDistribution {
                    percentage: pctls[i],
                    latency: data[i],
                };
            }
        }
        res
    }

    /// Go: `func (r *report) histogram() []Bucket`
    ///
    /// Indexed loops are kept throughout to mirror Go's, which walks
    /// `buckets` and `counts` in lockstep with a manually advanced index.
    #[allow(clippy::needless_range_loop)]
    fn histogram(&self) -> Vec<Bucket> {
        let bc = 10usize;
        let mut buckets = vec![0.0f64; bc + 1];
        let mut counts = vec![0i64; bc + 1];
        let bs = (self.slowest - self.fastest) / bc as f64;
        for i in 0..bc {
            buckets[i] = self.fastest + bs * i as f64;
        }
        buckets[bc] = self.slowest;
        let mut bi = 0usize;
        let mut max = 0i64;
        let mut i = 0usize;
        while i < self.lats.len() {
            if self.lats[i] <= buckets[bi] {
                i += 1;
                counts[bi] += 1;
                if max < counts[bi] {
                    max = counts[bi];
                }
            } else if bi < buckets.len() - 1 {
                bi += 1;
            }
        }
        let mut res = Vec::with_capacity(buckets.len());
        for i in 0..buckets.len() {
            res.push(Bucket {
                mark: buckets[i],
                count: counts[i],
                frequency: counts[i] as f64 / self.lats.len() as f64,
            });
        }
        res
    }
}

/// Go: `sort.Float64s` -- ascending, NaN sorts before everything.
///
/// Durations are i64 nanoseconds so a NaN cannot reach here through the
/// normal path; the ordering is still defined (and tested) so the function
/// matches `sort.Float64s` exactly.
pub(crate) fn sort_f64(v: &mut [f64]) {
    v.sort_by(|a, b| {
        a.partial_cmp(b).unwrap_or_else(|| {
            if a.is_nan() && !b.is_nan() {
                std::cmp::Ordering::Less
            } else if !a.is_nan() && b.is_nan() {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
    });
}

/// Go: `type Report struct` (exported snapshot handed to the template).
#[derive(Clone, Debug)]
pub struct Report {
    pub avg_total: f64,
    pub fastest: f64,
    pub slowest: f64,
    pub average: f64,
    pub rps: f64,

    pub avg_conn: f64,
    pub avg_dns: f64,
    pub avg_req: f64,
    pub avg_res: f64,
    pub avg_delay: f64,
    pub conn_max: f64,
    pub conn_min: f64,
    pub dns_max: f64,
    pub dns_min: f64,
    pub req_max: f64,
    pub req_min: f64,
    pub res_max: f64,
    pub res_min: f64,
    pub delay_max: f64,
    pub delay_min: f64,

    pub lats: Vec<f64>,
    pub conn_lats: Vec<f64>,
    pub dns_lats: Vec<f64>,
    pub req_lats: Vec<f64>,
    pub res_lats: Vec<f64>,
    pub delay_lats: Vec<f64>,
    pub offsets: Vec<f64>,
    pub status_codes: Vec<i64>,

    pub total: GoDuration,

    pub error_dist: BTreeMap<String, i64>,
    pub status_code_dist: BTreeMap<i64, i64>,
    pub size_total: i64,
    pub size_req: i64,
    pub num_res: i64,

    pub latency_distribution: Vec<LatencyDistribution>,
    pub histogram: Vec<Bucket>,
}

/// Go: `type LatencyDistribution struct`
#[derive(Clone, Copy, Debug, Default)]
pub struct LatencyDistribution {
    pub percentage: i64,
    pub latency: f64,
}

/// Go: `type Bucket struct`
#[derive(Clone, Copy, Debug, Default)]
pub struct Bucket {
    pub mark: f64,
    pub count: i64,
    pub frequency: f64,
}

fn f64_slice(v: &[f64]) -> Value {
    Value::Slice(v.iter().map(|x| Value::Float(*x)).collect())
}

impl Report {
    /// Expose the snapshot to the template engine under Go's exported field
    /// names, which is what the template source refers to.
    pub fn to_value(&self) -> Value {
        let mut m: BTreeMap<String, Value> = BTreeMap::new();
        m.insert("AvgTotal".into(), Value::Float(self.avg_total));
        m.insert("Fastest".into(), Value::Float(self.fastest));
        m.insert("Slowest".into(), Value::Float(self.slowest));
        m.insert("Average".into(), Value::Float(self.average));
        m.insert("Rps".into(), Value::Float(self.rps));
        m.insert("AvgConn".into(), Value::Float(self.avg_conn));
        m.insert("AvgDNS".into(), Value::Float(self.avg_dns));
        m.insert("AvgReq".into(), Value::Float(self.avg_req));
        m.insert("AvgRes".into(), Value::Float(self.avg_res));
        m.insert("AvgDelay".into(), Value::Float(self.avg_delay));
        m.insert("ConnMax".into(), Value::Float(self.conn_max));
        m.insert("ConnMin".into(), Value::Float(self.conn_min));
        m.insert("DnsMax".into(), Value::Float(self.dns_max));
        m.insert("DnsMin".into(), Value::Float(self.dns_min));
        m.insert("ReqMax".into(), Value::Float(self.req_max));
        m.insert("ReqMin".into(), Value::Float(self.req_min));
        m.insert("ResMax".into(), Value::Float(self.res_max));
        m.insert("ResMin".into(), Value::Float(self.res_min));
        m.insert("DelayMax".into(), Value::Float(self.delay_max));
        m.insert("DelayMin".into(), Value::Float(self.delay_min));
        m.insert("Lats".into(), f64_slice(&self.lats));
        m.insert("ConnLats".into(), f64_slice(&self.conn_lats));
        m.insert("DnsLats".into(), f64_slice(&self.dns_lats));
        m.insert("ReqLats".into(), f64_slice(&self.req_lats));
        m.insert("ResLats".into(), f64_slice(&self.res_lats));
        m.insert("DelayLats".into(), f64_slice(&self.delay_lats));
        m.insert("Offsets".into(), f64_slice(&self.offsets));
        m.insert(
            "StatusCodes".into(),
            Value::Slice(self.status_codes.iter().map(|c| Value::Int(*c)).collect()),
        );
        m.insert("Total".into(), Value::Duration(self.total));
        m.insert(
            "ErrorDist".into(),
            Value::MapStr(
                self.error_dist
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::Int(*v)))
                    .collect(),
            ),
        );
        m.insert(
            "StatusCodeDist".into(),
            Value::MapInt(
                self.status_code_dist
                    .iter()
                    .map(|(k, v)| (*k, Value::Int(*v)))
                    .collect(),
            ),
        );
        m.insert("SizeTotal".into(), Value::Int(self.size_total));
        m.insert("SizeReq".into(), Value::Int(self.size_req));
        m.insert("NumRes".into(), Value::Int(self.num_res));
        m.insert(
            "LatencyDistribution".into(),
            Value::Slice(
                self.latency_distribution
                    .iter()
                    .map(|l| {
                        let mut s = BTreeMap::new();
                        s.insert("Percentage".to_string(), Value::Int(l.percentage));
                        s.insert("Latency".to_string(), Value::Float(l.latency));
                        Value::Struct(s)
                    })
                    .collect(),
            ),
        );
        m.insert(
            "Histogram".into(),
            Value::Slice(
                self.histogram
                    .iter()
                    .map(|b| {
                        let mut s = BTreeMap::new();
                        s.insert("Mark".to_string(), Value::Float(b.mark));
                        s.insert("Count".to_string(), Value::Int(b.count));
                        s.insert("Frequency".to_string(), Value::Float(b.frequency));
                        Value::Struct(s)
                    })
                    .collect(),
            ),
        );
        Value::Struct(m)
    }
}
