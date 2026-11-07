// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! ESPHome's detection rule (micro_wake_word): the sum of the last N uint8 probabilities must
//! exceed cutoff * 255 * N (the window starts as zeros). After a detection the window is cleared,
//! and after a detection and at start the rule is not checked for a cool-off: ESPHome waits
//! MIN_SLICES_BEFORE_DETECTION = 100 feature slices, i.e. 1 s with 10 ms steps, so one utterance
//! gives one detection. Probabilities are still recorded during the cool-off, as in ESPHome.

pub struct SlidingWindow {
    probs: Vec<u8>,
    next: usize,
    threshold: u32,
    cooloff: usize,
    ignore: usize,
}

impl SlidingWindow {
    /// `cooloff` is the number of probabilities (model invocations) ignored after a detection
    /// and at start.
    pub fn new(cutoff: f32, size: usize, cooloff: usize) -> Self {
        assert!(size > 0);
        // ESPHome stores the cutoff as uint8 (cutoff * 255) and compares the sum with it * N.
        let cutoff_u8 = (cutoff * 255.0).round().clamp(0.0, 255.0) as u32;
        Self {
            probs: vec![0; size],
            next: 0,
            threshold: cutoff_u8 * size as u32,
            cooloff,
            ignore: cooloff,
        }
    }

    /// Invocations to ignore for a cool-off of `seconds`, given the feature step and model stride.
    pub fn cooloff_invocations(seconds: f64, step_ms: u32, stride: usize) -> usize {
        (seconds * 1000.0 / (step_ms as f64 * stride.max(1) as f64)).floor() as usize
    }

    /// Adds one probability; returns true on a detection.
    pub fn push(&mut self, p: u8) -> bool {
        // As ESPHome: the probability is always stored, the rule is only checked out of cool-off.
        self.probs[self.next] = p;
        self.next = (self.next + 1) % self.probs.len();
        if self.ignore > 0 {
            self.ignore -= 1;
            return false;
        }
        let sum: u32 = self.probs.iter().map(|&x| x as u32).sum();
        if sum > self.threshold {
            self.probs.fill(0);
            self.ignore = self.cooloff;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_a_full_window_above_cutoff() {
        let mut w = SlidingWindow::new(0.97, 5, 0); // threshold 247 * 5 = 1235
        for _ in 0..4 {
            assert!(!w.push(255));
        }
        assert!(w.push(255)); // 1275 > 1235
    }

    #[test]
    fn one_low_value_blocks_detection() {
        let mut w = SlidingWindow::new(0.97, 5, 0);
        for p in [255, 255, 255, 255, 200] {
            assert!(!w.push(p)); // 1220 <= 1235
        }
        for _ in 0..4 {
            assert!(!w.push(255)); // the 200 is still inside the window
        }
        assert!(w.push(255)); // 200 dropped out: 1275
    }

    #[test]
    fn exact_threshold_is_not_enough() {
        let mut w = SlidingWindow::new(1.0, 2, 0);
        assert!(!w.push(255));
        assert!(!w.push(255)); // 510 is not > 510
    }

    #[test]
    fn cool_off_at_start_and_after_detection() {
        let mut w = SlidingWindow::new(0.5, 3, 2);
        let got: Vec<bool> = (0..7).map(|_| w.push(255)).collect();
        // 2 ignored (but recorded), then the full window detects; cleared, 2 ignored again.
        let want = [false, false, true, false, false, true, false];
        assert_eq!(got, want);
    }

    #[test]
    fn esphome_cooloff_is_one_second() {
        // 10 ms steps, stride 3: ESPHome counts 100 slices, so the first 33 invocations are ignored.
        assert_eq!(SlidingWindow::cooloff_invocations(1.0, 10, 3), 33);
    }
}
