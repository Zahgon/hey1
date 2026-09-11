//! Shim for the handful of `fmt` verbs hey's output depends on.
//!
//! The report is byte-compared against the Go original, so the NaN/Inf
//! spellings and the width padding rules matter.

/// Go: `fmt.Sprintf("%<width>.<prec>f", v)`
///
/// Go renders non-finite float64 as "NaN", "+Inf", "-Inf" and then applies the
/// same right-alignment padding it applies to numbers. `%4.4f` of NaN is
/// therefore " NaN" (padded to width 4), not "NaN".
pub fn sprintf_f(v: f64, width: usize, prec: usize) -> String {
    let s = if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            "+Inf".to_string()
        } else {
            "-Inf".to_string()
        }
    } else {
        format_fixed(v, prec)
    };
    pad_left(s, width)
}

fn pad_left(s: String, width: usize) -> String {
    let n = s.chars().count();
    if n < width {
        let mut out = String::with_capacity(width);
        for _ in 0..(width - n) {
            out.push(' ');
        }
        out.push_str(&s);
        out
    } else {
        s
    }
}

/// Fixed-point rendering matching Go's `strconv.FormatFloat(v, 'f', prec, 64)`.
///
/// Go rounds the *exact* binary value to `prec` places, breaking exact ties
/// away from zero. Rust's `{:.*}` also rounds the exact binary value but
/// breaks exact ties to even. A tie at 4 decimal places requires the binary
/// value to terminate in exactly 5 at the 5th place, which needs a factor of
/// 1/10^5 -- not representable in binary -- so for the precisions hey uses the
/// two agree. `format_fixed` is kept as a seam in case that ever stops being
/// true.
fn format_fixed(v: f64, prec: usize) -> String {
    format!("{:.*}", prec, v)
}

/// Go: `fmt.Sprintf("%d", v)`
pub fn sprintf_d(v: i64) -> String {
    v.to_string()
}

/// Go's default `%v` formatting for the scalar types that reach the templates.
pub fn sprint_v_f64(v: f64) -> String {
    // Go's %v for float64 is %g with shortest-round-trip precision.
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v.is_sign_positive() { "+Inf" } else { "-Inf" }.to_string();
    }
    let mut s = format!("{}", v);
    // Rust prints "1" for 1.0_f64; Go prints "1" as well for %v/%g.
    if s == "-0" {
        s = "-0".to_string();
    }
    s
}

#[cfg(test)]
#[path = "gofmt_test.rs"]
mod gofmt_test;
