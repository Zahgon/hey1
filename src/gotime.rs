//! Shim for the parts of Go's `time` package that hey relies on.
//!
//! Go's `time.Duration` is an **i64 nanosecond count that may be negative**.
//! `std::time::Duration` is unsigned and panics on overflow, so a direct
//! substitution would change behaviour for the (reachable) cases where hey
//! subtracts a trace timestamp that was never set. `GoDuration` keeps the
//! original i64 semantics, including wrap-free negative results.

use std::fmt;
use std::ops::{Add, AddAssign, Sub};

pub const NANOSECOND: i64 = 1;
pub const MICROSECOND: i64 = 1000 * NANOSECOND;
pub const MILLISECOND: i64 = 1000 * MICROSECOND;
pub const SECOND: i64 = 1000 * MILLISECOND;
pub const MINUTE: i64 = 60 * SECOND;
pub const HOUR: i64 = 60 * MINUTE;

/// Go's `time.Duration`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct GoDuration(pub i64);

impl GoDuration {
    pub const ZERO: GoDuration = GoDuration(0);

    pub fn from_nanos(n: i64) -> Self {
        GoDuration(n)
    }

    pub fn nanoseconds(self) -> i64 {
        self.0
    }

    /// Go: `func (d Duration) Seconds() float64`
    ///
    /// Reproduced term by term -- Go deliberately splits whole seconds from
    /// the remainder instead of dividing by 1e9, which keeps precision for
    /// large durations.
    pub fn seconds(self) -> f64 {
        let sec = self.0 / SECOND;
        let nsec = self.0 % SECOND;
        sec as f64 + (nsec as f64) / 1e9
    }

    pub fn to_std(self) -> std::time::Duration {
        if self.0 <= 0 {
            std::time::Duration::ZERO
        } else {
            std::time::Duration::from_nanos(self.0 as u64)
        }
    }

    pub fn from_std(d: std::time::Duration) -> Self {
        GoDuration(d.as_nanos() as i64)
    }
}

impl Sub for GoDuration {
    type Output = GoDuration;
    fn sub(self, rhs: GoDuration) -> GoDuration {
        GoDuration(self.0.wrapping_sub(rhs.0))
    }
}

impl Add for GoDuration {
    type Output = GoDuration;
    fn add(self, rhs: GoDuration) -> GoDuration {
        GoDuration(self.0.wrapping_add(rhs.0))
    }
}

impl AddAssign for GoDuration {
    fn add_assign(&mut self, rhs: GoDuration) {
        self.0 = self.0.wrapping_add(rhs.0);
    }
}

/// Go's `time.Duration.String()`. Only needed for error/usage text parity.
impl fmt::Display for GoDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.0;
        if d == 0 {
            return write!(f, "0s");
        }
        let neg = d < 0;
        let mut u = (d as i128).unsigned_abs();
        let mut out = String::new();
        if u < SECOND as u128 {
            // Sub-second: use ns/us/ms with a fractional part.
            let (unit, prec) = if u < MICROSECOND as u128 {
                ("ns", 0)
            } else if u < MILLISECOND as u128 {
                ("\u{00b5}s", 3)
            } else {
                ("ms", 6)
            };
            out.push_str(&fmt_frac(&mut u, prec));
            out.push_str(unit);
            out = format!("{}{}", fmt_int(u), out);
        } else {
            let mut tail = String::new();
            tail.push_str(&fmt_frac(&mut u, 9));
            tail.push('s');
            let secs = u % 60;
            u /= 60;
            tail = format!("{}{}", fmt_int(secs), tail);
            if u > 0 {
                let mins = u % 60;
                u /= 60;
                tail = format!("{}m{}", fmt_int(mins), tail);
                if u > 0 {
                    tail = format!("{}h{}", fmt_int(u), tail);
                }
            }
            out = tail;
        }
        if neg {
            write!(f, "-{}", out)
        } else {
            write!(f, "{}", out)
        }
    }
}

fn fmt_frac(v: &mut u128, prec: usize) -> String {
    let mut print = false;
    let mut buf = Vec::new();
    for _ in 0..prec {
        let digit = *v % 10;
        print = print || digit != 0;
        if print {
            buf.push(b'0' + digit as u8);
        }
        *v /= 10;
    }
    if print {
        buf.push(b'.');
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

fn fmt_int(mut v: u128) -> String {
    if v == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while v > 0 {
        buf.push(b'0' + (v % 10) as u8);
        v /= 10;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

/// Go: `time.ParseDuration`. Used by the `-z` flag.
///
/// Accepts a possibly signed sequence of decimal numbers each with an optional
/// fraction and a unit suffix, e.g. "300ms", "-1.5h", "2h45m".
pub fn parse_duration(s: &str) -> Result<GoDuration, String> {
    let orig = s;
    let mut s = s;
    let mut d: i128 = 0;
    let mut neg = false;

    // Consume [-+]
    if let Some(rest) = s.strip_prefix('-') {
        neg = true;
        s = rest;
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    }

    // Special case: "0" retains its old meaning of 0s.
    if s == "0" {
        return Ok(GoDuration(0));
    }
    if s.is_empty() {
        return Err(format!("time: invalid duration {:?}", orig));
    }

    while !s.is_empty() {
        // The next character must be [0-9.]
        if !(s.starts_with('.') || s.starts_with(|c: char| c.is_ascii_digit())) {
            return Err(format!("time: invalid duration {:?}", orig));
        }
        // Consume [0-9]*
        let pl = s.len();
        let (v, rest) = leading_int(s).map_err(|_| format!("time: invalid duration {:?}", orig))?;
        let mut v = v as i128;
        s = rest;
        let pre = pl != s.len();

        // Consume (\.[0-9]*)?
        let mut post = false;
        let mut f: i128 = 0;
        let mut scale: f64 = 1.0;
        if s.starts_with('.') {
            s = &s[1..];
            let pl = s.len();
            let (nf, nscale, rest) = leading_fraction(s);
            f = nf as i128;
            scale = nscale;
            s = rest;
            post = pl != s.len();
        }
        if !pre && !post {
            return Err(format!("time: invalid duration {:?}", orig));
        }

        // Consume unit.
        let unit_end = s
            .char_indices()
            .find(|(_, c)| *c == '.' || c.is_ascii_digit())
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        if unit_end == 0 {
            return Err(format!("time: missing unit in duration {:?}", orig));
        }
        let u = &s[..unit_end];
        s = &s[unit_end..];
        let unit: i128 = match u {
            "ns" => NANOSECOND as i128,
            "us" | "\u{00b5}s" | "\u{03bc}s" => MICROSECOND as i128,
            "ms" => MILLISECOND as i128,
            "s" => SECOND as i128,
            "m" => MINUTE as i128,
            "h" => HOUR as i128,
            _ => return Err(format!("time: unknown unit {:?} in duration {:?}", u, orig)),
        };

        if v > (1i128 << 63) / unit {
            return Err(format!("time: invalid duration {:?}", orig));
        }
        v *= unit;
        if f > 0 {
            v += (f as f64 * (unit as f64 / scale)) as i128;
            if v < 0 {
                return Err(format!("time: invalid duration {:?}", orig));
            }
        }
        d += v;
        if d < 0 {
            return Err(format!("time: invalid duration {:?}", orig));
        }
    }

    if neg {
        d = -d;
    }
    if d > i64::MAX as i128 || d < i64::MIN as i128 {
        return Err(format!("time: invalid duration {:?}", orig));
    }
    Ok(GoDuration(d as i64))
}

fn leading_int(s: &str) -> Result<(i64, &str), ()> {
    let mut x: i64 = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        let c = (bytes[i] - b'0') as i64;
        if x > (i64::MAX - c) / 10 {
            return Err(());
        }
        x = x * 10 + c;
        i += 1;
    }
    Ok((x, &s[i..]))
}

fn leading_fraction(s: &str) -> (i64, f64, &str) {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut x: i64 = 0;
    let mut scale = 1.0f64;
    let mut overflow = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        let c = (bytes[i] - b'0') as i64;
        if overflow || x > (1i64 << 62) / 10 {
            overflow = true;
        } else {
            let y = x * 10 + c;
            if y < 0 {
                overflow = true;
            } else {
                x = y;
            }
        }
        scale *= 10.0;
        i += 1;
    }
    (x, scale, &s[i..])
}

#[cfg(test)]
#[path = "gotime_test.rs"]
mod gotime_test;
