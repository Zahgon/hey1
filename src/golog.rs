//! Minimal stand-in for Go's `log` package.
//!
//! report.go calls `log.Println("error:", err.Error())` when template
//! execution fails. Go's default logger writes to stderr with a
//! "YYYY/MM/DD HH:MM:SS " local-time prefix; that prefix is reproduced so the
//! failure mode looks the same.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn println_err(msg: &str) {
    eprintln!("{} {}", timestamp(), msg);
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = local_civil(secs);
    format!("{:04}/{:02}/{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
}

#[cfg(unix)]
fn local_offset(secs: i64) -> i64 {
    // Derive the UTC offset by asking libc for the local broken-down time and
    // diffing it against the UTC one.
    extern "C" {
        fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: i64,
        tm_zone: *const i8,
    }
    // `Tm` holds a raw pointer, which has no `Default`, so every field is
    // zeroed explicitly rather than derived.
    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    unsafe {
        if localtime_r(&secs as *const i64, &mut tm as *mut Tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff
    }
}

#[cfg(not(unix))]
fn local_offset(_secs: i64) -> i64 {
    0
}

fn local_civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = secs + local_offset(secs);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    (
        y,
        mo,
        d,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
