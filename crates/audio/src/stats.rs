// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Counters shared by the audio threads, CPU time per thread, SCHED_FIFO.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// A snapshot of the audio counters ([`crate::Audio::stats`]).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Stats {
    pub underruns: u64,
    pub overruns: u64,
    /// Successful snd_pcm_prepare / resume after an xrun or a suspend.
    pub recoveries: u64,
    pub capture_frames: u64,
    /// Frames dropped because a subscriber's queue was full (summed over subscribers).
    pub capture_dropped: u64,
    /// Periods in which a started answer stream had no data before its `finish` (silence
    /// was played instead).
    pub stream_starved: u64,
    /// CPU time of the playback thread (with cpal: our mixing thread, not cpal's callback).
    pub playback_cpu_s: f64,
    /// CPU time of the capture thread (channel pick, AGC, level, recorder, fan-out).
    pub capture_cpu_s: f64,
    /// Audio queued in the playback pcm (snd_pcm_delay), when the backend knows it.
    pub queued_ms: Option<f64>,
    pub pcm_open: bool,
    /// Seconds since `Audio::start`.
    pub uptime_s: f64,
    /// Seconds of audio captured (capture frames x 20 ms).
    pub capture_audio_s: f64,
    /// Seconds of audio written to the playback pcm.
    pub playback_audio_s: f64,
}

/// f64 stored in an AtomicU64.
#[derive(Default)]
pub(crate) struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

pub(crate) struct StatsShared {
    pub underruns: AtomicU64,
    pub overruns: AtomicU64,
    pub play_recoveries: AtomicU64,
    pub cap_recoveries: AtomicU64,
    pub capture_frames: AtomicU64,
    pub capture_dropped: AtomicU64,
    pub stream_starved: AtomicU64,
    pub playback_cpu_s: AtomicF64,
    pub capture_cpu_s: AtomicF64,
    /// NaN = unknown.
    pub queued_ms: AtomicF64,
    pub pcm_open: AtomicBool,
    pub playback_audio_s: AtomicF64,
    pub started: Instant,
}

impl StatsShared {
    pub fn new() -> Self {
        let s = Self {
            underruns: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            play_recoveries: AtomicU64::new(0),
            cap_recoveries: AtomicU64::new(0),
            capture_frames: AtomicU64::new(0),
            capture_dropped: AtomicU64::new(0),
            stream_starved: AtomicU64::new(0),
            playback_cpu_s: AtomicF64::default(),
            capture_cpu_s: AtomicF64::default(),
            queued_ms: AtomicF64::default(),
            pcm_open: AtomicBool::new(false),
            playback_audio_s: AtomicF64::default(),
            started: Instant::now(),
        };
        s.queued_ms.set(f64::NAN);
        s
    }

    pub fn snapshot(&self) -> Stats {
        let q = self.queued_ms.get();
        let frames = self.capture_frames.load(Ordering::Relaxed);
        Stats {
            underruns: self.underruns.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            recoveries: self.play_recoveries.load(Ordering::Relaxed)
                + self.cap_recoveries.load(Ordering::Relaxed),
            capture_frames: frames,
            capture_dropped: self.capture_dropped.load(Ordering::Relaxed),
            stream_starved: self.stream_starved.load(Ordering::Relaxed),
            playback_cpu_s: self.playback_cpu_s.get(),
            capture_cpu_s: self.capture_cpu_s.get(),
            queued_ms: (!q.is_nan()).then_some(q),
            pcm_open: self.pcm_open.load(Ordering::Relaxed),
            uptime_s: self.started.elapsed().as_secs_f64(),
            capture_audio_s: frames as f64 * crate::FRAME_MS as f64 / 1000.0,
            playback_audio_s: self.playback_audio_s.get(),
        }
    }
}

/// CPU time (user + system) of the calling thread, in seconds.
pub(crate) fn thread_cpu_seconds() -> f64 {
    clock_seconds(libc::CLOCK_THREAD_CPUTIME_ID)
}

/// CPU time of the whole process, in seconds.
pub fn process_cpu_seconds() -> f64 {
    clock_seconds(libc::CLOCK_PROCESS_CPUTIME_ID)
}

fn clock_seconds(clock: libc::clockid_t) -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime only writes into the struct we pass.
    if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
        return 0.0;
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Tries SCHED_FIFO priority 10 for the calling thread; logs the outcome, never fails.
pub(crate) fn try_realtime(thread_name: &str) -> bool {
    let param = libc::sched_param { sched_priority: 10 };
    // SAFETY: pthread_self is always valid; param is a plain struct.
    let rc = unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if rc == 0 {
        log::info!("{thread_name}: SCHED_FIFO 10 granted");
        true
    } else {
        log::warn!(
            "{thread_name}: SCHED_FIFO 10 refused ({}), running with normal priority",
            std::io::Error::from_raw_os_error(rc)
        );
        false
    }
}

/// RMS of i16-scale values in dBFS (-120 for silence).
pub(crate) fn dbfs(sum_sq: f64, n: u64) -> f64 {
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
    fn cpu_clock_moves() {
        let a = thread_cpu_seconds();
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_mul(31).wrapping_add(i);
        }
        std::hint::black_box(x);
        assert!(thread_cpu_seconds() > a);
        assert!(process_cpu_seconds() > 0.0);
    }

    #[test]
    fn dbfs_of_full_scale_sine() {
        // A full-scale sine has an RMS of -3.01 dBFS.
        let n = 48000;
        let sum: f64 = (0..n)
            .map(|i| {
                let s = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48000.0).sin() * 32767.0;
                s * s
            })
            .sum();
        assert!((dbfs(sum, n) + 3.01).abs() < 0.01);
        assert_eq!(dbfs(0.0, 10), -120.0);
    }
}
