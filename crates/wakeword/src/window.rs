// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! ESPHome's detection rule (micro_wake_word) plus a refractory period.
//!
//! The sum of the last N uint8 probabilities must exceed `round(cutoff * 255) * N` (the window
//! starts as zeros). After a detection the window is cleared, and after a detection and at start
//! the rule is not checked for a cool-off: ESPHome waits MIN_SLICES_BEFORE_DETECTION = 100 feature
//! slices, i.e. 1 s with 10 ms steps, so one utterance gives one detection. Probabilities are still
//! recorded during the cool-off, as in ESPHome.
//!
//! The refractory period is on top of that (the old `wakeword.service` used
//! `--refractory-seconds 2`): a detection less than `refractory` samples after the previous
//! reported one is not reported. It is counted in samples fed, so it is deterministic in tests.

pub(crate) struct SlidingWindow {
    probs: Vec<u8>,
    next: usize,
    threshold: u32,
    cooloff: usize,
    ignore: usize,
    /// Near misses: the window's sum from 70% of the cutoff up is followed, and its peak is
    /// handed out once it falls back without a detection (to tune the cutoff on real speech).
    near_floor: u32,
    peak: u32,
    near: Option<f32>,
}

impl SlidingWindow {
    /// `cooloff` is the number of probabilities (model invocations) ignored after a detection
    /// and at start.
    pub fn new(cutoff: f32, size: usize, cooloff: usize) -> Self {
        let size = size.max(1);
        // ESPHome stores the cutoff as uint8 (cutoff * 255) and compares the sum with it * N.
        let cutoff_u8 = (cutoff * 255.0).round().clamp(0.0, 255.0) as u32;
        Self {
            probs: vec![0; size],
            next: 0,
            threshold: cutoff_u8 * size as u32,
            cooloff,
            ignore: cooloff,
            near_floor: cutoff_u8 * size as u32 * 7 / 10,
            peak: 0,
            near: None,
        }
    }

    /// The mean probability of the last near miss (0..1), once.
    /// A new cutoff from the next probability on (Settings → the wake word's sensitivity): the
    /// probabilities in the window and the cool-off stay; a near miss being followed is dropped.
    pub fn set_cutoff(&mut self, cutoff: f32) {
        let size = self.probs.len() as u32;
        let cutoff_u8 = (cutoff * 255.0).round().clamp(0.0, 255.0) as u32;
        self.threshold = cutoff_u8 * size;
        self.near_floor = cutoff_u8 * size * 7 / 10;
        self.peak = 0;
        self.near = None;
    }

    pub fn take_near(&mut self) -> Option<f32> {
        self.near.take()
    }

    /// Invocations to ignore for a cool-off of `seconds`, given the feature step and model stride.
    pub fn cooloff_invocations(seconds: f64, step_ms: u32, stride: usize) -> usize {
        (seconds * 1000.0 / (step_ms.max(1) as f64 * stride.max(1) as f64)).floor() as usize
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
            self.peak = 0;
            return true;
        }
        if sum >= self.near_floor {
            self.peak = self.peak.max(sum);
        } else if self.peak > 0 {
            self.near = Some(self.peak as f32 / (self.probs.len() as f32 * 255.0));
            self.peak = 0;
        }
        false
    }

    /// Empty window, cool-off restarted (as at start).
    pub fn reset(&mut self) {
        self.probs.fill(0);
        self.next = 0;
        self.ignore = self.cooloff;
        self.peak = 0;
        self.near = None;
    }
}

/// The sliding window plus the refractory period.
pub(crate) struct Rules {
    pub window: SlidingWindow,
    refractory_samples: u64,
    last: Option<u64>,
}

impl Rules {
    pub fn new(window: SlidingWindow, refractory_samples: u64) -> Self {
        Self {
            window,
            refractory_samples,
            last: None,
        }
    }

    /// One probability at `at_samples`; true when a detection is reported.
    pub fn push(&mut self, p: u8, at_samples: u64) -> bool {
        if !self.window.push(p) {
            return false;
        }
        if let Some(last) = self.last {
            if at_samples.saturating_sub(last) < self.refractory_samples {
                log::debug!("wake word detection inside the refractory period, not reported");
                return false;
            }
        }
        self.last = Some(at_samples);
        true
    }

    /// Clears the window, restarts the cool-off and forgets the last reported detection: the
    /// refractory period only guards against double firing within one continuous listening period
    /// (the detector is not fed while streaming, so its clock would make the first seconds after a
    /// conversation deaf). The cool-off already protects against the tail of the answer.
    pub fn reset(&mut self) {
        self.window.reset();
        self.last = None;
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_near_miss_is_reported_once_with_its_peak() {
        let mut w = SlidingWindow::new(0.99, 2, 0);
        for p in [0, 240, 245, 10, 0] {
            assert!(!w.push(p));
        }
        let m = w.take_near().expect("near miss");
        assert!((m - 242.5 / 255.0).abs() < 1e-3, "{m}");
        assert_eq!(w.take_near(), None);
        // A detection is not a near miss.
        for p in [255, 255, 0, 0] {
            w.push(p);
        }
        assert_eq!(w.take_near(), None);
    }
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
    fn a_new_cutoff_applies_to_the_next_value() {
        // 0.96 x 5: 245 * 5 = 1225; five values of 240 (1200) are a near miss only.
        let mut w = SlidingWindow::new(0.96, 5, 0);
        for _ in 0..5 {
            assert!(!w.push(240));
        }
        // 0.92 x 5: 235 * 5 = 1175: the same values now detect, without refilling the window.
        w.set_cutoff(0.92);
        assert!(w.push(240));
        // Back up: 0.98 x 5 = 250 * 5 = 1250, and 240s stay under it.
        w.set_cutoff(0.98);
        for _ in 0..5 {
            assert!(!w.push(240));
        }
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
    fn reset_restarts_the_cool_off_and_empties_the_window() {
        let mut w = SlidingWindow::new(0.5, 3, 2);
        for _ in 0..2 {
            w.push(255);
        }
        w.reset();
        let got: Vec<bool> = (0..5).map(|_| w.push(255)).collect();
        assert_eq!(got, [false, false, true, false, false]);
    }

    #[test]
    fn esphome_cooloff_is_one_second() {
        // 10 ms steps, stride 3: ESPHome counts 100 slices, so the first 33 invocations are ignored.
        assert_eq!(SlidingWindow::cooloff_invocations(1.0, 10, 3), 33);
    }

    #[test]
    fn cutoff_is_rounded_to_uint8() {
        // 0.96 * 255 = 244.8 -> 245; 245 * 5 = 1225.
        let mut w = SlidingWindow::new(0.96, 5, 0);
        for _ in 0..4 {
            w.push(245);
        }
        assert!(!w.push(245)); // 1225 is not > 1225
        assert!(w.push(246));
    }

    #[test]
    fn refractory_suppresses_a_second_detection_half_a_second_later() {
        // Window of 1, no cool-off: every 255 is a window detection. Invocations every 30 ms.
        let step = 480u64; // samples per invocation (stride 3 x 10 ms)
        let mut r = Rules::new(SlidingWindow::new(0.5, 1, 0), 2 * 16_000);
        assert!(r.push(255, 0));
        // 0.5 s later: suppressed.
        assert!(!r.push(255, 8_000));
        // Still inside 2 s of the reported one (suppressed ones do not extend it).
        assert!(!r.push(255, 31_999 - step));
        assert!(r.push(255, 32_000));
    }

    #[test]
    fn reset_clears_the_refractory() {
        let mut r = Rules::new(SlidingWindow::new(0.5, 1, 0), 2 * 16_000);
        assert!(r.push(255, 0));
        r.reset();
        // 0.5 s later, after a reset: a new listening period, reported.
        assert!(r.push(255, 8_000));
        // Without a reset the refractory applies again.
        assert!(!r.push(255, 16_000));
    }

    #[test]
    fn no_refractory_reports_both() {
        let mut r = Rules::new(SlidingWindow::new(0.5, 1, 0), 0);
        assert!(r.push(255, 0));
        assert!(r.push(255, 8_000));
    }
}
