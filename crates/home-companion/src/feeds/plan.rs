// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! When each request of the feeds is due: pure, on a clock passed in (seconds, monotonic), so
//! every rule is a test. One [`Job`] per thing fetched (a Yahoo symbol; all the coins; all the
//! cities): it waits its interval after a success, doubles it after each error up to six
//! hours, honours a `Retry-After`, and always takes ±10% so that a room full of devices does
//! not ask at the same second. While the feeds are not needed (the page off, the night, the
//! screen off) nothing is asked; when they are needed again what is overdue goes within 20 s.

/// The longest wait, whatever the errors or the server say.
pub const MAX_WAIT: f64 = 6.0 * 3600.0;
/// Yahoo: every 15 minutes while its market is open, every hour while it is closed.
pub const YAHOO_OPEN: f64 = 15.0 * 60.0;
pub const YAHOO_CLOSED: f64 = 60.0 * 60.0;
/// The coins, all in one request (price, change and the day's curve).
pub const COINGECKO: f64 = 10.0 * 60.0;
/// The cities, all in one request.
pub const OPENMETEO: f64 = 30.0 * 60.0;
/// Two Yahoo requests are at least this far apart.
pub const YAHOO_GAP: f64 = 3.0;
/// Errors in a row on Yahoo's first host before its second one is tried, and for how long.
pub const FALLBACK_AFTER: u32 = 3;
pub const FALLBACK_FOR: f64 = 6.0 * 3600.0;

/// One thing fetched on its own schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// When it is due next (on the feeds' clock); 0 = at once.
    pub due: f64,
    /// Errors in a row.
    pub errors: u32,
}

impl Default for Job {
    fn default() -> Self {
        Job::new()
    }
}

impl Job {
    pub fn new() -> Job {
        Job {
            due: 0.0,
            errors: 0,
        }
    }

    pub fn is_due(&self, now: f64) -> bool {
        now >= self.due
    }

    /// Done: the next one an interval from now. `jitter` in -1..=1 (±10%).
    pub fn success(&mut self, now: f64, interval: f64, jitter: f64) {
        self.errors = 0;
        self.due = now + interval * (1.0 + 0.1 * jitter.clamp(-1.0, 1.0));
    }

    /// Failed: the interval doubled per error in a row, up to [`MAX_WAIT`]; a server's
    /// `Retry-After` wins, but never shorter than the interval nor longer than the maximum.
    pub fn failure(&mut self, now: f64, interval: f64, retry_after: Option<f64>, jitter: f64) {
        self.errors = self.errors.saturating_add(1);
        let backoff = (interval * 2f64.powi(self.errors.min(16) as i32)).min(MAX_WAIT);
        let wait = match retry_after {
            Some(r) => r.clamp(interval, MAX_WAIT),
            None => backoff,
        };
        self.due = now + (wait * (1.0 + 0.1 * jitter.clamp(-1.0, 1.0))).min(MAX_WAIT * 1.1);
    }

    /// The feeds are needed again: an overdue job goes `spread` seconds from now (0-20 s,
    /// chosen at random by the caller).
    pub fn wake(&mut self, now: f64, spread: f64) {
        if self.due <= now {
            self.due = now + spread.clamp(0.0, 20.0);
        }
    }
}

/// Which of Yahoo's hosts is used now: the first, unless it refused us lately.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct YahooHost {
    /// Errors in a row on the first host.
    errors: u32,
    /// The second host until then.
    second_until: Option<f64>,
}

impl YahooHost {
    pub fn second(&self, now: f64) -> bool {
        self.second_until.is_some_and(|t| now < t)
    }

    /// An answer from the host in use: `refused` for 401, 403 and 429 (the host does not want
    /// us: switch at once), `ok` for a good one. Returns true when it just switched.
    pub fn answer(&mut self, now: f64, ok: bool, refused: bool) -> bool {
        if self.second(now) {
            return false;
        }
        if self.second_until.is_some() {
            // The six hours are over: back to the first, with a clean slate.
            self.second_until = None;
            self.errors = 0;
        }
        if ok {
            self.errors = 0;
            return false;
        }
        self.errors += 1;
        if refused || self.errors >= FALLBACK_AFTER {
            self.second_until = Some(now + FALLBACK_FOR);
            self.errors = 0;
            return true;
        }
        false
    }
}

/// Whether the feeds may ask anything now (see the module's doc).
pub fn active(page_on: bool, has_items: bool, night: bool, screen_on: bool) -> bool {
    page_on && has_items && !night && screen_on
}

/// A small random number in -1..=1 (no crate: the clock's nanoseconds and a counter, mixed).
pub fn jitter() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0x2545_f491_4f6c_dd1d);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = N.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed) ^ t;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    (x % 2001) as f64 / 1000.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_waits_the_interval_with_a_tenth_either_way() {
        let mut j = Job::new();
        assert!(j.is_due(0.0));
        j.success(100.0, 900.0, 0.0);
        assert_eq!(j.due, 1000.0);
        j.success(100.0, 900.0, 1.0);
        assert_eq!(j.due, 1090.0);
        j.success(100.0, 900.0, -5.0);
        assert_eq!(j.due, 910.0);
        assert!(!j.is_due(909.0) && j.is_due(910.0));
    }

    #[test]
    fn errors_double_up_to_six_hours_and_a_success_resets() {
        let mut j = Job::new();
        let mut waits = Vec::new();
        for _ in 0..8 {
            j.failure(0.0, 900.0, None, 0.0);
            waits.push(j.due);
        }
        assert_eq!(&waits[..4], [1800.0, 3600.0, 7200.0, 14400.0]);
        assert!(waits[4..].iter().all(|w| *w == MAX_WAIT));
        j.success(0.0, 900.0, 0.0);
        assert_eq!((j.due, j.errors), (900.0, 0));
    }

    #[test]
    fn retry_after_is_honoured_within_limits() {
        let mut j = Job::new();
        j.failure(0.0, 600.0, Some(1200.0), 0.0);
        assert_eq!(j.due, 1200.0);
        // Shorter than the interval: the interval.
        j.failure(0.0, 600.0, Some(5.0), 0.0);
        assert_eq!(j.due, 600.0);
        // Longer than six hours: six hours.
        j.failure(0.0, 600.0, Some(1e9), 0.0);
        assert_eq!(j.due, MAX_WAIT);
    }

    #[test]
    fn waking_moves_only_what_is_overdue() {
        let mut late = Job {
            due: 50.0,
            errors: 0,
        };
        late.wake(100.0, 7.0);
        assert_eq!(late.due, 107.0);
        let mut later = Job {
            due: 500.0,
            errors: 0,
        };
        later.wake(100.0, 7.0);
        assert_eq!(later.due, 500.0);
        let mut j = Job {
            due: 0.0,
            errors: 0,
        };
        j.wake(100.0, 99.0);
        assert_eq!(j.due, 120.0);
    }

    #[test]
    fn yahoo_falls_back_and_comes_back() {
        let mut y = YahooHost::default();
        assert!(!y.second(0.0));
        assert!(!y.answer(0.0, false, false));
        assert!(!y.answer(1.0, false, false));
        assert!(y.answer(2.0, false, false), "the third error in a row");
        assert!(y.second(3.0));
        assert!(y.second(2.0 + FALLBACK_FOR - 1.0));
        assert!(!y.second(2.0 + FALLBACK_FOR));
        // Back on the first: a success keeps it there.
        assert!(!y.answer(2.0 + FALLBACK_FOR, true, false));
        assert!(!y.second(2.0 + FALLBACK_FOR + 1.0));
        // A refusal switches at once; a success in between resets the count.
        let mut y = YahooHost::default();
        y.answer(0.0, false, false);
        y.answer(1.0, true, false);
        y.answer(2.0, false, false);
        assert!(!y.second(3.0));
        assert!(y.answer(4.0, false, true));
        assert!(y.second(5.0));
    }

    #[test]
    fn active_only_when_shown() {
        assert!(active(true, true, false, true));
        assert!(!active(false, true, false, true));
        assert!(!active(true, false, false, true));
        assert!(!active(true, true, true, true));
        assert!(!active(true, true, false, false));
    }

    #[test]
    fn jitter_stays_in_range_and_varies() {
        let v: Vec<f64> = (0..200).map(|_| jitter()).collect();
        assert!(v.iter().all(|x| (-1.0..=1.0).contains(x)));
        assert!(v.iter().any(|x| *x > 0.3) && v.iter().any(|x| *x < -0.3));
    }
}
