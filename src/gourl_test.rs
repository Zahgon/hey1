//! Tests for the `net/url` shim. Expectations captured from the Go binary.

use super::*;

#[test]
fn parses_ordinary_urls() {
    let u = parse("http://127.0.0.1:8743/path?q=1").unwrap();
    assert_eq!(u.scheme, "http");
    assert_eq!(u.host, "127.0.0.1:8743");
    assert_eq!(u.hostname(), "127.0.0.1");
    assert_eq!(u.port(), Some(8743));
    assert_eq!(u.path, "/path");
    assert_eq!(u.raw_query.as_deref(), Some("q=1"));
    assert_eq!(u.request_uri(), "/path?q=1");
    assert_eq!(u.authority(), "127.0.0.1:8743");
}

#[test]
fn fills_in_the_default_port_per_scheme() {
    assert_eq!(
        parse("http://example.test/").unwrap().authority(),
        "example.test:80"
    );
    assert_eq!(
        parse("https://example.test/").unwrap().authority(),
        "example.test:443"
    );
}

#[test]
fn a_missing_path_still_requests_slash() {
    // httptest servers hand out "http://127.0.0.1:PORT" with no trailing
    // slash; Go still puts "/" on the wire, which TestRequest asserts on.
    let u = parse("http://127.0.0.1:5000").unwrap();
    assert_eq!(u.path, "");
    assert_eq!(u.request_uri(), "/");
}

#[test]
fn handles_ipv6_literals_and_userinfo() {
    let u = parse("http://[::1]:8743/x").unwrap();
    assert_eq!(u.hostname(), "::1");
    assert_eq!(u.port(), Some(8743));
    let u = parse("http://user:pw@example.test/").unwrap();
    assert_eq!(u.user.as_deref(), Some("user"));
    assert_eq!(u.password.as_deref(), Some("pw"));
    // Go's stripPassword masks the password in url.Error messages.
    assert_eq!(u.to_string(), "http://user:***@example.test/");
}

#[test]
fn rejects_exactly_what_go_rejects() {
    assert_eq!(
        parse("://bad").unwrap_err(),
        r#"parse "://bad": missing protocol scheme"#
    );
    assert_eq!(
        parse("127.0.0.1:8743").unwrap_err(),
        r#"parse "127.0.0.1:8743": first path segment in URL cannot contain colon"#
    );
}

#[test]
fn empty_host_is_accepted_and_prints_as_go_does() {
    // Go's url.Parse("http://") succeeds; the failure surfaces later as
    // "http: no Host in request URL".
    let u = parse("http://").unwrap();
    assert_eq!(u.host, "");
    assert_eq!(u.to_string(), "http:");
    let u = parse("http:///path").unwrap();
    assert_eq!(u.to_string(), "http:///path");
}

#[test]
fn out_of_range_ports_survive_parsing() {
    // url.Parse only checks that the port is digits; the range check happens
    // at dial time, which is why Go says "address 99999: invalid port".
    let u = parse("http://127.0.0.1:99999/").unwrap();
    assert_eq!(u.raw_port(), Some("99999"));
    assert_eq!(u.port(), None);
    assert_eq!(u.authority(), "127.0.0.1:99999");
}

#[test]
fn resolves_redirect_targets() {
    let base = parse("http://127.0.0.1:8743/a/b").unwrap();
    assert_eq!(
        base.resolve_reference("/loop").unwrap().to_string(),
        "http://127.0.0.1:8743/loop"
    );
    assert_eq!(
        base.resolve_reference("c").unwrap().to_string(),
        "http://127.0.0.1:8743/a/c"
    );
    assert_eq!(
        base.resolve_reference("../d").unwrap().to_string(),
        "http://127.0.0.1:8743/d"
    );
    assert_eq!(
        base.resolve_reference("https://other.test/x")
            .unwrap()
            .to_string(),
        "https://other.test/x"
    );
    assert_eq!(
        base.resolve_reference("//other.test/x")
            .unwrap()
            .to_string(),
        "http://other.test/x"
    );
}
