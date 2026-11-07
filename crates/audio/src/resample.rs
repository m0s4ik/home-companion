// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Streaming mono resampler on top of rubato's `Async` (fixed input chunks), from spike-audio.
//!
//! Push input as it comes, pop output as needed; `finish` flushes the tail so the total output
//! is exactly `round(input * ratio)` frames, aligned with the input (the filter delay trimmed).
//! Two qualities: cubic polynomial (answers, as measured in M0) and windowed sinc (capture
//! decimation in the simulator: a polynomial interpolator does not filter before decimating,
//! the M0 review note about aliasing).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Indexing, PolynomialDegree, Resampler as _, SincInterpolationParameters,
    WindowFunction,
};
use std::collections::VecDeque;

const CHUNK: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quality {
    /// Cubic polynomial: cheap, fine for upsampling speech.
    Cubic,
    /// Windowed sinc with an anti-aliasing cutoff: for decimation.
    Sinc,
}

pub(crate) struct StreamResampler {
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

impl StreamResampler {
    pub fn new(from: u32, to: u32, quality: Quality) -> Self {
        let ratio = to as f64 / from.max(1) as f64;
        let rs = (from != to).then(|| match quality {
            Quality::Cubic => Async::<f32>::new_poly(
                ratio,
                1.0,
                PolynomialDegree::Cubic,
                CHUNK,
                1,
                FixedAsync::Input,
            ),
            Quality::Sinc => Async::<f32>::new_sinc(
                ratio,
                1.0,
                &SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2),
                CHUNK,
                1,
                FixedAsync::Input,
            ),
        });
        // Constructing with a positive finite ratio and these sizes cannot fail; if it ever
        // does, fall back to passing the samples through (wrong pitch beats a panic).
        let rs = match rs {
            Some(Ok(r)) => Some(r),
            Some(Err(e)) => {
                log::error!("resampler {from} -> {to} Hz: {e}; passing samples through");
                None
            }
            None => None,
        };
        let (skip, scratch) = match &rs {
            Some(r) => (r.output_delay(), vec![0.0; r.output_frames_max()]),
            None => (0, Vec::new()),
        };
        Self {
            rs,
            ratio: if from == to { 1.0 } else { ratio },
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
        if self.finished {
            return;
        }
        self.pushed += input.len() as u64;
        if self.rs.is_none() {
            self.out.extend(input);
            self.produced += input.len() as u64;
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
        let Some(rs) = self.rs.as_mut() else { return };
        let mut tmp = [0f32; CHUNK];
        let input: &[f32] = match partial {
            None => &self.pending[at..at + CHUNK],
            Some(n) => {
                tmp[..n].copy_from_slice(&self.pending[at..at + n]);
                &tmp
            }
        };
        let cap = self.scratch.len();
        let (Ok(inp), Ok(mut outp)) = (
            InterleavedSlice::new(input, 1, CHUNK),
            InterleavedSlice::new_mut(&mut self.scratch, 1, cap),
        ) else {
            return;
        };
        let mut idx = Indexing::new();
        idx.partial_len = partial;
        let n = match rs.process_into_buffer(&inp, &mut outp, Some(&idx)) {
            Ok((_, n)) => n,
            Err(e) => {
                log::error!("resampling: {e}");
                return;
            }
        };
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
        let mut guard = 0;
        while self.produced < expected && guard < 64 {
            let before = self.produced;
            self.run_chunk(0, Some(0));
            guard += if self.produced == before { 1 } else { 0 };
        }
        let extra = self.produced.saturating_sub(expected) as usize;
        let keep = self.out.len().saturating_sub(extra);
        self.out.truncate(keep);
        self.produced = expected.min(self.produced);
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

    /// Moves everything available into `dst`.
    pub fn drain_into(&mut self, dst: &mut VecDeque<f32>) {
        dst.extend(self.out.drain(..));
    }
}

/// Resamples a whole mono clip at once (cues and WAV clips at load time).
pub(crate) fn resample_clip(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to {
        return samples.to_vec();
    }
    let quality = if to < from {
        Quality::Sinc
    } else {
        Quality::Cubic
    };
    let mut rs = StreamResampler::new(from, to, quality);
    let f: Vec<f32> = samples.iter().map(|&s| s as f32).collect();
    rs.push(&f);
    rs.finish();
    let mut out = vec![0f32; rs.available()];
    rs.pop(&mut out);
    out.iter()
        .map(|&s| s.round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, freq: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * amp)
            .collect()
    }

    #[test]
    fn exact_length_after_finish() {
        for (from, to) in [
            (22050, 48000),
            (16000, 48000),
            (48000, 16000),
            (44100, 16000),
        ] {
            for n in [0usize, 1, 255, 256, 1000, 22050 + 17] {
                for q in [Quality::Cubic, Quality::Sinc] {
                    let mut rs = StreamResampler::new(from, to, q);
                    let x = tone(from, 440.0, n, 8000.0);
                    for c in x.chunks(700) {
                        rs.push(c);
                    }
                    rs.finish();
                    let expected = (n as f64 * to as f64 / from as f64).round() as usize;
                    assert_eq!(rs.available(), expected, "{from}->{to} n={n} {q:?}");
                }
            }
        }
    }

    #[test]
    fn sinc_decimation_removes_what_cannot_be_represented() {
        // A 7 kHz tone at 48 kHz cannot exist at 16 kHz (Nyquist 8 kHz is fine) but 10 kHz
        // must be filtered out instead of folding back to 6 kHz.
        let mut rs = StreamResampler::new(48000, 16000, Quality::Sinc);
        rs.push(&tone(48000, 10_000.0, 48000, 10000.0));
        rs.finish();
        let mut out = vec![0f32; rs.available()];
        rs.pop(&mut out);
        let rms = (out[2000..14000].iter().map(|s| s * s).sum::<f32>() / 12000.0).sqrt();
        assert!(rms < 100.0, "aliased energy left: rms {rms}");
    }

    #[test]
    fn clip_resampling_keeps_level() {
        let x: Vec<i16> = tone(16000, 500.0, 16000, 10000.0)
            .iter()
            .map(|&s| s as i16)
            .collect();
        let y = resample_clip(&x, 16000, 48000);
        assert_eq!(y.len(), 48000);
        let peak = y[1000..47000]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap();
        assert!((9800..10200).contains(&peak), "{peak}");
    }
}
