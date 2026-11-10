// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The music page's spectrum: the levels of a few bands of the AirPlay audio, as the faint
//! moving background under the title (not a meter). The PCM on its way to the mixer is fed here;
//! about 15 times a second the last [`WINDOW`] samples (mono, a Hann window) go through a small
//! FFT (radix 2, in place, no library: ~50 k multiplications) and the power of the bins in each
//! of [`BANDS`] log-spaced bands is summed, mapped from -60..0 dBFS to 0..1. Bars rise at once
//! and fall smoothly; each peak holds a moment, then falls slowly.

use std::time::{Duration, Instant};

/// The bands, low to high.
pub const BANDS: usize = 16;
/// Samples measured each time (23 ms at 44.1 kHz).
pub const WINDOW: usize = 1024;
/// How often the levels are measured and sent.
pub const EVERY: Duration = Duration::from_millis(66);
const LOW_HZ: f32 = 50.0;
const HIGH_HZ: f32 = 12_000.0;
/// dBFS shown as an empty bar.
const FLOOR_DB: f32 = -60.0;
/// What a bar keeps of its last level each time (its fall).
const FALL: f32 = 0.78;
/// How long a peak holds, then how much it falls each time.
const PEAK_HOLD: u32 = 8;
const PEAK_FALL: f32 = 0.03;

pub struct Spectrum {
    ring: Vec<f32>,
    at: usize,
    filled: bool,
    /// Each band's FFT bins, [first, last).
    bins: [(usize, usize); BANDS],
    hann: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    bands: [f32; BANDS],
    peaks: [f32; BANDS],
    held: [u32; BANDS],
    last: Option<Instant>,
}

impl Spectrum {
    pub fn new(rate: u32) -> Spectrum {
        let rate = rate as f32;
        let hz = rate / WINDOW as f32;
        let edge = |i: usize| LOW_HZ * (HIGH_HZ / LOW_HZ).powf(i as f32 / BANDS as f32);
        let mut bins = [(0, 0); BANDS];
        for (i, b) in bins.iter_mut().enumerate() {
            let lo = (edge(i) / hz).round() as usize;
            let hi = (edge(i + 1) / hz).round() as usize;
            // The narrow low bands get one bin at least.
            *b = (lo.max(1), hi.max(lo.max(1) + 1).min(WINDOW / 2));
        }
        let hann = (0..WINDOW)
            .map(|n| {
                let x = std::f32::consts::PI * n as f32 / (WINDOW - 1) as f32;
                x.sin() * x.sin()
            })
            .collect();
        Spectrum {
            ring: vec![0.0; WINDOW],
            at: 0,
            filled: false,
            bins,
            hann,
            re: vec![0.0; WINDOW],
            im: vec![0.0; WINDOW],
            bands: [0.0; BANDS],
            peaks: [0.0; BANDS],
            held: [0; BANDS],
            last: None,
        }
    }

    /// Interleaved S16LE stereo: kept (as mono) for the next measure; the levels and peaks when
    /// it is time for one.
    pub fn feed(&mut self, pcm: &[u8], now: Instant) -> Option<([f32; BANDS], [f32; BANDS])> {
        for frame in pcm.chunks_exact(4) {
            let l = i16::from_le_bytes([frame[0], frame[1]]) as f32;
            let r = i16::from_le_bytes([frame[2], frame[3]]) as f32;
            self.ring[self.at] = (l + r) / (2.0 * 32768.0);
            self.at = (self.at + 1) % WINDOW;
            if self.at == 0 {
                self.filled = true;
            }
        }
        if !self.filled || self.last.is_some_and(|t| now.duration_since(t) < EVERY) {
            return None;
        }
        self.last = Some(now);
        Some(self.measure())
    }

    /// The sound stopped: every bar and peak empty.
    pub fn silence(&mut self) -> ([f32; BANDS], [f32; BANDS]) {
        self.bands = [0.0; BANDS];
        self.peaks = [0.0; BANDS];
        self.held = [0; BANDS];
        self.filled = false;
        self.last = None;
        (self.bands, self.peaks)
    }

    fn measure(&mut self) -> ([f32; BANDS], [f32; BANDS]) {
        for n in 0..WINDOW {
            self.re[n] = self.ring[(self.at + n) % WINDOW] * self.hann[n];
            self.im[n] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        // A full-scale sine through the Hann window: its bins' power sums to 1.5 (N/4)^2.
        let full = 1.5 * (WINDOW as f32 / 4.0).powi(2);
        for b in 0..BANDS {
            let (lo, hi) = self.bins[b];
            let power: f32 = (lo..hi)
                .map(|k| self.re[k] * self.re[k] + self.im[k] * self.im[k])
                .sum();
            let db = 10.0 * (power / full).max(1e-12).log10();
            let level = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
            self.bands[b] = level.max(self.bands[b] * FALL);
            if self.bands[b] >= self.peaks[b] {
                self.peaks[b] = self.bands[b];
                self.held[b] = PEAK_HOLD;
            } else if self.held[b] > 0 {
                self.held[b] -= 1;
            } else {
                self.peaks[b] = (self.peaks[b] - PEAK_FALL).max(self.bands[b]);
            }
        }
        (self.bands, self.peaks)
    }
}

/// In-place radix-2 FFT (Cooley-Tukey, iterative); the length is a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let a = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (a.cos(), a.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (i, j) = (start + k, start + k + len / 2);
                let tr = re[j] * cr - im[j] * ci;
                let ti = re[j] * ci + im[j] * cr;
                re[j] = re[i] - tr;
                im[j] = im[i] - ti;
                re[i] += tr;
                im[i] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amp: f32, frames: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(frames * 4);
        for n in 0..frames {
            let v = (amp * 32767.0 * (2.0 * std::f32::consts::PI * hz * n as f32 / 44100.0).sin())
                as i16;
            out.extend_from_slice(&v.to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_low_tone_lights_the_low_bands() {
        let mut s = Spectrum::new(44100);
        let t0 = Instant::now();
        assert_eq!(
            s.feed(&tone(60.0, 0.5, 512), t0),
            None,
            "the window is not full yet"
        );
        let (b, p) = s.feed(&tone(60.0, 0.5, 1024), t0).unwrap();
        assert!(b[0] > 0.8, "{b:?}");
        assert!(b[BANDS - 1] < 0.3, "{b:?}");
        assert_eq!(p, b, "the first peaks are the levels");
        // Not again before EVERY.
        assert_eq!(
            s.feed(&tone(60.0, 0.5, 100), t0 + Duration::from_millis(10)),
            None
        );
    }

    #[test]
    fn a_high_tone_lights_the_high_bands_and_silence_lets_them_fall() {
        let mut s = Spectrum::new(44100);
        let t0 = Instant::now();
        let (b, _) = s.feed(&tone(11_000.0, 0.5, 2048), t0).unwrap();
        assert!(b[BANDS - 1] > 0.8, "{b:?}");
        assert!(b[0] < 0.3, "{b:?}");
        // Silence: the bar falls a step each time, the peak holds, then falls.
        let mut t = t0;
        let mut last = b[BANDS - 1];
        for _ in 0..3 {
            t += EVERY;
            let (b, p) = s.feed(&vec![0; 4096], t).unwrap();
            assert!(b[BANDS - 1] < last);
            assert!((p[BANDS - 1] - 0.0).abs() > 0.5, "the peak holds");
            last = b[BANDS - 1];
        }
        let (b, p) = s.silence();
        assert_eq!((b, p), ([0.0; BANDS], [0.0; BANDS]));
    }
}
