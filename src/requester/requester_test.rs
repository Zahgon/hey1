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

//! Port of requester/requester_test.go
//!
//! `httptest.NewServer` has no Rust equivalent, so `new_test_server` below
//! plays the same role: bind an ephemeral loopback port, serve the handler,
//! expose `.url`, and shut down on `.close()`.

use crate::gohttp::{Header, Request};
use crate::requester::requester::Work;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// httptest stand-in
// ---------------------------------------------------------------------------

pub struct TestServer {
    pub url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TestServer {
    /// Go: `defer server.Close()`
    pub fn close(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.close();
    }
}

/// Go: `httptest.NewServer(http.HandlerFunc(handler))`
async fn new_test_server<H>(handler: H) -> TestServer
where
    H: Fn(hyper::http::request::Parts, Vec<u8>) + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let handler = Arc::new(handler);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => {
                    let stream = match accepted {
                        Ok((s, _)) => s,
                        Err(_) => continue,
                    };
                    let h = handler.clone();
                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let svc = service_fn(move |req: hyper::Request<Incoming>| {
                            let h = h.clone();
                            async move {
                                let (parts, body) = req.into_parts();
                                let bytes = body
                                    .collect()
                                    .await
                                    .map(|c| c.to_bytes().to_vec())
                                    .unwrap_or_default();
                                h(parts, bytes);
                                Ok::<_, hyper::Error>(hyper::Response::new(Full::new(
                                    Bytes::new(),
                                )))
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .await;
                    });
                }
            }
        }
    });

    TestServer {
        url: format!("http://{}", addr),
        shutdown: Some(tx),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn test_n() {
    let count = Arc::new(AtomicI64::new(0));
    let c = count.clone();
    let handler = move |_parts: hyper::http::request::Parts, _body: Vec<u8>| {
        c.fetch_add(1, Ordering::SeqCst);
    };
    let mut server = new_test_server(handler).await;

    let req = Request::new("GET", &server.url).unwrap();
    let w = Work {
        request: req,
        n: 20,
        c: 2,
        ..Default::default()
    };
    w.run().await;
    let got = count.load(Ordering::SeqCst);
    if got != 20 {
        panic!("Expected to send 20 requests, found {}", got);
    }
    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_qps() {
    let count = Arc::new(AtomicI64::new(0));
    let c = count.clone();
    let handler = move |_parts: hyper::http::request::Parts, _body: Vec<u8>| {
        c.fetch_add(1, Ordering::SeqCst);
    };
    let mut server = new_test_server(handler).await;

    let req = Request::new("GET", &server.url).unwrap();
    let w = Arc::new(Work {
        request: req,
        n: 20,
        c: 2,
        qps: 1.0,
        ..Default::default()
    });

    // Go: `go w.Run()` plus a `time.AfterFunc(time.Second, ...)` assertion.
    let rw = w.clone();
    tokio::spawn(async move {
        rw.run().await;
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let got = count.load(Ordering::SeqCst);
    if got > 2 {
        panic!("Expected to work at most 2 times, found {}", got);
    }
    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_request() {
    let seen: Arc<Mutex<(String, String, String, String)>> = Arc::new(Mutex::new((
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    )));
    let s = seen.clone();
    let handler = move |parts: hyper::http::request::Parts, _body: Vec<u8>| {
        let hv = |k: &str| {
            parts
                .headers
                .get(k)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        let uri = parts
            .uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_else(|| parts.uri.to_string());
        *s.lock().unwrap() = (uri, hv("Content-type"), hv("X-some"), hv("Authorization"));
    };
    let mut server = new_test_server(handler).await;

    let mut header = Header::new();
    header.add("Content-type", "text/html");
    header.add("X-some", "value");
    let mut req = Request::new("GET", &server.url).unwrap();
    req.header = header;
    req.set_basic_auth("username", "password");
    let w = Work {
        request: req,
        n: 1,
        c: 1,
        ..Default::default()
    };
    w.run().await;

    let (uri, content_type, some, auth) = seen.lock().unwrap().clone();
    if uri != "/" {
        panic!("Uri is expected to be /, {} is found", uri);
    }
    if content_type != "text/html" {
        panic!(
            "Content type is expected to be text/html, {} is found",
            content_type
        );
    }
    if some != "value" {
        panic!("X-some header is expected to be value, {} is found", some);
    }
    if auth != "Basic dXNlcm5hbWU6cGFzc3dvcmQ=" {
        panic!("Basic authorization is not properly set");
    }
    server.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_body() {
    let count = Arc::new(AtomicI64::new(0));
    let c = count.clone();
    let handler = move |_parts: hyper::http::request::Parts, body: Vec<u8>| {
        if String::from_utf8_lossy(&body) == "Body" {
            c.fetch_add(1, Ordering::SeqCst);
        }
    };
    let mut server = new_test_server(handler).await;

    let mut req = Request::new("POST", &server.url).unwrap();
    req.body = b"Body".to_vec();
    req.content_length = 4;
    let w = Work {
        request: req,
        request_body: b"Body".to_vec(),
        n: 10,
        c: 1,
        ..Default::default()
    };
    w.run().await;

    let got = count.load(Ordering::SeqCst);
    if got != 10 {
        panic!("Expected to work 10 times, found {}", got);
    }
    server.close();
}

// ---------------------------------------------------------------------------
// Trace -> duration mapping (the httptrace.ClientTrace callbacks in Go)
// ---------------------------------------------------------------------------

mod durations {
    use crate::gohttp::RawTrace;
    use crate::gotime::GoDuration;
    use crate::requester::requester::derive_durations;
    use std::sync::atomic::Ordering;

    fn at(ns: i64) -> GoDuration {
        GoDuration::from_nanos(ns)
    }

    fn trace(get_conn: i64, got_conn: i64, wrote: i64, first: i64, reused: bool) -> RawTrace {
        let t = RawTrace::new();
        t.get_conn.store(get_conn, Ordering::SeqCst);
        t.got_conn.store(got_conn, Ordering::SeqCst);
        t.wrote_request.store(wrote, Ordering::SeqCst);
        t.got_first_byte.store(first, Ordering::SeqCst);
        t.conn_reused.store(reused, Ordering::SeqCst);
        t
    }

    #[test]
    fn fresh_connection_charges_dial_time() {
        let tr = trace(100, 400, 500, 900, false);
        tr.dns_start.store(120, Ordering::SeqCst);
        tr.dns_done.store(300, Ordering::SeqCst);
        let d = derive_durations(&tr, at(1000));
        assert_eq!(d.dns, at(180), "dnsDone - dnsStart");
        assert_eq!(d.conn, at(300), "gotConn - getConn");
        assert_eq!(d.req, at(100), "wroteRequest - gotConn");
        assert_eq!(d.delay, at(400), "firstByte - wroteRequest");
        assert_eq!(d.res, at(100), "t - firstByte");
    }

    #[test]
    fn reused_connection_reports_zero_dial_time() {
        // Go only assigns connDuration inside `if !connInfo.Reused`, so a
        // pooled connection contributes 0 to the DNS+dialup column even
        // though GetConn/GotConn both fired.
        let tr = trace(100, 400, 500, 900, true);
        let d = derive_durations(&tr, at(1000));
        assert_eq!(d.conn, GoDuration::ZERO);
        // reqStart is still set by GotConn, so the other columns are unaffected.
        assert_eq!(d.req, at(100));
        assert_eq!(d.delay, at(400));
    }

    #[test]
    fn hooks_that_never_fired_stay_at_zero() {
        // Go's locals start at the zero value; a DNS-less dial (IP literal)
        // leaves dnsDuration at 0 rather than producing garbage.
        let tr = trace(100, 400, 500, 900, false);
        let d = derive_durations(&tr, at(1000));
        assert_eq!(d.dns, GoDuration::ZERO, "no DNS hooks fired");
    }

    #[test]
    fn a_response_with_no_first_byte_measures_res_from_process_start() {
        // Upstream quirk: resDuration is computed unconditionally, so when
        // GotFirstResponseByte never fired resStart is 0 and resDuration
        // becomes the whole elapsed time. Such results always carry an error
        // and are dropped from the report, so this never surfaces -- but it is
        // reproduced rather than silently corrected.
        let tr = RawTrace::new();
        tr.get_conn.store(100, Ordering::SeqCst);
        let d = derive_durations(&tr, at(999_000));
        assert_eq!(d.res, at(999_000));
        assert_eq!(d.req, GoDuration::ZERO);
        assert_eq!(d.delay, GoDuration::ZERO);
    }
}
