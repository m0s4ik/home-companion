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
//! - **the settings pages**: the home clip under way is finished, then `settings_in` once, then
//!   still on its last frame; every `pause` seconds (the `settings_hold` animation's, while the
//!   screen is awake) `settings_hold` plays once. Leaving them for the home: the settings clip
//!   under way is finished, then `settings_out`, then the home as by day (its pause, `idle`).
//!   Opening them again while `settings_out` plays finishes it first. A conversation cuts these
//!   at once (decoration).
//! - **the More page**: the same with `more_in` (he turns to its tiles), `more_hold` now and then
//!   and `more_out`. Going from one of these pages to the other puts the first page's things
//!   away (its `_out`) before the second's `_in`.
//! - **a house command went through**: `done` once (a bow) on the home, then as after `wake`;
//!   when it comes during the conversation, it plays when the home comes back (within
//!   [`DONE_WAIT`]).
//!
//! Without an animation the still pose is shown, so a character without animations still works.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use character::{Animation, Sheet};

/// How long a `done` waits for the home to come back (the conversation closes a few seconds
/// after the command went through).
pub const DONE_WAIT: Duration = Duration::from_secs(15);

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
    /// A settings page: `settings_in`, then `settings_hold` now and then.
    Settings,
    /// The More page: `more_in`, then `more_hold` now and then.
    More,
    /// One fixed frame (the demos' screenshots of an animation).
    Frame(&'static str, u32),
}

/// A page with its own clips: `into` once, then `hold` now and then, `out` when it is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Act {
    into: &'static str,
    hold: &'static str,
    out: &'static str,
}

const SETTINGS: Act = Act {
    into: "settings_in",
    hold: "settings_hold",
    out: "settings_out",
};
const MORE: Act = Act {
    into: "more_in",
    hold: "more_hold",
    out: "more_out",
};

impl Mode {
    /// The page's clips, for the modes that have them.
    fn act(self) -> Option<Act> {
        match self {
            Mode::Settings => Some(SETTINGS),
            Mode::More => Some(MORE),
            _ => None,
        }
    }
}

/// The page whose `into` or `hold` this clip is (it ends with the things held: leaving plays
/// that page's `out` after it).
fn held_by(clip: &str) -> Option<Act> {
    [SETTINGS, MORE]
        .into_iter()
        .find(|a| clip == a.into || clip == a.hold)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tools {
    /// The clip under way when the settings page opened (a home clip, or `settings_out`):
    /// played to its end first.
    Finish { clip: &'static str, start: Instant },
    /// No settings clip yet (the sheet is being decoded): the pose.
    Wait,
    /// `settings_in` or one round of `settings_hold`.
    Clip { clip: &'static str, start: Instant },
    /// Still on the clip's last frame until `settings_hold` plays again.
    Still { clip: &'static str, until: Instant },
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
    tools: Tools,
    /// `done` waiting for the home, until then.
    done_until: Option<Instant>,
}

impl Sequencer {
    pub fn new(now: Instant) -> Sequencer {
        Sequencer {
            clips: HashMap::new(),
            mode: Mode::Still,
            since: now,
            home: Home::Still,
            night: Night::Still { until: None },
            tools: Tools::Wait,
            done_until: None,
        }
    }

    /// A house command went through: `done` now on the home, or when the home comes back
    /// within [`DONE_WAIT`]. Nothing without the animation, or at night.
    pub fn done(&mut self, now: Instant) {
        if !self.clips.contains_key("done") {
            return;
        }
        match self.mode {
            Mode::Home => {
                self.home = Home::Clip {
                    clip: "done",
                    start: now,
                };
                self.done_until = None;
            }
            Mode::Night => self.done_until = None,
            _ => self.done_until = Some(now + DONE_WAIT),
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
        let (prev, home, night, tools) = (self.mode, self.home, self.night, self.tools);
        self.mode = mode;
        self.since = now;
        self.home = Home::Still;
        match (prev, mode) {
            (_, Mode::Settings | Mode::More) => {
                self.tools = match (prev.act(), home, tools) {
                    (None, Home::Clip { clip, start }, _) if prev == Mode::Home => {
                        Tools::Finish { clip, start }
                    }
                    // From the other page: its clip under way, or its things put away.
                    (Some(_), _, Tools::Finish { clip, start } | Tools::Clip { clip, start }) => {
                        Tools::Finish { clip, start }
                    }
                    (Some(_), _, Tools::Still { clip, .. }) => match self.put_away(clip) {
                        Some(out) => Tools::Finish {
                            clip: out,
                            start: now,
                        },
                        None => self.tools_in(now),
                    },
                    _ => self.tools_in(now),
                };
            }
            // Leaving the page: its clip under way, then its things put away.
            (Mode::Settings | Mode::More, Mode::Home) => {
                self.done_until = None;
                self.home = match tools {
                    Tools::Finish { clip, start } => Home::Clip { clip, start },
                    Tools::Clip { clip, start } => Home::Clip { clip, start },
                    Tools::Still { clip, .. } => match self.put_away(clip) {
                        Some(out) => Home::Clip {
                            clip: out,
                            start: now,
                        },
                        None => Home::Still,
                    },
                    _ => Home::Still,
                };
            }
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
            (_, Mode::Home) if self.done_until.take().is_some_and(|t| now < t) => {
                self.home = Home::Clip {
                    clip: "done",
                    start: now,
                };
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

    /// The page's `into` from `start`; without it its `hold`; without either the pose.
    fn tools_in(&self, start: Instant) -> Tools {
        let Some(act) = self.mode.act() else {
            return Tools::Wait;
        };
        [act.into, act.hold]
            .into_iter()
            .find(|n| self.clips.contains_key(n))
            .map_or(Tools::Wait, |clip| Tools::Clip { clip, start })
    }

    /// The `out` that follows `clip` when it ends with a page's things held (and is there).
    fn put_away(&self, clip: &str) -> Option<&'static str> {
        held_by(clip)
            .map(|a| a.out)
            .filter(|out| self.clips.contains_key(out))
    }

    /// A settings clip is on screen or still to play (its sheet must stay).
    pub fn uses_settings(&self) -> bool {
        let settings = |c: &str| c.starts_with("settings_");
        match self.mode {
            Mode::Settings => true,
            Mode::More => matches!(self.tools, Tools::Finish { clip, .. } if settings(clip)),
            Mode::Home => match self.home {
                Home::Clip { clip, .. } | Home::Pause { clip, .. } => settings(clip),
                Home::Still => false,
            },
            Mode::Night => matches!(self.night, Night::Finish { clip, .. } if settings(clip)),
            _ => false,
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
            Mode::Settings | Mode::More => self.tools(now, awake),
            Mode::Frame(clip, index) => match self.clips.get(clip) {
                Some(t) => Show::Frame {
                    clip,
                    index: index.min(t.frames - 1),
                },
                None => Show::Pose,
            },
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
            Mode::Settings | Mode::More => {
                matches!(self.tools, Tools::Finish { .. } | Tools::Clip { .. })
            }
            Mode::Frame(..) => false,
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
        let t = self.clips.get("idle").or_else(|| self.clips.get(clip));
        Duration::from_secs_f32(t.map_or(0.0, |t| t.pause.max(0.0)))
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

    fn tools(&mut self, now: Instant, awake: bool) -> Show {
        loop {
            match self.tools {
                Tools::Wait => {
                    self.tools = self.tools_in(now);
                    if self.tools == Tools::Wait {
                        return Show::Pose;
                    }
                }
                Tools::Finish { clip, start } | Tools::Clip { clip, start } => {
                    let Some(t) = self.clips.get(clip).copied() else {
                        self.tools = Tools::Wait;
                        return Show::Pose;
                    };
                    let end = start + t.length();
                    if now < end {
                        let index = t.at(now - start).min(t.frames - 1);
                        return Show::Frame { clip, index };
                    }
                    let act = self.mode.act();
                    let finished = matches!(self.tools, Tools::Finish { .. });
                    self.tools = match held_by(clip) {
                        // The other page's things: put away first.
                        Some(other) if finished && Some(other) != act => {
                            match self.put_away(clip) {
                                Some(out) => Tools::Finish {
                                    clip: out,
                                    start: end,
                                },
                                None => self.tools_in(end),
                            }
                        }
                        // This page's things held (a clip of it, or one carried from the home):
                        // still until the next round.
                        Some(_) => {
                            let pause = act
                                .and_then(|a| self.clips.get(a.hold))
                                .map_or(f32::INFINITY, |h| h.pause.max(0.0));
                            Tools::Still {
                                clip,
                                until: end + Duration::from_secs_f32(pause.min(86400.0)),
                            }
                        }
                        None => self.tools_in(end),
                    };
                }
                Tools::Still { clip, until } => {
                    // From now, not from `until` (the clock ticks once a second while still);
                    // only while the screen is awake.
                    let hold = self.mode.act().map(|a| a.hold);
                    if let Some(hold) =
                        hold.filter(|h| awake && now >= until && self.clips.contains_key(h))
                    {
                        self.tools = Tools::Clip {
                            clip: hold,
                            start: now,
                        };
                        continue;
                    }
                    return match self.clips.get(clip) {
                        Some(t) => Show::Frame {
                            clip,
                            index: t.frames - 1,
                        },
                        None => Show::Pose,
                    };
                }
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
                    let Some(t) = self.clips.get(clip).copied() else {
                        // Its sheet went (freed): the pose.
                        self.home = Home::Still;
                        return Show::Pose;
                    };
                    let end = start + t.length();
                    if now < end {
                        let index = t.at(now - start).min(t.frames - 1);
                        return Show::Frame { clip, index };
                    }
                    // A page's clip finished after the page was left: its things go away.
                    if held_by(clip).is_some() {
                        self.home = match self.put_away(clip) {
                            Some(out) => Home::Clip {
                                clip: out,
                                start: end,
                            },
                            None => Home::Still,
                        };
                        continue;
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
                        return match self.clips.get(clip) {
                            Some(t) => Show::Frame {
                                clip,
                                index: t.frames - 1,
                            },
                            None => Show::Pose,
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

    #[test]
    fn done_plays_on_the_home_now_or_when_it_comes_back() {
        let t0 = Instant::now();
        let mut s = Sequencer::new(t0);
        let t = Timing {
            frames: 48,
            fps: 16.0,
            pause: 5.0,
        };
        s.add("done", t);
        s.add("idle", t);
        s.add("thinking", t);
        // During the conversation: waits, then plays when the home comes back.
        s.set_mode(Mode::Loop("thinking"), t0);
        s.done(t0);
        s.set_mode(Mode::Home, t0 + ms(2000));
        assert_eq!(s.show(t0 + ms(2000), true), frame("done", 0));
        assert_eq!(s.show(t0 + ms(2500), true), frame("done", 8));
        // Too late: the home as usual.
        s.set_mode(Mode::Loop("thinking"), t0 + ms(10_000));
        s.done(t0 + ms(10_000));
        s.set_mode(Mode::Home, t0 + ms(10_000) + DONE_WAIT + ms(1));
        assert_ne!(
            s.show(t0 + ms(10_000) + DONE_WAIT + ms(1), true),
            frame("done", 0)
        );
        // Already home: at once.
        s.done(t0 + ms(40_000));
        assert_eq!(s.show(t0 + ms(40_000), true), frame("done", 0));
    }

    /// `seq` with the settings clips: in, hold (pause 1 s) and out, 4 frames (0.25 s) each.
    fn tools_seq(t0: Instant) -> Sequencer {
        let mut s = seq(t0);
        for (name, pause) in [
            ("settings_in", 0.0),
            ("settings_hold", 1.0),
            ("settings_out", 0.0),
        ] {
            s.add(
                name,
                Timing {
                    frames: 4,
                    fps: 16.0,
                    pause,
                },
            );
        }
        s
    }

    #[test]
    fn settings_in_then_hold_now_and_then() {
        let t0 = Instant::now();
        let mut s = tools_seq(t0);
        s.set_mode(Mode::Settings, t0);
        assert!(s.uses_settings());
        assert_eq!(s.show(t0, true), frame("settings_in", 0));
        assert!(s.moving(true));
        assert_eq!(s.show(t0 + ms(200), true), frame("settings_in", 3));
        // Over: still on its last frame (no frame timer) until the hold's pause is over.
        assert_eq!(s.show(t0 + ms(300), true), frame("settings_in", 3));
        assert!(!s.moving(true));
        assert_eq!(s.show(t0 + ms(1250), true), frame("settings_hold", 0));
        assert_eq!(s.show(t0 + ms(1400), true), frame("settings_hold", 2));
        assert_eq!(s.show(t0 + ms(1600), true), frame("settings_hold", 3));
        assert!(!s.moving(true));
        // Due again, but the screen is asleep: it waits, still.
        assert_eq!(s.show(t0 + ms(2600), false), frame("settings_hold", 3));
        assert_eq!(s.show(t0 + ms(2700), true), frame("settings_hold", 0));
    }

    #[test]
    fn leaving_settings_puts_the_tools_away_then_home_as_by_day() {
        let t0 = Instant::now();
        let mut s = tools_seq(t0);
        s.set_mode(Mode::Settings, t0);
        assert_eq!(s.show(t0 + ms(300), true), frame("settings_in", 3));
        s.set_mode(Mode::Home, t0 + ms(400));
        assert_eq!(s.show(t0 + ms(400), true), frame("settings_out", 0));
        assert_eq!(s.show(t0 + ms(640), true), frame("settings_out", 3));
        // Then the idle pause on its last frame (the idle pose), and idle.
        assert_eq!(s.show(t0 + ms(700), true), frame("settings_out", 3));
        assert!(s.uses_settings());
        assert_eq!(s.show(t0 + ms(1650), true), frame("idle", 0));
        assert!(!s.uses_settings());
    }

    #[test]
    fn quick_open_and_close_finish_the_clip_under_way() {
        let t0 = Instant::now();
        let mut s = tools_seq(t0);
        s.set_mode(Mode::Settings, t0);
        assert_eq!(s.show(t0 + ms(100), true), frame("settings_in", 1));
        // Left while the tools come out: they finish coming out, then go away.
        s.set_mode(Mode::Home, t0 + ms(100));
        assert_eq!(s.show(t0 + ms(200), true), frame("settings_in", 3));
        assert_eq!(s.show(t0 + ms(300), true), frame("settings_out", 0));
        // Opened again while they go away: put away first, then out again.
        s.set_mode(Mode::Settings, t0 + ms(350));
        assert_eq!(s.show(t0 + ms(400), true), frame("settings_out", 2));
        assert_eq!(s.show(t0 + ms(500), true), frame("settings_in", 0));
        // A conversation cuts at once; back on the page, the tools come out again.
        s.set_mode(Mode::Loop("listening"), t0 + ms(550));
        assert!(!s.uses_settings());
        assert_eq!(s.show(t0 + ms(550), true), frame("listening", 0));
        s.set_mode(Mode::Settings, t0 + ms(900));
        assert_eq!(s.show(t0 + ms(900), true), frame("settings_in", 0));
    }

    #[test]
    fn settings_wait_for_the_home_clip_and_for_the_sheet() {
        let t0 = Instant::now();
        let mut s = tools_seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        // Opened while wake plays: wake ends (0.5 s), then the tools.
        s.set_mode(Mode::Settings, t0 + ms(100));
        assert_eq!(s.show(t0 + ms(300), true), frame("wake", 4));
        assert_eq!(s.show(t0 + ms(500), true), frame("settings_in", 0));
        // Without the settings clips (the sheet not decoded yet): the pose, nothing moves; they
        // start when the sheet comes.
        let mut s = seq(t0);
        s.set_mode(Mode::Settings, t0);
        assert_eq!(s.show(t0, true), Show::Pose);
        assert!(!s.moving(true));
        s.add(
            "settings_in",
            Timing {
                frames: 4,
                fps: 16.0,
                pause: 0.0,
            },
        );
        assert_eq!(s.show(t0 + ms(800), true), frame("settings_in", 0));
        assert_eq!(s.show(t0 + ms(1200), true), frame("settings_in", 3));
        // No hold and no out: still on the last frame; leaving shows the pose at once.
        s.set_mode(Mode::Home, t0 + ms(1300));
        assert_eq!(s.show(t0 + ms(1300), false), Show::Pose);
        // The sheet freed under a clip: the pose, no panic.
        let mut s = tools_seq(t0);
        s.set_mode(Mode::Settings, t0);
        s.set_mode(Mode::Home, t0 + ms(50));
        s.remove("settings_in");
        s.remove("settings_out");
        assert_eq!(s.show(t0 + ms(100), true), Show::Pose);
    }

    #[test]
    fn a_fixed_frame_for_the_demos() {
        let t0 = Instant::now();
        let mut s = tools_seq(t0);
        s.set_mode(Mode::Frame("settings_hold", 99), t0);
        assert_eq!(s.show(t0, true), frame("settings_hold", 3));
        assert!(!s.moving(true));
        s.set_mode(Mode::Frame("missing", 0), t0);
        assert_eq!(s.show(t0, true), Show::Pose);
    }

    /// `tools_seq` with the More page's clips too, 4 frames (0.25 s) each, hold pause 1 s.
    fn both_seq(t0: Instant) -> Sequencer {
        let mut s = tools_seq(t0);
        for (name, pause) in [("more_in", 0.0), ("more_hold", 1.0), ("more_out", 0.0)] {
            s.add(
                name,
                Timing {
                    frames: 4,
                    fps: 16.0,
                    pause,
                },
            );
        }
        s
    }

    #[test]
    fn more_looks_at_the_tiles_and_back() {
        let t0 = Instant::now();
        let mut s = both_seq(t0);
        s.set_mode(Mode::More, t0);
        assert!(!s.uses_settings());
        assert_eq!(s.show(t0, true), frame("more_in", 0));
        assert_eq!(s.show(t0 + ms(300), true), frame("more_in", 3));
        // Now and then: one round of more_hold.
        assert_eq!(s.show(t0 + ms(1250), true), frame("more_hold", 0));
        assert_eq!(s.show(t0 + ms(1600), true), frame("more_hold", 3));
        // Back home: looks ahead again, then the home.
        s.set_mode(Mode::Home, t0 + ms(1700));
        assert_eq!(s.show(t0 + ms(1700), true), frame("more_out", 0));
        assert_eq!(s.show(t0 + ms(1940), true), frame("more_out", 3));
    }

    #[test]
    fn from_more_to_settings_he_looks_ahead_first() {
        let t0 = Instant::now();
        let mut s = both_seq(t0);
        s.set_mode(Mode::More, t0);
        assert_eq!(s.show(t0 + ms(300), true), frame("more_in", 3));
        // Settings opened from More: more_out, then the tools come out.
        s.set_mode(Mode::Settings, t0 + ms(400));
        assert!(s.uses_settings());
        assert_eq!(s.show(t0 + ms(400), true), frame("more_out", 0));
        assert_eq!(s.show(t0 + ms(640), true), frame("more_out", 3));
        assert_eq!(s.show(t0 + ms(660), true), frame("settings_in", 0));
        // Back to More from the settings: the tools put away, then he turns to the tiles.
        assert_eq!(s.show(t0 + ms(950), true), frame("settings_in", 3));
        s.set_mode(Mode::More, t0 + ms(1000));
        assert!(s.uses_settings());
        assert_eq!(s.show(t0 + ms(1000), true), frame("settings_out", 0));
        assert_eq!(s.show(t0 + ms(1260), true), frame("more_in", 0));
        assert!(!s.uses_settings());
        // A More clip under way when Settings opens is finished, then put away.
        s.set_mode(Mode::Settings, t0 + ms(1300));
        assert_eq!(s.show(t0 + ms(1300), true), frame("more_in", 0));
        assert_eq!(s.show(t0 + ms(1520), true), frame("more_out", 0));
    }
}
