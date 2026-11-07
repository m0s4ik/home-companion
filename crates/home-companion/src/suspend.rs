// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The whole device to sleep (Settings → Suspend, Andrea, 2026-11-04): a system suspend
//! (s2idle, the only mode of the ThinkSmart View's kernel: under 0.5 W). Vol+ or Vol− wakes it,
//! and so does the RTC: if a timer is pending, an RTC alarm is set shortly before it rings, so
//! it still rings. Touch does not wake it, and nothing is heard while it sleeps.
//!
//! The body writes `/sys/power/state` and `/sys/class/rtc/rtc0/wakealarm` itself, as user
//! `home-companion` without polkit or sudo: packaging/home-companion.service gives group `home-companion` write access to both
//! at start (`ExecStartPre=+`, like the CPU governors). `HOME_COMPANION_SUSPEND_DRY_RUN=1` checks that
//! both can be opened for writing and logs what would be written, without writing.
//!
//! The PMIC's RTC is not on the epoch (it counts from its own zero), so the alarm is written
//! relative to now (`+N`).

use std::io::Write;
use std::time::{Duration, SystemTime};

pub const STATE: &str = "/sys/power/state";
pub const WAKEALARM: &str = "/sys/class/rtc/rtc0/wakealarm";
/// The device is up this long before a timer is due (resume, Wi-Fi, the screen).
pub const EARLY: f64 = 10.0;
/// The shortest alarm (the kernel needs a moment to go to sleep).
pub const MIN_ALARM: u64 = 5;

/// Seconds from `now` to the RTC alarm for the next timer due at `next_due` (both seconds
/// since the epoch); None without one.
pub fn alarm_in(now: f64, next_due: Option<f64>) -> Option<u64> {
    let due = next_due.filter(|d| d.is_finite())?;
    let secs = (due - EARLY - now).floor();
    Some(if secs < MIN_ALARM as f64 {
        MIN_ALARM
    } else {
        secs as u64
    })
}

/// The dry-run switch from the environment.
pub fn dry_run() -> bool {
    std::env::var("HOME_COMPANION_SUSPEND_DRY_RUN").is_ok_and(|v| !v.is_empty() && v != "0")
}

fn open(path: &str) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))
}

fn write(path: &str, value: &str) -> Result<(), String> {
    open(path)?
        .write_all(value.as_bytes())
        .map_err(|e| format!("{path}: write {value:?}: {e}"))
}

/// Suspends now, with an RTC alarm `alarm` seconds from now if Some; returns when the device
/// is back (or at once in a dry run), with the time it slept (wall clock).
pub fn suspend(alarm: Option<u64>, dry: bool) -> Result<Duration, String> {
    if dry {
        open(STATE)?;
        open(WAKEALARM)?;
        match alarm {
            Some(n) => log::info!("suspend (dry run): would write \"+{n}\" to {WAKEALARM}"),
            None => log::info!("suspend (dry run): no timer, no RTC alarm"),
        }
        log::info!("suspend (dry run): would write \"mem\" to {STATE}");
        return Ok(Duration::ZERO);
    }
    // An alarm already set refuses a new one: clear it first (and so an old one never wakes us).
    write(WAKEALARM, "0")?;
    if let Some(n) = alarm {
        write(WAKEALARM, &format!("+{n}"))?;
    }
    let before = SystemTime::now();
    // Blocks until the device is back.
    let r = write(STATE, "mem");
    let slept = SystemTime::now()
        .duration_since(before)
        .unwrap_or(Duration::ZERO);
    if alarm.is_some() {
        let _ = write(WAKEALARM, "0");
    }
    r.map(|()| slept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_alarm_before_the_next_timer() {
        assert_eq!(alarm_in(1000.0, None), None);
        assert_eq!(alarm_in(1000.0, Some(f64::NAN)), None);
        // Ten seconds early.
        assert_eq!(alarm_in(1000.0, Some(1600.0)), Some(590));
        assert_eq!(alarm_in(1000.5, Some(1600.0)), Some(589));
        // Soon or already due: the shortest alarm.
        assert_eq!(alarm_in(1000.0, Some(1012.0)), Some(MIN_ALARM));
        assert_eq!(alarm_in(1000.0, Some(900.0)), Some(MIN_ALARM));
    }
}
