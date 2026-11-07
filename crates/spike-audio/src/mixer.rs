// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The single owner of the speaker mixes every source into one S16 stream.
//!
//! Sources are mono. An `Answer` is a WAV at its own rate, resampled to the output rate
//! with rubato while it plays; a `Beep` is a synthesized sine. Mixing is done in i32 and
//! saturated to i16, then duplicated to every output channel.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler as _};
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::Instant;

/// Reads a 16-bit WAV and returns it as mono (channels averaged) with its rate.
pub fn read_wav_mono(path: &str) -> io::Result<(Vec<i16>, u32)> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| io::Error::other(format!("{path}: {e}")))?;
    let spec = reader.spec();
    if spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
        return Err(io::Error::other(format!(
            "{path}: only 16-bit integer WAV is supported"
        )));
    }
    let ch = spec.channels as usize;
    let all: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .map_err(io::Error::other)?;
    let mono = all
        .chunks_exact(ch)
        .map(|f| (f.iter().map(|&s| s as i32).sum::<i32>() / ch as i32) as i16)
        .collect();
    Ok((mono, spec.sample_rate))
}

/// A beep: `freq` Hz sine, `dur_ms` long, with linear fades, at `amp` of full scale.
pub fn synth_beep(rate: u32, freq: f32, dur_ms: u32, fade_ms: u32, amp: f32) -> Vec<i16> {
    let n = (rate as u64 * dur_ms as u64 / 1000) as usize;
    let fade = (rate as u64 * fade_ms as u64 / 1000).max(1) as usize;
    (0..n)
        .map(|i| {
            let env = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
            let x = (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin();
            (x * env * amp * 32767.0) as i16
        })
        .collect()
}

/// A short broadband click used to find the speaker-to-microphone round trip.
pub fn synth_click(rate: u32) -> Vec<i16> {
    // 4 ms of a 2 kHz burst under a Hann window: easy to find by cross-correlation.
    let n = (rate as usize * 4 / 1000).max(8);
    (0..n)
        .map(|i| {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
            let x = (2.0 * std::f32::consts::PI * 2000.0 * i as f32 / rate as f32).sin();
            (x * w * 0.7 * 32767.0) as i16
        })
        .collect()
}

/// Streaming mono resampler on top of rubato's polynomial `Async` (fixed input chunks).
/// Push input as it comes, pop output as needed; `finish` flushes the tail so the total
/// output is exactly `round(input * ratio)` frames, aligned with the input (delay trimmed).
pub struct StreamResampler {
    rs: Option<Async<f32>>,
    ratio: f64,
    pending: Vec<f32>,
    out: VecDeque<f32>,
    scratch: Vec<f32>,
    skip: usize,
    pushed: u64,
    produced: u64,
    finished: bool,
}

const CHUNK: usize = 256;

impl StreamResampler {
    pub fn new(from: u32, to: u32) -> Self {
        let ratio = to as f64 / from as f64;
        let rs = (from != to).then(|| {
            Async::<f32>::new_poly(
                ratio,
                1.0,
                PolynomialDegree::Cubic,
                CHUNK,
                1,
                FixedAsync::Input,
            )
            .expect("valid resampler parameters")
        });
        let (skip, scratch) = match &rs {
            Some(r) => (r.output_delay(), vec![0.0; r.output_frames_max()]),
            None => (0, Vec::new()),
        };
        Self {
            rs,
            ratio,
            pending: Vec::new(),
            out: VecDeque::new(),
            scratch,
            skip,
            pushed: 0,
            produced: 0,
            finished: false,
        }
    }

    pub fn push(&mut self, input: &[f32]) {
        self.pushed += input.len() as u64;
        if self.rs.is_none() {
            self.out.extend(input);
            return;
        }
        self.pending.extend_from_slice(input);
        let full = self.pending.len() / CHUNK * CHUNK;
        let mut at = 0;
        while at < full {
            self.run_chunk(at, None);
            at += CHUNK;
        }
        self.pending.drain(..full);
    }

    /// Processes one chunk starting at `pending[at]`; `partial` = valid frames when short.
    fn run_chunk(&mut self, at: usize, partial: Option<usize>) {
        let rs = self.rs.as_mut().expect("resampler present");
        let mut tmp = [0f32; CHUNK];
        let input: &[f32] = match partial {
            None => &self.pending[at..at + CHUNK],
            Some(n) => {
                tmp[..n].copy_from_slice(&self.pending[at..at + n]);
                &tmp
            }
        };
        let inp = InterleavedSlice::new(input, 1, CHUNK).expect("input adapter");
        let cap = self.scratch.len();
        let mut outp =
            InterleavedSlice::new_mut(&mut self.scratch, 1, cap).expect("output adapter");
        let mut idx = Indexing::new();
        idx.partial_len = partial;
        let (_, n) = rs
            .process_into_buffer(&inp, &mut outp, Some(&idx))
            .expect("resampling");
        let drop = self.skip.min(n);
        self.skip -= drop;
        self.out.extend(&self.scratch[drop..n]);
        self.produced += (n - drop) as u64;
    }

    /// No more input: flushes the resampler and trims the output to the exact length.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.rs.is_none() {
            return;
        }
        let expected = (self.pushed as f64 * self.ratio).round() as u64;
        let rest = self.pending.len();
        self.run_chunk(0, Some(rest));
        self.pending.clear();
        while self.produced < expected {
            self.run_chunk(0, Some(0));
        }
        let extra = (self.produced - expected) as usize;
        self.out.truncate(self.out.len() - extra);
        self.produced = expected;
    }

    pub fn available(&self) -> usize {
        self.out.len()
    }

    /// Pops up to `dst.len()` frames; returns how many were written.
    pub fn pop(&mut self, dst: &mut [f32]) -> usize {
        let n = dst.len().min(self.out.len());
        for (d, s) in dst.iter_mut().zip(self.out.drain(..n)) {
            *d = s;
        }
        n
    }

    pub fn is_drained(&self) -> bool {
        self.finished && self.out.is_empty()
    }
}

/// A WAV answer played through a streaming resampler.
pub struct AnswerStream {
    clip: Vec<i16>,
    fed: usize,
    rs: StreamResampler,
}

impl AnswerStream {
    pub fn new(clip: Vec<i16>, clip_rate: u32, out_rate: u32) -> Self {
        Self {
            clip,
            fed: 0,
            rs: StreamResampler::new(clip_rate, out_rate),
        }
    }

    /// Writes up to `dst.len()` output frames; returns how many (fewer = the answer ended).
    fn pull(&mut self, dst: &mut [f32]) -> usize {
        while self.rs.available() < dst.len() && !self.rs.finished {
            let end = (self.fed + CHUNK).min(self.clip.len());
            let block: Vec<f32> = self.clip[self.fed..end].iter().map(|&s| s as f32).collect();
            self.rs.push(&block);
            self.fed = end;
            if self.fed == self.clip.len() {
                self.rs.finish();
            }
        }
        self.rs.pop(dst)
    }
}

pub enum SourceKind {
    Answer(Box<AnswerStream>),
    /// Precomputed samples at the output rate, shared between beeps.
    Clip(Arc<[i16]>),
}

/// A sound queued on the mixer.
pub struct Source {
    pub label: String,
    pub kind: SourceKind,
    /// Absolute output frame where it should start; it starts at once if already past.
    pub start_frame: u64,
    /// When it was requested (for request-to-first-frame).
    pub requested: Instant,
}

struct Voice {
    src: Source,
    pos: usize,
    started: bool,
}

/// A source that contributed to a period for the first time.
pub struct Started {
    pub label: String,
    pub requested: Instant,
    /// Offset of its first frame inside the rendered period.
    pub offset_frames: usize,
}

pub struct MixBus {
    channels: usize,
    pos: u64,
    voices: Vec<Voice>,
    acc: Vec<i32>,
    tmp: Vec<f32>,
}

impl MixBus {
    pub fn new(channels: usize) -> Self {
        Self {
            channels,
            pos: 0,
            voices: Vec::new(),
            acc: Vec::new(),
            tmp: Vec::new(),
        }
    }

    pub fn add(&mut self, src: Source) {
        self.voices.push(Voice {
            src,
            pos: 0,
            started: false,
        });
    }

    /// True while any source is playing or waiting for its start frame.
    pub fn busy(&self) -> bool {
        !self.voices.is_empty()
    }

    /// Output frames rendered so far.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Jumps the timeline forward (close policy: time passed while the pcm was closed).
    pub fn skip_to(&mut self, frame: u64) {
        self.pos = self.pos.max(frame);
    }

    /// Renders `out.len() / channels` interleaved frames.
    pub fn render(&mut self, out: &mut [i16]) -> Vec<Started> {
        let frames = out.len() / self.channels;
        self.acc.clear();
        self.acc.resize(frames, 0);
        self.tmp.resize(frames, 0.0);
        let (p0, p1) = (self.pos, self.pos + frames as u64);
        let mut started = Vec::new();
        for v in &mut self.voices {
            if v.src.start_frame >= p1 {
                continue;
            }
            let first = v.src.start_frame.saturating_sub(p0) as usize;
            let want = frames - first;
            let got = match &mut v.src.kind {
                SourceKind::Clip(samples) => {
                    let n = want.min(samples.len() - v.pos);
                    for (a, &s) in self.acc[first..first + n]
                        .iter_mut()
                        .zip(&samples[v.pos..v.pos + n])
                    {
                        *a += s as i32;
                    }
                    n
                }
                SourceKind::Answer(stream) => {
                    let n = stream.pull(&mut self.tmp[..want]);
                    for (a, &s) in self.acc[first..first + n].iter_mut().zip(&self.tmp[..n]) {
                        *a += s.round() as i32;
                    }
                    n
                }
            };
            if !v.started && got > 0 {
                v.started = true;
                started.push(Started {
                    label: v.src.label.clone(),
                    requested: v.src.requested,
                    offset_frames: first,
                });
            }
            v.pos += got;
        }
        self.voices.retain(|v| match &v.src.kind {
            SourceKind::Clip(s) => v.pos < s.len(),
            SourceKind::Answer(a) => !a.rs.is_drained(),
        });
        for (frame, &a) in out.chunks_exact_mut(self.channels).zip(&self.acc) {
            frame.fill(saturate(a));
        }
        self.pos = p1;
        started
    }
}

/// i32 mix bus to i16 with saturation (never wraps, never panics).
pub fn saturate(x: i32) -> i16 {
    x.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beep(rate: u32) -> Arc<[i16]> {
        synth_beep(rate, 880.0, 200, 10, 0.3).into()
    }

    #[test]
    fn beep_lands_on_requested_frame() {
        let rate = 48000;
        let mut m = MixBus::new(2);
        let at = 30_000u64; // not a multiple of the period on purpose
        m.add(Source {
            label: "b".into(),
            kind: SourceKind::Clip(beep(rate)),
            start_frame: at,
            requested: Instant::now(),
        });
        let mut out = Vec::new();
        let mut buf = vec![0i16; 960 * 2];
        while out.len() < 48000 * 2 {
            m.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        let left: Vec<i16> = out.iter().step_by(2).copied().collect();
        let first = left.iter().position(|&s| s != 0).unwrap() as u64;
        assert!(
            (at..at + 10).contains(&first),
            "first non-zero frame {first}"
        );
        // Energy over 5 ms windows: silence before, signal inside, silence after.
        let energy =
            |a: usize, b: usize| left[a..b].iter().map(|&s| (s as f64).powi(2)).sum::<f64>();
        assert_eq!(energy(0, at as usize), 0.0);
        assert!(energy(at as usize + 960, at as usize + 1200) > 1e6);
        assert_eq!(energy(at as usize + 9600 + 1, left.len()), 0.0);
        assert!(!m.busy());
    }

    #[test]
    fn resample_22050_to_48000_length() {
        for n in [1usize, 255, 256, 1000, 22050, 66150 + 17] {
            let clip: Vec<i16> = (0..n)
                .map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i16)
                .collect();
            let mut a = AnswerStream::new(clip, 22050, 48000);
            let mut total = 0;
            let mut buf = vec![0f32; 960];
            loop {
                let got = a.pull(&mut buf);
                total += got;
                if got < buf.len() {
                    break;
                }
            }
            let expected = n as f64 * 48000.0 / 22050.0;
            assert!(
                (total as f64 - expected).abs() <= 2.0,
                "n={n}: {total} vs {expected}"
            );
        }
    }

    #[test]
    fn resampled_tone_keeps_its_frequency_and_level() {
        // 440 Hz at 22050 -> 48000: count zero crossings over one second.
        let clip: Vec<i16> = (0..22050)
            .map(|i| {
                ((2.0 * std::f32::consts::PI * 440.0 * i as f32 / 22050.0).sin() * 10000.0) as i16
            })
            .collect();
        let mut a = AnswerStream::new(clip, 22050, 48000);
        let mut out = vec![0f32; 48000];
        assert_eq!(a.pull(&mut out), 48000);
        let crossings = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((438..=442).contains(&crossings), "{crossings}");
        let peak = out[1000..47000].iter().fold(0f32, |m, &s| m.max(s.abs()));
        assert!((9500.0..10500.0).contains(&peak), "{peak}");
    }

    #[test]
    fn saturation_never_panics() {
        let mut m = MixBus::new(1);
        let loud: Arc<[i16]> = vec![i16::MAX; 4800].into();
        let low: Arc<[i16]> = vec![i16::MIN; 4800].into();
        for k in [&loud, &loud, &loud, &low] {
            m.add(Source {
                label: "x".into(),
                kind: SourceKind::Clip(k.clone()),
                start_frame: 0,
                requested: Instant::now(),
            });
        }
        let mut buf = vec![0i16; 480];
        m.render(&mut buf);
        assert!(buf.iter().all(|&s| s == i16::MAX));
        assert_eq!(saturate(i32::MIN), i16::MIN);
        assert_eq!(saturate(i32::MAX), i16::MAX);
    }
}
