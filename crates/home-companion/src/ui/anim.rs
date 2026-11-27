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
//! - **the microphone off**: the same with `muted_in` (the earmuffs and the tape), the `muted`
//!   loop and `muted_out`. `muted_out` is `muted_in` backwards: switched on again while the
//!   earmuffs go on, they come off from that same picture (and the other way round).
//! - **the Home Assistant page**: `ha_in` (the laptop), `ha_hold` in a loop; while a job runs,
//!   `ha_type_in`, `ha_type` in a loop, then `ha_type_out` (`ha_type_in` backwards) when it ends;
//!   leaving the page, the keys first, then `ha_out`.
//! - Every change between these (and the home) finishes the clip under way, puts away what is
//!   held and not wanted, then takes out what is wanted: never a cut.
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

    /// The frame `elapsed` after the start (not wrapped). In `f64`: an `f32` of seconds steps
    /// by 1/16 s after a few days in the same mode.
    fn at(&self, elapsed: Duration) -> u32 {
        (elapsed.as_secs_f64() * self.fps as f64) as u32
    }
}

/// The frame of a loop `elapsed` after it started, its pause on the last frame included.
/// Reduced in `f64` and integers: a mode may last for weeks (offline with Home Assistant away).
fn looped_frame(t: &Timing, elapsed: Duration) -> u32 {
    if t.frames == 0 || t.fps <= 0.0 {
        return 0;
    }
    let fps = t.fps as f64;
    let pause = t.pause.max(0.0) as f64;
    if pause == 0.0 {
        return ((elapsed.as_secs_f64() * fps) as u64 % t.frames as u64) as u32;
    }
    let length = t.frames as f64 / fps;
    let into = elapsed.as_secs_f64() % (length + pause);
    if into >= length {
        t.frames - 1
    } else {
        ((into * fps) as u32).min(t.frames - 1)
    }
}

/// How much of a loop's pause is left `elapsed` after it started (None: it is playing, or it
/// has no pause).
fn loop_rest(t: &Timing, elapsed: Duration) -> Option<Duration> {
    let pause = t.pause.max(0.0) as f64;
    if t.frames == 0 || t.fps <= 0.0 || pause == 0.0 {
        return None;
    }
    let length = t.frames as f64 / t.fps as f64;
    let into = elapsed.as_secs_f64() % (length + pause);
    (into >= length).then(|| Duration::from_secs_f64(length + pause - into))
}

/// A loop resting in its pause stops the frame timer while more than this is left: the screen's
/// clock (once a second) starts it again in time for the next round's first frame.
pub const REST_WAKE: Duration = Duration::from_millis(1250);

/// What the screen shows of the character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    Pose,
    Frame {
        clip: &'static str,
        index: u32,
    },
    /// A frame of a clip that started by fading from the picture on screen (a loop left
    /// halfway): `step` of [`FADE_FRAMES`], blended over what was shown before it.
    Fade {
        clip: &'static str,
        index: u32,
        step: u32,
    },
}

/// How many frames a clip started from the middle of a loop fades in over the loop's picture.
pub const FADE_FRAMES: u32 = 4;

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
    /// The microphone off: `muted_in` (the earmuffs), then the `muted` loop.
    Muted,
    /// The Home Assistant page: `ha_in` (the laptop), then `ha_hold` in a loop.
    Ha,
    /// The Home Assistant link pages: `link_in` (the plug), then `link_hold` in a loop.
    Link,
    /// A job went well: `satisfied_in`, then `satisfied_hold` in a loop.
    Satisfied,
    /// The Home Assistant page while a job runs: the laptop out, `ha_type_in`, then `ha_type`
    /// in a loop.
    HaBusy,
    /// One fixed frame (the demos' screenshots of an animation).
    Frame(&'static str, u32),
}

/// Things the character takes out for a page or a state: `into` once, then `hold` now and then
/// (every `pause` seconds of the hold animation; at once when it is 0: a loop), `out` when they
/// go away. `mirrored`: `out` is `into` played backwards frame by frame, so a change of mind
/// turns the clip under way back from the same picture.
///
/// `leave`: how a round of `hold` under way is left when the things must change. `linger`: once
/// `into` has started, at least one whole round of `hold` plays before `out` while the outer
/// things stay (the typing reads as typing even when the job was short).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Act {
    into: &'static str,
    hold: &'static str,
    out: &'static str,
    mirrored: bool,
    leave: Leave,
    linger: bool,
}

/// How a round of an act's `hold` under way is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leave {
    /// Played to its end (a gesture: the spanner looked at, a blink).
    Finish,
    /// At once from any frame: the loop hardly moves (every frame is as close to the next clip
    /// as the loop's own end is to its start).
    Cut,
    /// At once from any frame, the next clip fading in over [`FADE_FRAMES`] frames (a breath
    /// moves the whole figure a little; same sheet, same box).
    Fade,
}

const SETTINGS: Act = Act {
    into: "settings_in",
    hold: "settings_hold",
    out: "settings_out",
    mirrored: false,
    leave: Leave::Finish,
    linger: false,
};
const MORE: Act = Act {
    into: "more_in",
    hold: "more_hold",
    out: "more_out",
    mirrored: false,
    leave: Leave::Finish,
    linger: false,
};
const MUTED: Act = Act {
    into: "muted_in",
    hold: "muted",
    out: "muted_out",
    mirrored: true,
    leave: Leave::Cut,
    linger: false,
};
const HA: Act = Act {
    into: "ha_in",
    hold: "ha_hold",
    out: "ha_out",
    mirrored: false,
    leave: Leave::Fade,
    linger: false,
};
/// The hands on the laptop's keys: only with the laptop out ([`HA`] first).
const TYPING: Act = Act {
    into: "ha_type_in",
    hold: "ha_type",
    out: "ha_type_out",
    mirrored: true,
    leave: Leave::Fade,
    linger: true,
};

const LINK: Act = Act {
    into: "link_in",
    hold: "link_hold",
    out: "link_out",
    mirrored: false,
    leave: Leave::Fade,
    linger: false,
};
const SATISFIED: Act = Act {
    into: "satisfied_in",
    hold: "satisfied_hold",
    out: "satisfied_out",
    mirrored: true,
    leave: Leave::Cut,
    linger: false,
};

/// What the character can hold, outermost first: the modes' things.
const STACKS: [&[Act]; 7] = [
    &[SETTINGS],
    &[MORE],
    &[MUTED],
    &[HA],
    &[HA, TYPING],
    &[LINK],
    &[SATISFIED],
];

impl Mode {
    /// What the character holds in this mode, outermost first (nothing: none of these).
    fn stack(self) -> &'static [Act] {
        match self {
            Mode::Settings => STACKS[0],
            Mode::More => STACKS[1],
            Mode::Muted => STACKS[2],
            Mode::Ha => STACKS[3],
            Mode::HaBusy => STACKS[4],
            Mode::Link => STACKS[5],
            Mode::Satisfied => STACKS[6],
            _ => &[],
        }
    }

    /// A mode whose things come out, are held and go away (the Act sequence).
    fn staged(self) -> bool {
        !self.stack().is_empty()
    }
}

/// What the character holds once `clip` has played: what it took out or holds, or what is left
/// once it put something away; nothing after any other clip.
fn held_after(clip: &str) -> &'static [Act] {
    for s in STACKS {
        let top = s[s.len() - 1];
        if clip == top.into || clip == top.hold {
            return s;
        }
        if clip == top.out {
            return &s[..s.len() - 1];
        }
    }
    &[]
}

/// What comes next towards the mode's things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// This clip: the top thing put away, or the next one taken out.
    Play(&'static str),
    /// The things are as the mode wants them.
    Held,
    /// Nothing held and nothing to take out (or its clips not decoded yet).
    Free,
}

/// A clip of the act modes that may start up to this late on the end of a loop's round and
/// still go on from it (no stop on the last frame).
const LATE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tools {
    /// No clip to play yet (the sheet is being decoded): the pose.
    Wait,
    /// A clip under way: one of the mode's (in, a round of hold, out), or the one that was under
    /// way when the mode came (finished first).
    Clip { clip: &'static str, start: Instant },
    /// Still on the clip's last frame until the hold plays again; `seamless`: the hold is a loop
    /// (no pause), it goes on at `until` exactly.
    Still {
        clip: &'static str,
        until: Instant,
        seamless: bool,
    },
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

/// What was on screen when the mode changed.
#[derive(Debug, Clone, Copy)]
enum Under {
    /// A clip under way.
    Playing(&'static str, Instant),
    /// The last frame of a clip that has played.
    Ended(&'static str),
}

#[derive(Debug)]
pub struct Sequencer {
    clips: HashMap<&'static str, Timing>,
    mode: Mode,
    since: Instant,
    home: Home,
    night: Night,
    tools: Tools,
    /// A clip played once on the home (`done`, `nod`) waiting for the home, until then.
    once: Option<(&'static str, Instant)>,
    /// The clip (and its start) that fades in over the picture before it.
    fade_in: Option<(&'static str, Instant)>,
    /// The round of a lingering act's hold that must play whole (`Act::linger`).
    protect: Option<(&'static str, Instant)>,
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
            once: None,
            fade_in: None,
            protect: None,
        }
    }

    /// A house command went through: `done` (a bow) once, as [`Sequencer::once`].
    pub fn done(&mut self, now: Instant) {
        self.once("done", now);
    }

    /// `clip` once on the home: now, or at the end of the home clip under way (they all start and
    /// end on idle, nothing is cut), or when the home comes back within [`DONE_WAIT`]. Nothing
    /// without the animation, or at night.
    pub fn once(&mut self, clip: &'static str, now: Instant) {
        if !self.has(clip) {
            return;
        }
        match self.mode {
            Mode::Home => {
                let under_way = match self.home {
                    Home::Clip { clip: c, start } => {
                        self.clips.get(c).is_some_and(|t| now < start + t.length())
                    }
                    _ => false,
                };
                if under_way {
                    self.once = Some((clip, now + DONE_WAIT));
                } else {
                    self.home = Home::Clip { clip, start: now };
                    self.once = None;
                }
            }
            Mode::Night => self.once = None,
            _ => self.once = Some((clip, now + DONE_WAIT)),
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

    fn has(&self, clip: &str) -> bool {
        self.clips.contains_key(clip)
    }

    /// A new mode starts its loop from the first frame; the same mode goes on. Home to night
    /// finishes the home clip under way and falls asleep; night to home wakes up from sleep.
    /// Into and out of the modes with things (settings, More, muted, Home Assistant): the clip
    /// under way is finished (or turned back, [`Sequencer::turn`]), then what is held but not
    /// wanted is put away, then what is wanted comes out.
    pub fn set_mode(&mut self, mode: Mode, now: Instant) {
        if mode == self.mode {
            return;
        }
        let (prev, home, night, tools) = (self.mode, self.home, self.night, self.tools);
        self.mode = mode;
        self.since = now;
        self.home = Home::Still;
        let under = match (prev, home, tools) {
            (Mode::Home, Home::Clip { clip, start }, _) => Some(Under::Playing(clip, start)),
            (Mode::Home, Home::Pause { clip, .. }, _) => Some(Under::Ended(clip)),
            (p, _, Tools::Clip { clip, start }) if p.staged() => Some(Under::Playing(clip, start)),
            (p, _, Tools::Still { clip, .. }) if p.staged() => Some(Under::Ended(clip)),
            _ => None,
        };
        match (prev, mode) {
            (_, m) if m.staged() => {
                self.tools = match under {
                    Some(Under::Playing(clip, start)) => {
                        let (clip, start) = self.go_on(clip, start, now);
                        Tools::Clip { clip, start }
                    }
                    Some(Under::Ended(clip)) => self.after(clip, now),
                    None => self.start(now),
                };
            }
            // Leaving the things: the clip under way, then they are put away.
            (p, Mode::Home) if p.staged() => {
                self.once = None;
                self.home = match under {
                    Some(Under::Playing(clip, start)) => {
                        let (clip, start) = self.go_on(clip, start, now);
                        Home::Clip { clip, start }
                    }
                    Some(Under::Ended(clip)) => match self.next(held_after(clip)) {
                        Next::Play(out) => Home::Clip {
                            clip: out,
                            start: now,
                        },
                        _ => Home::Still,
                    },
                    None => Home::Still,
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
            (_, Mode::Home) if self.once.is_some_and(|(_, t)| now < t) => {
                if let Some((clip, _)) = self.once.take() {
                    self.home = Home::Clip { clip, start: now };
                }
            }
            (Mode::Night, Mode::Home) if self.has("wake_from_sleep") => {
                self.home = Home::Clip {
                    clip: "wake_from_sleep",
                    start: now,
                };
            }
            _ => {}
        }
    }

    /// What comes after `held` in this mode: the top thing put away if it is not wanted, else
    /// the next wanted thing taken out (its `into`, or its `hold` without it). A clip not decoded
    /// is skipped: its things count as put away, or as not out yet.
    fn next(&self, held: &'static [Act]) -> Next {
        let want = self.mode.stack();
        let mut held = held;
        while !want.starts_with(held) {
            let top = held[held.len() - 1];
            held = &held[..held.len() - 1];
            if self.has(top.out) {
                return Next::Play(top.out);
            }
        }
        if let Some(a) = want.get(held.len()) {
            if let Some(c) = [a.into, a.hold].into_iter().find(|c| self.has(c)) {
                return Next::Play(c);
            }
        }
        if held.is_empty() {
            Next::Free
        } else {
            Next::Held
        }
    }

    /// The mode's first thing taken out from `now`, or the pose until its clips are there.
    fn start(&self, now: Instant) -> Tools {
        match self.next(&[]) {
            Next::Play(clip) => Tools::Clip { clip, start: now },
            _ => Tools::Wait,
        }
    }

    /// `clip` is the `into` of a lingering act whose outer things stay: one whole round of its
    /// hold follows, wanted or not (`Act::linger`).
    fn lingers(&self, clip: &str) -> Option<Act> {
        let want = self.mode.stack();
        STACKS.into_iter().find_map(|s| {
            let a = s[s.len() - 1];
            (a.linger && clip == a.into && want.starts_with(&s[..s.len() - 1]) && self.has(a.hold))
                .then_some(a)
        })
    }

    /// What follows `clip`, ended at `end`, in a mode with things.
    fn after(&mut self, clip: &'static str, end: Instant) -> Tools {
        if let Some(a) = self.lingers(clip) {
            self.protect = Some((a.hold, end));
            return Tools::Clip {
                clip: a.hold,
                start: end,
            };
        }
        match self.next(held_after(clip)) {
            Next::Play(c) => Tools::Clip {
                clip: c,
                start: end,
            },
            Next::Held => {
                let hold = self.mode.stack().last().map(|a| a.hold);
                let pause = hold
                    .and_then(|h| self.clips.get(h))
                    .map_or(f32::INFINITY, |h| h.pause.max(0.0));
                Tools::Still {
                    clip,
                    until: end + Duration::from_secs_f32(pause.min(86400.0)),
                    seamless: pause == 0.0,
                }
            }
            Next::Free => Tools::Wait,
        }
    }

    /// The clip under way when the mode changed, as it goes on: turned back ([`Sequencer::turn`]),
    /// or a round of a hold left at once when its act allows it ([`Leave`]; not the protected
    /// round of a lingering act while its outer things stay), else played to its end.
    fn go_on(
        &mut self,
        clip: &'static str,
        start: Instant,
        now: Instant,
    ) -> (&'static str, Instant) {
        let turned = self.turn(clip, start, now);
        if turned != (clip, start) {
            return turned;
        }
        let held = held_after(clip);
        let Some(a) = held.last().filter(|a| a.hold == clip) else {
            return (clip, start);
        };
        let want = self.mode.stack();
        let protected =
            self.protect == Some((clip, start)) && want.starts_with(&held[..held.len() - 1]);
        if a.leave == Leave::Finish || protected {
            return (clip, start);
        }
        match self.next(held) {
            Next::Play(n) => {
                if a.leave == Leave::Fade {
                    self.fade_in = Some((n, now));
                }
                (n, now)
            }
            _ => (clip, start),
        }
    }

    /// The clip under way when the mode changed, as it goes on: the `into` of a mirrored act
    /// whose things are no longer wanted turns into its `out` at the same picture, the `out` of
    /// things wanted again into the `into`; any other clip plays to its end.
    fn turn(&self, clip: &'static str, start: Instant, now: Instant) -> (&'static str, Instant) {
        let want = self.mode.stack();
        for s in STACKS {
            let a = s[s.len() - 1];
            let back = if !a.mirrored {
                continue;
            } else if clip == a.into && !want.starts_with(s) && self.lingers(clip).is_none() {
                a.out
            } else if clip == a.out && want.starts_with(s) {
                a.into
            } else {
                continue;
            };
            let (Some(t), Some(b)) = (self.clips.get(clip), self.clips.get(back)) else {
                break;
            };
            let i = t.at(now.saturating_duration_since(start)).min(t.frames - 1);
            let j = (t.frames - 1 - i).min(b.frames - 1);
            // Halfway into frame j, so the frame shown now is j.
            let into = Duration::from_secs_f32((j as f32 + 0.5) / b.fps);
            return (back, now.checked_sub(into).unwrap_or(now));
        }
        (clip, start)
    }

    /// One of `clips` is on screen, under way or still to play (its sheet must stay).
    pub fn uses(&self, clips: &[&str]) -> bool {
        let current = match (self.mode, self.home, self.night, self.tools) {
            (m, _, _, Tools::Clip { clip, .. } | Tools::Still { clip, .. }) if m.staged() => {
                Some(clip)
            }
            (Mode::Home, Home::Clip { clip, .. } | Home::Pause { clip, .. }, _, _) => Some(clip),
            (Mode::Night, _, Night::Finish { clip, .. }, _) => Some(clip),
            _ => None,
        };
        let held = current.map_or(&[][..], held_after);
        current.is_some_and(|c| clips.contains(&c))
            || self
                .mode
                .stack()
                .iter()
                .chain(held)
                .any(|a| [a.into, a.hold, a.out].iter().any(|c| clips.contains(c)))
    }

    /// `fall_asleep` from `start`, or straight to the still sleeping pose.
    fn fall_asleep(&self, start: Instant) -> Night {
        if self.has("fall_asleep") {
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
        let show = self.picture(now, awake);
        let current = match (self.mode, self.home, self.tools) {
            (Mode::Home, Home::Clip { clip, start }, _) => Some((clip, start)),
            (m, _, Tools::Clip { clip, start }) if m.staged() => Some((clip, start)),
            _ => None,
        };
        if self.fade_in.is_some() && self.fade_in != current {
            self.fade_in = None;
        }
        match show {
            Show::Frame { clip, index } if self.fade_in.is_some() && index < FADE_FRAMES => {
                Show::Fade {
                    clip,
                    index,
                    step: index + 1,
                }
            }
            s => s,
        }
    }

    fn picture(&mut self, now: Instant, awake: bool) -> Show {
        match self.mode {
            Mode::Still => Show::Pose,
            Mode::Loading => self.looped("loading", now),
            Mode::Loop(name) => self.looped(name, now),
            Mode::Home => self.home(now, awake),
            Mode::Night => self.night(now),
            Mode::Settings
            | Mode::More
            | Mode::Muted
            | Mode::Ha
            | Mode::HaBusy
            | Mode::Link
            | Mode::Satisfied => self.tools(now, awake),
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
            Mode::Loading => self.has("loading"),
            Mode::Loop(name) => self.has(name),
            Mode::Home => {
                (awake && (self.has("idle") || self.has("wake"))) || self.home != Home::Still
            }
            Mode::Night => !matches!(self.night, Night::Still { .. }),
            Mode::Settings
            | Mode::More
            | Mode::Muted
            | Mode::Ha
            | Mode::HaBusy
            | Mode::Link
            | Mode::Satisfied => match self.tools {
                Tools::Clip { .. } => true,
                Tools::Still { seamless, .. } => seamless && awake,
                Tools::Wait => false,
            },
            Mode::Frame(..) => false,
        }
    }

    /// [`Self::moving`] at `now`: false also while a loop rests in its pause (its last frame
    /// stays, nothing to redraw) with more than [`REST_WAKE`] of it left.
    pub fn moving_now(&self, now: Instant, awake: bool) -> bool {
        let name = match self.mode {
            Mode::Loading => "loading",
            Mode::Loop(name) => name,
            _ => return self.moving(awake),
        };
        let resting = self.clips.get(name).is_some_and(|t| {
            loop_rest(t, now.saturating_duration_since(self.since)).is_some_and(|r| r > REST_WAKE)
        });
        !resting && self.moving(awake)
    }

    /// A loop; with a `pause` (Alfred's sip in the deck chair) the clip plays, then its last
    /// frame stays that many seconds before the next round.
    fn looped(&self, name: &'static str, now: Instant) -> Show {
        match self.clips.get(name) {
            Some(t) => Show::Frame {
                clip: name,
                index: looped_frame(t, now.saturating_duration_since(self.since)),
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
                    let Some(t) = self.clips.get(clip).copied() else {
                        self.night = self.fall_asleep(now);
                        continue;
                    };
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
                    self.tools = self.start(now);
                    if self.tools == Tools::Wait {
                        return Show::Pose;
                    }
                }
                Tools::Clip { clip, start } => {
                    let Some(t) = self.clips.get(clip).copied() else {
                        self.tools = Tools::Wait;
                        return Show::Pose;
                    };
                    let end = start + t.length();
                    if now < end {
                        let index = t.at(now - start).min(t.frames - 1);
                        return Show::Frame { clip, index };
                    }
                    self.tools = self.after(clip, end);
                }
                Tools::Still {
                    clip,
                    until,
                    seamless,
                } => {
                    // A round with a pause from now, not from `until` (the clock ticks once a
                    // second while still); a loop from `until` when on time. Only while the
                    // screen is awake.
                    let hold = self.mode.stack().last().map(|a| a.hold);
                    if let Some(hold) = hold.filter(|h| awake && now >= until && self.has(h)) {
                        let start = if seamless && now - until < LATE {
                            until
                        } else {
                            now
                        };
                        self.tools = Tools::Clip { clip: hold, start };
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
        let first = ["wake", "idle"].into_iter().find(|n| self.has(n));
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
                    // A clip of things finished after they were left: they go away.
                    let held = held_after(clip);
                    if !held.is_empty() {
                        self.home = match self.next(held) {
                            Next::Play(out) => Home::Clip {
                                clip: out,
                                start: end,
                            },
                            _ => Home::Still,
                        };
                        continue;
                    }
                    // A clip asked for once while this one played: now.
                    if let Some((next, _)) = self.once.filter(|(c, t)| end < *t && self.has(c)) {
                        self.once = None;
                        self.home = Home::Clip {
                            clip: next,
                            start: end,
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
                        clip: if self.has("idle") {
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

/// The animation of a downloaded pose a card of Home Assistant shows (`<name>.toml` next to its
/// picture, Settings → Poses): one at a time, under this name.
pub const CARD_POSE: &str = "card_pose";

/// The name as a `&'static str` (the sequencer's keys), if it is a known animation.
pub fn static_name(name: &str) -> Option<&'static str> {
    if name == CARD_POSE {
        return Some(CARD_POSE);
    }
    character::ANIMATIONS.into_iter().find(|n| *n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_with_a_pause_rests_on_its_last_frame() {
        let t = Timing {
            frames: 80,
            fps: 16.0,
            pause: 10.0,
        };
        let s = Duration::from_secs_f32;
        assert_eq!(looped_frame(&t, s(0.0)), 0);
        assert_eq!(looped_frame(&t, s(2.5)), 40);
        // The clip lasts 5 s, then 10 s on its last frame.
        assert_eq!(looped_frame(&t, s(5.5)), 79);
        assert_eq!(looped_frame(&t, s(14.9)), 79);
        // The next round.
        assert_eq!(looped_frame(&t, s(15.1)), 1);
        // Without a pause: a plain loop, as before.
        let plain = Timing { pause: 0.0, ..t };
        assert_eq!(looped_frame(&plain, s(5.5)), 8);
    }

    #[test]
    fn a_loop_keeps_its_frames_after_a_month() {
        // 2.5625 s into a round, 30 days on: frame 41 (an f32 of seconds gives 40, its step
        // there is 1/4 s). Both rounds (5 s + 10 s, and 5 s) divide 30 days.
        let month = Duration::from_secs(30 * 24 * 3600);
        let into = month + Duration::from_micros(2_562_500);
        let t = Timing {
            frames: 80,
            fps: 16.0,
            pause: 10.0,
        };
        assert_eq!(looped_frame(&t, into), 41);
        let plain = Timing { pause: 0.0, ..t };
        assert_eq!(looped_frame(&plain, into), 41);
        assert_eq!(t.at(Duration::from_micros(2_562_500)), 41);
    }

    #[test]
    fn a_loop_resting_in_its_pause_stops_the_frame_timer() {
        let t0 = Instant::now();
        let mut s = Sequencer::new(t0);
        s.add(
            "offline",
            Timing {
                frames: 80,
                fps: 16.0,
                pause: 10.0,
            },
        );
        s.set_mode(Mode::Loop("offline"), t0);
        let at = |secs: f64| t0 + Duration::from_secs_f64(secs);
        // The clip plays (5 s): frames to draw.
        assert!(s.moving_now(at(2.0), true));
        // Resting on its last frame: nothing to draw...
        assert!(!s.moving_now(at(5.1), true));
        assert_eq!(s.show(at(5.1), true), frame("offline", 79));
        assert!(!s.moving_now(at(13.5), true));
        // ...until the last REST_WAKE of the pause, so the clock's tick (once a second) starts
        // the timer before the next round's first frame.
        assert!(s.moving_now(at(14.0), true));
        assert!(s.moving_now(at(15.1), true));
        assert_eq!(s.show(at(15.1), true), frame("offline", 1));
        // A loop without a pause always moves.
        s.add(
            "thinking",
            Timing {
                frames: 4,
                fps: 16.0,
                pause: 0.0,
            },
        );
        s.set_mode(Mode::Loop("thinking"), t0);
        assert!(s.moving_now(at(100.0), true));
    }

    const SETTINGS_CLIPS: [&str; 3] = ["settings_in", "settings_hold", "settings_out"];

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
        assert!(s.uses(&SETTINGS_CLIPS));
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
        assert!(s.uses(&SETTINGS_CLIPS));
        assert_eq!(s.show(t0 + ms(1650), true), frame("idle", 0));
        assert!(!s.uses(&SETTINGS_CLIPS));
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
        assert!(!s.uses(&SETTINGS_CLIPS));
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
        assert!(!s.uses(&SETTINGS_CLIPS));
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
        assert!(s.uses(&SETTINGS_CLIPS));
        assert_eq!(s.show(t0 + ms(400), true), frame("more_out", 0));
        assert_eq!(s.show(t0 + ms(640), true), frame("more_out", 3));
        assert_eq!(s.show(t0 + ms(660), true), frame("settings_in", 0));
        // Back to More from the settings: the tools put away, then he turns to the tiles.
        assert_eq!(s.show(t0 + ms(950), true), frame("settings_in", 3));
        s.set_mode(Mode::More, t0 + ms(1000));
        assert!(s.uses(&SETTINGS_CLIPS));
        assert_eq!(s.show(t0 + ms(1000), true), frame("settings_out", 0));
        assert_eq!(s.show(t0 + ms(1260), true), frame("more_in", 0));
        assert!(!s.uses(&SETTINGS_CLIPS));
        // A More clip under way when Settings opens is finished, then put away.
        s.set_mode(Mode::Settings, t0 + ms(1300));
        assert_eq!(s.show(t0 + ms(1300), true), frame("more_in", 0));
        assert_eq!(s.show(t0 + ms(1520), true), frame("more_out", 0));
    }

    /// `seq` with the earmuffs: muted_in and muted_out 8 frames (0.5 s), the muted loop 4
    /// frames (0.25 s) with no pause.
    fn muted_seq(t0: Instant) -> Sequencer {
        let mut s = seq(t0);
        for (name, frames) in [("muted_in", 8), ("muted", 4), ("muted_out", 8)] {
            s.add(
                name,
                Timing {
                    frames,
                    fps: 16.0,
                    pause: 0.0,
                },
            );
        }
        s
    }

    const MUTED_CLIPS: [&str; 2] = ["muted_in", "muted_out"];

    #[test]
    fn muted_puts_the_earmuffs_on_loops_and_takes_them_off() {
        let t0 = Instant::now();
        let mut s = muted_seq(t0);
        s.set_mode(Mode::Muted, t0);
        assert!(s.uses(&MUTED_CLIPS));
        assert_eq!(s.show(t0, true), frame("muted_in", 0));
        assert_eq!(s.show(t0 + ms(440), true), frame("muted_in", 7));
        // Then the loop, round after round with no stop on the last frame.
        assert_eq!(s.show(t0 + ms(500), true), frame("muted", 0));
        assert_eq!(s.show(t0 + ms(690), true), frame("muted", 3));
        assert_eq!(s.show(t0 + ms(750), true), frame("muted", 0));
        assert!(s.moving(true));
        // The screen asleep: still on the loop's last frame, nothing moves.
        assert_eq!(s.show(t0 + ms(1000), false), frame("muted", 3));
        assert!(!s.moving(false));
        assert_eq!(s.show(t0 + ms(2000), true), frame("muted", 0));
        // The microphone on again halfway through a round: the earmuffs come off at once (the
        // loop hardly moves), then idle.
        assert_eq!(s.show(t0 + ms(2080), true), frame("muted", 1));
        s.set_mode(Mode::Home, t0 + ms(2100));
        assert!(s.uses(&MUTED_CLIPS));
        assert_eq!(s.show(t0 + ms(2100), true), frame("muted_out", 0));
        assert_eq!(s.show(t0 + ms(2550), true), frame("muted_out", 7));
        // The idle pause on its last frame (the idle pose), then idle; the sheet may go.
        assert_eq!(s.show(t0 + ms(3000), true), frame("muted_out", 7));
        assert_eq!(s.show(t0 + ms(3600), true), frame("idle", 0));
        assert!(!s.uses(&MUTED_CLIPS));
    }

    #[test]
    fn muted_turns_back_from_the_same_picture() {
        let t0 = Instant::now();
        let mut s = muted_seq(t0);
        s.set_mode(Mode::Muted, t0);
        assert_eq!(s.show(t0 + ms(190), true), frame("muted_in", 3));
        // Switched on again halfway: muted_out from the same picture (frame 7 - 3).
        s.set_mode(Mode::Home, t0 + ms(200));
        assert_eq!(s.show(t0 + ms(200), true), frame("muted_out", 4));
        assert_eq!(s.show(t0 + ms(300), true), frame("muted_out", 6));
        // Off again while they come off: on again from there.
        s.set_mode(Mode::Muted, t0 + ms(320));
        assert_eq!(s.show(t0 + ms(320), true), frame("muted_in", 1));
        assert_eq!(s.show(t0 + ms(700), true), frame("muted_in", 7));
        assert_eq!(s.show(t0 + ms(760), true), frame("muted", 0));
    }

    #[test]
    fn muted_waits_for_the_home_clip_and_for_its_sheet() {
        let t0 = Instant::now();
        let mut s = muted_seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        // Switched off during wake: wake to its end (0.5 s), then the earmuffs.
        s.set_mode(Mode::Muted, t0 + ms(100));
        assert_eq!(s.show(t0 + ms(300), true), frame("wake", 4));
        assert_eq!(s.show(t0 + ms(500), true), frame("muted_in", 0));
        // Without the earmuffs' sheet yet: the pose (the idle one, the screen's choice) until
        // it comes, then muted_in from its first frame.
        let mut s = seq(t0);
        s.set_mode(Mode::Muted, t0);
        assert_eq!(s.show(t0, true), Show::Pose);
        assert!(!s.moving(true));
        assert!(s.uses(&MUTED_CLIPS));
        s.add(
            "muted_in",
            Timing {
                frames: 8,
                fps: 16.0,
                pause: 0.0,
            },
        );
        assert_eq!(s.show(t0 + ms(800), true), frame("muted_in", 0));
    }

    #[test]
    fn leaving_settings_with_the_microphone_off_puts_the_tools_away_then_the_earmuffs_on() {
        let t0 = Instant::now();
        let mut s = muted_seq(t0);
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
        s.set_mode(Mode::Settings, t0);
        assert_eq!(s.show(t0 + ms(300), true), frame("settings_in", 3));
        s.set_mode(Mode::Muted, t0 + ms(400));
        assert_eq!(s.show(t0 + ms(400), true), frame("settings_out", 0));
        assert_eq!(s.show(t0 + ms(650), true), frame("muted_in", 0));
        // And from the muted loop to the settings: the earmuffs off at once, then the tools.
        assert_eq!(s.show(t0 + ms(1200), true), frame("muted", 0));
        s.set_mode(Mode::Settings, t0 + ms(1210));
        assert_eq!(s.show(t0 + ms(1210), true), frame("muted_out", 0));
        assert_eq!(s.show(t0 + ms(1720), true), frame("settings_in", 0));
    }

    /// `seq` with the laptop: ha_in and ha_out 8 frames (0.5 s), ha_hold 4 (0.25 s, a loop),
    /// ha_type_in and ha_type_out 4, ha_type 4 (a loop).
    fn ha_seq(t0: Instant) -> Sequencer {
        let mut s = seq(t0);
        for (name, frames) in [
            ("ha_in", 8),
            ("ha_hold", 4),
            ("ha_type_in", 4),
            ("ha_type", 4),
            ("ha_type_out", 4),
            ("ha_out", 8),
        ] {
            s.add(
                name,
                Timing {
                    frames,
                    fps: 16.0,
                    pause: 0.0,
                },
            );
        }
        s
    }

    const HA_CLIPS: [&str; 6] = [
        "ha_in",
        "ha_hold",
        "ha_type_in",
        "ha_type",
        "ha_type_out",
        "ha_out",
    ];

    fn fade(clip: &'static str, index: u32) -> Show {
        Show::Fade {
            clip,
            index,
            step: index + 1,
        }
    }

    #[test]
    fn ha_laptop_out_held_typed_on_and_put_away() {
        let t0 = Instant::now();
        let mut s = ha_seq(t0);
        s.set_mode(Mode::Ha, t0);
        assert!(s.uses(&HA_CLIPS));
        assert_eq!(s.show(t0, true), frame("ha_in", 0));
        assert_eq!(s.show(t0 + ms(500), true), frame("ha_hold", 0));
        assert_eq!(s.show(t0 + ms(800), true), frame("ha_hold", 0)); // the loop again
        // A job: the fingers go to the keys at once, fading in over the breath's picture.
        s.set_mode(Mode::HaBusy, t0 + ms(820));
        assert_eq!(s.show(t0 + ms(820), true), fade("ha_type_in", 0));
        assert_eq!(s.show(t0 + ms(890), true), fade("ha_type_in", 1));
        assert_eq!(s.show(t0 + ms(1080), true), frame("ha_type", 0));
        // A short job, over already: the first round of typing plays whole all the same.
        s.set_mode(Mode::Ha, t0 + ms(1100));
        assert_eq!(s.show(t0 + ms(1300), true), frame("ha_type", 3));
        assert_eq!(s.show(t0 + ms(1330), true), frame("ha_type_out", 0));
        assert_eq!(s.show(t0 + ms(1580), true), frame("ha_hold", 0));
        // Another job: typing at once again; over after the first round, the hands leave the
        // keys at once.
        s.set_mode(Mode::HaBusy, t0 + ms(1600));
        assert_eq!(s.show(t0 + ms(1600), true), fade("ha_type_in", 0));
        assert_eq!(s.show(t0 + ms(1860), true), frame("ha_type", 0));
        assert_eq!(s.show(t0 + ms(2150), true), frame("ha_type", 0)); // the second round
        s.set_mode(Mode::Ha, t0 + ms(2160));
        assert_eq!(s.show(t0 + ms(2160), true), fade("ha_type_out", 0));
        // Leaving the page while the hands come back: they come back, the laptop goes.
        s.set_mode(Mode::Home, t0 + ms(2300));
        assert_eq!(s.show(t0 + ms(2500), true), frame("ha_out", 1));
        assert_eq!(s.show(t0 + ms(2950), true), frame("ha_out", 7));
        assert!(s.uses(&HA_CLIPS));
        assert_eq!(s.show(t0 + ms(3910), true), frame("idle", 0));
        assert!(!s.uses(&HA_CLIPS));
    }

    #[test]
    fn ha_job_ends_while_the_fingers_go_to_the_keys() {
        let t0 = Instant::now();
        let mut s = ha_seq(t0);
        // The page opens with its check running: laptop out, then the keys.
        s.set_mode(Mode::HaBusy, t0);
        assert_eq!(s.show(t0, true), frame("ha_in", 0));
        assert_eq!(s.show(t0 + ms(500), true), frame("ha_type_in", 0));
        // The check ends halfway: the keys reached all the same, one round typed, then back.
        s.set_mode(Mode::Ha, t0 + ms(630));
        assert_eq!(s.show(t0 + ms(630), true), frame("ha_type_in", 2));
        assert_eq!(s.show(t0 + ms(760), true), frame("ha_type", 0));
        assert_eq!(s.show(t0 + ms(1010), true), frame("ha_type_out", 0));
        assert_eq!(s.show(t0 + ms(1260), true), frame("ha_hold", 0));
    }

    #[test]
    fn leaving_the_ha_page_while_typing() {
        let t0 = Instant::now();
        let mut s = ha_seq(t0);
        s.set_mode(Mode::HaBusy, t0);
        assert_eq!(s.show(t0 + ms(800), true), frame("ha_type", 0));
        // Home while typing (even the first round): the hands off the keys at once, the laptop
        // away, the home.
        s.set_mode(Mode::Home, t0 + ms(850));
        assert_eq!(s.show(t0 + ms(850), true), fade("ha_type_out", 0));
        assert_eq!(s.show(t0 + ms(1110), true), frame("ha_out", 0));
        assert_eq!(s.show(t0 + ms(1550), true), frame("ha_out", 7));
        assert_eq!(s.show(t0 + ms(2600), true), frame("idle", 0));
        // Home while the fingers go to the keys: they come back from that picture.
        let mut s = ha_seq(t0);
        s.set_mode(Mode::HaBusy, t0);
        assert_eq!(s.show(t0 + ms(560), true), frame("ha_type_in", 0));
        s.set_mode(Mode::Home, t0 + ms(560));
        assert_eq!(s.show(t0 + ms(560), true), frame("ha_type_out", 3));
        assert_eq!(s.show(t0 + ms(600), true), frame("ha_out", 0));
        // Home halfway through a breath: the laptop goes at once, fading in.
        let mut s = ha_seq(t0);
        s.set_mode(Mode::Ha, t0);
        assert_eq!(s.show(t0 + ms(600), true), frame("ha_hold", 1));
        s.set_mode(Mode::Home, t0 + ms(600));
        assert_eq!(s.show(t0 + ms(600), true), fade("ha_out", 0));
        assert_eq!(s.show(t0 + ms(800), true), fade("ha_out", 3));
        assert_eq!(s.show(t0 + ms(850), true), frame("ha_out", 4));
        // The screen asleep: still once the laptop is out (no hold round); the page left: it goes
        // away at once.
        let mut s = ha_seq(t0);
        s.set_mode(Mode::Ha, t0);
        assert_eq!(s.show(t0 + ms(800), false), frame("ha_in", 7));
        assert!(!s.moving(false));
        s.set_mode(Mode::Home, t0 + ms(900));
        assert_eq!(s.show(t0 + ms(900), false), frame("ha_out", 0));
        assert_eq!(s.show(t0 + ms(1500), false), Show::Pose);
    }

    #[test]
    fn from_ha_to_settings_the_laptop_goes_first() {
        let t0 = Instant::now();
        let mut s = ha_seq(t0);
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
        s.set_mode(Mode::HaBusy, t0);
        assert_eq!(s.show(t0 + ms(800), true), frame("ha_type", 0));
        s.set_mode(Mode::Settings, t0 + ms(800));
        assert!(s.uses(&HA_CLIPS) && s.uses(&SETTINGS_CLIPS));
        assert_eq!(s.show(t0 + ms(800), true), fade("ha_type_out", 0));
        assert_eq!(s.show(t0 + ms(1060), true), frame("ha_out", 0));
        assert_eq!(s.show(t0 + ms(1560), true), frame("settings_in", 0));
        assert!(!s.uses(&HA_CLIPS));
        // And back: the tools away, then the laptop.
        assert_eq!(s.show(t0 + ms(1900), true), frame("settings_in", 3));
        s.set_mode(Mode::Ha, t0 + ms(1950));
        assert_eq!(s.show(t0 + ms(1950), true), frame("settings_out", 0));
        assert_eq!(s.show(t0 + ms(2210), true), frame("ha_in", 0));
    }

    /// `seq` with the moments: link and satisfied (in, hold loop, out: 4 frames each), nod 8.
    fn moments_seq(t0: Instant) -> Sequencer {
        let mut s = seq(t0);
        for (name, frames) in [
            ("link_in", 4),
            ("link_hold", 4),
            ("link_out", 4),
            ("satisfied_in", 4),
            ("satisfied_hold", 4),
            ("satisfied_out", 4),
            ("nod", 8),
        ] {
            s.add(
                name,
                Timing {
                    frames,
                    fps: 16.0,
                    pause: 0.0,
                },
            );
        }
        s
    }

    #[test]
    fn link_comes_out_is_held_and_goes_without_a_cut() {
        let t0 = Instant::now();
        let mut s = moments_seq(t0);
        assert_eq!(s.show(t0, true), frame("wake", 0));
        // The link page while wake plays: wake ends, the plug comes out, held in a loop.
        s.set_mode(Mode::Link, t0 + ms(100));
        assert_eq!(s.show(t0 + ms(510), true), frame("link_in", 0));
        assert_eq!(s.show(t0 + ms(760), true), frame("link_hold", 0));
        assert_eq!(s.show(t0 + ms(1010), true), frame("link_hold", 0));
        // Linked, home: the plug goes at once, fading in over the loop's picture.
        assert_eq!(s.show(t0 + ms(1080), true), frame("link_hold", 1));
        s.set_mode(Mode::Home, t0 + ms(1080));
        assert_eq!(s.show(t0 + ms(1080), true), fade("link_out", 0));
        assert_eq!(s.show(t0 + ms(1340), true), frame("link_out", 3));
        assert_eq!(s.show(t0 + ms(2330), true), frame("idle", 0));
    }

    #[test]
    fn satisfied_comes_after_the_bow_and_turns_back() {
        let t0 = Instant::now();
        let mut s = moments_seq(t0);
        s.add(
            "done",
            Timing {
                frames: 8,
                fps: 16.0,
                pause: 0.0,
            },
        );
        // The bow, then satisfied: the bow ends first.
        s.done(t0);
        assert_eq!(s.show(t0, true), frame("done", 0));
        s.set_mode(Mode::Satisfied, t0 + ms(100));
        assert_eq!(s.show(t0 + ms(510), true), frame("satisfied_in", 0));
        assert_eq!(s.show(t0 + ms(760), true), frame("satisfied_hold", 0));
        // Over halfway through a round: out at once (the loop hardly moves).
        s.set_mode(Mode::Home, t0 + ms(830));
        assert_eq!(s.show(t0 + ms(830), true), frame("satisfied_out", 0));
        // Again while it goes: back from the same picture (out is in backwards).
        s.set_mode(Mode::Satisfied, t0 + ms(900));
        assert_eq!(s.show(t0 + ms(900), true), frame("satisfied_in", 2));
    }

    #[test]
    fn nod_plays_once_on_the_home_now_or_when_it_comes_back() {
        let t0 = Instant::now();
        let mut s = moments_seq(t0);
        s.once("nod", t0);
        assert_eq!(s.show(t0, true), frame("nod", 0));
        assert_eq!(s.show(t0 + ms(480), true), frame("nod", 7));
        // Then the home as after wake: the idle pause on its last frame, idle.
        assert_eq!(s.show(t0 + ms(1400), true), frame("nod", 7));
        assert_eq!(s.show(t0 + ms(1500), true), frame("idle", 0));
        // Asked while idle plays: at its end (nothing cut).
        assert_eq!(s.show(t0 + ms(1600), true), frame("idle", 1));
        s.once("nod", t0 + ms(1600));
        assert_eq!(s.show(t0 + ms(1700), true), frame("idle", 3));
        assert_eq!(s.show(t0 + ms(1750), true), frame("nod", 0));
        // Asked during a conversation: when the home comes back.
        s.set_mode(Mode::Loop("listening"), t0 + ms(2000));
        s.once("nod", t0 + ms(2000));
        s.set_mode(Mode::Home, t0 + ms(3000));
        assert_eq!(s.show(t0 + ms(3000), true), frame("nod", 0));
    }
}
