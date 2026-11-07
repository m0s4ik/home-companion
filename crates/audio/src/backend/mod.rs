// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Playback and capture backends behind two small traits (from spike-audio): ALSA, cpal,
//! null and WAV files. Only pcm devices are opened here; there is no mixer-control code.

#[cfg(all(target_os = "linux", feature = "alsa"))]
pub(crate) mod alsa;
#[cfg(feature = "cpal")]
pub(crate) mod cpal;
pub(crate) mod file;
pub(crate) mod null;

use crate::{AudioConfig, Backend, CAPTURE_RATE, Error, OUTPUT_CHANNELS, OUTPUT_RATE};
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// xrun counters of one direction.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Xruns {
    /// EPIPE on playback.
    pub underruns: u64,
    /// EPIPE on capture.
    pub overruns: u64,
    /// Successful snd_pcm_prepare / resume after an xrun or suspend.
    pub recoveries: u64,
}

pub(crate) trait Playback: Send {
    fn channels(&self) -> usize;
    fn period_frames(&self) -> usize;
    /// Writes interleaved frames, blocking like a card; recovers from underruns and counts them.
    fn write(&mut self, buf: &[i16]) -> io::Result<()>;
    /// Frames queued but not yet played (snd_pcm_delay), when known.
    fn delay_frames(&mut self) -> Option<i64>;
    /// Same as `delay_frames`, clamped to 0.
    #[allow(dead_code)]
    fn queued_frames(&mut self) -> Option<u64> {
        self.delay_frames().map(|d| d.max(0) as u64)
    }
    fn xruns(&self) -> Xruns;
    /// Lets queued audio play out, then releases the device (close policy).
    fn suspend(&mut self) -> io::Result<()>;
    /// Opens the device again. Returns how long it took.
    fn resume(&mut self) -> io::Result<Duration>;
    /// Lets queued audio play out at the end (file: finalizes the WAV).
    fn finish(&mut self) -> io::Result<()>;
    fn is_open(&self) -> bool {
        true
    }
    /// Non-real-time file backend only: whether the mixer has something to play right now
    /// (then periods are written as fast as possible, otherwise paced like a card).
    fn hint_busy(&mut self, _busy: bool) {}
}

pub(crate) trait Capture: Send {
    fn channels(&self) -> usize;
    fn period_frames(&self) -> usize;
    /// Reads exactly one period of interleaved frames at 16 kHz, blocking like a card.
    fn read(&mut self, buf: &mut [i16]) -> io::Result<()>;
    fn xruns(&self) -> Xruns;
}

/// What to open and how.
#[derive(Debug, Clone)]
pub(crate) struct StreamSpec {
    pub rate: u32,
    pub channels: usize,
    pub period_ms: u32,
}

impl StreamSpec {
    pub fn period_frames(&self) -> usize {
        (self.rate as usize * self.period_ms as usize / 1000).max(16)
    }
}

pub(crate) struct Opened {
    pub playback: Box<dyn Playback>,
    pub capture: Box<dyn Capture>,
    pub output_rate: u32,
    /// false: file backend in non-real-time mode.
    pub realtime: bool,
    pub description: String,
    /// Objects that must live as long as the audio (cpal streams).
    pub keep: Option<Box<dyn std::any::Any + Send>>,
}

pub(crate) fn playback_spec(cfg: &AudioConfig) -> StreamSpec {
    StreamSpec {
        rate: OUTPUT_RATE,
        channels: OUTPUT_CHANNELS as usize,
        period_ms: cfg.period_ms.clamp(5, 200),
    }
}

pub(crate) fn capture_spec(cfg: &AudioConfig) -> StreamSpec {
    StreamSpec {
        rate: CAPTURE_RATE,
        channels: cfg.capture_channels as usize,
        period_ms: cfg.period_ms.clamp(5, 200),
    }
}

pub(crate) fn open(cfg: &AudioConfig, stop: Arc<AtomicBool>) -> Result<Opened, Error> {
    match &cfg.backend {
        Backend::Alsa => open_alsa(cfg),
        Backend::Cpal => open_cpal(cfg, stop),
        Backend::Null => Ok(Opened {
            playback: Box::new(null::NullPlayback::new(&playback_spec(cfg))),
            capture: Box::new(null::NullCapture::new(&capture_spec(cfg))),
            output_rate: OUTPUT_RATE,
            realtime: true,
            description: "null".into(),
            keep: None,
        }),
        Backend::File {
            capture,
            playback,
            realtime,
        } => file::open(
            cfg,
            capture.as_deref(),
            playback.as_deref(),
            *realtime,
            stop,
        ),
    }
}

#[cfg(all(target_os = "linux", feature = "alsa"))]
fn open_alsa(cfg: &AudioConfig) -> Result<Opened, Error> {
    let p = alsa::AlsaPlayback::new(&cfg.playback_device, playback_spec(cfg), true)
        .map_err(|e| Error::Device(format!("playback {}: {e}", cfg.playback_device)))?;
    let c = alsa::AlsaCapture::new(&cfg.capture_device, capture_spec(cfg))
        .map_err(|e| Error::Device(format!("capture {}: {e}", cfg.capture_device)))?;
    let description = format!(
        "alsa {} period {} frames, {} period {} frames",
        cfg.playback_device,
        p.period_frames(),
        cfg.capture_device,
        c.period_frames()
    );
    Ok(Opened {
        playback: Box::new(p),
        capture: Box::new(c),
        output_rate: OUTPUT_RATE,
        realtime: true,
        description,
        keep: None,
    })
}

#[cfg(not(all(target_os = "linux", feature = "alsa")))]
fn open_alsa(_cfg: &AudioConfig) -> Result<Opened, Error> {
    Err(Error::Feature("alsa"))
}

#[cfg(feature = "cpal")]
fn open_cpal(cfg: &AudioConfig, stop: Arc<AtomicBool>) -> Result<Opened, Error> {
    cpal::open(cfg, stop)
}

#[cfg(not(feature = "cpal"))]
fn open_cpal(_cfg: &AudioConfig, _stop: Arc<AtomicBool>) -> Result<Opened, Error> {
    Err(Error::Feature("cpal"))
}

/// Opens only the capture side (used by `meter`, which never touches the speaker).
pub(crate) fn open_capture(
    cfg: &AudioConfig,
    stop: Arc<AtomicBool>,
) -> Result<Box<dyn Capture>, Error> {
    match &cfg.backend {
        #[cfg(all(target_os = "linux", feature = "alsa"))]
        Backend::Alsa => Ok(Box::new(
            alsa::AlsaCapture::new(&cfg.capture_device, capture_spec(cfg))
                .map_err(|e| Error::Device(format!("capture {}: {e}", cfg.capture_device)))?,
        )),
        #[cfg(not(all(target_os = "linux", feature = "alsa")))]
        Backend::Alsa => Err(Error::Feature("alsa")),
        #[cfg(feature = "cpal")]
        Backend::Cpal => cpal::open_capture_only(cfg, stop),
        #[cfg(not(feature = "cpal"))]
        Backend::Cpal => {
            let _ = stop;
            Err(Error::Feature("cpal"))
        }
        Backend::Null => Ok(Box::new(null::NullCapture::new(&capture_spec(cfg)))),
        Backend::File { capture, .. } => Ok(Box::new(file::FileCapture::new(
            capture.as_deref(),
            &capture_spec(cfg),
            true,
            None,
        )?)),
    }
}

/// Keeps a stream from running faster than real time when the backend does not block
/// (null, file). `lead` is how many frames may be ahead of the wall clock, like a sound card
/// buffer. On real hardware the pcm blocks first and this never sleeps.
pub(crate) struct Pacer {
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
