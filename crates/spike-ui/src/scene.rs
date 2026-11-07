// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Drives the screen from one timer at `--fps`: the box moves one step per
//! tick, the clock changes when the wall-clock second changes, the volume bar
//! and the pose change on their own periods. Each change sets a bit in
//! `changes`, so the renderer can class the frame it draws.

use std::cell::{Cell, RefCell};
use std::f64::consts::TAU;
use std::rc::Rc;
use std::time::Duration;

use slint::{Color, ComponentHandle, Image, Timer, TimerMode};

use crate::AppWindow;
use crate::args::Args;
use crate::measure;
use crate::stats::{Class, Stats, change};

/// Top-left of the box on its rounded (elliptic) path, at time `t` seconds.
pub fn box_position(t: f64) -> (f32, f32) {
    let a = TAU * t / 6.0; // one lap every 6 s, about 12 px per tick at 16 fps
    ((368.0 + 280.0 * a.cos()) as f32, (986.0 + 50.0 * a.sin()) as f32)
}

/// The box colour pulses between two colours every 2 s.
pub fn box_color(t: f64) -> Color {
    let k = (1.0 + (TAU * t / 2.0).sin()) / 2.0;
    let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * k).round() as u8;
    Color::from_rgb_u8(mix(0x3f, 0xfc), mix(0xc1, 0x51), mix(0xc9, 0x85))
}

pub const VOLUME_MAX: u32 = 85; // 'Speaker Amp Master' range on the device

/// Volume level after `step` steps: up from 0 to 85 and back down, by 5.
pub fn volume_level(step: u64) -> u32 {
    let period = 2 * VOLUME_MAX as u64 / 5; // 34 steps up and down
    let i = (step % period) as u32 * 5;
    if i <= VOLUME_MAX { i } else { 2 * VOLUME_MAX - i }
}

pub struct Scene {
    pub changes: Rc<Cell<u8>>,
    pub quit: Rc<Cell<bool>>,
    pub stats: Rc<RefCell<Stats>>,
    _timer: Timer,
}

pub struct SceneConfig {
    /// The backend renders internally (winit, linuxkms): every tick is counted
    /// as a frame and `--frames` counts ticks.
    pub count_ticks: bool,
    /// Called once when `--seconds` or `--frames` is reached (inside the event loop).
    pub on_quit: Box<dyn Fn()>,
}

struct State {
    tick: u64,
    hms: (u32, u32, u32),
    pose_index: usize,
    pose_swaps: u64,
    volume_steps: u64,
}

pub fn start(app: &AppWindow, args: &Args, poses: Vec<Image>, stats: Rc<RefCell<Stats>>, cfg: SceneConfig) -> Scene {
    let changes = Rc::new(Cell::new(0u8));
    let quit = Rc::new(Cell::new(false));
    let fps = args.fps;

    app.set_pose(poses[0].clone());
    app.set_volume(0.0);
    app.set_volume_label(format!("Volume 0/{VOLUME_MAX}").into());
    let (x, y) = box_position(0.0);
    app.set_box_x(x);
    app.set_box_y(y);
    if args.slint_animate {
        app.set_box_animation(500);
    }
    if args.no_status {
        app.set_status(format!("{fps} fps | status line frozen (--no-status)").into());
    }

    let st = RefCell::new(State { tick: 0, hms: (99, 0, 0), pose_index: 0, pose_swaps: 0, volume_steps: 0 });
    let weak = app.as_weak();
    let timer = Timer::default();
    let (args2, changes2, quit2, stats2) = (args.clone(), changes.clone(), quit.clone(), stats.clone());
    let backgrounds = [Color::from_rgb_u8(0x1d, 0x24, 0x33), Color::from_rgb_u8(0x24, 0x1d, 0x2e)];
    stats.borrow_mut().scene_started();
    timer.start(TimerMode::Repeated, Duration::from_secs_f64(1.0 / fps), move || {
        let Some(app) = weak.upgrade() else { return };
        if quit2.get() {
            return;
        }
        let mut s = st.borrow_mut();
        s.tick += 1;
        // Wall-clock time of every tick: a late tick is rescheduled from `now`
        // by Slint, so lateness only shows up here (scene time below is tick based).
        stats2.borrow_mut().tick();
        // Scene time advances by exactly one tick per callback, so the box path
        // is frame-exact even when a tick is late.
        let t = s.tick as f64 / fps;
        let mut bits = 0;
        let a = &args2;

        if !a.no_box {
            if !a.slint_animate {
                let (x, y) = box_position(t);
                app.set_box_x(x);
                app.set_box_y(y);
                app.set_box_color(box_color(t));
                bits |= change::BOX;
            } else if s.tick % (fps / 2.0).round().max(1.0) as u64 == 0 {
                // A waypoint every 0.5 s; Slint animates in between.
                let (x, y) = box_position(t + 0.5);
                app.set_box_x(x);
                app.set_box_y(y);
                bits |= change::BOX;
            }
        }

        let hms = measure::local_hms();
        if hms != s.hms {
            s.hms = hms;
            app.set_clock(format!("{:02}:{:02}:{:02}", hms.0, hms.1, hms.2).into());
            let mut st = stats2.borrow_mut();
            let cpu = st.maybe_sample();
            if !a.no_status {
                let recent = st.recent_ms(fps as usize);
                let frame = recent.map_or(String::new(), |(m, x)| format!(" | frame {m:.1} ms (max {x:.1})"));
                let cpu = cpu.map_or(String::new(), |c| format!(" | CPU {c:.0}%"));
                app.set_status(
                    format!("{fps} fps{frame} | RSS {:.1} MB{cpu}", measure::rss_mb()).into(),
                );
            }
            bits |= change::CLOCK;
        }

        if a.volume_step_every > 0.0 && t >= (s.volume_steps + 1) as f64 * a.volume_step_every {
            s.volume_steps += 1;
            let level = volume_level(s.volume_steps);
            app.set_volume(level as f32 / VOLUME_MAX as f32);
            app.set_volume_label(format!("Volume {level}/{VOLUME_MAX}").into());
            bits |= change::VOLUME;
        }

        if a.pose_swap_every > 0.0 && t >= (s.pose_swaps + 1) as f64 * a.pose_swap_every {
            s.pose_swaps += 1;
            s.pose_index = (s.pose_index + 1) % poses.len();
            app.set_pose(poses[s.pose_index].clone());
            if a.full_swap {
                app.set_bg(backgrounds[s.pose_swaps as usize % 2]);
            }
            bits |= change::POSE;
        }

        changes2.set(changes2.get() | bits);
        // --seconds counts scene time (ticks), so a slow device runs longer
        // instead of fewer frames; a signal ends every backend the same way.
        let mut done = a.seconds.is_some_and(|n| t >= n) || crate::platform_fb::stop_requested();
        if cfg.count_ticks {
            stats2.borrow_mut().tick_unmeasured(Class::from_changes(changes2.replace(0), false));
            done |= a.frames.is_some_and(|n| s.tick as usize >= n);
        }
        if done {
            quit2.set(true);
            (cfg.on_quit)();
        }
    });

    Scene { changes, quit, stats, _timer: timer }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_stays_in_its_band() {
        for i in 0..96 {
            let (x, y) = box_position(i as f64 / 16.0);
            assert!((88.0..=648.0).contains(&x) && (936.0..=1036.0).contains(&y), "{x},{y}");
            let (x2, y2) = box_position((i + 1) as f64 / 16.0);
            // one tick moves less than the box size: old and new overlap or touch
            assert!((x2 - x).abs() < 32.0 && (y2 - y).abs() < 32.0);
        }
    }

    #[test]
    fn volume_ramps() {
        assert_eq!(volume_level(0), 0);
        assert_eq!(volume_level(17), 85);
        assert_eq!(volume_level(18), 80);
        assert_eq!(volume_level(34), 0);
    }
}
