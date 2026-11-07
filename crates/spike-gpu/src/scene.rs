// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Drives the screen from one Slint timer at `--fps`: one animation frame per
//! tick, the next pose every `--pose-every` seconds (from its first frame),
//! the clock when the wall-clock second changes. Each change sets a bit in
//! `changes`, so the mode can class the frame it draws. The same scene runs
//! every mode; the mode only says how a frame is shown.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::{Timer, TimerMode};

use crate::measure;
use crate::stats::{Stats, change};

/// Position in the animation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub pose: usize,
    pub frame: usize,
}

/// Timeline state: pure, so it can be tested without a window.
#[derive(Clone, Debug)]
pub struct Timeline {
    pub fps: f64,
    pub pose_every: f64,
    pub frame_counts: Vec<usize>,
    pub tick: u64,
    pub pose_swaps: u64,
    pub cursor: Cursor,
}

impl Timeline {
    pub fn new(fps: f64, pose_every: f64, frame_counts: Vec<usize>) -> Self {
        Timeline { fps, pose_every, frame_counts, tick: 0, pose_swaps: 0, cursor: Cursor::default() }
    }

    /// Scene time in seconds (ticks, not the wall clock: a late tick shows
    /// up as a slower animation, which the pacing numbers catch).
    pub fn t(&self) -> f64 {
        self.tick as f64 / self.fps
    }

    /// Advances one tick; returns the change bits.
    pub fn advance(&mut self) -> u8 {
        self.tick += 1;
        let t = self.t();
        if self.pose_every > 0.0 && self.frame_counts.len() > 1 && t >= (self.pose_swaps + 1) as f64 * self.pose_every {
            self.pose_swaps += 1;
            self.cursor = Cursor { pose: (self.cursor.pose + 1) % self.frame_counts.len(), frame: 0 };
            return change::POSE | change::ANIM;
        }
        let n = self.frame_counts[self.cursor.pose];
        if n > 1 {
            self.cursor.frame = (self.cursor.frame + 1) % n;
            change::ANIM
        } else {
            0
        }
    }
}

pub struct Scene {
    pub changes: Rc<Cell<u8>>,
    pub quit: Rc<Cell<bool>>,
    pub stats: Rc<RefCell<Stats>>,
    _timer: Timer,
}

pub struct Hooks {
    /// Shows animation frame `cursor` (set an image property, or queue a blit).
    pub show: Box<dyn Fn(Cursor)>,
    pub set_clock: Box<dyn Fn(String)>,
    /// Called once when `--seconds` is reached or a signal arrived (inside the event loop).
    pub on_quit: Box<dyn Fn()>,
}

pub fn clock_text() -> String {
    let (h, m, s) = measure::local_hms();
    format!("{h:02}:{m:02}:{s:02}")
}

pub fn start(timeline: Timeline, seconds: f64, stats: Rc<RefCell<Stats>>, hooks: Hooks) -> Scene {
    let changes = Rc::new(Cell::new(0u8));
    let quit = Rc::new(Cell::new(false));
    (hooks.show)(timeline.cursor);
    (hooks.set_clock)(clock_text());
    let st = RefCell::new((timeline, measure::local_hms()));
    let timer = Timer::default();
    let period = Duration::from_secs_f64(1.0 / st.borrow().0.fps);
    let (changes2, quit2, stats2) = (changes.clone(), quit.clone(), stats.clone());
    stats.borrow_mut().scene_started();
    timer.start(TimerMode::Repeated, period, move || {
        if quit2.get() {
            return;
        }
        let mut s = st.borrow_mut();
        let (tl, last_hms) = &mut *s;
        stats2.borrow_mut().tick();
        let mut bits = tl.advance();
        if bits & change::ANIM != 0 {
            (hooks.show)(tl.cursor);
        }
        let hms = measure::local_hms();
        if hms != *last_hms {
            *last_hms = hms;
            (hooks.set_clock)(format!("{:02}:{:02}:{:02}", hms.0, hms.1, hms.2));
            bits |= change::CLOCK;
        }
        stats2.borrow_mut().maybe_sample();
        changes2.set(changes2.get() | bits);
        if tl.t() >= seconds || crate::platform_fb::stop_requested() {
            quit2.set(true);
            (hooks.on_quit)();
        }
    });
    Scene { changes, quit, stats, _timer: timer }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_cycles_frames_and_poses() {
        let mut tl = Timeline::new(16.0, 1.0, vec![4, 3]);
        let mut seen = Vec::new();
        for _ in 0..40 {
            let bits = tl.advance();
            seen.push((tl.cursor.pose, tl.cursor.frame, bits & change::POSE != 0));
        }
        // Frames loop within the pose; at t = 1 s the next pose starts at frame 0.
        assert_eq!(seen[0], (0, 1, false));
        assert_eq!(seen[3], (0, 0, false));
        assert_eq!(seen[15], (1, 0, true));
        assert_eq!(seen[16], (1, 1, false));
        assert_eq!(seen[31], (0, 0, true));
        assert_eq!(seen.iter().filter(|s| s.2).count(), 2);
    }

    #[test]
    fn single_pose_never_switches() {
        let mut tl = Timeline::new(16.0, 0.5, vec![2]);
        for _ in 0..32 {
            assert_eq!(tl.advance() & change::POSE, 0);
        }
        let mut still = Timeline::new(16.0, 0.0, vec![1]);
        assert_eq!(still.advance(), 0);
    }
}
