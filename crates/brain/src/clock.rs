// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Local time for the scheduler (the device's time zone, from libc), quiet hours and RFC 3339;
//! the injectable [`Clock`] of the Yes/No questions, the turns and the undo (tests drive a
//! [`ManualClock`] instead of sleeping).

use std::sync::atomic::{AtomicI64, Ordering};

/// Milliseconds since the epoch, from somewhere: the wall clock, or a test's hand.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// The wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    }
}

/// A clock that moves only when told (tests).
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(start_ms: i64) -> ManualClock {
        ManualClock(AtomicI64::new(start_ms))
    }

    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A moment on the local clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    /// Days since 1970-01-01 on the local calendar (changes at local midnight).
    pub day: i64,
    /// Minutes after local midnight.
    pub minute: u32,
    /// 0 = Sunday.
    pub weekday: u32,
    /// Seconds east of UTC.
    pub offset: i64,
}

pub fn local(ts: i64) -> Local {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes into `tm` only.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return Local {
            day: ts.div_euclid(86400),
            minute: (ts.rem_euclid(86400) / 60) as u32,
            weekday: ((ts.div_euclid(86400) + 4).rem_euclid(7)) as u32,
            offset: 0,
        };
    }
    let offset = tm.tm_gmtoff;
    Local {
        day: (ts + offset).div_euclid(86400),
        minute: (tm.tm_hour * 60 + tm.tm_min) as u32,
        weekday: tm.tm_wday as u32,
        offset,
    }
}

/// "+01:00" for the system prompt.
pub fn offset_text(offset: i64) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let o = offset.abs();
    format!("{sign}{:02}:{:02}", o / 3600, (o % 3600) / 60)
}

/// Inside the quiet hours `start..end` (minutes after midnight; across midnight when start > end).
pub fn quiet(minute: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        (start..end).contains(&minute)
    } else {
        minute >= start || minute < end
    }
}

/// The next moment the local clock shows `minute` (after `ts`), to the minute.
pub fn next_at(ts: i64, minute: u32) -> i64 {
    let l = local(ts);
    let wait = (minute as i64 - l.minute as i64).rem_euclid(1440);
    let wait = if wait == 0 { 1440 } else { wait };
    ts - ts.rem_euclid(60) + wait * 60
}

// RFC 3339 parsing: written with Gemini (agy), reviewed and tested here.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    fn parse_2(b: &[u8]) -> Option<u32> {
        if b.len() >= 2 && b[0].is_ascii_digit() && b[1].is_ascii_digit() {
            Some((b[0] - b'0') as u32 * 10 + (b[1] - b'0') as u32)
        } else {
            None
        }
    }

    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return None;
    }

    if bytes[0..4].iter().any(|c| !c.is_ascii_digit()) {
        return None;
    }
    let year = (bytes[0] - b'0') as i64 * 1000
        + (bytes[1] - b'0') as i64 * 100
        + (bytes[2] - b'0') as i64 * 10
        + (bytes[3] - b'0') as i64;
    if !(1970..=9999).contains(&year) {
        return None;
    }

    if bytes[4] != b'-' {
        return None;
    }

    let month = parse_2(&bytes[5..7])?;
    if !(1..=12).contains(&month) {
        return None;
    }

    if bytes[7] != b'-' {
        return None;
    }

    let day = parse_2(&bytes[8..10])?;
    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let max_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if day < 1 || day > max_days {
        return None;
    }

    let mut rem = &bytes[10..];
    if rem.is_empty() {
        return None;
    }
    let sep = rem[0];
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    rem = &rem[1..];

    if rem.len() < 5 {
        return None;
    }
    let hour = parse_2(&rem[0..2])?;
    if hour > 23 {
        return None;
    }
    if rem[2] != b':' {
        return None;
    }
    let minute = parse_2(&rem[3..5])?;
    if minute > 59 {
        return None;
    }
    rem = &rem[5..];

    let mut second = 0u32;
    if rem.starts_with(b":") {
        rem = &rem[1..];
        if rem.len() < 2 {
            return None;
        }
        let sec = parse_2(&rem[0..2])?;
        if sec > 60 {
            return None;
        }
        second = sec;
        rem = &rem[2..];

        if rem.starts_with(b".") {
            rem = &rem[1..];
            if rem.is_empty() || !rem[0].is_ascii_digit() {
                return None;
            }
            let mut idx = 0;
            while idx < rem.len() && rem[idx].is_ascii_digit() {
                idx += 1;
            }
            rem = &rem[idx..];
        }
    }

    let offset_seconds: i64 = if rem == b"Z" || rem == b"z" {
        0
    } else if rem.starts_with(b"+") || rem.starts_with(b"-") {
        let sign: i64 = if rem[0] == b'+' { 1 } else { -1 };
        let off_rem = &rem[1..];
        let (off_h, off_m) = if off_rem.len() == 5 && off_rem[2] == b':' {
            let h = parse_2(&off_rem[0..2])?;
            let m = parse_2(&off_rem[3..5])?;
            (h, m)
        } else if off_rem.len() == 4 {
            let h = parse_2(&off_rem[0..2])?;
            let m = parse_2(&off_rem[2..4])?;
            (h, m)
        } else {
            return None;
        };
        if off_h > 23 || off_m > 59 {
            return None;
        }
        sign * (off_h as i64 * 3600 + off_m as i64 * 60)
    } else {
        return None;
    };

    let days = days_from_civil(year, month, day);
    let total_seconds =
        days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64 - offset_seconds;
    Some(total_seconds)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as u64;
    era * 146097 + doe as i64 - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-05T18:00:00+01:00"), Some(1793898000));
        assert_eq!(
            parse_rfc3339("2026-11-05T17:00:00Z"),
            parse_rfc3339("2026-11-05T18:00:00+01:00")
        );
        assert_eq!(parse_rfc3339("2026-11-05T17:00:00Z"), Some(1793898000));
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1709208000));
        assert_eq!(parse_rfc3339("2023-02-29T12:00:00Z"), None);
        assert_eq!(parse_rfc3339("2026-11-05 18:00+01:00"), Some(1793898000));
        assert_eq!(
            parse_rfc3339("2026-11-05T17:00:00.123456Z"),
            Some(1793898000)
        );
        assert_eq!(parse_rfc3339("2026-11-05T12:00:00-05:00"), Some(1793898000));
        assert_eq!(parse_rfc3339("2026-11-05T18:00:00+0100"), Some(1793898000));
        assert_eq!(parse_rfc3339("2026-11-05T18:00:00"), None);
        assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("garbage"), None);
        assert_eq!(parse_rfc3339("2026-11-05T18:00:00+01:00x"), None);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 11, 5), 20762);
        assert_eq!(parse_rfc3339(""), None);
        assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), None);
    }

    #[test]
    fn quiet_hours_across_midnight() {
        let (s, e) = (23 * 60, 7 * 60);
        assert!(quiet(23 * 60 + 30, s, e));
        assert!(quiet(3 * 60, s, e));
        assert!(!quiet(7 * 60, s, e));
        assert!(!quiet(12 * 60, s, e));
        assert!(quiet(13 * 60, 12 * 60, 14 * 60));
        assert!(!quiet(14 * 60, 12 * 60, 14 * 60));
        assert!(!quiet(0, 0, 0));
    }

    #[test]
    fn offsets_and_next() {
        assert_eq!(offset_text(3600), "+01:00");
        assert_eq!(offset_text(-16200), "-04:30");
        let t = 1_800_000_000;
        let n = next_at(t, local(t).minute);
        assert_eq!(n - (t - t.rem_euclid(60)), 86400);
    }
}
