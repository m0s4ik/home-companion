// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! ALSA pcm backend (interleaved S16_LE, blocking), from spike-audio, measured on the device:
//! period from the configuration, buffer 4 periods, playback start threshold 2 periods,
//! EPIPE -> snd_pcm_prepare (counted), ESTRPIPE -> resume loop.
//!
//! Only pcm devices are opened: the card's controls are never touched.

use super::{Capture, Pacer, Playback, StreamSpec, Xruns};
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use std::io;
use std::time::{Duration, Instant};

pub(crate) fn alsa_err(e: alsa::Error) -> io::Error {
    io::Error::other(e)
}

/// Opens a pcm with interleaved S16_LE, the exact rate and channel count, period from
/// `spec`, buffer = 4 periods. Fails when the pcm does not accept the exact values.
fn open_pcm(name: &str, dir: Direction, spec: &StreamSpec) -> io::Result<(PCM, usize, usize)> {
    let pcm = PCM::new(name, dir, false).map_err(alsa_err)?;
    let (period, buffer) = {
        let hwp = HwParams::any(&pcm).map_err(alsa_err)?;
        hwp.set_access(Access::RWInterleaved).map_err(alsa_err)?;
        hwp.set_format(Format::S16LE).map_err(alsa_err)?;
        hwp.set_channels(spec.channels as u32).map_err(|e| {
            io::Error::other(format!(
                "{name}: {} channels refused ({e}); run `audio-bench probe`",
                spec.channels
            ))
        })?;
        hwp.set_rate_resample(false).map_err(alsa_err)?;
        hwp.set_rate(spec.rate, ValueOr::Nearest).map_err(|e| {
            io::Error::other(format!(
                "{name}: {} Hz refused ({e}); run `audio-bench probe`",
                spec.rate
            ))
        })?;
        let want = spec.period_frames() as alsa::pcm::Frames;
        let period = hwp
            .set_period_size_near(want, ValueOr::Nearest)
            .map_err(alsa_err)?;
        let buffer = hwp.set_buffer_size_near(period * 4).map_err(alsa_err)?;
        pcm.hw_params(&hwp).map_err(alsa_err)?;
        (period as usize, buffer as usize)
    };
    {
        let swp = pcm.sw_params_current().map_err(alsa_err)?;
        // Playback starts after two periods are queued; capture starts on the first read.
        let threshold = if dir == Direction::Playback {
            (period * 2) as i64
        } else {
            1
        };
        swp.set_start_threshold(threshold).map_err(alsa_err)?;
        pcm.sw_params(&swp).map_err(alsa_err)?;
    }
    pcm.prepare().map_err(alsa_err)?;
    Ok((pcm, period, buffer))
}

/// ALSA's `null` plugin accepts everything at once; real devices block in writei/readi.
fn is_null(name: &str) -> bool {
    name == "null" || name.starts_with("null:")
}

/// Recovers a pcm after an xrun (counted) or a suspend; other errors are returned.
fn recover(pcm: &PCM, err: alsa::Error, xr: &mut Xruns, playback: bool) -> io::Result<()> {
    match err.errno() {
        libc::EPIPE => {
            if playback {
                xr.underruns += 1;
            } else {
                xr.overruns += 1;
            }
            pcm.prepare().map_err(alsa_err)?;
        }
        libc::ESTRPIPE => {
            while pcm.resume().is_err_and(|e| e.errno() == libc::EAGAIN) {
                std::thread::sleep(Duration::from_millis(10));
            }
            pcm.prepare().map_err(alsa_err)?;
        }
        _ => return Err(alsa_err(err)),
    }
    xr.recoveries += 1;
    Ok(())
}

pub(crate) struct AlsaPlayback {
    name: String,
    spec: StreamSpec,
    pcm: Option<PCM>,
    period: usize,
    xruns: Xruns,
    pacer: Pacer,
}

impl AlsaPlayback {
    /// `open_now = false` starts closed (close policy: opened on the first sound).
    pub fn new(name: &str, spec: StreamSpec, open_now: bool) -> io::Result<Self> {
        let mut me = Self {
            name: name.into(),
            period: spec.period_frames(),
            spec,
            pcm: None,
            xruns: Xruns::default(),
            pacer: Pacer::new(1, 0, false),
        };
        if open_now {
            me.resume()?;
        }
        Ok(me)
    }
}

impl Playback for AlsaPlayback {
    fn channels(&self) -> usize {
        self.spec.channels
    }

    fn period_frames(&self) -> usize {
        self.period
    }

    fn write(&mut self, buf: &[i16]) -> io::Result<()> {
        let ch = self.spec.channels;
        let pcm = self
            .pcm
            .as_ref()
            .ok_or_else(|| io::Error::other("pcm is closed"))?;
        let io = pcm.io_i16().map_err(alsa_err)?;
        let mut done = 0;
        while done < buf.len() {
            match io.writei(&buf[done..]) {
                Ok(frames) => done += frames * ch,
                Err(e) => recover(pcm, e, &mut self.xruns, true)?,
            }
        }
        self.pacer.advance(buf.len() / ch);
        Ok(())
    }

    fn delay_frames(&mut self) -> Option<i64> {
        self.pcm.as_ref()?.delay().ok()
    }

    fn xruns(&self) -> Xruns {
        self.xruns
    }

    fn suspend(&mut self) -> io::Result<()> {
        if let Some(pcm) = self.pcm.take() {
            // drain fails harmlessly on a pcm that never started; nothing is left to play then.
            let _ = pcm.drain();
        }
        Ok(())
    }

    fn resume(&mut self) -> io::Result<Duration> {
        let t = Instant::now();
        let (pcm, period, buffer) = open_pcm(&self.name, Direction::Playback, &self.spec)?;
        let took = t.elapsed();
        self.pcm = Some(pcm);
        self.period = period;
        // The `null` plugin never blocks: pace it like a card with this buffer would.
        self.pacer = Pacer::new(self.spec.rate, buffer as u64, is_null(&self.name));
        Ok(took)
    }

    fn finish(&mut self) -> io::Result<()> {
        self.suspend()
    }

    fn is_open(&self) -> bool {
        self.pcm.is_some()
    }
}

pub(crate) struct AlsaCapture {
    pcm: PCM,
    channels: usize,
    period: usize,
    xruns: Xruns,
    pacer: Pacer,
}

impl AlsaCapture {
    pub fn new(name: &str, spec: StreamSpec) -> io::Result<Self> {
        let (pcm, period, _) = open_pcm(name, Direction::Capture, &spec)?;
        Ok(Self {
            pcm,
            channels: spec.channels,
            period,
            xruns: Xruns::default(),
            pacer: Pacer::new(spec.rate, 0, is_null(name)),
        })
    }
}

impl Capture for AlsaCapture {
    fn channels(&self) -> usize {
        self.channels
    }

    fn period_frames(&self) -> usize {
        self.period
    }

    fn read(&mut self, buf: &mut [i16]) -> io::Result<()> {
        let io = self.pcm.io_i16().map_err(alsa_err)?;
        let mut done = 0;
        while done < buf.len() {
            match io.readi(&mut buf[done..]) {
                Ok(frames) => done += frames * self.channels,
                Err(e) => recover(&self.pcm, e, &mut self.xruns, false)?,
            }
        }
        self.pacer.advance(buf.len() / self.channels);
        Ok(())
    }

    fn xruns(&self) -> Xruns {
        self.xruns
    }
}

/// What a pcm accepts (hw_params tests only, nothing is changed), as JSON.
pub(crate) fn probe(name: &str, playback: bool) -> io::Result<serde_json::Value> {
    let dir = if playback {
        Direction::Playback
    } else {
        Direction::Capture
    };
    // Non-blocking open: a busy device fails at once instead of hanging.
    let pcm = PCM::new(name, dir, true).map_err(|e| {
        io::Error::other(format!(
            "{name}: cannot open ({e}); is another program using it?"
        ))
    })?;
    let hwp = HwParams::any(&pcm).map_err(alsa_err)?;
    let rates: Vec<u32> = [8000, 16000, 22050, 32000, 44100, 48000]
        .into_iter()
        .filter(|&r| hwp.test_rate(r).is_ok())
        .collect();
    let channels: Vec<u32> = [1, 2, 4]
        .into_iter()
        .filter(|&c| hwp.test_channels(c).is_ok())
        .collect();
    let formats: Vec<&str> = [
        (Format::S16LE, "S16_LE"),
        (Format::S24LE, "S24_LE"),
        (Format::S32LE, "S32_LE"),
    ]
    .into_iter()
    .filter(|(f, _)| hwp.test_format(*f).is_ok())
    .map(|(_, n)| n)
    .collect();
    Ok(serde_json::json!({
        "pcm": name,
        "direction": if playback { "playback" } else { "capture" },
        "rates_hz": rates,
        "rate_min": hwp.get_rate_min().ok(),
        "rate_max": hwp.get_rate_max().ok(),
        "channels": channels,
        "channels_min": hwp.get_channels_min().ok(),
        "channels_max": hwp.get_channels_max().ok(),
        "formats": formats,
        "access_rw_interleaved": hwp.test_access(Access::RWInterleaved).is_ok(),
        "access_mmap_interleaved": hwp.test_access(Access::MMapInterleaved).is_ok(),
        "period_frames_min": hwp.get_period_size_min().ok(),
        "period_frames_max": hwp.get_period_size_max().ok(),
        "buffer_frames_max": hwp.get_buffer_size_max().ok(),
    }))
}
