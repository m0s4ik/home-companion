// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The character's animations: which frame to show when ([`Sequencer`], plain logic over time),
//! the frames themselves ([`Clip`], [`Playhead`]) and the thread that decodes the sheets.
//!
//! What plays (as the Python screen did):
//! - **loading**: from start-up until the body is ready (Home Assistant connected, or a timeout),
//!   in a loop;
//! - **home, awake** (touch, keys, movement, end of a conversation): `wake` once, then still on its
//!   last frame for `pause` seconds (the idle animation's), then `idle` once, still, `idle`… until
//!   the awake time ends; a clip under way is finished, a pause is cut short; then the still pose;
//! - **listening / thinking / speaking / error** (and the ringing timer, like `error`): the state's
//!   animation in a loop from its first frame;
//! - **night, the home at rest**: the home clip under way is finished, then `fall_asleep` once,
//!   then the still sleeping pose; every `pause` seconds (the `sleeping` animation's) `sleeping`
//!   plays once, then the still pose again (no frame drawn in between). When the screen wakes up
//!   or the night ends: `wake_from_sleep` once, then the home as by day, skipping `wake`. A
//!   conversation cuts any of these at once (they are decoration).
//!
//! Without an animation the still pose is shown, so a character without animations still works.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use character::{Animation, Sheet};

/// What the sequencer needs to know of an available animation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    pub frames: u32,
    pub fps: f32,
    pub pause: f32,
}

impl Timing {
    pub fn of(a: &Animation) -> Timing {
        Timing {
            frames: a.frames(),
            fps: a.fps,
            pause: a.pause,
        }
    }

    fn length(&self) -> Duration {
        Duration::from_secs_f32(self.frames as f32 / self.fps)
    }

    /// The frame `elapsed` after the start (not wrapped).
    fn at(&self, elapsed: Duration) -> u32 {
        (elapsed.as_secs_f32() * self.fps) as u32
    }
}

/// What the screen shows of the character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Pose,
    Frame { clip: &'static str, index: u32 },
}

/// What the sequencer plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The still pose, nothing moves (demo screenshots, offline).
    Still,
    /// Start-up: `loading` in a loop.
    Loading,
    /// The home screen: `wake` and `idle` while awake.
    Home,
    /// A state's animation in a loop (`listening`, `thinking`, `speaking`, `error`).
    Loop(&'static str),
    /// Night mode, the home at rest: `fall_asleep`, then asleep (`sleeping` now and then).
    Night,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Home {
    Still,
    Clip { clip: &'static str, start: Instant },
    Pause { clip: &'static str, until: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Night {
    /// The home clip under way when the night layout came: played to its end first.
    Finish { clip: &'static str, start: Instant },
    /// `fall_asleep` or one round of `sleeping`.
    Clip { clip: &'static str, start: Instant },
    /// The still sleeping pose until `sleeping` plays again (None: never).
    Still { until: Option<Instant> },
}

#[derive(Debug)]
pub struct Sequencer {
    clips: HashMap<&'static str, Timing>,
    mode: Mode,
    since: Instant,
    home: Home,
    night: Night,
}

impl Sequencer {
    pub fn new(now: Instant) -> Sequencer {
        Sequencer {
            clips: HashMap::new(),
            mode: Mode::Still,
            since: now,
            home: Home::Still,
            night: Night::Still { until: None },
        }
    }

    /// An animation became available (decoded).
    pub fn add(&mut self, name: &'static str, t: Timing) {
        self.clips.insert(name, t);
    }

    /// An animation let go (its sheet freed): the sequence goes on without it.
    pub fn remove(&mut self, name: &str) {
        self.clips.remove(name);
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// A new mode starts its loop from the first frame; the same mode goes on. Home to night
    /// finishes the home clip under way and falls asleep; night to home wakes up from sleep.
    pub fn set_mode(&mut self, mode: Mode, now: Instant) {
        if mode == self.mode {
            return;
        }
        let (prev, home, night) = (self.mode, self.home, self.night);
        self.mode = mode;
        self.since = now;
        self.home = Home::Still;
        match (prev, mode) {
            (_, Mode::Night) => {
                self.night = match home {
                    Home::Clip { clip, start } if prev == Mode::Home => {
                        Night::Finish { clip, start }
                    }
                    _ => self.fall_asleep(now),
                };
            }
            // Woken before he fell asleep: the home clip goes on.
            (Mode::Night, Mode::Home) if matches!(night, Night::Finish { .. }) => {
                if let Night::Finish { clip, start } = night {
                    self.home = Home::Clip { clip, start };
                }
            }
            (Mode::Night, Mode::Home) if self.clips.contains_key("wake_from_sleep") => {
                self.home = Home::Clip {
                    clip: "wake_from_sleep",
                    start: now,
                };
            }
            _ => {}
        }
    }

    /// `fall_asleep` from `start`, or straight to the still sleeping pose.
    fn fall_asleep(&self, start: Instant) -> Night {
        if self.clips.contains_key("fall_asleep") {
            Night::Clip {
                clip: "fall_asleep",
                start,
            }
        } else {
            self.asleep(start)
        }
    }

    /// The still sleeping pose from `from`, until `sleeping` plays again.
    fn asleep(&self, from: Instant) -> Night {
        Night::Still {
            until: self
                .clips
                .get("sleeping")
                .map(|t| from + Duration::from_secs_f32(t.pause.max(0.0))),
        }
    }

    /// The picture at `now` (advances the home sequence).
    pub fn show(&mut self, now: Instant, awake: bool) -> Show {
        match self.mode {
            Mode::Still => Show::Pose,
            Mode::Loading => self.looped("loading", now),
            Mode::Loop(name) => self.looped(name, now),
            Mode::Home => self.home(now, awake),
            Mode::Night => self.night(now),
        }
    }

    /// True while frames may change (the frame timer keeps running).
    pub fn moving(&self, awake: bool) -> bool {
        match self.mode {
            Mode::Still => false,
            Mode::Loading => self.clips.contains_key("loading"),
            Mode::Loop(name) => self.clips.contains_key(name),
            Mode::Home => {
                (awake && (self.clips.contains_key("idle") || self.clips.contains_key("wake")))
                    || self.home != Home::Still
            }
            Mode::Night => !matches!(self.night, Night::Still { .. }),
        }
    }

    fn looped(&self, name: &'static str, now: Instant) -> Show {
        match self.clips.get(name) {
            Some(t) => Show::Frame {
                clip: name,
                index: t.at(now.saturating_duration_since(self.since)) % t.frames,
            },
            None => Show::Pose,
        }
    }

    /// The pause after a home clip: the idle animation's (or the clip's own without idle).
    fn pause(&self, clip: &'static str) -> Duration {
        let t = self.clips.get("idle").unwrap_or(&self.clips[clip]);
        Duration::from_secs_f32(t.pause.max(0.0))
    }

    fn night(&mut self, now: Instant) -> Show {
        loop {
            match self.night {
                Night::Finish { clip, start } | Night::Clip { clip, start } => {
                    let t = self.clips[clip];
                    let end = start + t.length();
                    if now < end {
                        let index = t.at(now - start).min(t.frames - 1);
                        return Show::Frame { clip, index };
                    }
                    self.night = if matches!(self.night, Night::Finish { .. }) {
                        self.fall_asleep(end)
                    } else {
                        self.asleep(end)
                    };
                }
                Night::Still { until } => match until {
                    // From now, not from `until`: the clock ticks once a second while still,
                    // and the round starts from its first frame.
                    Some(t) if now >= t => {
                        self.night = Night::Clip {
                            clip: "sleeping",
                            start: now,
                        };
                    }
                    _ => return Show::Pose,
                },
            }
        }
    }

    fn home(&mut self, now: Instant, awake: bool) -> Show {
        let first = ["wake", "idle"]
            .into_iter()
            .find(|n| self.clips.contains_key(n));
        loop {
            match self.home {
                Home::Still => {
                    let (true, Some(first)) = (awake, first) else {
                        return Show::Pose;
                    };
                    self.home = Home::Clip {
                        clip: first,
                        start: now,
                    };
                }
                Home::Clip { clip, start } => {
                    let t = self.clips[clip];
                    let end = start + t.length();
                    if now < end {
                        let index = t.at(now - start).min(t.frames - 1);
                        return Show::Frame { clip, index };
                    }
                    self.home = if awake {
                        Home::Pause {
                            clip,
                            until: end + self.pause(clip),
                        }
                    } else {
                        Home::Still
                    };
                }
                Home::Pause { clip, until } => {
                    if !awake {
                        self.home = Home::Still;
                        return Show::Pose;
                    }
                    if now < until {
                        return Show::Frame {
                            clip,
                            index: self.clips[clip].frames - 1,
                        };
                    }
                    self.home = Home::Clip {
                        clip: if self.clips.contains_key("idle") {
                            "idle"
                        } else {
                            first.unwrap_or(clip)
                        },
                        start: until,
                    };
                }
            }
        }
    }
}

/// An animation ready to play: its sheet and its first frame.
pub struct Clip {
    pub anim: Animation,
    sheet: Arc<Sheet>,
    key: Vec<u8>,
}

impl Clip {
    pub fn new(anim: Animation, sheet: Arc<Sheet>) -> Clip {
        let key = sheet.frame(anim.first as usize);
        Clip { anim, sheet, key }
    }

    pub fn width(&self) -> u32 {
        self.sheet.width
    }

    pub fn height(&self) -> u32 {
        self.sheet.height
    }
}

/// The frame being shown of a clip, kept up to date by applying the changes of each frame.
pub struct Playhead {
    pub clip: Arc<Clip>,
    pub index: u32,
    /// RGBA, `width * height * 4`.
    pub pixels: Vec<u8>,
}

impl Playhead {
    pub fn new(clip: Arc<Clip>) -> Playhead {
        let pixels = clip.key.clone();
        Playhead {
            clip,
            index: 0,
            pixels,
        }
    }

    /// Moves to frame `index` of the clip: the next frame applies its changes, any other one
    /// starts again from the first frame. Returns whether the picture changed.
    pub fn seek(&mut self, index: u32) -> bool {
        let index = index.min(self.clip.anim.frames() - 1);
        if index == self.index {
            return false;
        }
        let first = self.clip.anim.first as usize;
        let frames = &self.clip.sheet.frames;
        if index == self.index + 1 {
            frames[first + index as usize].apply(&mut self.pixels);
        } else {
            self.pixels.copy_from_slice(&self.clip.key);
            for d in &frames[first + 1..=first + index as usize] {
                d.apply(&mut self.pixels);
            }
        }
        self.index = index;
        true
    }
}

/// Decodes the sheets of `anims` in a thread, in the given order (the loading animation first),
/// each sheet once even when animations share it. Sends each animation as soon as its sheet is
/// ready (their plates flattened to `bg`, like the poses); a sheet that fails is logged and its animations are left out (the still pose shows).
pub fn load_in_background(anims: Vec<Animation>, bg: character::Rgb) -> Receiver<Clip> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("animations".into())
        .spawn(move || {
            // Sheet file and frame size -> the decoded sheet (None: it failed).
            type Key = (PathBuf, (u32, u32));
            let mut done: HashMap<Key, Option<Arc<Sheet>>> = HashMap::new();
            for a in anims {
                let key = (a.file.clone(), a.size);
                let sheet = done
                    .entry(key)
                    .or_insert_with(|| {
                        let t = Instant::now();
                        match Sheet::load(&a.file, a.size) {
                            Ok(mut s) => {
                                s.flatten(bg, super::screen::PLATE_TOLERANCE);
                                log::info!(
                                    "animation sheet {}: {} frames {}x{}, {:.1} MB of changes, decoded in {} ms",
                                    a.file.display(),
                                    s.frames.len(),
                                    s.width,
                                    s.height,
                                    s.bytes() as f64 / 1048576.0,
                                    t.elapsed().as_millis()
                                );
                                Some(Arc::new(s))
                            }
                            Err(e) => {
                                log::warn!("animation {}: {e} (the still pose is shown)", a.name);
                                None
                            }
                        }
                    })
                    .clone();
                if let Some(s) = sheet {
                    if tx.send(Clip::new(a, s)).is_err() {
                        return;
                    }
                }
            }
        });
    rx
}

/// The name as a `&'static str` (the sequencer's keys), if it is a known animation.
pub fn static_name(name: &str) -> Option<&'static str> {
    character::ANIMATIONS.into_iter().find(|n| *n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// 16 fps; wake 8 frames (0.5 s), idle 4 frames (0.25 s), pause 1 s.
    fn seq(t0: Instant) -> Sequencer {
        let mut s = Sequencer::new(t0);
        s.add(
            "wake",
            Timing {
                frames: 8,
                fps: 16.0,
                pause: 0.0,
            },
        );
        s.add(
            "idle",
            Timing {
                frames: 4,
                fps: 16.0,
                pause: 1.0,
            },
        );
        s.add(
            "listening",
            Timing {
                frames: 4,
                fps: 16.0,
                pause: 0.0,
            },
        );
        s.set_mode(Mode::Home, t0);
        s
    }

    fn frame(clip: &'static str, index: u32) -> Show {
        Show::Frame { clip, index }
    }

    #[test]
    fn home_asleep_is_the_pose() {
        let t0 = Instant::now();
        let mut s = seq(t0);
        assert_eq!(s.show(t0, false), Show::Pose);
        assert!(!s.moving(false));
    }

    #[test]
    fn awake_plays_wake_pause_idle_pause_idle() {
        let t0 = Instant::now();
        let mut s = seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        assert_eq!(s.show(t0 + ms(130), true), frame("wake", 2));
        // End of wake (0.5 s): still on its last frame for the idle pause (1 s).
        assert_eq!(s.show(t0 + ms(500), true), frame("wake", 7));
        assert_eq!(s.show(t0 + ms(1400), true), frame("wake", 7));
        // 1.5 s: idle from its first frame, 0.25 s long.
        assert_eq!(s.show(t0 + ms(1500), true), frame("idle", 0));
        assert_eq!(s.show(t0 + ms(1700), true), frame("idle", 3));
        assert_eq!(s.show(t0 + ms(2000), true), frame("idle", 3)); // pause
        assert_eq!(s.show(t0 + ms(2750), true), frame("idle", 0)); // again at 1.75 + 1
        assert!(s.moving(true));
    }

    #[test]
    fn falling_asleep_finishes_the_clip_and_cuts_the_pause() {
        let t0 = Instant::now();
        let mut s = seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        // Asleep during the clip: it plays to the end, then the pose.
        assert_eq!(s.show(t0 + ms(250), false), frame("wake", 4));
        assert!(s.moving(false));
        assert_eq!(s.show(t0 + ms(600), false), Show::Pose);
        assert!(!s.moving(false));
        // Awake again: from wake.
        assert_eq!(s.show(t0 + ms(700), true), frame("wake", 0));
        // Asleep during the pause: the pose at once.
        assert_eq!(s.show(t0 + ms(1300), true), frame("wake", 7));
        assert_eq!(s.show(t0 + ms(1400), false), Show::Pose);
    }

    #[test]
    fn a_late_tick_skips_through_the_sequence() {
        let t0 = Instant::now();
        let mut s = seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        // 1.6 s later in one step: wake (0.5) + pause (1.0) → idle frame 1.
        assert_eq!(s.show(t0 + ms(1600), true), frame("idle", 1));
    }

    #[test]
    fn states_loop_and_restart_on_a_new_mode() {
        let t0 = Instant::now();
        let mut s = seq(t0);
        s.set_mode(Mode::Loop("listening"), t0);
        assert_eq!(s.show(t0 + ms(70), true), frame("listening", 1));
        assert_eq!(s.show(t0 + ms(320), true), frame("listening", 1)); // 5 % 4
        s.set_mode(Mode::Loop("listening"), t0 + ms(400)); // same mode: goes on
        assert_eq!(s.show(t0 + ms(400), true), frame("listening", 2));
        // No animation for a state, or still: the pose.
        s.set_mode(Mode::Loop("thinking"), t0);
        assert_eq!(s.show(t0, true), Show::Pose);
        assert!(!s.moving(true));
        s.set_mode(Mode::Still, t0);
        assert_eq!(s.show(t0, true), Show::Pose);
        // Back home awake: wake again from its first frame.
        s.set_mode(Mode::Home, t0 + ms(900));
        assert_eq!(s.show(t0 + ms(900), true), frame("wake", 0));
    }

    /// `seq` plus the night: fall_asleep 8 frames (0.5 s), sleeping 4 (0.25 s) every 2 s,
    /// wake_from_sleep 8 (0.5 s).
    fn night_seq(t0: Instant) -> Sequencer {
        let mut s = seq(t0);
        for (name, frames, pause) in [
            ("fall_asleep", 8, 0.0),
            ("sleeping", 4, 2.0),
            ("wake_from_sleep", 8, 0.0),
        ] {
            s.add(
                name,
                Timing {
                    frames,
                    fps: 16.0,
                    pause,
                },
            );
        }
        s
    }

    #[test]
    fn night_falls_asleep_then_sleeps_now_and_then() {
        let t0 = Instant::now();
        let mut s = night_seq(t0);
        assert_eq!(s.show(t0, false), Show::Pose);
        s.set_mode(Mode::Night, t0);
        assert_eq!(s.show(t0, false), frame("fall_asleep", 0));
        assert!(s.moving(false));
        assert_eq!(s.show(t0 + ms(440), false), frame("fall_asleep", 7));
        // Asleep: the still pose, nothing moves (no frame timer), for the pause (2 s).
        assert_eq!(s.show(t0 + ms(500), false), Show::Pose);
        assert!(!s.moving(false));
        assert_eq!(s.show(t0 + ms(2400), false), Show::Pose);
        // A round of sleeping from when it is seen (the clock's tick), then still again.
        assert_eq!(s.show(t0 + ms(2900), false), frame("sleeping", 0));
        assert!(s.moving(false));
        assert_eq!(s.show(t0 + ms(3100), false), frame("sleeping", 3));
        assert_eq!(s.show(t0 + ms(3150), false), Show::Pose);
        assert!(!s.moving(false));
        assert_eq!(s.show(t0 + ms(5100), false), Show::Pose);
        assert_eq!(s.show(t0 + ms(5150), false), frame("sleeping", 0));
    }

    #[test]
    fn night_finishes_the_home_clip_first() {
        let t0 = Instant::now();
        let mut s = night_seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        // The night layout comes during wake (0.5 s): wake to its end, then fall_asleep.
        s.set_mode(Mode::Night, t0 + ms(250));
        assert_eq!(s.show(t0 + ms(250), false), frame("wake", 4));
        assert_eq!(s.show(t0 + ms(500), false), frame("fall_asleep", 0));
        assert_eq!(s.show(t0 + ms(1000), false), Show::Pose);
        // From the idle pause (or still): fall_asleep at once.
        let mut s = night_seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        assert_eq!(s.show(t0 + ms(600), true), frame("wake", 7));
        s.set_mode(Mode::Night, t0 + ms(700));
        assert_eq!(s.show(t0 + ms(700), false), frame("fall_asleep", 0));
    }

    #[test]
    fn waking_at_night_plays_wake_from_sleep_then_idle() {
        let t0 = Instant::now();
        let mut s = night_seq(t0);
        s.set_mode(Mode::Night, t0);
        assert_eq!(s.show(t0 + ms(1000), false), Show::Pose);
        // A touch: wake_from_sleep, the idle pause on its last frame, then idle (not wake).
        s.set_mode(Mode::Home, t0 + ms(1000));
        assert_eq!(s.show(t0 + ms(1000), true), frame("wake_from_sleep", 0));
        assert_eq!(s.show(t0 + ms(1490), true), frame("wake_from_sleep", 7));
        assert_eq!(s.show(t0 + ms(2400), true), frame("wake_from_sleep", 7));
        assert_eq!(s.show(t0 + ms(2500), true), frame("idle", 0));
        // Awake time over, still night: fall_asleep, asleep.
        s.set_mode(Mode::Night, t0 + ms(2600));
        assert_eq!(s.show(t0 + ms(2600), false), frame("idle", 1));
        assert_eq!(s.show(t0 + ms(2750), false), frame("fall_asleep", 0));
        // The morning, nobody awake: wake_from_sleep, then the idle pose.
        s.set_mode(Mode::Home, t0 + ms(4000));
        assert_eq!(s.show(t0 + ms(4000), false), frame("wake_from_sleep", 0));
        assert!(s.moving(false));
        assert_eq!(s.show(t0 + ms(4500), false), Show::Pose);
        assert!(!s.moving(false));
    }

    #[test]
    fn a_conversation_cuts_the_night_clips() {
        let t0 = Instant::now();
        let mut s = night_seq(t0);
        s.set_mode(Mode::Night, t0);
        assert_eq!(s.show(t0 + ms(100), false), frame("fall_asleep", 1));
        s.set_mode(Mode::Loop("listening"), t0 + ms(150));
        assert_eq!(s.show(t0 + ms(150), true), frame("listening", 0));
        // Woken during wake_from_sleep: listening at once too.
        s.set_mode(Mode::Night, t0 + ms(200));
        s.set_mode(Mode::Home, t0 + ms(800));
        assert_eq!(s.show(t0 + ms(850), true), frame("wake_from_sleep", 0));
        s.set_mode(Mode::Loop("listening"), t0 + ms(900));
        assert_eq!(s.show(t0 + ms(900), true), frame("listening", 0));
        // After the conversation, home as by day: wake.
        s.set_mode(Mode::Home, t0 + ms(1000));
        assert_eq!(s.show(t0 + ms(1000), true), frame("wake", 0));
    }

    #[test]
    fn night_without_animations_is_the_pose() {
        let t0 = Instant::now();
        let mut s = seq(t0);
        s.set_mode(Mode::Night, t0);
        assert_eq!(s.show(t0, false), Show::Pose);
        assert!(!s.moving(false));
        // Back home: wake as usual (no wake_from_sleep).
        s.set_mode(Mode::Home, t0 + ms(100));
        assert_eq!(s.show(t0 + ms(100), true), frame("wake", 0));
    }

    #[test]
    fn idle_alone_without_wake() {
        let t0 = Instant::now();
        let mut s = Sequencer::new(t0);
        s.add(
            "idle",
            Timing {
                frames: 4,
                fps: 16.0,
                pause: 1.0,
            },
        );
        s.set_mode(Mode::Home, t0);
        assert_eq!(s.show(t0, true), frame("idle", 0));
        assert_eq!(s.show(t0 + ms(500), true), frame("idle", 3));
        // Without any animation home is the pose even when awake.
        let mut empty = Sequencer::new(t0);
        empty.set_mode(Mode::Home, t0);
        assert_eq!(empty.show(t0, true), Show::Pose);
        assert!(!empty.moving(true));
    }

    #[test]
    fn playhead_applies_changes_and_seeks() {
        let c = character::load(&character::repo_characters().join("alfred")).unwrap();
        let a = c.animation("wake").unwrap().clone();
        let sheet = Arc::new(Sheet::load(&a.file, a.size).unwrap());
        let clip = Arc::new(Clip::new(a.clone(), sheet.clone()));
        let mut p = Playhead::new(clip);
        for i in 1..10 {
            assert!(p.seek(i));
        }
        assert_eq!(p.pixels, sheet.frame(a.first as usize + 9));
        assert!(!p.seek(9));
        // Backwards: from the first frame again.
        assert!(p.seek(3));
        assert_eq!(p.pixels, sheet.frame(a.first as usize + 3));
        // Past the end: the last frame.
        p.seek(10_000);
        assert_eq!(p.index, a.frames() - 1);
    }
}
