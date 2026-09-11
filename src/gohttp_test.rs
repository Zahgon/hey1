//! Tests for the `net/http` shim: header semantics, request construction and
//! the url.Error rendering that feeds the report's error distribution.

use super::*;

#[test]
fn header_keys_are_canonicalised_like_gos_mime_form() {
    assert_eq!(Header::canonical("content-type"), "Content-Type");
    assert_eq!(Header::canonical("X-SOME"), "X-Some");
    assert_eq!(Header::canonical("x-a-b"), "X-A-B");
    assert_eq!(Header::canonical("host"), "Host");

    let mut h = Header::new();
    h.set("content-type", "text/html");
    // Lookup is case-insensitive because both sides canonicalise.
    assert_eq!(h.get("Content-Type"), "text/html");
    assert_eq!(h.get("CONTENT-TYPE"), "text/html");
    assert!(h.has("content-TYPE"));
}

#[test]
fn set_replaces_while_add_appends() {
    let mut h = Header::new();
    h.set("X-A", "1");
    h.set("X-A", "2");
    assert_eq!(h.iter().filter(|(k, _)| k == "X-A").count(), 1);
    assert_eq!(h.get("X-A"), "2");

    h.add("X-B", "1");
    h.add("X-B", "2");
    assert_eq!(h.iter().filter(|(k, _)| k == "X-B").count(), 2);
    // Get returns the first value, as Go's does.
    assert_eq!(h.get("X-B"), "1");

    h.del("X-B");
    assert!(!h.has("X-B"));
}

#[test]
fn new_request_seeds_host_from_the_url() {
    // Go's http.NewRequest sets Host: u.Host -- including the port. hey feeds
    // that into tls.Config.ServerName, so it is load-bearing.
    let r = Request::new("GET", "http://127.0.0.1:8743/p").unwrap();
    assert_eq!(r.host, "127.0.0.1:8743");
    assert_eq!(r.effective_host(), "127.0.0.1:8743");

    // An explicit -host overrides it.
    let mut r = Request::new("GET", "http://127.0.0.1:8743/p").unwrap();
    r.host = "example.test".to_string();
    assert_eq!(r.effective_host(), "example.test");
}

#[test]
fn new_request_rejects_invalid_methods_and_urls() {
    assert!(Request::new("GE T", "http://x/").is_err());
    assert!(Request::new("", "http://x/").is_err());
    assert!(Request::new("GET", "://bad").is_err());
}

#[test]
fn set_basic_auth_matches_gos_encoding() {
    let mut r = Request::new("GET", "http://x/").unwrap();
    r.set_basic_auth("username", "password");
    // The exact value requester_test.go asserts on.
    assert_eq!(
        r.header.get("Authorization"),
        "Basic dXNlcm5hbWU6cGFzc3dvcmQ="
    );
}

#[test]
fn url_error_renders_like_gos() {
    let u = crate::gourl::parse("http://127.0.0.1:9/").unwrap();
    // Exactly what the report's error distribution shows.
    assert_eq!(
        url_error(
            "GET",
            &u,
            "dial tcp 127.0.0.1:9: connect: connection refused"
        ),
        r#"Get "http://127.0.0.1:9/": dial tcp 127.0.0.1:9: connect: connection refused"#
    );
    // urlErrorOp title-cases the method.
    assert!(url_error("POST", &u, "boom").starts_with(r#"Post ""#));
    assert!(url_error("DELETE", &u, "boom").starts_with(r#"Delete ""#));
    assert!(url_error("", &u, "boom").starts_with(r#"Get ""#));
}

#[test]
fn trace_slots_start_unset() {
    // "-1" is how the port represents "this hook never fired", which maps to
    // Go's local variable staying at its zero value.
    let t = RawTrace::new();
    assert!(RawTrace::get(&t.dns_start).is_none());
    assert!(RawTrace::get(&t.got_conn).is_none());
    assert!(RawTrace::get(&t.wrote_request).is_none());
    assert!(RawTrace::get(&t.got_first_byte).is_none());
}
