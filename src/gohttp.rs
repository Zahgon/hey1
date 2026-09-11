//! Stand-in for the slice of `net/http` + `net/http/httptrace` that hey uses:
//! a `Transport` with connection pooling and the knobs requester.go sets, a
//! `Client` with a total-request timeout and Go's redirect policy, and the
//! five trace hooks the load generator times each request with.
//!
//! Timing hooks
//! ------------
//! Go's httptrace fires `WroteRequest` when the request has been flushed and
//! `GotFirstResponseByte` on the first byte of the response. Over HTTP/1 those
//! are observed here at the socket: the wrapper records the completion of each
//! write and the arrival of the first read after it. Over HTTP/2 a connection
//! is shared by concurrent streams, so socket-level events cannot be
//! attributed to a single request; there the hooks are taken around the
//! per-request send/response futures instead. This is called out because it is
//! the one place the timings are derived differently from Go.

use crate::gotime::GoDuration;
use crate::gourl::{self, Url};
use crate::requester::now;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

pub const MAX_IDLE_CONN: usize = 500;

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

/// Go: `http.Header` -- canonical MIME keys, multiple values per key.
#[derive(Clone, Debug, Default)]
pub struct Header {
    entries: Vec<(String, String)>,
}

impl Header {
    pub fn new() -> Self {
        Header::default()
    }

    /// Go: `textproto.CanonicalMIMEHeaderKey`
    pub fn canonical(k: &str) -> String {
        let mut out = String::with_capacity(k.len());
        let mut upper = true;
        for c in k.chars() {
            if upper {
                out.extend(c.to_uppercase());
            } else {
                out.extend(c.to_lowercase());
            }
            upper = c == '-';
        }
        out
    }

    pub fn set(&mut self, k: &str, v: &str) {
        let ck = Self::canonical(k);
        self.entries.retain(|(ek, _)| ek != &ck);
        self.entries.push((ck, v.to_string()));
    }

    pub fn add(&mut self, k: &str, v: &str) {
        self.entries.push((Self::canonical(k), v.to_string()));
    }

    pub fn get(&self, k: &str) -> String {
        let ck = Self::canonical(k);
        self.entries
            .iter()
            .find(|(ek, _)| ek == &ck)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    pub fn has(&self, k: &str) -> bool {
        let ck = Self::canonical(k);
        self.entries.iter().any(|(ek, _)| ek == &ck)
    }

    pub fn del(&mut self, k: &str) {
        let ck = Self::canonical(k);
        self.entries.retain(|(ek, _)| ek != &ck);
    }

    pub fn iter(&self) -> impl Iterator<Item = &(String, String)> {
        self.entries.iter()
    }
}

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// Go: `http.Request` (client side).
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub header: Header,
    /// Go: `Request.Host` -- overrides the Host header when non-empty.
    pub host: String,
    pub body: Vec<u8>,
    pub content_length: i64,
}

impl Request {
    /// Go: `http.NewRequest(method, url, nil)`
    pub fn new(method: &str, raw_url: &str) -> Result<Request, String> {
        if !valid_method(method) {
            return Err(format!("net/http: invalid method {:?}", method));
        }
        let url = gourl::parse(raw_url)?;
        Ok(Request {
            method: method.to_string(),
            // Go's NewRequest seeds `Host` from `u.Host` -- including the
            // port. requester.go feeds that straight into
            // tls.Config.ServerName, so this is load-bearing, not cosmetic.
            host: url.host.clone(),
            url,
            header: Header::new(),
            body: Vec::new(),
            content_length: 0,
        })
    }

    /// Go: `Request.SetBasicAuth`
    pub fn set_basic_auth(&mut self, username: &str, password: &str) {
        use base64::Engine;
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", username, password));
        self.header
            .set("Authorization", &format!("Basic {}", token));
    }

    /// The Host header actually sent.
    pub fn effective_host(&self) -> String {
        if !self.host.is_empty() {
            self.host.clone()
        } else {
            self.url.host.clone()
        }
    }
}

fn valid_method(m: &str) -> bool {
    !m.is_empty()
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
}

/// Go: `http.Response` (only the fields hey reads).
#[derive(Clone, Debug)]
pub struct Response {
    pub status_code: i64,
    /// Go: -1 when unknown (chunked, or transparently gunzipped).
    pub content_length: i64,
    pub header: Header,
}

// ---------------------------------------------------------------------------
// Trace
// ---------------------------------------------------------------------------

/// Raw event timestamps, the equivalent of the `httptrace.ClientTrace`
/// callbacks in requester.go. `-1` means "the hook never fired", which is how
/// the caller reproduces Go's "the local variable stayed at its zero value"
/// behaviour.
#[derive(Debug)]
pub struct RawTrace {
    pub dns_start: AtomicI64,
    pub dns_done: AtomicI64,
    pub get_conn: AtomicI64,
    pub got_conn: AtomicI64,
    pub conn_reused: AtomicBool,
    pub wrote_request: AtomicI64,
    pub got_first_byte: AtomicI64,
}

impl Default for RawTrace {
    fn default() -> Self {
        RawTrace {
            dns_start: AtomicI64::new(-1),
            dns_done: AtomicI64::new(-1),
            get_conn: AtomicI64::new(-1),
            got_conn: AtomicI64::new(-1),
            conn_reused: AtomicBool::new(false),
            wrote_request: AtomicI64::new(-1),
            got_first_byte: AtomicI64::new(-1),
        }
    }
}

impl RawTrace {
    pub fn new() -> Self {
        Self::default()
    }
    fn stamp(f: &AtomicI64) {
        f.store(now().nanoseconds(), Ordering::SeqCst);
    }
    pub fn get(f: &AtomicI64) -> Option<GoDuration> {
        let v = f.load(Ordering::SeqCst);
        if v < 0 {
            None
        } else {
            Some(GoDuration::from_nanos(v))
        }
    }
}

// ---------------------------------------------------------------------------
// Socket-level timing wrapper
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ConnTiming {
    armed: AtomicBool,
    last_write: AtomicI64,
    first_read: AtomicI64,
}

impl ConnTiming {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
        self.last_write.store(-1, Ordering::SeqCst);
        self.first_read.store(-1, Ordering::SeqCst);
    }
}

struct TimingStream<S> {
    inner: S,
    t: Arc<ConnTiming>,
}

impl<S: AsyncRead + Unpin> AsyncRead for TimingStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            if buf.filled().len() > before && self.t.armed.swap(false, Ordering::SeqCst) {
                self.t
                    .first_read
                    .store(now().nanoseconds(), Ordering::SeqCst);
            }
        }
        r
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for TimingStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            if *n > 0 {
                self.t
                    .last_write
                    .store(now().nanoseconds(), Ordering::SeqCst);
            }
        }
        r
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = &r {
            if *n > 0 {
                self.t
                    .last_write
                    .store(now().nanoseconds(), Ordering::SeqCst);
            }
        }
        r
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

enum Sender {
    H1(hyper::client::conn::http1::SendRequest<Full<Bytes>>),
    H2(hyper::client::conn::http2::SendRequest<Full<Bytes>>),
}

impl Sender {
    fn is_closed(&self) -> bool {
        match self {
            Sender::H1(s) => s.is_closed(),
            Sender::H2(s) => s.is_closed(),
        }
    }
}

struct PooledConn {
    sender: Sender,
    timing: Arc<ConnTiming>,
}

/// Go: `http.Transport` with the fields requester.go sets.
pub struct Transport {
    pub server_name: String,
    pub max_idle_conns_per_host: usize,
    pub disable_compression: bool,
    pub disable_keep_alives: bool,
    pub proxy: Option<Url>,
    pub h2: bool,
    pool: Mutex<HashMap<String, Vec<PooledConn>>>,
    tls: Mutex<Option<Arc<rustls::ClientConfig>>>,
}

impl Transport {
    pub fn new(
        server_name: String,
        max_idle_conns_per_host: usize,
        disable_compression: bool,
        disable_keep_alives: bool,
        proxy: Option<Url>,
        h2: bool,
    ) -> Transport {
        Transport {
            server_name,
            max_idle_conns_per_host,
            disable_compression,
            disable_keep_alives,
            proxy,
            h2,
            pool: Mutex::new(HashMap::new()),
            tls: Mutex::new(None),
        }
    }

    fn tls_config(&self) -> Arc<rustls::ClientConfig> {
        let mut guard = self.tls.lock().unwrap();
        if let Some(c) = guard.as_ref() {
            return c.clone();
        }
        // Go: tls.Config{InsecureSkipVerify: true, ...}
        let mut cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();
        // Go clears TLSNextProto unless H2 is requested, which is what stops
        // the stdlib from negotiating h2 over ALPN.
        cfg.alpn_protocols = if self.h2 {
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        } else {
            vec![b"http/1.1".to_vec()]
        };
        let cfg = Arc::new(cfg);
        *guard = Some(cfg.clone());
        cfg
    }

    fn pool_key(&self, url: &Url) -> String {
        match &self.proxy {
            Some(p) => format!("{}|{}|{}", url.scheme, url.authority(), p),
            None => format!("{}|{}", url.scheme, url.authority()),
        }
    }

    fn take_idle(&self, key: &str) -> Option<PooledConn> {
        if self.disable_keep_alives {
            return None;
        }
        let mut pool = self.pool.lock().unwrap();
        let list = pool.get_mut(key)?;
        while let Some(c) = list.pop() {
            if !c.sender.is_closed() {
                return Some(c);
            }
        }
        None
    }

    fn put_idle(&self, key: &str, conn: PooledConn) {
        if self.disable_keep_alives || conn.sender.is_closed() {
            return;
        }
        let mut pool = self.pool.lock().unwrap();
        let list = pool.entry(key.to_string()).or_default();
        if list.len() < self.max_idle_conns_per_host {
            list.push(conn);
        }
    }

    async fn dial(&self, url: &Url, trace: &RawTrace) -> Result<PooledConn, String> {
        let use_tls = url.scheme == "https";
        let target_authority = url.authority();

        // Where do we open the TCP connection?
        let (dial_authority, tunnel) = match &self.proxy {
            Some(p) => (p.authority(), use_tls),
            None => (target_authority.clone(), false),
        };

        // --- DNS ---------------------------------------------------------
        let host = strip_port_host(&dial_authority);
        // Go's dialer range-checks the port here, not during url.Parse.
        let port_str = dial_authority.rsplit(':').next().unwrap_or("").to_string();
        let port = match port_str.parse::<u16>() {
            Ok(p) => p,
            Err(_) => {
                return Err(format!("dial tcp: address {}: invalid port", port_str));
            }
        };

        let addrs: Vec<std::net::SocketAddr> = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            vec![std::net::SocketAddr::new(ip, port)]
        } else {
            RawTrace::stamp(&trace.dns_start);
            let looked = tokio::net::lookup_host((host.clone(), port))
                .await
                .map_err(|e| format!("dial tcp: lookup {}: {}", host, dns_err_text(&e)))?
                .collect::<Vec<_>>();
            RawTrace::stamp(&trace.dns_done);
            if looked.is_empty() {
                return Err(format!("dial tcp: lookup {}: no such host", host));
            }
            looked
        };

        // --- TCP ---------------------------------------------------------
        let mut last_err: Option<io::Error> = None;
        let mut tcp: Option<TcpStream> = None;
        for a in &addrs {
            match TcpStream::connect(a).await {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    tcp = Some(s);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let mut tcp = match tcp {
            Some(s) => s,
            None => {
                let e = last_err.unwrap();
                return Err(format!(
                    "dial tcp {}: connect: {}",
                    addrs.first().map(|a| a.to_string()).unwrap_or_default(),
                    sys_err_text(&e)
                ));
            }
        };

        // --- CONNECT tunnel through the proxy for https ------------------
        if tunnel {
            proxy_connect(&mut tcp, &target_authority).await?;
        }

        let timing = Arc::new(ConnTiming::default());

        let io: Box<dyn Stream> = if use_tls {
            // Go: Transport.addTLS fills an empty ServerName with the dialed
            // host, so an unset -host still yields correct SNI.
            // Go: Transport.addTLS fills an empty ServerName with the dialed
            // host, so an unset -host still yields correct SNI.
            let raw_sni = if self.server_name.is_empty() {
                url.host.clone()
            } else {
                self.server_name.clone()
            };
            let server_name = pick_server_name(&raw_sni)
                .ok_or_else(|| format!("tls: invalid server name {:?}", raw_sni))?;
            let connector = tokio_rustls::TlsConnector::from(self.tls_config());
            let tls = connector
                .connect(server_name, tcp)
                .await
                .map_err(|e| format!("tls: {}", e))?;
            let negotiated_h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
            let stream = TimingStream {
                inner: tls,
                t: timing.clone(),
            };
            let io = hyper_util::rt::TokioIo::new(stream);
            return self.handshake(io, negotiated_h2, timing).await;
        } else {
            Box::new(TimingStream {
                inner: tcp,
                t: timing.clone(),
            })
        };

        // Plaintext: h2 only if explicitly requested (prior knowledge).
        let io = hyper_util::rt::TokioIo::new(io);
        self.handshake(io, self.h2, timing).await
    }

    async fn handshake<I>(
        &self,
        io: I,
        h2: bool,
        timing: Arc<ConnTiming>,
    ) -> Result<PooledConn, String>
    where
        I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    {
        if h2 {
            let (sender, conn) =
                hyper::client::conn::http2::handshake(hyper_util::rt::TokioExecutor::new(), io)
                    .await
                    .map_err(|e| e.to_string())?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            Ok(PooledConn {
                sender: Sender::H2(sender),
                timing,
            })
        } else {
            let (sender, conn) = hyper::client::conn::http1::handshake(io)
                .await
                .map_err(|e| e.to_string())?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            Ok(PooledConn {
                sender: Sender::H1(sender),
                timing,
            })
        }
    }

    /// Go: `Transport.RoundTrip`
    pub async fn round_trip(&self, req: &Request, trace: &RawTrace) -> Result<Response, String> {
        if req.url.scheme != "http" && req.url.scheme != "https" {
            return Err(format!("unsupported protocol scheme {:?}", req.url.scheme));
        }
        if req.url.host.is_empty() {
            return Err("http: no Host in request URL".to_string());
        }

        let key = self.pool_key(&req.url);
        RawTrace::stamp(&trace.get_conn);

        let (mut conn, reused) = match self.take_idle(&key) {
            Some(c) => (c, true),
            None => (self.dial(&req.url, trace).await?, false),
        };
        trace.conn_reused.store(reused, Ordering::SeqCst);
        RawTrace::stamp(&trace.got_conn);

        conn.timing.arm();

        // --- build the hyper request ------------------------------------
        let absolute_form = self.proxy.is_some() && req.url.scheme == "http";
        let is_h2 = matches!(conn.sender, Sender::H2(_));
        let uri_str = if absolute_form {
            req.url.to_string()
        } else if is_h2 {
            // :authority comes from Request.Host so `-host` overrides it, the
            // same way the HTTP/1 Host header does.
            format!(
                "{}://{}{}",
                req.url.scheme,
                req.effective_host(),
                req.url.request_uri()
            )
        } else {
            req.url.request_uri()
        };

        let mut builder = hyper::Request::builder()
            .method(req.method.as_str())
            .uri(uri_str.as_str());
        if !is_h2 {
            // HTTP/1: Host must be sent explicitly; HTTP/2 derives :authority.
            builder = builder.header("Host", req.effective_host());
        }
        for (k, v) in req.header.iter() {
            builder = builder.header(k.as_str(), v.as_str());
        }
        // Go's Transport adds this itself and gunzips transparently.
        let auto_gzip = !self.disable_compression
            && !req.header.has("Accept-Encoding")
            && !req.header.has("Range")
            && req.method != "HEAD";
        if auto_gzip {
            builder = builder.header("Accept-Encoding", "gzip");
        }
        if self.disable_keep_alives && !is_h2 {
            builder = builder.header("Connection", "close");
        }

        let hreq = builder
            .body(Full::new(Bytes::from(req.body.clone())))
            .map_err(|e| e.to_string())?;

        // --- send --------------------------------------------------------
        let resp: hyper::Response<Incoming> = match &mut conn.sender {
            Sender::H1(s) => {
                s.ready().await.map_err(|e| e.to_string())?;
                s.send_request(hreq).await.map_err(|e| e.to_string())?
            }
            Sender::H2(s) => {
                s.ready().await.map_err(|e| e.to_string())?;
                // See the module note: over h2 the socket cannot attribute
                // bytes to a stream, so the hooks bracket the request future.
                RawTrace::stamp(&trace.wrote_request);
                let r = s.send_request(hreq).await.map_err(|e| e.to_string())?;
                RawTrace::stamp(&trace.got_first_byte);
                r
            }
        };

        if matches!(conn.sender, Sender::H1(_)) {
            let lw = conn.timing.last_write.load(Ordering::SeqCst);
            if lw >= 0 {
                trace.wrote_request.store(lw, Ordering::SeqCst);
            }
            let fr = conn.timing.first_read.load(Ordering::SeqCst);
            if fr >= 0 {
                trace.got_first_byte.store(fr, Ordering::SeqCst);
            }
        }

        let status = resp.status().as_u16() as i64;
        let mut header = Header::new();
        for (k, v) in resp.headers().iter() {
            header.add(k.as_str(), v.to_str().unwrap_or(""));
        }

        let mut content_length: i64 = match resp.headers().get(hyper::header::CONTENT_LENGTH) {
            Some(v) => v
                .to_str()
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(-1),
            None => -1,
        };

        // Go: when the Transport added Accept-Encoding itself and the peer
        // gzipped the body, it decodes transparently, drops Content-Encoding
        // and Content-Length, and sets ContentLength to -1.
        let gzipped = auto_gzip && header.get("Content-Encoding").eq_ignore_ascii_case("gzip");
        if gzipped {
            header.del("Content-Encoding");
            header.del("Content-Length");
            content_length = -1;
        }

        // --- drain the body (Go: io.Copy(io.Discard, resp.Body)) ---------
        let mut body = resp.into_body();
        let mut raw: Vec<u8> = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| e.to_string())?;
            if let Some(chunk) = frame.data_ref() {
                if gzipped {
                    raw.extend_from_slice(chunk);
                }
            }
        }
        if gzipped {
            // Decompress so a corrupt stream surfaces as an error, exactly as
            // it would when Go's gzip reader is drained.
            use std::io::Read as _;
            let mut d = flate2::read::GzDecoder::new(&raw[..]);
            let mut sink = Vec::new();
            d.read_to_end(&mut sink).map_err(|e| e.to_string())?;
        }

        let closed = conn.sender.is_closed();
        if !closed {
            self.put_idle(&key, conn);
        }

        Ok(Response {
            status_code: status,
            content_length,
            header,
        })
    }
}

fn strip_port_host(hostport: &str) -> String {
    if let Some(rest) = hostport.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return rest[..end].to_string();
        }
    }
    match hostport.rfind(':') {
        Some(i) if hostport[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
            hostport[..i].to_string()
        }
        _ => hostport.to_string(),
    }
}

/// Choose the TLS ServerName.
///
/// Go's `hostnameInSNI` sends whatever string is in `tls.Config.ServerName`
/// unless it parses as an IP literal, in which case no SNI is sent. Because
/// hey seeds ServerName from `Request.Host`, that string usually still carries
/// the port -- and Go happily sends "127.0.0.1:8744" as SNI. rustls only
/// accepts syntactically valid DNS names or IPs, so the port-bearing form is
/// retried without its port. See the migration notes: this is the one place
/// the ClientHello can differ from Go's.
fn pick_server_name(raw: &str) -> Option<rustls_pki_types::ServerName<'static>> {
    if let Ok(n) = rustls_pki_types::ServerName::try_from(raw.to_string()) {
        return Some(n.to_owned());
    }
    let stripped = strip_port_host(raw);
    rustls_pki_types::ServerName::try_from(stripped)
        .ok()
        .map(|n| n.to_owned())
}

async fn proxy_connect(tcp: &mut TcpStream, authority: &str) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let req = format!(
        "CONNECT {a} HTTP/1.1\r\nHost: {a}\r\nProxy-Connection: Keep-Alive\r\n\r\n",
        a = authority
    );
    tcp.write_all(req.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = tcp.read(&mut byte).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("proxy: unexpected EOF".to_string());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return Err("proxy: response header too long".to_string());
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");
    if status != "200" {
        return Err(format!(
            "proxyconnect tcp: {}",
            head.lines().next().unwrap_or("bad CONNECT response")
        ));
    }
    Ok(())
}

/// Render an OS error the way Go's `syscall.Errno.Error()` does.
///
/// Go's per-platform errno table holds the strerror text with a lowercase
/// first letter and no numeric suffix ("connection refused", "can't assign
/// requested address"). Rust's Display adds " (os error N)" and keeps the
/// leading capital, so strip and downcase.
fn sys_err_text(e: &io::Error) -> String {
    let s = e.to_string();
    let s = match s.rfind(" (os error ") {
        Some(i) if s.ends_with(')') => s[..i].to_string(),
        _ => s,
    };
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_uppercase() => c.to_lowercase().collect::<String>() + chars.as_str(),
        _ => s,
    }
}

fn dns_err_text(_e: &io::Error) -> String {
    "no such host".to_string()
}

#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            RSA_PKCS1_SHA256,
            RSA_PKCS1_SHA384,
            RSA_PKCS1_SHA512,
            ECDSA_NISTP256_SHA256,
            ECDSA_NISTP384_SHA384,
            ECDSA_NISTP521_SHA512,
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            ED25519,
        ]
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Go: `http.Client`
pub struct Client {
    pub transport: Arc<Transport>,
    /// Go: `Client.Timeout`; zero means no timeout.
    pub timeout: GoDuration,
    /// Go: `CheckRedirect` returning `http.ErrUseLastResponse`.
    pub disable_redirects: bool,
}

impl Client {
    /// Go: `Client.Do`
    pub async fn do_request(&self, req: &Request, trace: &RawTrace) -> Result<Response, String> {
        let fut = self.do_inner(req, trace);
        if self.timeout.nanoseconds() > 0 {
            match tokio::time::timeout(self.timeout.to_std(), fut).await {
                Ok(r) => r,
                Err(_) => Err(url_error(
                    &req.method,
                    &req.url,
                    "context deadline exceeded (Client.Timeout exceeded while awaiting headers)",
                )),
            }
        } else {
            fut.await
        }
    }

    async fn do_inner(&self, req: &Request, trace: &RawTrace) -> Result<Response, String> {
        let mut current = req.clone();
        // Go's `reqs` slice: the requests already sent, passed to
        // CheckRedirect as `via`.
        let mut reqs_sent: usize = 0;
        loop {
            reqs_sent += 1;
            let resp = self
                .transport
                .round_trip(&current, trace)
                .await
                .map_err(|e| url_error(&req.method, &req.url, &e))?;

            if self.disable_redirects || !is_redirect(resp.status_code) {
                return Ok(resp);
            }
            let loc = resp.header.get("Location");
            if loc.is_empty() {
                // Go treats a redirect without Location as an error rather
                // than handing the response back.
                return Err(url_error(
                    &req.method,
                    &current.url,
                    &format!("{} response missing Location header", resp.status_code),
                ));
            }
            // Go: `defaultCheckRedirect` errors when `len(via) >= 10`, and
            // `via` holds the requests already sent -- so the 10th request is
            // the last one made and the 11th is refused. The resulting
            // url.Error has its URL overwritten with the raw Location value
            // (the Go 1 compatibility special case in Client.do), so the
            // message shows "/loop" rather than the resolved absolute URL.
            if reqs_sent >= 10 {
                return Err(format!(
                    "{} {:?}: stopped after 10 redirects",
                    url_error_op(&req.method),
                    loc
                ));
            }
            let next_url = current.url.resolve_reference(&loc).map_err(|_| {
                url_error(
                    &req.method,
                    &current.url,
                    &format!("failed to parse Location header {:?}", loc),
                )
            })?;

            let mut next = current.clone();
            // Go: 301/302/303 degrade to GET and drop the body; 307/308 keep
            // both.
            if matches!(resp.status_code, 301..=303) {
                if next.method != "GET" && next.method != "HEAD" {
                    next.method = "GET".to_string();
                }
                next.body = Vec::new();
                next.content_length = 0;
                next.header.del("Content-Type");
                next.header.del("Content-Length");
            }
            // Go strips sensitive headers when the host changes.
            if next_url.hostname() != current.url.hostname() {
                next.header.del("Authorization");
                next.header.del("Www-Authenticate");
                next.header.del("Cookie");
                next.header.del("Cookie2");
            }
            next.url = next_url;
            current = next;
        }
    }
}

fn is_redirect(code: i64) -> bool {
    // Go: redirectBehavior handles exactly these five.
    matches!(code, 301 | 302 | 303 | 307..=308)
}

/// Go: `(*url.Error).Error()` -- `fmt.Sprintf("%s %q: %s", op, url, err)`
pub fn url_error(method: &str, url: &Url, err: &str) -> String {
    format!("{} {:?}: {}", url_error_op(method), url.to_string(), err)
}

/// Go: `urlErrorOp` -- "GET" becomes "Get", "POST" becomes "Post".
fn url_error_op(method: &str) -> String {
    if method.is_empty() {
        return "Get".to_string();
    }
    let mut c = method.chars();
    let first: String = c.next().unwrap().to_string();
    format!("{}{}", first, c.as_str().to_lowercase())
}

#[cfg(test)]
#[path = "gohttp_test.rs"]
mod gohttp_test;

#[cfg(test)]
#[path = "gohttp_client_test.rs"]
mod gohttp_client_test;
