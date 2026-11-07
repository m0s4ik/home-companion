// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Playback and capture backends: ALSA (interleaved S16_LE, blocking) and WAV files.
//!
//! This module only opens pcm devices. It has no mixer-control code on purpose: the spike
//! must never change the speaker volume.

use crate::measure::Pacer;
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use std::io;
use std::time::{Duration, Instant};

/// xrun counters, shared by both directions.
#[derive(Debug, Default, Clone, Copy)]
pub struct Xruns {
    /// EPIPE on playback.
    pub underruns: u64,
    /// EPIPE on capture.
    pub overruns: u64,
    /// Successful snd_pcm_prepare / resume after an xrun or suspend.
    pub recoveries: u64,
}

pub trait Playback: Send {
    fn channels(&self) -> usize;
    fn period_frames(&self) -> usize;
    /// Writes interleaved frames, blocking; recovers from underruns and counts them.
    fn write(&mut self, buf: &[i16]) -> io::Result<()>;
    /// Frames queued but not yet played (snd_pcm_delay), when known.
    fn delay_frames(&mut self) -> Option<i64>;
    fn xruns(&self) -> Xruns;
    /// Lets queued audio play out, then releases the device (close policy).
    fn suspend(&mut self) -> io::Result<()>;
    /// Opens the device again (open + hw_params + prepare). Returns how long it took.
    fn resume(&mut self) -> io::Result<Duration>;
    /// Lets queued audio play out at the end of a run.
    fn finish(&mut self) -> io::Result<()>;
}

pub trait Capture: Send {
    fn channels(&self) -> usize;
    fn period_frames(&self) -> usize;
    /// Reads exactly one period of interleaved frames, blocking; counts overruns.
    fn read(&mut self, buf: &mut [i16]) -> io::Result<()>;
    fn xruns(&self) -> Xruns;
}

/// What to open and how.
#[derive(Debug, Clone)]
pub struct StreamSpec {
    pub rate: u32,
    pub channels: usize,
    pub period_ms: u32,
}

impl StreamSpec {
    pub fn period_frames(&self) -> usize {
        (self.rate as usize * self.period_ms as usize / 1000).max(16)
    }
}

fn alsa_err(e: alsa::Error) -> io::Error {
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
                "{name}: {} channels refused ({e}); run `probe`",
                spec.channels
            ))
        })?;
        hwp.set_rate_resample(false).map_err(alsa_err)?;
        hwp.set_rate(spec.rate, ValueOr::Nearest).map_err(|e| {
            io::Error::other(format!(
                "{name}: {} Hz refused ({e}); run `probe`",
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

const EPIPE: i32 = libc::EPIPE;
const ESTRPIPE: i32 = libc::ESTRPIPE;

/// Recovers a pcm after an xrun (counted) or a suspend; other errors are returned.
fn recover(pcm: &PCM, err: alsa::Error, xr: &mut Xruns, playback: bool) -> io::Result<()> {
    match err.errno() {
        EPIPE => {
            if playback {
                xr.underruns += 1;
            } else {
                xr.overruns += 1;
            }
            pcm.prepare().map_err(alsa_err)?;
        }
        ESTRPIPE => {
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

pub struct AlsaPlayback {
    name: String,
    spec: StreamSpec,
    pcm: Option<PCM>,
    period: usize,
    buffer: usize,
    xruns: Xruns,
    pacer: Pacer,
}

impl AlsaPlayback {
    /// `open_now = false` starts closed (close policy: opened on the first sound).
    pub fn new(name: &str, spec: StreamSpec, open_now: bool) -> io::Result<Self> {
        let mut me = Self {
            name: name.into(),
            period: spec.period_frames(),
            buffer: spec.period_frames() * 4,
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

    pub fn buffer_frames(&self) -> usize {
        self.buffer
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
        self.buffer = buffer;
        // The `null` plugin never blocks: pace it like a card with this buffer would.
        self.pacer = Pacer::new(self.spec.rate, buffer as u64, is_null(&self.name));
        Ok(took)
    }

    fn finish(&mut self) -> io::Result<()> {
        self.suspend()
    }
}

pub struct AlsaCapture {
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

/// Writes the mixed stream to a WAV file, at real-time pace or as fast as possible.
pub struct FilePlayback {
    writer: Option<hound::WavWriter<io::BufWriter<std::fs::File>>>,
    channels: usize,
    period: usize,
    pacer: Pacer,
    realtime: bool,
}

impl FilePlayback {
    pub fn new(path: &str, spec: &StreamSpec, realtime: bool) -> io::Result<Self> {
        let wav = hound::WavSpec {
            channels: spec.channels as u16,
            sample_rate: spec.rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, wav).map_err(io::Error::other)?;
        let period = spec.period_frames();
        Ok(Self {
            writer: Some(writer),
            channels: spec.channels,
            period,
            pacer: Pacer::new(spec.rate, period as u64 * 4, realtime),
            realtime,
        })
    }
}

impl Playback for FilePlayback {
    fn channels(&self) -> usize {
        self.channels
    }

    fn period_frames(&self) -> usize {
        self.period
    }

    fn write(&mut self, buf: &[i16]) -> io::Result<()> {
        let w = self
            .writer
            .as_mut()
            .ok_or_else(|| io::Error::other("file is finished"))?;
        let mut w16 = w.get_i16_writer(buf.len() as u32);
        for &s in buf {
            w16.write_sample(s);
        }
        w16.flush().map_err(io::Error::other)?;
        self.pacer.advance(buf.len() / self.channels);
        Ok(())
    }

    fn delay_frames(&mut self) -> Option<i64> {
        if self.realtime {
            self.pacer.ahead_frames()
        } else {
            None
        }
    }

    fn xruns(&self) -> Xruns {
        Xruns::default()
    }

    // A file cannot be closed and reopened without losing the timeline, so the close
    // policy keeps the file and only measures the (zero) cost.
    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn resume(&mut self) -> io::Result<Duration> {
        Ok(Duration::ZERO)
    }

    fn finish(&mut self) -> io::Result<()> {
        if let Some(w) = self.writer.take() {
            w.finalize().map_err(io::Error::other)?;
        }
        Ok(())
    }
}

/// Reads a WAV (same rate; channels are averaged or duplicated) or produces silence.
pub struct FileCapture {
    samples: Vec<i16>,
    pos: usize,
    channels: usize,
    period: usize,
    pacer: Pacer,
}

impl FileCapture {
    pub fn new(path: Option<&str>, spec: &StreamSpec, realtime: bool) -> io::Result<Self> {
        let mut samples = Vec::new();
        if let Some(path) = path {
            let (mono, rate) = crate::mix::read_wav_mono(path)?;
            if rate != spec.rate {
                return Err(io::Error::other(format!(
                    "{path}: {rate} Hz, capture runs at {} Hz",
                    spec.rate
                )));
            }
            samples = mono
                .iter()
                .flat_map(|&s| std::iter::repeat_n(s, spec.channels))
                .collect();
        }
        Ok(Self {
            samples,
            pos: 0,
            channels: spec.channels,
            period: spec.period_frames(),
            pacer: Pacer::new(spec.rate, 0, realtime),
        })
    }
}

impl Capture for FileCapture {
    fn channels(&self) -> usize {
        self.channels
    }

    fn period_frames(&self) -> usize {
        self.period
    }

    fn read(&mut self, buf: &mut [i16]) -> io::Result<()> {
        let n = buf.len().min(self.samples.len().saturating_sub(self.pos));
        buf[..n].copy_from_slice(&self.samples[self.pos..self.pos + n]);
        buf[n..].fill(0);
        self.pos += n;
        self.pacer.advance(buf.len() / self.channels);
        Ok(())
    }

    fn xruns(&self) -> Xruns {
        Xruns::default()
    }
}

/// One line per tested value: what a pcm accepts (hw_params test only, nothing is changed).
pub fn probe(name: &str, dir: Direction) -> io::Result<serde_json::Value> {
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
    let access_rw = hwp.test_access(Access::RWInterleaved).is_ok();
    let access_mmap = hwp.test_access(Access::MMapInterleaved).is_ok();
    Ok(serde_json::json!({
        "pcm": name,
        "direction": if dir == Direction::Playback { "playback" } else { "capture" },
        "rates_hz": rates,
        "rate_min": hwp.get_rate_min().ok(),
        "rate_max": hwp.get_rate_max().ok(),
        "channels": channels,
        "channels_min": hwp.get_channels_min().ok(),
        "channels_max": hwp.get_channels_max().ok(),
        "formats": formats,
        "access_rw_interleaved": access_rw,
        "access_mmap_interleaved": access_mmap,
        "period_frames_min": hwp.get_period_size_min().ok(),
        "period_frames_max": hwp.get_period_size_max().ok(),
        "buffer_frames_max": hwp.get_buffer_size_max().ok(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("spike-audio-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.wav");
        let path = path.to_str().unwrap();
        let spec = StreamSpec {
            rate: 16000,
            channels: 1,
            period_ms: 20,
        };
        let data: Vec<i16> = (0..1600).map(|i| (i * 13 % 2000 - 1000) as i16).collect();
        let mut out = FilePlayback::new(path, &spec, false).unwrap();
        for chunk in data.chunks(out.period_frames()) {
            out.write(chunk).unwrap();
        }
        out.finish().unwrap();

        let mut cap = FileCapture::new(Some(path), &spec, false).unwrap();
        let mut back = vec![0i16; cap.period_frames()];
        let mut all = Vec::new();
        while all.len() < data.len() + 320 {
            cap.read(&mut back).unwrap();
            all.extend_from_slice(&back);
        }
        assert_eq!(&all[..data.len()], &data[..]);
        assert!(
            all[data.len()..].iter().all(|&s| s == 0),
            "silence after the end"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
