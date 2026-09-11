//! Client-level behaviour tests.
//!
//! Each expectation here was first observed by running the real Go `hey`
//! binary against the same scenario, so these lock in parity rather than
//! merely describing what this implementation happens to do.

use super::*;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

type Resp = hyper::Response<Full<Bytes>>;

struct Srv {
    url: String,
    hits: Arc<AtomicI64>,
    conns: Arc<AtomicI64>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for Srv {
    fn drop(&mut self) {
        if let Some(t) = self.shutdown.take() {
            let _ = t.send(());
        }
    }
}

async fn serve<H>(handler: H) -> Srv
where
    H: Fn(&str) -> Resp + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let handler = Arc::new(handler);
    let hits = Arc::new(AtomicI64::new(0));
    let conns = Arc::new(AtomicI64::new(0));
    let (h2, c2) = (hits.clone(), conns.clone());
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                a = listener.accept() => {
                    let Ok((stream, _)) = a else { continue };
                    c2.fetch_add(1, Ordering::SeqCst);
                    let (h, hh) = (handler.clone(), h2.clone());
                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let svc = service_fn(move |req: hyper::Request<Incoming>| {
                            let (h, hh) = (h.clone(), hh.clone());
                            let path = req.uri().path().to_string();
                            async move {
                                hh.fetch_add(1, Ordering::SeqCst);
                                Ok::<_, hyper::Error>(h(&path))
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc).await;
                    });
                }
            }
        }
    });
    Srv {
        url: format!("http://{}", addr),
        hits,
        conns,
        shutdown: Some(tx),
    }
}

fn transport(disable_compression: bool, disable_keep_alives: bool) -> Arc<Transport> {
    Arc::new(Transport::new(
        String::new(),
        50,
        disable_compression,
        disable_keep_alives,
        None,
        false,
    ))
}

fn client(t: Arc<Transport>, disable_redirects: bool) -> Client {
    Client {
        transport: t,
        timeout: crate::gotime::GoDuration::ZERO,
        disable_redirects,
    }
}

fn redirect_to(loc: &str) -> Resp {
    hyper::Response::builder()
        .status(302)
        .header("Location", loc)
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn ok_body(b: &'static [u8]) -> Resp {
    hyper::Response::builder()
        .status(200)
        .header("Content-Length", b.len().to_string())
        .body(Full::new(Bytes::from_static(b)))
        .unwrap()
}

/// `/redir/N` bounces down to `/redir/0`, which returns 200.
fn chain_handler(path: &str) -> Resp {
    if let Some(rest) = path.strip_prefix("/redir/") {
        let n: i32 = rest.parse().unwrap_or(0);
        if n > 0 {
            return redirect_to(&format!("/redir/{}", n - 1));
        }
        return ok_body(b"done");
    }
    if path == "/loop" {
        return redirect_to("/loop");
    }
    if path == "/noloc" {
        return hyper::Response::builder()
            .status(302)
            .header("Content-Length", "0")
            .body(Full::new(Bytes::new()))
            .unwrap();
    }
    ok_body(b"hello world!")
}

#[tokio::test(flavor = "multi_thread")]
async fn follows_redirects_up_to_the_go_limit() {
    let srv = serve(chain_handler).await;
    let c = client(transport(false, false), false);

    // Go follows a 9-hop chain to completion.
    let req = Request::new("GET", &format!("{}/redir/9", srv.url)).unwrap();
    let r = c.do_request(&req, &RawTrace::new()).await.unwrap();
    assert_eq!(r.status_code, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_the_eleventh_request_and_reports_the_raw_location() {
    let srv = serve(chain_handler).await;
    let c = client(transport(false, false), false);

    // Go's defaultCheckRedirect errors when len(via) >= 10, i.e. 10 requests
    // are made and the 11th is refused.
    let req = Request::new("GET", &format!("{}/redir/10", srv.url)).unwrap();
    let err = c.do_request(&req, &RawTrace::new()).await.unwrap_err();
    // The URL is the *raw Location value*, not the resolved absolute URL --
    // Client.do overwrites it for Go 1 compatibility.
    assert_eq!(err, r#"Get "/redir/0": stopped after 10 redirects"#);

    let req = Request::new("GET", &format!("{}/loop", srv.url)).unwrap();
    let err = c.do_request(&req, &RawTrace::new()).await.unwrap_err();
    assert_eq!(err, r#"Get "/loop": stopped after 10 redirects"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn disable_redirects_returns_the_redirect_itself() {
    let srv = serve(chain_handler).await;
    let c = client(transport(false, false), true);
    let req = Request::new("GET", &format!("{}/redir/3", srv.url)).unwrap();
    let r = c.do_request(&req, &RawTrace::new()).await.unwrap();
    assert_eq!(r.status_code, 302);
    assert_eq!(srv.hits.load(Ordering::SeqCst), 1, "must not follow");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_without_location_is_an_error() {
    let srv = serve(chain_handler).await;
    let c = client(transport(false, false), false);
    let req = Request::new("GET", &format!("{}/noloc", srv.url)).unwrap();
    let err = c.do_request(&req, &RawTrace::new()).await.unwrap_err();
    assert!(
        err.ends_with("302 response missing Location header"),
        "got {}",
        err
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gzip_is_decoded_transparently_and_clears_content_length() {
    use std::io::Write as _;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&b"x".repeat(5000)).unwrap();
    let gz: &'static [u8] = Box::leak(enc.finish().unwrap().into_boxed_slice());

    let srv = serve(move |_p: &str| {
        hyper::Response::builder()
            .status(200)
            .header("Content-Encoding", "gzip")
            .header("Content-Length", gz.len().to_string())
            .body(Full::new(Bytes::from_static(gz)))
            .unwrap()
    })
    .await;

    // Go's Transport adds Accept-Encoding itself, gunzips, drops the header
    // and sets ContentLength to -1 -- which is why the summary omits
    // "Total data" for gzipped endpoints.
    let c = client(transport(false, false), false);
    let req = Request::new("GET", &srv.url).unwrap();
    let r = c.do_request(&req, &RawTrace::new()).await.unwrap();
    assert_eq!(r.content_length, -1);
    assert_eq!(r.header.get("Content-Encoding"), "");

    // With -disable-compression the response is passed through untouched.
    let c = client(transport(true, false), false);
    let r = c.do_request(&req, &RawTrace::new()).await.unwrap();
    assert_eq!(r.content_length, gz.len() as i64);
    assert_eq!(r.header.get("Content-Encoding"), "gzip");
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_alive_reuses_connections_unless_disabled() {
    let srv = serve(chain_handler).await;
    let req = Request::new("GET", &srv.url).unwrap();

    let c = client(transport(false, false), false);
    for _ in 0..4 {
        c.do_request(&req, &RawTrace::new()).await.unwrap();
    }
    assert_eq!(
        srv.conns.load(Ordering::SeqCst),
        1,
        "should pool one connection"
    );

    let srv2 = serve(chain_handler).await;
    let req2 = Request::new("GET", &srv2.url).unwrap();
    let c2 = client(transport(false, true), false);
    for _ in 0..4 {
        c2.do_request(&req2, &RawTrace::new()).await.unwrap();
    }
    assert_eq!(
        srv2.conns.load(Ordering::SeqCst),
        4,
        "-disable-keepalive must dial per request"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trace_hooks_fire_for_a_real_request() {
    let srv = serve(chain_handler).await;
    let c = client(transport(false, false), false);
    let req = Request::new("GET", &srv.url).unwrap();
    let t = RawTrace::new();
    c.do_request(&req, &t).await.unwrap();

    // These four drive every timing column in the report.
    assert!(RawTrace::get(&t.get_conn).is_some(), "GetConn");
    assert!(RawTrace::get(&t.got_conn).is_some(), "GotConn");
    assert!(RawTrace::get(&t.wrote_request).is_some(), "WroteRequest");
    assert!(
        RawTrace::get(&t.got_first_byte).is_some(),
        "GotFirstResponseByte"
    );
    assert!(
        !t.conn_reused.load(Ordering::SeqCst),
        "first request is not reused"
    );

    // A second request on the pooled connection reports Reused, which is what
    // makes connDuration zero in Go.
    let t2 = RawTrace::new();
    c.do_request(&req, &t2).await.unwrap();
    assert!(
        t2.conn_reused.load(Ordering::SeqCst),
        "second request reuses"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_produces_gos_message() {
    let srv = serve(|_p: &str| ok_body(b"slow")).await;
    // Point at a black-holed address so the dial hangs.
    let mut req = Request::new("GET", &srv.url).unwrap();
    req.url = crate::gourl::parse("http://192.0.2.1:80/").unwrap();
    req.host = "192.0.2.1:80".to_string();

    let c = Client {
        transport: transport(false, false),
        timeout: crate::gotime::GoDuration::from_nanos(crate::gotime::SECOND),
        disable_redirects: false,
    };
    let err = c.do_request(&req, &RawTrace::new()).await.unwrap_err();
    assert_eq!(
        err,
        r#"Get "http://192.0.2.1:80/": context deadline exceeded (Client.Timeout exceeded while awaiting headers)"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_scheme_and_missing_host_match_go() {
    let c = client(transport(false, false), false);
    let req = Request::new("GET", "ftp://127.0.0.1/").unwrap();
    assert_eq!(
        c.do_request(&req, &RawTrace::new()).await.unwrap_err(),
        r#"Get "ftp://127.0.0.1/": unsupported protocol scheme "ftp""#
    );
    let req = Request::new("GET", "http://").unwrap();
    assert_eq!(
        c.do_request(&req, &RawTrace::new()).await.unwrap_err(),
        r#"Get "http:": http: no Host in request URL"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn out_of_range_port_is_rejected_at_dial_time() {
    let c = client(transport(false, false), false);
    let req = Request::new("GET", "http://127.0.0.1:99999/").unwrap();
    assert_eq!(
        c.do_request(&req, &RawTrace::new()).await.unwrap_err(),
        r#"Get "http://127.0.0.1:99999/": dial tcp: address 99999: invalid port"#
    );
}
