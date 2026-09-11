//! Port of requester/requester.go
//!
//! > Package requester provides commands to run load tests and display results.

use crate::gohttp::{Client, Header, RawTrace, Request, Transport, MAX_IDLE_CONN};
use crate::gotime::GoDuration;
use crate::gourl::Url;
use crate::requester::now;
use crate::requester::report::{new_report, Reporter};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc;

/// Go: `const maxResult = 1000000` -- "Max size of the buffer of result channel."
pub const MAX_RESULT: i64 = 1_000_000;

/// Go: `type result struct`
#[derive(Clone, Debug)]
pub struct ResultRecord {
    pub err: Option<String>,
    pub status_code: i64,
    pub offset: GoDuration,
    pub duration: GoDuration,
    /// connection setup(DNS lookup + Dial up) duration
    pub conn_duration: GoDuration,
    /// dns lookup duration
    pub dns_duration: GoDuration,
    /// request "write" duration
    pub req_duration: GoDuration,
    /// response "read" duration
    pub res_duration: GoDuration,
    /// delay between response and request
    pub delay_duration: GoDuration,
    pub content_length: i64,
}

/// Lazily created channels, the equivalent of the fields guarded by Go's
/// `initOnce sync.Once`.
pub(crate) struct WorkState {
    results_tx: Mutex<Option<mpsc::Sender<ResultRecord>>>,
    results_rx: Mutex<Option<mpsc::Receiver<ResultRecord>>>,
    /// Go's `stopCh chan struct{}` with capacity C, modelled as a token count:
    /// `Stop` deposits C tokens and each worker consumes at most one.
    stop_tokens: AtomicI64,
    start: AtomicI64,
    started: AtomicBool,
}

/// Go: `type Work struct`
pub struct Work {
    /// Request is the request to be made.
    pub request: Request,

    pub request_body: Vec<u8>,

    /// RequestFunc is a function to generate requests. If it is nil, then
    /// Request and RequestData are cloned for each request.
    pub request_func: Option<Arc<dyn Fn() -> Request + Send + Sync>>,

    /// N is the total number of requests to make.
    pub n: i64,

    /// C is the concurrency level, the number of concurrent workers to run.
    pub c: i64,

    /// H2 is an option to make HTTP/2 requests
    pub h2: bool,

    /// Timeout in seconds.
    pub timeout: i64,

    /// Qps is the rate limit in queries per second.
    pub qps: f64,

    /// DisableCompression is an option to disable compression in response
    pub disable_compression: bool,

    /// DisableKeepAlives is an option to prevents re-use of TCP connections
    /// between different HTTP requests
    pub disable_keep_alives: bool,

    /// DisableRedirects is an option to prevent the following of HTTP redirects
    pub disable_redirects: bool,

    /// Output represents the output type. If "csv" is provided, the output
    /// will be dumped as a csv stream.
    pub output: String,

    /// ProxyAddr is the address of HTTP proxy server in the format on
    /// "host:port". Optional.
    pub proxy_addr: Option<Url>,

    /// Writer is where results will be written. If nil, results are written
    /// to stdout.
    pub writer: Mutex<Option<Box<dyn Write + Send>>>,

    pub(crate) state: OnceLock<WorkState>,
}

impl Default for Work {
    fn default() -> Self {
        Work {
            request: Request::new("GET", "http://localhost/").expect("default request"),
            request_body: Vec::new(),
            request_func: None,
            n: 0,
            c: 0,
            h2: false,
            timeout: 0,
            qps: 0.0,
            disable_compression: false,
            disable_keep_alives: false,
            disable_redirects: false,
            output: String::new(),
            proxy_addr: None,
            writer: Mutex::new(None),
            state: OnceLock::new(),
        }
    }
}

/// Standard output routed through `print!` rather than a raw `io::stdout()`
/// handle.
///
/// Go's `os.Stdout` is just fd 1, but Rust's test harness captures output by
/// swapping the sink behind the `print!` family only. A raw handle writes
/// straight past that capture and interleaves report text with libtest's own
/// output, which corrupts the machine-readable test report even though every
/// test still passes.
struct CapturedStdout;

impl Write for CapturedStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match std::str::from_utf8(buf) {
            Ok(text) => print!("{text}"),
            // Report output is text; anything else goes to the real handle
            // rather than being lossily mangled.
            Err(_) => std::io::stdout().write_all(buf)?,
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

impl Work {
    /// Go: `func (b *Work) writer() io.Writer`
    fn writer(&self) -> Box<dyn Write + Send> {
        match self.writer.lock().unwrap().take() {
            None => Box::new(CapturedStdout),
            Some(w) => w,
        }
    }

    /// Go: `func (b *Work) Init()` -- initializes internal data-structures.
    pub fn init(&self) {
        self.state.get_or_init(|| {
            let cap = std::cmp::min(self.c.saturating_mul(1000), MAX_RESULT).max(1) as usize;
            let (tx, rx) = mpsc::channel(cap);
            WorkState {
                results_tx: Mutex::new(Some(tx)),
                results_rx: Mutex::new(Some(rx)),
                stop_tokens: AtomicI64::new(0),
                start: AtomicI64::new(0),
                started: AtomicBool::new(false),
            }
        });
    }

    fn state(&self) -> &WorkState {
        self.init();
        self.state.get().unwrap()
    }

    /// Go: `func (b *Work) Run()` -- makes all the requests, prints the
    /// summary. It blocks until all work is done.
    pub async fn run(&self) {
        self.init();
        let st = self.state();
        let start = now();
        st.start.store(start.nanoseconds(), Ordering::SeqCst);
        st.started.store(true, Ordering::SeqCst);

        let reporter = new_report(self.writer(), &self.output, self.n);
        let rx = st
            .results_rx
            .lock()
            .unwrap()
            .take()
            .expect("Run called twice");

        // Go: "Run the reporter first, it polls the result channel until it is
        // closed."
        let handle = tokio::spawn(run_reporter(reporter, rx));

        self.run_workers().await;
        self.finish(handle).await;
    }

    /// Go: `func (b *Work) Stop()`
    pub fn stop(&self) {
        // Send stop signal so that workers can stop gracefully.
        let st = self.state();
        for _ in 0..self.c {
            st.stop_tokens.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Go: `func (b *Work) Finish()`
    async fn finish(&self, handle: tokio::task::JoinHandle<Reporter>) {
        let st = self.state();
        // Go: close(b.results). Dropping the last sender is the equivalent;
        // the workers' clones are already gone at this point.
        st.results_tx.lock().unwrap().take();
        let total = now() - GoDuration::from_nanos(st.start.load(Ordering::SeqCst));
        // Wait until the reporter is done.
        let mut reporter = handle.await.expect("reporter task panicked");
        reporter.finalize(total);
    }

    /// Go: `func (b *Work) makeRequest(c *http.Client)`
    async fn make_request(&self, c: &Client, tx: &mpsc::Sender<ResultRecord>) {
        let s = now();
        let mut size: i64 = 0;
        let mut code: i64 = 0;

        let req = match &self.request_func {
            Some(f) => f(),
            None => clone_request(&self.request, &self.request_body),
        };

        // Go installs an httptrace.ClientTrace on the request context; the
        // Rust client records the same events into `RawTrace`.
        let trace = RawTrace::new();
        let resp = c.do_request(&req, &trace).await;

        let err: Option<String> = match resp {
            Ok(r) => {
                size = r.content_length;
                code = r.status_code;
                // Go: io.Copy(io.Discard, resp.Body); resp.Body.Close()
                // (the transport drains the body before this returns)
                None
            }
            Err(e) => Some(e),
        };

        let t = now();
        let d = derive_durations(&trace, t);
        let finish = t - s;

        let _ = tx
            .send(ResultRecord {
                offset: s,
                status_code: code,
                duration: finish,
                err,
                content_length: size,
                conn_duration: d.conn,
                dns_duration: d.dns,
                req_duration: d.req,
                res_duration: d.res,
                delay_duration: d.delay,
            })
            .await;
    }

    /// Go: `func (b *Work) runWorker(client *http.Client, n int)`
    async fn run_worker(&self, client: &Client, n: i64, tx: mpsc::Sender<ResultRecord>) {
        let mut throttle = if self.qps > 0.0 {
            // Go: time.NewTicker(time.Duration(1e6/(b.QPS)) * time.Microsecond)
            // The float division is truncated to an integer number of
            // microseconds before being scaled, so reproduce that order.
            let micros = (1e6 / self.qps) as i64;
            let period = std::time::Duration::from_micros(micros.max(0) as u64);
            let mut iv = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            // Go's Ticker drops ticks that the receiver was too slow for.
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            Some(iv)
        } else {
            None
        };

        let st = self.state();
        for _ in 0..n {
            // Go: `select { case <-b.stopCh: return; default: ... }`
            // Check if application is stopped. Do not send into a closed channel.
            if take_stop_token(&st.stop_tokens) {
                return;
            }
            if let Some(iv) = throttle.as_mut() {
                iv.tick().await;
            }
            self.make_request(client, &tx).await;
        }
    }

    /// Go: `func (b *Work) runWorkers()`
    async fn run_workers(&self) {
        let server_name = self.request.host.clone();
        let transport = Arc::new(Transport::new(
            server_name,
            std::cmp::min(self.c.max(0) as usize, MAX_IDLE_CONN),
            self.disable_compression,
            self.disable_keep_alives,
            self.proxy_addr.clone(),
            self.h2,
        ));
        let client = Arc::new(Client {
            transport,
            timeout: GoDuration::from_nanos(self.timeout.saturating_mul(crate::gotime::SECOND)),
            // Go sets this inside runWorker on the shared client; hoisted here
            // because every worker assigns the same value.
            disable_redirects: self.disable_redirects,
        });

        let tx = self
            .state()
            .results_tx
            .lock()
            .unwrap()
            .clone()
            .expect("Run called after Finish");

        let mut set = Vec::new();
        // Go: "Ignore the case where b.N % b.C != 0." Each worker runs exactly
        // N/C requests, so a non-divisible N really does under-shoot.
        let per_worker = if self.c > 0 { self.n / self.c } else { 0 };
        for _ in 0..self.c {
            set.push(self.run_worker(&client, per_worker, tx.clone()));
        }
        drop(tx);
        futures_join_all(set).await;
    }
}

/// Consume one stop token if any remain -- the non-blocking receive in Go's
/// `select`.
fn take_stop_token(tokens: &AtomicI64) -> bool {
    tokens
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
            if v > 0 {
                Some(v - 1)
            } else {
                None
            }
        })
        .is_ok()
}

/// Run every worker future concurrently on the current task, which is what a
/// `sync.WaitGroup` over `b.C` goroutines amounts to here.
async fn futures_join_all<F: std::future::Future<Output = ()>>(futs: Vec<F>) {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    struct JoinAll<F> {
        futs: Vec<Option<Pin<Box<F>>>>,
        remaining: usize,
    }

    impl<F: std::future::Future<Output = ()>> std::future::Future for JoinAll<F> {
        type Output = ();
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            let this = self.get_mut();
            for slot in this.futs.iter_mut() {
                if let Some(f) = slot.as_mut() {
                    if f.as_mut().poll(cx).is_ready() {
                        *slot = None;
                        this.remaining -= 1;
                    }
                }
            }
            if this.remaining == 0 {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }

    let remaining = futs.len();
    if remaining == 0 {
        return;
    }
    JoinAll {
        futs: futs.into_iter().map(|f| Some(Box::pin(f))).collect(),
        remaining,
    }
    .await
}

/// The five durations requester.go derives inside its `httptrace.ClientTrace`
/// callbacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Durations {
    pub dns: GoDuration,
    pub conn: GoDuration,
    pub req: GoDuration,
    pub res: GoDuration,
    pub delay: GoDuration,
}

/// Translate raw trace timestamps into Go's derived durations.
///
/// Each Go callback assigns into a local that starts at the zero value, so a
/// hook that never fired leaves its duration at zero. `t` is the timestamp
/// taken right after the response body has been drained.
pub fn derive_durations(trace: &RawTrace, t: GoDuration) -> Durations {
    let dns_start = RawTrace::get(&trace.dns_start).unwrap_or(GoDuration::ZERO);
    // Go: `DNSDone: dnsDuration = now() - dnsStart`
    let dns = match RawTrace::get(&trace.dns_done) {
        Some(done) => done - dns_start,
        None => GoDuration::ZERO,
    };

    let conn_start = RawTrace::get(&trace.get_conn).unwrap_or(GoDuration::ZERO);
    let got_conn = RawTrace::get(&trace.got_conn);
    let reused = trace.conn_reused.load(Ordering::SeqCst);
    // Go: `GotConn: if !connInfo.Reused { connDuration = now() - connStart }`
    let conn = match got_conn {
        Some(g) if !reused => g - conn_start,
        _ => GoDuration::ZERO,
    };
    // Go: GotConn always sets `reqStart = now()`.
    let req_start = got_conn.unwrap_or(GoDuration::ZERO);

    let wrote = RawTrace::get(&trace.wrote_request);
    // Go: `WroteRequest: reqDuration = now() - reqStart`
    let req = match wrote {
        Some(w) => w - req_start,
        None => GoDuration::ZERO,
    };
    // Go: WroteRequest sets `delayStart = now()`.
    let delay_start = wrote.unwrap_or(GoDuration::ZERO);

    let first_byte = RawTrace::get(&trace.got_first_byte);
    // Go: `GotFirstResponseByte: delayDuration = now() - delayStart`
    let delay = match first_byte {
        Some(f) => f - delay_start,
        None => GoDuration::ZERO,
    };
    // Go: GotFirstResponseByte sets `resStart = now()`.
    let res_start = first_byte.unwrap_or(GoDuration::ZERO);

    // Note: Go computes this unconditionally, so when the response never
    // produced a first byte `resStart` is still 0 and `resDuration` ends up
    // being the whole time since process start. Such results always carry an
    // error and are excluded from the report, so the quirk is preserved
    // rather than papered over.
    let res = t - res_start;

    Durations {
        dns,
        conn,
        req,
        res,
        delay,
    }
}

/// Go: `func runReporter(r *report)`
async fn run_reporter(mut r: Reporter, mut rx: mpsc::Receiver<ResultRecord>) -> Reporter {
    // Loop will continue until channel is closed
    while let Some(res) = rx.recv().await {
        r.record(&res);
    }
    // Signal reporter is done. (The JoinHandle plays the part of `r.done`.)
    r
}

/// Go: `func cloneRequest(r *http.Request, body []byte) *http.Request`
pub fn clone_request(r: &Request, body: &[u8]) -> Request {
    let mut r2 = r.clone();
    if !body.is_empty() {
        r2.body = body.to_vec();
    }
    r2
}

/// Re-exported so callers can build headers the way hey.go does.
pub type WorkHeader = Header;
