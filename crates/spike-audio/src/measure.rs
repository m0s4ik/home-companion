// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Small measurement helpers: CPU time (getrusage), peak RSS, percentiles, real-time pacing.

use std::time::{Duration, Instant};

/// CPU time (user + system) in seconds, of the whole process or of the calling thread.
pub fn cpu_seconds(thread: bool) -> f64 {
    let who = if thread {
        libc::RUSAGE_THREAD
    } else {
        libc::RUSAGE_SELF
    };
    // SAFETY: getrusage only writes into the zeroed struct we pass.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(who, &mut ru) } != 0 {
        return 0.0;
    }
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// Peak resident set size in MB (VmHWM from /proc/self/status).
pub fn rss_max_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

/// min / p50 / p95 / max / mean of a list of values (all 0 and count 0 when empty).
#[derive(Debug, Clone, Copy, Default)]
pub struct Summary {
    pub count: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

impl Summary {
    pub fn of(values: &[f64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mut v = values.to_vec();
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        Self {
            count: v.len(),
            min: v[0],
            p50: at(0.5),
            p95: at(0.95),
            max: v[v.len() - 1],
            mean: v.iter().sum::<f64>() / v.len() as f64,
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "count": self.count, "min": round3(self.min), "p50": round3(self.p50),
            "p95": round3(self.p95), "max": round3(self.max), "mean": round3(self.mean),
        })
    }

    pub fn line(&self, unit: &str) -> String {
        if self.count == 0 {
            return "n/a".into();
        }
        format!(
            "min {:.2} / p50 {:.2} / p95 {:.2} / max {:.2} {unit} (n={})",
            self.min, self.p50, self.p95, self.max, self.count
        )
    }
}

pub fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Keeps a stream from running faster than real time when the backend does not block
/// (ALSA `null`, file backend). `lead` is how many frames may be ahead of the wall clock,
/// like a sound card buffer. On real hardware the pcm blocks first and this never sleeps.
pub struct Pacer {
    start: Option<Instant>,
    rate: f64,
    frames: u64,
    lead: u64,
    enabled: bool,
}

impl Pacer {
    pub fn new(rate: u32, lead: u64, enabled: bool) -> Self {
        Self {
            start: None,
            rate: rate as f64,
            frames: 0,
            lead,
            enabled,
        }
    }

    /// Call after `n` frames went through the stream.
    pub fn advance(&mut self, n: usize) {
        let start = *self.start.get_or_insert_with(Instant::now);
        self.frames += n as u64;
        if !self.enabled {
            return;
        }
        let due = start.elapsed().as_secs_f64() * self.rate + self.lead as f64;
        let ahead = self.frames as f64 - due;
        if ahead > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(ahead / self.rate));
        }
    }

    /// Frames handed over but not yet "played" by the wall clock (a stand-in for snd_pcm_delay).
    pub fn ahead_frames(&self) -> Option<i64> {
        let start = self.start?;
        let played = start.elapsed().as_secs_f64() * self.rate;
        Some((self.frames as f64 - played).max(0.0) as i64)
    }
}

/// Tries SCHED_FIFO priority 10 for the calling thread. Returns a description; never fails.
pub fn try_realtime(thread_name: &str) -> String {
    let param = libc::sched_param { sched_priority: 10 };
    // SAFETY: pthread_self is always valid; param is a plain struct.
    let rc = unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if rc == 0 {
        format!("{thread_name}: SCHED_FIFO 10 granted")
    } else {
        let msg = format!(
            "{thread_name}: SCHED_FIFO 10 refused ({}), running with normal priority",
            std::io::Error::from_raw_os_error(rc)
        );
        eprintln!("warning: {msg}");
        msg
    }
}

/// RMS of i16-scale f32 samples in dBFS (-120 for silence).
pub fn dbfs(sum_sq: f64, n: u64) -> f64 {
    if n == 0 || sum_sq <= 0.0 {
        return -120.0;
    }
    let rms = (sum_sq / n as f64).sqrt() / 32768.0;
    (20.0 * rms.log10()).max(-120.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_percentiles() {
        let s = Summary::of(&[5.0, 1.0, 3.0, 2.0, 4.0]);
        assert_eq!((s.min, s.p50, s.max, s.count), (1.0, 3.0, 5.0, 5));
        assert_eq!(Summary::of(&[]).count, 0);
    }

    #[test]
    fn cpu_and_rss_are_readable() {
        assert!(cpu_seconds(false) >= 0.0);
        assert!(rss_max_mb() > 0.0);
    }
}
