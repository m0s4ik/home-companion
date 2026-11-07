// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Helpers shared by the integration tests (no sound device is ever opened).

#![allow(dead_code)]

use audio::{Audio, AudioConfig, AudioEvent, Backend, ChannelPick, SourceId};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

/// A fresh directory under the target's test tmpdir.
pub fn tmpdir(name: &str) -> PathBuf {
    let d =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("audio-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn file_cfg(
    capture: Option<PathBuf>,
    playback: Option<PathBuf>,
    realtime: bool,
) -> AudioConfig {
    AudioConfig {
        backend: Backend::File {
            capture,
            playback,
            realtime,
        },
        capture_channels: 2,
        capture_channel: ChannelPick::Left,
        realtime: false,
        volume: 1.0,
        ..AudioConfig::default()
    }
}

pub fn start(cfg: &AudioConfig) -> (Audio, Receiver<AudioEvent>) {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let a = Audio::start(
        cfg,
        Box::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        }),
    )
    .expect("audio starts");
    (a, rx)
}

/// Waits for `Finished(id)` (or `Stopped(id)` when `stopped`), collecting every event seen.
pub fn wait_event(
    rx: &Receiver<AudioEvent>,
    seen: &mut Vec<AudioEvent>,
    timeout: Duration,
    pred: impl Fn(&AudioEvent) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(e) => {
                let hit = pred(&e);
                seen.push(e);
                if hit {
                    return true;
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

pub fn is_finished(id: SourceId) -> impl Fn(&AudioEvent) -> bool {
    move |e| matches!(e, AudioEvent::Finished(x) if *x == id)
}

/// Left channel of a 16-bit WAV, with its rate.
pub fn read_left(path: &Path) -> (Vec<i16>, u32) {
    let mut r = hound::WavReader::open(path).unwrap();
    let spec = r.spec();
    let ch = spec.channels as usize;
    let s: Vec<i16> = r.samples::<i16>().map(Result::unwrap).collect();
    (s.iter().step_by(ch).copied().collect(), spec.sample_rate)
}

pub fn rms_dbfs(s: &[i16]) -> f64 {
    let sum: f64 = s.iter().map(|&x| (x as f64).powi(2)).sum();
    if sum == 0.0 {
        return -120.0;
    }
    20.0 * ((sum / s.len() as f64).sqrt() / 32768.0).log10()
}

/// Rising zero crossings.
pub fn crossings(s: &[i16]) -> usize {
    s.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count()
}

pub fn write_wav(path: &Path, rate: u32, channels: u16, s: &[i16]) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &x in s {
        w.write_sample(x).unwrap();
    }
    w.finalize().unwrap();
}

pub fn sine(rate: u32, freq: f64, n: usize, amp: f64, phase: f64) -> Vec<i16> {
    (0..n)
        .map(|i| {
            ((2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64 + phase).sin()
                * amp
                * 32767.0)
                .round() as i16
        })
        .collect()
}

/// Quadratic matched filter for an 880 Hz, 200 ms beep with 10 ms fades (spike-audio's
/// check-mix.py): (onset frame, level in dBFS) of the best match within `span` around `t`.
pub fn find_beep(left: &[i16], rate: u32, t: f64, span: f64) -> (usize, f64) {
    let n_t = rate as usize / 5;
    let fade = rate as usize / 100;
    let env: Vec<f64> = (0..n_t)
        .map(|i| (i.min(n_t - 1 - i) as f64 / fade as f64).min(1.0))
        .collect();
    let w = |i: usize| 2.0 * std::f64::consts::PI * 880.0 * i as f64 / rate as f64;
    let tc: Vec<f64> = env
        .iter()
        .enumerate()
        .map(|(i, e)| e * w(i).cos())
        .collect();
    let ts: Vec<f64> = env
        .iter()
        .enumerate()
        .map(|(i, e)| e * w(i).sin())
        .collect();
    let lo = ((t - span) * rate as f64).max(0.0) as usize;
    let hi = (((t + span) * rate as f64) as usize).min(left.len().saturating_sub(n_t));
    let (mut best, mut onset) = (-1.0f64, lo);
    for i in lo..hi {
        let seg = &left[i..i + n_t];
        let c: f64 = seg.iter().zip(&tc).map(|(&x, y)| x as f64 * y).sum();
        let q: f64 = seg.iter().zip(&ts).map(|(&x, y)| x as f64 * y).sum();
        if c * c + q * q > best {
            best = c * c + q * q;
            onset = i;
        }
    }
    let e2: f64 = env.iter().map(|e| e * e).sum();
    let amp = 2.0 * best.max(0.0).sqrt() / e2;
    (onset, 20.0 * (amp.max(1e-9) / 32768.0).log10())
}
