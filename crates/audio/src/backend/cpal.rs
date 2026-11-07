// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! cpal backend for the simulator (macOS, desktop Linux).
//!
//! cpal drives audio with callbacks of whatever size the host likes, so this backend makes
//! them look like a card to the shared playback and capture threads:
//!
//! - playback: our playback thread mixes 20 ms periods at the device's rate and channel count
//!   and "writes" them into a small queue (at most 4 periods; `write` blocks like a full card
//!   buffer); the output callback drains it and plays silence (counted as an underrun) when it
//!   is empty. The mixer runs at the device's rate (`Audio::output_rate`).
//! - capture: the input callback converts what it gets to f32 and passes it through a bounded
//!   channel (never blocking; a full channel counts as an overrun); `read` resamples each
//!   channel to 16 kHz with a windowed-sinc resampler when the device runs at another rate
//!   (anti-aliasing before decimation) and returns exact periods. The shared capture thread
//!   then picks the channel and re-chunks to 20 ms frames as for every backend.

use super::{Capture, Opened, Playback, Xruns, capture_spec, playback_spec};
use crate::resample::{Quality, StreamResampler};
use crate::{AudioConfig, CAPTURE_RATE, Error, lock};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

fn dev_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Device(format!("cpal {what}: {e}"))
}

/// "", "default" and ALSA names such as "hw:0,0" (the device defaults) mean the host's default.
fn wants_default(name: &str) -> bool {
    name.is_empty() || name == "default" || name.starts_with("hw:") || name.starts_with("plughw:")
}

fn find_device(host: &cpal::Host, name: &str, input: bool) -> Result<cpal::Device, Error> {
    let what = if input { "input" } else { "output" };
    if wants_default(name) {
        let d = if input {
            host.default_input_device()
        } else {
            host.default_output_device()
        };
        return d.ok_or_else(|| Error::Device(format!("cpal: no default {what} device")));
    }
    let devices: Vec<cpal::Device> = if input {
        host.input_devices()
            .map_err(|e| dev_err(what, e))?
            .collect()
    } else {
        host.output_devices()
            .map_err(|e| dev_err(what, e))?
            .collect()
    };
    let mut names = Vec::new();
    for d in devices {
        let label = d
            .description()
            .map(|x| x.name().to_string())
            .unwrap_or_default();
        let id = d.id().map(|x| x.to_string()).unwrap_or_default();
        if label == name || id == name {
            return Ok(d);
        }
        names.push(label);
    }
    Err(Error::Device(format!(
        "cpal: no {what} device named {name:?} (found: {})",
        names.join(", ")
    )))
}

pub(crate) fn open(cfg: &AudioConfig, stop: Arc<AtomicBool>) -> Result<Opened, Error> {
    let host = cpal::default_host();
    let out_dev = find_device(&host, &cfg.playback_device, false)?;
    let out_cfg = out_dev
        .default_output_config()
        .map_err(|e| dev_err("output config", e))?;
    let rate = out_cfg.sample_rate();
    let channels = out_cfg.channels().max(1) as usize;
    let mut pspec = playback_spec(cfg);
    pspec.rate = rate;
    pspec.channels = channels;
    let period = pspec.period_frames();
    let ring = Arc::new(OutRing {
        q: Mutex::new(VecDeque::with_capacity(period * channels * 5)),
        cond: Condvar::new(),
        underruns: AtomicU64::new(0),
    });
    let sc = out_cfg.config();
    let out_stream = match out_cfg.sample_format() {
        SampleFormat::F32 => build_output::<f32>(&out_dev, sc, ring.clone()),
        SampleFormat::I16 => build_output::<i16>(&out_dev, sc, ring.clone()),
        SampleFormat::I32 => build_output::<i32>(&out_dev, sc, ring.clone()),
        SampleFormat::U16 => build_output::<u16>(&out_dev, sc, ring.clone()),
        f => Err(Error::Unsupported(format!("cpal output sample format {f}"))),
    }?;
    out_stream.start().map_err(|e| dev_err("output start", e))?;

    let (capture, in_stream, in_desc) = open_input(cfg, stop.clone())?;
    let out_name = out_dev
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_default();
    Ok(Opened {
        playback: Box::new(CpalPlayback {
            ring,
            channels,
            period,
            max_queued: period * 4 * channels,
            stop,
        }),
        capture: Box::new(capture),
        output_rate: rate,
        realtime: true,
        description: format!(
            "cpal out {out_name:?} {rate} Hz {channels} ch {}, {in_desc}",
            out_cfg.sample_format()
        ),
        keep: Some(Box::new((out_stream, in_stream))),
    })
}

/// Only the capture side, for `meter`.
pub(crate) fn open_capture_only(
    cfg: &AudioConfig,
    stop: Arc<AtomicBool>,
) -> Result<Box<dyn Capture>, Error> {
    let (c, s, _) = open_input(cfg, stop)?;
    Ok(Box::new(KeepAlive { c, _s: s }))
}

struct KeepAlive {
    c: CpalCapture,
    _s: cpal::Stream,
}

impl Capture for KeepAlive {
    fn channels(&self) -> usize {
        self.c.channels()
    }
    fn period_frames(&self) -> usize {
        self.c.period_frames()
    }
    fn read(&mut self, buf: &mut [i16]) -> io::Result<()> {
        self.c.read(buf)
    }
    fn xruns(&self) -> Xruns {
        self.c.xruns()
    }
}

fn open_input(
    cfg: &AudioConfig,
    stop: Arc<AtomicBool>,
) -> Result<(CpalCapture, cpal::Stream, String), Error> {
    let host = cpal::default_host();
    let dev = find_device(&host, &cfg.capture_device, true)?;
    let in_cfg = dev
        .default_input_config()
        .map_err(|e| dev_err("input config", e))?;
    let rate = in_cfg.sample_rate();
    let dev_ch = in_cfg.channels().max(1) as usize;
    let (tx, rx) = sync_channel::<Vec<f32>>(64);
    let overruns = Arc::new(AtomicU64::new(0));
    let sc = in_cfg.config();
    let stream = match in_cfg.sample_format() {
        SampleFormat::F32 => build_input::<f32>(&dev, sc, tx, overruns.clone()),
        SampleFormat::I16 => build_input::<i16>(&dev, sc, tx, overruns.clone()),
        SampleFormat::I32 => build_input::<i32>(&dev, sc, tx, overruns.clone()),
        SampleFormat::U16 => build_input::<u16>(&dev, sc, tx, overruns.clone()),
        f => Err(Error::Unsupported(format!("cpal input sample format {f}"))),
    }?;
    stream.start().map_err(|e| dev_err("input start", e))?;
    let spec = capture_spec(cfg);
    let channels = spec.channels;
    let quality = if rate > CAPTURE_RATE {
        Quality::Sinc
    } else {
        Quality::Cubic
    };
    let name = dev
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_default();
    let desc = format!(
        "in {name:?} {rate} Hz {dev_ch} ch {}{}",
        in_cfg.sample_format(),
        if rate != CAPTURE_RATE {
            " (resampled to 16 kHz)"
        } else {
            ""
        }
    );
    Ok((
        CpalCapture {
            rx,
            dev_ch,
            channels,
            period: spec.period_frames(),
            rs: (0..channels)
                .map(|_| StreamResampler::new(rate, CAPTURE_RATE, quality))
                .collect(),
            out: VecDeque::new(),
            tmp: Vec::new(),
            overruns,
            stop,
        },
        stream,
        desc,
    ))
}

struct OutRing {
    q: Mutex<VecDeque<i16>>,
    cond: Condvar,
    underruns: AtomicU64,
}

fn build_output<T>(
    dev: &cpal::Device,
    sc: cpal::StreamConfig,
    ring: Arc<OutRing>,
) -> Result<cpal::Stream, Error>
where
    T: SizedSample + FromSample<i16> + Send + 'static,
{
    dev.build_output_stream::<T, _, _>(
        sc,
        move |data: &mut [T], _info: &cpal::CallbackInfo| {
            let mut q = lock(&ring.q);
            let n = data.len().min(q.len());
            for (d, s) in data[..n].iter_mut().zip(q.drain(..n)) {
                *d = T::from_sample(s);
            }
            drop(q);
            if n < data.len() {
                for d in &mut data[n..] {
                    *d = T::from_sample(0i16);
                }
                ring.underruns.fetch_add(1, Ordering::Relaxed);
            }
            ring.cond.notify_all();
        },
        |e| log::warn!("cpal output: {e}"),
        None,
    )
    .map_err(|e| dev_err("output stream", e))
}

fn build_input<T>(
    dev: &cpal::Device,
    sc: cpal::StreamConfig,
    tx: SyncSender<Vec<f32>>,
    overruns: Arc<AtomicU64>,
) -> Result<cpal::Stream, Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    dev.build_input_stream::<T, _, _>(
        sc,
        move |data: &[T], _info: &cpal::CallbackInfo| {
            let v: Vec<f32> = data.iter().map(|&s| f32::from_sample(s)).collect();
            if let Err(TrySendError::Full(_)) = tx.try_send(v) {
                overruns.fetch_add(1, Ordering::Relaxed);
            }
        },
        |e| log::warn!("cpal input: {e}"),
        None,
    )
    .map_err(|e| dev_err("input stream", e))
}

struct CpalPlayback {
    ring: Arc<OutRing>,
    channels: usize,
    period: usize,
    max_queued: usize,
    stop: Arc<AtomicBool>,
}

impl Playback for CpalPlayback {
    fn channels(&self) -> usize {
        self.channels
    }

    fn period_frames(&self) -> usize {
        self.period
    }

    fn write(&mut self, buf: &[i16]) -> io::Result<()> {
        let mut q = lock(&self.ring.q);
        while q.len() + buf.len() > self.max_queued && !self.stop.load(Ordering::Relaxed) {
            q = match self.ring.cond.wait_timeout(q, Duration::from_millis(50)) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        q.extend(buf.iter().copied());
        Ok(())
    }

    fn delay_frames(&mut self) -> Option<i64> {
        Some((lock(&self.ring.q).len() / self.channels) as i64)
    }

    fn xruns(&self) -> Xruns {
        Xruns {
            underruns: self.ring.underruns.load(Ordering::Relaxed),
            ..Xruns::default()
        }
    }

    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn resume(&mut self) -> io::Result<Duration> {
        Ok(Duration::ZERO)
    }

    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CpalCapture {
    rx: Receiver<Vec<f32>>,
    dev_ch: usize,
    channels: usize,
    period: usize,
    rs: Vec<StreamResampler>,
    /// Interleaved at 16 kHz, `channels` wide.
    out: VecDeque<i16>,
    tmp: Vec<f32>,
    overruns: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl CpalCapture {
    fn feed(&mut self, data: &[f32]) {
        for c in 0..self.channels {
            let src = c.min(self.dev_ch - 1);
            self.tmp.clear();
            self.tmp
                .extend(data.chunks_exact(self.dev_ch).map(|f| f[src] * 32768.0));
            self.rs[c].push(&self.tmp);
        }
        let ready = self.rs.iter().map(|r| r.available()).min().unwrap_or(0);
        if ready == 0 {
            return;
        }
        let mut per: Vec<Vec<f32>> = vec![vec![0.0; ready]; self.channels];
        for (r, p) in self.rs.iter_mut().zip(per.iter_mut()) {
            r.pop(p);
        }
        for i in 0..ready {
            for p in &per {
                self.out
                    .push_back(p[i].round().clamp(-32768.0, 32767.0) as i16);
            }
        }
    }
}

impl Capture for CpalCapture {
    fn channels(&self) -> usize {
        self.channels
    }

    fn period_frames(&self) -> usize {
        self.period
    }

    fn read(&mut self, buf: &mut [i16]) -> io::Result<()> {
        while self.out.len() < buf.len() {
            if self.stop.load(Ordering::Relaxed) {
                buf.fill(0);
                return Ok(());
            }
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(data) => self.feed(&data),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("cpal input stream ended"));
                }
            }
        }
        let n = buf.len();
        for (b, s) in buf.iter_mut().zip(self.out.drain(..n)) {
            *b = s;
        }
        Ok(())
    }

    fn xruns(&self) -> Xruns {
        Xruns {
            overruns: self.overruns.load(Ordering::Relaxed),
            ..Xruns::default()
        }
    }
}
