// Copyright 2014 Google Inc. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Command hey is an HTTP load generator.
//!
//! Port of hey.go (Go's `package main`).

use hey::goflag::FlagSet;
use hey::gohttp::{Header, Request};
use hey::gotime::GoDuration;
use hey::gourl;
use hey::requester::requester::Work;
use regex::Regex;
use std::sync::Arc;

const HEADER_REGEXP: &str = r"^([\w-]+):\s*(.+)";
const AUTH_REGEXP: &str = r"^(.+):([^\s].+)";
const HEY_UA: &str = "hey/0.0.1";

/// Go: `math.MaxInt32`
const MAX_INT32: i64 = 2147483647;

static USAGE: &str = USAGE_TMPL;

fn main() {
    // Go samples `startTime` during package init, before main runs.
    hey::requester::init_clock();

    let num_cpu = num_cpus::get() as i64;

    // Go: flag.Usage = func() { fmt.Fprint(os.Stderr, fmt.Sprintf(usage, runtime.NumCPU())) }
    let usage_fn = move || {
        eprint!("{}", USAGE.replacen("%d", &num_cpu.to_string(), 1));
    };

    let mut fs = FlagSet::new(Box::new(usage_fn));
    fs.string("m", "GET");
    fs.string("h", "");
    fs.string("d", "");
    fs.string("D", "");
    fs.string("A", "");
    fs.string("T", "text/html");
    fs.string("a", "");
    fs.string("host", "");
    fs.string("U", "");
    fs.string("o", "");
    fs.int("c", 50);
    fs.int("n", 200);
    fs.float64("q", 0.0);
    fs.int("t", 20);
    fs.duration("z", GoDuration::ZERO);
    fs.bool("h2", false);
    // Go: `runtime.GOMAXPROCS(-1)` -- the current setting, i.e. NumCPU.
    fs.int("cpus", num_cpu);
    fs.bool("disable-compression", false);
    fs.bool("disable-keepalive", false);
    fs.bool("disable-redirects", false);
    fs.string("x", "");

    // Go: `var hs headerSlice; flag.Var(&hs, "H", "")`
    fs.var_slice("H");

    fs.parse(std::env::args().skip(1).collect());
    if fs.narg() < 1 {
        usage_and_exit("");
    }

    let cpus = fs.get_int("cpus");
    let mut num = fs.get_int("n");
    let mut conc = fs.get_int("c");
    let q = fs.get_float("q");
    let dur = fs.get_dur("z");

    if dur.nanoseconds() > 0 {
        num = MAX_INT32;
        if conc <= 0 {
            usage_and_exit("-c cannot be smaller than 1.");
        }
    } else {
        if num <= 0 || conc <= 0 {
            usage_and_exit("-n and -c cannot be smaller than 1.");
        }
        if num < conc {
            usage_and_exit("-n cannot be less than -c.");
        }
    }
    // Keep `conc` visibly used the way Go's local does.
    conc = conc.max(conc);

    let url = fs.args()[0].clone();
    let method = fs.get_str("m").to_uppercase();

    // set content-type
    let mut header = Header::new();
    header.set("Content-Type", &fs.get_str("T"));
    // set any other additional headers
    if !fs.get_str("h").is_empty() {
        usage_and_exit("Flag '-h' is deprecated, please use '-H' instead.");
    }
    // set any other additional repeatable headers
    for h in fs.get_slice("H") {
        match parse_input_with_regexp(&h, HEADER_REGEXP) {
            Ok(m) => header.set(&m[1], &m[2]),
            Err(e) => usage_and_exit(&e),
        }
    }

    if !fs.get_str("A").is_empty() {
        header.set("Accept", &fs.get_str("A"));
    }

    // set basic auth if set
    let mut username = String::new();
    let mut password = String::new();
    if !fs.get_str("a").is_empty() {
        match parse_input_with_regexp(&fs.get_str("a"), AUTH_REGEXP) {
            Ok(m) => {
                username = m[1].clone();
                password = m[2].clone();
            }
            Err(e) => usage_and_exit(&e),
        }
    }

    let mut body_all: Vec<u8> = Vec::new();
    if !fs.get_str("d").is_empty() {
        body_all = fs.get_str("d").into_bytes();
    }
    if !fs.get_str("D").is_empty() {
        let path = fs.get_str("D");
        match std::fs::read(&path) {
            Ok(slurp) => body_all = slurp,
            Err(e) => err_and_exit(&format!("open {}: {}", path, io_err_text(&e))),
        }
    }

    let mut proxy_url = None;
    if !fs.get_str("x").is_empty() {
        match gourl::parse(&fs.get_str("x")) {
            Ok(u) => proxy_url = Some(u),
            Err(e) => usage_and_exit(&e),
        }
    }

    let mut req = match Request::new(&method, &url) {
        Ok(r) => r,
        Err(e) => usage_and_exit(&e),
    };
    req.content_length = body_all.len() as i64;
    if !username.is_empty() || !password.is_empty() {
        req.set_basic_auth(&username, &password);
    }

    // set host header if set
    if !fs.get_str("host").is_empty() {
        req.host = fs.get_str("host");
    }

    let mut ua = header.get("User-Agent");
    if ua.is_empty() {
        ua = HEY_UA.to_string();
    } else {
        ua = format!("{} {}", ua, HEY_UA);
    }
    header.set("User-Agent", &ua);

    // set userAgent header if set
    if !fs.get_str("U").is_empty() {
        ua = format!("{} {}", fs.get_str("U"), HEY_UA);
        header.set("User-Agent", &ua);
    }

    // NOTE: upstream assigns the whole header map here, which discards the
    // Authorization entry `SetBasicAuth` wrote above -- so `-a` has no effect
    // on the wire. Reproduced verbatim; see the migration notes.
    req.header = header;

    // Go: `w := &requester.Work{ ... }`. Written as field assignment because
    // Work owns private once-initialised channel state.
    let mut w = Work::default();
    w.request = req;
    w.request_body = body_all;
    w.n = num;
    w.c = conc;
    w.qps = q;
    w.timeout = fs.get_int("t");
    w.disable_compression = fs.get_bool("disable-compression");
    w.disable_keep_alives = fs.get_bool("disable-keepalive");
    w.disable_redirects = fs.get_bool("disable-redirects");
    w.h2 = fs.get_bool("h2");
    w.proxy_addr = proxy_url;
    w.output = fs.get_str("o");
    let w = Arc::new(w);
    w.init();

    // Go: runtime.GOMAXPROCS(*cpus)
    let worker_threads = if cpus > 0 { cpus as usize } else { 1 };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async move {
        // Go: signal.Notify(c, os.Interrupt); go func() { <-c; w.Stop() }()
        let sw = w.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                sw.stop();
            }
        });
        if dur.nanoseconds() > 0 {
            let dw = w.clone();
            tokio::spawn(async move {
                tokio::time::sleep(dur.to_std()).await;
                dw.stop();
            });
        }
        w.run().await;
    });
}

fn io_err_text(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => e.to_string(),
    }
}

/// Go: `func errAndExit(msg string)`
// The separate newline writes mirror Go's two Fprintf calls.
#[allow(clippy::print_with_newline)]
fn err_and_exit(msg: &str) -> ! {
    eprint!("{}", msg);
    eprint!("\n");
    std::process::exit(1);
}

/// Go: `func usageAndExit(msg string)`
#[allow(clippy::print_with_newline)]
fn usage_and_exit(msg: &str) -> ! {
    if !msg.is_empty() {
        eprint!("{}", msg);
        eprint!("\n\n");
    }
    eprint!("{}", USAGE.replacen("%d", &num_cpus::get().to_string(), 1));
    eprint!("\n");
    std::process::exit(1);
}

/// Go: `func parseInputWithRegexp(input, regx string) ([]string, error)`
///
/// Returns the full match followed by the capture groups, matching Go's
/// `FindStringSubmatch` layout so `match[1]` / `match[2]` line up.
fn parse_input_with_regexp(input: &str, regx: &str) -> Result<Vec<String>, String> {
    let re = Regex::new(regx).expect("regexp.MustCompile");
    match re.captures(input) {
        Some(caps) => Ok((0..caps.len())
            .map(|i| {
                caps.get(i)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default()
            })
            .collect()),
        None => Err(format!(
            "could not parse the provided input; input = {}",
            input
        )),
    }
}

/// Go: `type headerSlice []string`
#[derive(Debug, Default, Clone)]
struct HeaderSlice(Vec<String>);

#[allow(dead_code)]
impl HeaderSlice {
    /// Go: `func (h *headerSlice) String() string`
    fn string(&self) -> String {
        format!("[{}]", self.0.join(" "))
    }
    /// Go: `func (h *headerSlice) Set(value string) error`
    fn set(&mut self, value: &str) -> Result<(), String> {
        self.0.push(value.to_string());
        Ok(())
    }
}

#[cfg(test)]
#[path = "hey_test.rs"]
mod hey_test;

const USAGE_TMPL: &str = r###"Usage: hey [options...] <url>

Options:
  -n  Number of requests to run. Default is 200.
  -c  Number of workers to run concurrently. Total number of requests cannot
      be smaller than the concurrency level. Default is 50.
  -q  Rate limit, in queries per second (QPS) per worker. Default is no rate limit.
  -z  Duration of application to send requests. When duration is reached,
      application stops and exits. If duration is specified, n is ignored.
      Examples: -z 10s -z 3m.
  -o  Output type. If none provided, a summary is printed.
      "csv" is the only supported alternative. Dumps the response
      metrics in comma-separated values format.

  -m  HTTP method, one of GET, POST, PUT, DELETE, HEAD, OPTIONS.
  -H  Custom HTTP header. You can specify as many as needed by repeating the flag.
      For example, -H "Accept: text/html" -H "Content-Type: application/xml" .
  -t  Timeout for each request in seconds. Default is 20, use 0 for infinite.
  -A  HTTP Accept header.
  -d  HTTP request body.
  -D  HTTP request body from file. For example, /home/user/file.txt or ./file.txt.
  -T  Content-type, defaults to "text/html".
  -U  User-Agent, defaults to version "hey/0.0.1".
  -a  Basic authentication, username:password.
  -x  HTTP Proxy address as host:port.
  -h2 Enable HTTP/2.

  -host	HTTP Host header.

  -disable-compression  Disable compression.
  -disable-keepalive    Disable keep-alive, prevents re-use of TCP
                        connections between different HTTP requests.
  -disable-redirects    Disable following of HTTP redirects
  -cpus                 Number of used cpu cores.
                        (default for current machine is %d cores)
"###;
