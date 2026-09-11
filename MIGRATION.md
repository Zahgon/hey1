# Migration notes: Go → Rust

Port of [rakyll/hey](https://github.com/rakyll/hey). The goal was behavioural
equivalence, not idiomatic rewriting: where upstream has a quirk or a bug, it is
reproduced and commented rather than fixed.

## File mapping

| Go | Rust | Notes |
|---|---|---|
| `hey.go` | `src/main.rs` | `package main` |
| `hey_test.go` | `src/hey_test.rs` | included from `main.rs` as a `#[cfg(test)] mod`, so it reaches private items like Go's in-package test |
| `requester/requester.go` | `src/requester/requester.rs` | `Work`, `result`, worker pool |
| `requester/requester_test.go` | `src/requester/requester_test.rs` | `httptest.NewServer` replaced by `new_test_server` |
| `requester/report.go` | `src/requester/report.rs` | `report`, `Report`, `Bucket`, `LatencyDistribution` |
| `requester/print.go` | `src/requester/print.rs` | template literals copied byte-for-byte |
| `requester/now_other.go` | `src/requester/now_other.rs` | `#[cfg(not(windows))]` replaces `// +build !windows` |
| `requester/now_windows.go` | `src/requester/now_windows.rs` | `#[cfg(windows)]` replaces the `_windows` filename suffix |
| `Makefile`, `Dockerfile`, `.github/workflows/go.yml` | same names, `rust.yml` | equivalents |
| — | `src/requester/print_test.rs` | **added**: byte-parity harness (see below) |

### Standard-library shims

Go stdlib packages with no direct Rust equivalent are reimplemented under
`src/go*.rs`. They exist to preserve observable behaviour, not to be
general-purpose libraries.

| Shim | Stands in for | Why it is needed |
|---|---|---|
| `gotime.rs` | `time.Duration`, `time.ParseDuration` | Go durations are **signed** i64 nanoseconds; `std::time::Duration` is unsigned and panics on underflow |
| `gofmt.rs` | `fmt` `%4.4f` / `%d` | Go prints non-finite floats as `NaN`/`+Inf` and still applies width padding, so `%4.4f` of NaN is `" NaN"` |
| `gotemplate.rs` | `text/template` | `-o` is passed straight to `template.Must(...)`, so arbitrary user templates must work |
| `goflag.rs` | `flag` | Go-style single-dash flags, `-h` bound to a *string*, parsing stops at the first non-flag argument |
| `gourl.rs` | `net/url` | Go-specific parse errors and `URL.String()` rules |
| `gohttp.rs` | `net/http` + `net/http/httptrace` | pooled transport, redirect policy, and the five per-request timing hooks |
| `golog.rs` | `log` | the `YYYY/MM/DD HH:MM:SS` stderr prefix |

## Upstream behaviour deliberately preserved

These are quirks of the Go code, kept because changing them would change output:

1. **`-a` basic auth never reaches the wire.** `hey.go` calls
   `req.SetBasicAuth(...)` and then overwrites the whole map with
   `req.Header = header`, discarding the `Authorization` entry. Verified: neither
   binary sends the header.
2. **`%%` in the latency distribution.** `text/template` does not collapse `%%`
   the way `fmt` does, so both binaries literally print `10%% in 0.0011 secs`.
3. **`Report.ConnMax` / `ConnMin` are swapped.** `ConnMax` is assigned
   `connLats[0]` (the minimum) after an ascending sort. The rendered output is
   correct because the template labels them "(average, fastest, slowest)"; only
   the field names are backwards.
4. **`N` is truncated to `C * (N / C)`.** `-n 201 -c 50` sends 200 requests, and
   `-n 20 -c 3` sends 18. Upstream comments this as "Ignore the case where
   `b.N % b.C != 0`".
5. **Empty percentile slots print as `0%% in 0.0000 secs`** for small samples,
   because `latencies()` leaves unfilled entries at the zero value.
6. **A divide-by-zero when every request fails** yields `Average: NaN` and
   `Requests/sec: +Inf`.
7. **TLS `ServerName` is `Request.Host`, which includes the port**, because
   `http.NewRequest` seeds `Host` from `u.Host`. See the divergence below.
8. **`resDuration` on an errored request** is measured from process start,
   because `resStart` is still zero. Such results always carry an error and are
   excluded from the report, so it never surfaces.

## Validation

### Build

`cargo build --release`, `cargo build --all-targets`, `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` all pass from a clean tree.

### Test cases

| | Go | Rust |
|---|---|---|
| ported from Go | 9 | 9 (1:1) |
| added | — | 82 |
| **total** | **9** | **91** |

No Go test was dropped, weakened, or merged. Every `Test*` function has a
counterpart with the same inputs and the same assertions:

| Go | Rust |
|---|---|
| `TestParseValidHeaderFlag` | `hey_test::test_parse_valid_header_flag` |
| `TestParseInvalidHeaderFlag` | `hey_test::test_parse_invalid_header_flag` |
| `TestParseValidAuthFlag` | `hey_test::test_parse_valid_auth_flag` |
| `TestParseInvalidAuthFlag` | `hey_test::test_parse_invalid_auth_flag` |
| `TestParseAuthMetaCharacters` | `hey_test::test_parse_auth_meta_characters` |
| `TestN` | `requester_test::test_n` |
| `TestQps` | `requester_test::test_qps` |
| `TestRequest` | `requester_test::test_request` |
| `TestBody` | `requester_test::test_body` |

### Code coverage

Per ported file, Go statement coverage vs Rust line coverage:

| Go file | Go | Rust file | Rust |
|---|---|---|---|
| `hey.go` | 3.60% | `main.rs` | 8.67% |
| `requester.go` | 89.80% | `requester/requester.rs` | 94.83% |
| `report.go` | 98.74% | `requester/report.rs` | 100.00% |
| `print.go` | 80.00% | `requester/print.rs` | 94.83% |
| `now_other.go` | 100.00% | `requester/now_other.rs` | 100.00% |
| **ported subtotal** | **73.57%** | | **74.98%** |
| whole project | 73.57% | | 82.17% |

No ported file regressed. The whole-project figure is higher despite the Rust
tree carrying ~2,900 extra lines of stdlib shims that Go got for free from
`net/http`, `text/template`, `flag`, `net/url` and `time` -- code that Go's
`-cover` never measured because it lives outside the packages under test.

### Test quality (mutation testing)

A passing suite proves little on its own, so 25 deliberate bugs were injected
one at a time -- each one removing a preserved upstream quirk or breaking a
Go-matching rule -- and the suite was re-run against each.

* **25/25 killable mutants were caught.**
* 2 of the original 25 were proven *equivalent mutants* (no test can catch
  them because they cannot change behaviour): `>` vs `>=` on a
  `content_length` that only ever adds zero, and `Duration.Seconds()`'
  split-vs-naive division, which agree bit-for-bit across the whole i64 range.
* The first run surfaced 3 genuine gaps -- the golden fixtures hand-built
  their `Report`, so `snapshot()`, `latencies()` and `histogram()` were never
  executed; latency ordering was untestable because the fixture happened to be
  sorted; and nothing asserted that a reused connection contributes zero dial
  time. All three are now covered, and re-running those mutations kills them.

Two of the tests written during this pass initially failed on hand-computed
expectations (a column number and a bar length) and the implementation -- which
matches Go -- was right; the expectations were corrected against the Go binary.

## Verified equivalence

Both binaries were run side by side against the same servers and compared:

- usage text -- **byte-identical** (1691 bytes)
- report/CSV rendering -- **byte-identical** against golden fixtures generated
  by executing the *original Go templates* (`requester/print_test.rs`)
- the whole statistics pipeline -- `record` -> `finalize` -> `snapshot` ->
  `histogram`/`latencies` -> template -- **byte-identical** against fixtures
  produced by the original Go `runReporter`/`finalize` over the same synthetic
  results, across 8 scenarios including out-of-order latencies, an all-failed
  run, and a zero-width histogram (`requester/report_test.rs`)
- `%4.4f` / `%4.3f` float formatting -- **zero differences over 200,012 values**
- HTTP requests on the wire -- identical across 14 flag combinations (methods,
  headers, bodies, User-Agent precedence, `-host`, compression, keep-alive)
- socket-level connection reuse -- identical connection counts for
  `-c 4`, `-c 8`, `-c 3` and `-disable-keepalive`
- flag parsing and error text, including exit codes (2 for flag errors, 1 for
  validation failures)
- error distribution strings, e.g.
  `Get "http://127.0.0.1:9/": dial tcp 127.0.0.1:9: connect: connection refused`
- redirect policy: 10 requests made, the 11th refused, error URL is the raw
  `Location` value (Go's Go-1-compatibility special case)
- timeouts, unsupported schemes, invalid ports, missing hosts, DNS failures
- gzip transparent decoding (`ContentLength` -> -1, so `Total data` is omitted)
- chunked responses, HTTP proxy (`-x`) in absolute-form and CONNECT modes
- TLS with `InsecureSkipVerify`, HTTP/2 via ALPN, `-h2` with `-host`
- `-o` with arbitrary templates, including `range` over maps/slices/integers,
  `if`/`else`, `index`, `jsonify` and variable assignment
- `template.Must` parse panics and template *execute* errors -- same message,
  line, column, failing expression, Go type name and exit code
- SIGINT: graceful stop, report printed, exit 0
- QPS rate limiting and `-z` duration mode

### Peer-to-peer

hey has no peer-to-peer behaviour to preserve: it is a client-only HTTP load
generator. The original opens no listening socket outside its tests, and its
only dependency is `golang.org/x/net`. The nearest analogue -- the
point-to-point connection layer -- was verified explicitly: connection pooling
and reuse, `-disable-keepalive`, concurrency level, HTTP/2 multiplexing over a
single connection, and proxy tunnelling all match Go at the socket level.

## Known differences

**TLS SNI when the ServerName carries a port.** Because `Request.Host` includes
the port, Go puts e.g. `127.0.0.1:8744` or `localhost:8744` in the ClientHello
SNI — which RFC 6066 forbids (SNI must be a DNS name, never an IP or a
host:port). rustls enforces that rule and will not emit such a name, so this
port-bearing form is retried without its port:

| ServerName | Go sends | This port sends |
|---|---|---|
| `127.0.0.1:8744` | `127.0.0.1:8744` | *(none — IP literal)* |
| `localhost:8744` | `localhost:8744` | `localhost` |
| `example.test` (via `-host`) | `example.test` | `example.test` |

The `Host` header — which is what actually routes a request — matches Go in
every case. Servers that select a certificate by SNI would already have failed
against upstream hey, so this port is, if anything, more likely to work.

**HTTP/2 request timing granularity.** Over HTTP/1 the `WroteRequest` and
`GotFirstResponseByte` hooks are observed at the socket, matching Go. Over
HTTP/2 a connection is shared by concurrent streams, so socket events cannot be
attributed to one request; there the hooks bracket the per-request send/response
futures instead. Totals and all other columns are unaffected.

**OS error text** is derived from the platform `strerror` with the first letter
lowercased, matching Go's errno tables on the platforms tested. A platform whose
table diverges from `strerror` would produce different wording.
