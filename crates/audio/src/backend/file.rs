// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! WAV files instead of devices (tests and CI).
//!
//! Real-time mode: both directions are paced by the wall clock like a card.
//!
//! Non-real-time mode (deterministic): the playback writes as fast as possible while the mixer
//! has something to play and paces idle periods like a card (so a long run of silence does
//! not turn into gigabytes of WAV), and the capture never runs ahead of the playback: frame k
//! of the capture is handed out only once the playback has written up to the same moment.
//! A sound started in reaction to a captured frame therefore lands a fixed, small distance
//! after that frame in the output file, whatever the machine's speed.

use super::{Capture, Opened, Pacer, Playback, StreamSpec, Xruns, capture_spec, playback_spec};
use crate::{AudioConfig, CAPTURE_RATE, Error, OUTPUT_RATE, lock};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Output frames written so far, for the capture to follow (non-real-time mode).
pub(crate) struct FileClock {
    written: Mutex<u64>,
    cond: Condvar,
    stop: Arc<AtomicBool>,
    out_rate: u32,
}

impl FileClock {
    fn advance(&self, frames: u64) {
        let mut w = lock(&self.written);
        *w = w.saturating_add(frames);
        drop(w);
        self.cond.notify_all();
    }

    /// Waits until the playback reached `out_frames` (or the audio is stopping).
    fn wait_for(&self, out_frames: u64) {
        let mut w = lock(&self.written);
        while *w < out_frames && !self.stop.load(Ordering::Relaxed) {
            w = match self.cond.wait_timeout(w, Duration::from_millis(20)) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
    }

    pub fn finish(&self) {
        // The playback is gone: let the capture run on its own from now on.
        *lock(&self.written) = u64::MAX;
        self.cond.notify_all();
    }
}

pub(crate) fn open(
    cfg: &AudioConfig,
    capture: Option<&Path>,
    playback: Option<&Path>,
    realtime: bool,
    stop: Arc<AtomicBool>,
) -> Result<Opened, Error> {
    let clock = (!realtime).then(|| {
        Arc::new(FileClock {
            written: Mutex::new(0),
            cond: Condvar::new(),
            stop,
            out_rate: OUTPUT_RATE,
        })
    });
    let p = FilePlayback::new(playback, &playback_spec(cfg), realtime, clock.clone())?;
    let c = FileCapture::new(capture, &capture_spec(cfg), realtime, clock)?;
    Ok(Opened {
        playback: Box::new(p),
        capture: Box::new(c),
        output_rate: OUTPUT_RATE,
        realtime,
        description: format!(
            "file in {} out {}{}",
            capture.map_or("(silence)".into(), |p| p.display().to_string()),
            playback.map_or("(discarded)".into(), |p| p.display().to_string()),
            if realtime { "" } else { ", not real time" }
        ),
        keep: None,
    })
}

/// Writes the mix to a WAV (or discards it).
pub(crate) struct FilePlayback {
    writer: Option<hound::WavWriter<io::BufWriter<std::fs::File>>>,
    channels: usize,
    period: usize,
    rate: u32,
    pacer: Pacer,
    realtime: bool,
    busy: bool,
    clock: Option<Arc<FileClock>>,
}

impl FilePlayback {
    pub fn new(
        path: Option<&Path>,
        spec: &StreamSpec,
        realtime: bool,
        clock: Option<Arc<FileClock>>,
    ) -> Result<Self, Error> {
        let writer = match path {
            Some(path) => {
                let wav = hound::WavSpec {
                    channels: spec.channels as u16,
                    sample_rate: spec.rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                Some(
                    hound::WavWriter::create(path, wav)
                        .map_err(|e| Error::Device(format!("{}: {e}", path.display())))?,
                )
            }
            None => None,
        };
        let period = spec.period_frames();
        Ok(Self {
            writer,
            channels: spec.channels,
            period,
            rate: spec.rate,
            pacer: Pacer::new(spec.rate, period as u64 * 4, true),
            realtime,
            busy: false,
            clock,
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
        if let Some(w) = self.writer.as_mut() {
            let mut w16 = w.get_i16_writer(buf.len() as u32);
            for &s in buf {
                w16.write_sample(s);
            }
            w16.flush().map_err(io::Error::other)?;
        }
        let frames = buf.len() / self.channels;
        if self.realtime || !self.busy {
            self.pacer.advance(frames);
        }
        if let Some(c) = &self.clock {
            c.advance(frames as u64);
        }
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

    // A file cannot be closed and reopened without losing the timeline: the close policy
    // keeps it open.
    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn resume(&mut self) -> io::Result<Duration> {
        Ok(Duration::ZERO)
    }

    fn finish(&mut self) -> io::Result<()> {
        if let Some(c) = &self.clock {
            c.finish();
        }
        if let Some(w) = self.writer.take() {
            w.finalize().map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn hint_busy(&mut self, busy: bool) {
        if self.realtime || busy == self.busy {
            return;
        }
        self.busy = busy;
        if !busy {
            // Back to card pacing from now on (the time spent racing is not slept off).
            self.pacer = Pacer::new(self.rate, self.period as u64 * 4, true);
        }
    }
}

/// Reads a 16 kHz WAV (mono or 2 ch, mapped to the configured channel count), then silence.
pub(crate) struct FileCapture {
    samples: Vec<i16>,
    pos: usize,
    channels: usize,
    period: usize,
    pacer: Pacer,
    clock: Option<Arc<FileClock>>,
    frames: u64,
}

impl FileCapture {
    pub fn new(
        path: Option<&Path>,
        spec: &StreamSpec,
        realtime: bool,
        clock: Option<Arc<FileClock>>,
    ) -> Result<Self, Error> {
        let mut samples = Vec::new();
        if let Some(path) = path {
            let (s, rate, ch) = crate::wav::read_interleaved(path)?;
            if rate != CAPTURE_RATE {
                return Err(Error::Unsupported(format!(
                    "{}: {rate} Hz, the capture runs at {CAPTURE_RATE} Hz",
                    path.display()
                )));
            }
            if ch > 2 {
                return Err(Error::Unsupported(format!(
                    "{}: {ch} channels (1 or 2)",
                    path.display()
                )));
            }
            samples = match (ch, spec.channels) {
                (a, b) if a == b => s,
                (1, n) => s.iter().flat_map(|&x| std::iter::repeat_n(x, n)).collect(),
                (_, _) => s
                    .chunks_exact(2)
                    .map(|f| ((f[0] as i32 + f[1] as i32) >> 1) as i16)
                    .collect(),
            };
        }
        Ok(Self {
            samples,
            pos: 0,
            channels: spec.channels,
            period: spec.period_frames(),
            pacer: Pacer::new(spec.rate, 0, realtime),
            clock,
            frames: 0,
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
        let frames = (buf.len() / self.channels) as u64;
        if let Some(c) = &self.clock {
            // Never ahead of the playback: wait until it reached the end of this period.
            let need = ((self.frames + frames) * c.out_rate as u64).div_ceil(CAPTURE_RATE as u64);
            c.wait_for(need);
        }
        let n = buf.len().min(self.samples.len().saturating_sub(self.pos));
        buf[..n].copy_from_slice(&self.samples[self.pos..self.pos + n]);
        buf[n..].fill(0);
        self.pos += n;
        self.frames += frames;
        self.pacer.advance(frames as usize);
        Ok(())
    }

    fn xruns(&self) -> Xruns {
        Xruns::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("audio-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.wav");
        let spec = StreamSpec {
            rate: 16000,
            channels: 1,
            period_ms: 20,
        };
        let data: Vec<i16> = (0..1600).map(|i| (i * 13 % 2000 - 1000) as i16).collect();
        let mut out = FilePlayback::new(Some(&path), &spec, false, None).unwrap();
        out.hint_busy(true);
        for chunk in data.chunks(out.period_frames()) {
            out.write(chunk).unwrap();
        }
        out.finish().unwrap();

        let mut cap = FileCapture::new(Some(&path), &spec, false, None).unwrap();
        let mut back = vec![0i16; cap.period_frames()];
        let mut all = Vec::new();
        while all.len() < data.len() + 320 {
            cap.read(&mut back).unwrap();
            all.extend_from_slice(&back);
        }
        assert_eq!(&all[..data.len()], &data[..]);
        assert!(all[data.len()..].iter().all(|&s| s == 0));
        std::fs::remove_dir_all(&dir).ok();
    }
}
