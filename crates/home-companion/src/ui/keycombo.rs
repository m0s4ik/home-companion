// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Vol+ and Vol− pressed together (Andrea, 2026-11-04) depend on the screen ([`pair_action`]):
//! in "Casa" they go home, the way out that works even when the web view or its helper hangs,
//! since it never goes through them; anywhere else they put the screen to sleep at once
//! (backlight.rs; not a system suspend, home-companion keeps listening). A press of one key is held back until it is clear it is not
//! half of the pair, so the pair never changes the volume (no step, no beep, no bar).
//!
//! The rule: the pair is both keys down at the same time, the second pressed within `window`
//! of the first. A single key counts as a volume press when it is released (before the window
//! ends, without the other key) or when the window ends with it still down; from then on, while
//! it stays down, it repeats in software (the ThinkSmart View's kernel does not repeat these
//! keys; the kernel's own repeats, value 2, are ignored where it does): the first repeat
//! `repeat_delay` after the press (and at least `repeat_rate` after the decided press), then
//! one every `repeat_rate`. Releases of a pair's keys are swallowed until both are up.
//!
//! How the key reader threads (sensors.rs, one per input device) use it, through the one
//! tracker shared by both keys:
//!
//! ```text
//! on key event (code, value):              value 1 = press, 0 = release, 2 = auto-repeat
//!     for out in feed_shared(up, value) { act(out) }
//! poll timeout min(wait_shared(), 50 ms) while a key is pending or repeating, then:
//!     for out in poll_shared() { act(out) }
//! act(Output::Volume(key)) → a volume step for that key (bar, beep, save)
//! act(Output::Repeat(key)) → a repeated volume step
//! Output::Home             → handled inside (pair_action): webview::request_home() or
//!                            backlight::request_sleep(); the pair's presses do not wake the screen
//! ```
//!
//! Volume steps come only from these outputs, so the pair never changes the volume.
//!
//! A press that only wakes something is not a volume step either (Andrea, 2026-11-04): with the
//! screen off (backlight 0: the pair's screen sleep, or a system suspend) a press wakes the
//! screen and nothing else, and for [`RESUME_QUIET`] after a resume from suspend every press,
//! and any key already down at resume, is swallowed until it is released (the kernel delivers
//! the press that woke the device as an ordinary key event right after the resume). The dim
//! idle home is visible, so there a press is a volume step as before.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a press waits for the other key before it counts as a volume press.
pub const WINDOW: Duration = Duration::from_millis(300);
/// First repeat of a held volume key (from the press), then one step every [`REPEAT_RATE`].
pub const REPEAT_DELAY: Duration = Duration::from_millis(400);
pub const REPEAT_RATE: Duration = Duration::from_millis(150);
/// After a resume from suspend, presses only wake for this long.
pub const RESUME_QUIET: Duration = Duration::from_secs(1);

/// One tracker for the whole process (Vol+ and Vol− may come from different input devices,
/// each read by its own thread).
static SHARED: Mutex<Option<KeyCombo>> = Mutex::new(None);

fn with_shared<T>(f: impl FnOnce(&mut KeyCombo) -> T) -> T {
    let mut g = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(KeyCombo::default))
}

/// What the pair does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairAction {
    /// Leave "Casa" for the home screen.
    Home,
    /// The screen to sleep (backlight 0) until the next wake.
    Sleep,
}

/// The pair goes home from "Casa" and puts the screen to sleep anywhere else.
pub fn pair_action(casa_visible: bool) -> PairAction {
    if casa_visible {
        PairAction::Home
    } else {
        PairAction::Sleep
    }
}

/// Feeds a volume key event to the shared tracker and acts on the pair ([`pair_action`]).
/// Returns all outputs: the volume handling acts on single presses and repeats; an
/// [`Output::Home`] among them means this event completed the pair (it must not wake the
/// screen).
pub fn feed_shared(up: bool, value: i32) -> Vec<Output> {
    let key = if up { Key::Up } else { Key::Down };
    let screen_off = super::backlight::screen_off();
    let out = with_shared(|c| c.key_screen(key, value, Instant::now(), screen_off));
    if out.contains(&Output::Home) {
        match pair_action(super::webview::visible()) {
            PairAction::Home => {
                log::info!("Vol+ and Vol- together: home");
                super::webview::request_home();
            }
            PairAction::Sleep => {
                log::info!("Vol+ and Vol- together: screen to sleep");
                super::backlight::request_sleep();
            }
        }
    }
    out
}

/// Decides pending presses whose window has ended and gives the repeats that are due (call it
/// every ≤ 50 ms while [`wait_shared`] is Some).
pub fn poll_shared() -> Vec<Output> {
    with_shared(|c| c.poll(Instant::now()))
}

/// Back from a system suspend: presses only wake for [`RESUME_QUIET`] (keys already down too).
pub fn resumed_shared() {
    with_shared(|c| c.wake_only_for(Instant::now(), RESUME_QUIET));
}

/// How long until the shared tracker needs [`poll_shared`] (None = no key down).
pub fn wait_shared() -> Option<Duration> {
    let now = Instant::now();
    with_shared(|c| c.deadline().map(|t| t.saturating_duration_since(now)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
}

impl Key {
    fn other(self) -> Key {
        match self {
            Key::Up => Key::Down,
            Key::Down => Key::Up,
        }
    }
    fn index(self) -> usize {
        match self {
            Key::Up => 0,
            Key::Down => 1,
        }
    }
}

/// What the volume handling does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// One volume step for this key (the press, decided).
    Volume(Key),
    /// A repeated step of a key held down after it was decided as a single press.
    Repeat(Key),
    /// Both keys together: go home; nothing changes the volume.
    Home,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyState {
    Up,
    /// Down, undecided since this instant.
    Pending(Instant),
    /// Down, decided as a single press; the next repeat is due then.
    Single(Instant),
    /// Down, part of the pair (swallowed).
    Pair,
    /// Down, a press that only woke the screen or the device (swallowed until released).
    WakeOnly,
}

#[derive(Debug, Clone)]
pub struct KeyCombo {
    window: Duration,
    repeat_delay: Duration,
    repeat_rate: Duration,
    keys: [KeyState; 2],
    /// Until then a press only wakes ([`KeyCombo::wake_only_for`]).
    wake_only_until: Option<Instant>,
}

impl Default for KeyCombo {
    fn default() -> Self {
        KeyCombo::new(WINDOW, REPEAT_DELAY, REPEAT_RATE)
    }
}

impl KeyCombo {
    pub fn new(window: Duration, repeat_delay: Duration, repeat_rate: Duration) -> KeyCombo {
        KeyCombo {
            window,
            repeat_delay,
            repeat_rate: repeat_rate.max(Duration::from_millis(10)),
            keys: [KeyState::Up; 2],
            wake_only_until: None,
        }
    }

    /// From `now` for `quiet`, presses only wake; keys down now are swallowed until released.
    pub fn wake_only_for(&mut self, now: Instant, quiet: Duration) {
        self.wake_only_until = Some(now + quiet);
        for k in &mut self.keys {
            if *k != KeyState::Up {
                *k = KeyState::WakeOnly;
            }
        }
    }

    /// The time by which [`KeyCombo::poll`] must be called (a key is pending or repeating), if
    /// any.
    pub fn deadline(&self) -> Option<Instant> {
        self.keys
            .iter()
            .filter_map(|k| match k {
                KeyState::Pending(t) => Some(*t + self.window),
                KeyState::Single(t) => Some(*t),
                _ => None,
            })
            .min()
    }

    /// A key event: `value` as in evdev (1 press, 0 release, 2 auto-repeat).
    pub fn key(&mut self, key: Key, value: i32, now: Instant) -> Vec<Output> {
        self.key_screen(key, value, now, false)
    }

    /// [`KeyCombo::key`] knowing whether the screen is off: then a press only wakes it.
    pub fn key_screen(
        &mut self,
        key: Key,
        value: i32,
        now: Instant,
        screen_off: bool,
    ) -> Vec<Output> {
        let mut out = self.poll(now);
        let (me, other) = (key.index(), key.other().index());
        let quiet = self.wake_only_until.is_some_and(|t| now < t);
        match value {
            1 if screen_off || quiet => self.keys[me] = KeyState::WakeOnly,
            1 => match self.keys[other] {
                KeyState::Pending(t) if now.duration_since(t) <= self.window => {
                    self.keys = [KeyState::Pair; 2];
                    out.push(Output::Home);
                }
                KeyState::Pair => self.keys[me] = KeyState::Pair,
                _ => self.keys[me] = KeyState::Pending(now),
            },
            0 => {
                if let KeyState::Pending(_) = self.keys[me] {
                    out.push(Output::Volume(key));
                }
                self.keys[me] = KeyState::Up;
            }
            // The kernel's own repeat: ours runs instead (one mechanism).
            _ => {}
        }
        out
    }

    /// Decides the keys whose window has ended; gives the repeats that are due (at most one
    /// per key and call: a late caller does not get a burst).
    pub fn poll(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        for (i, key) in [Key::Up, Key::Down].into_iter().enumerate() {
            match self.keys[i] {
                KeyState::Pending(t) if now.duration_since(t) > self.window => {
                    let next = (t + self.repeat_delay).max(now + self.repeat_rate);
                    self.keys[i] = KeyState::Single(next);
                    out.push(Output::Volume(key));
                }
                KeyState::Single(next) if now >= next => {
                    let mut n = next + self.repeat_rate;
                    if n <= now {
                        n = now + self.repeat_rate;
                    }
                    self.keys[i] = KeyState::Single(n);
                    out.push(Output::Repeat(key));
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    /// Volume steps (+1 / -1 each, repeats included) in a list of outputs.
    fn steps(out: &[Output]) -> i32 {
        out.iter()
            .map(|o| match o {
                Output::Volume(Key::Up) | Output::Repeat(Key::Up) => 1,
                Output::Volume(Key::Down) | Output::Repeat(Key::Down) => -1,
                Output::Home => 0,
            })
            .sum()
    }

    /// Feeds a timeline of key events, polling every 20 ms in between (as the reader threads
    /// do); returns all outputs.
    fn run(c: &mut KeyCombo, t0: Instant, events: &[(u64, Key, i32)], until: u64) -> Vec<Output> {
        let mut out = Vec::new();
        let mut ev = events.iter().peekable();
        let mut t = 0;
        while t <= until {
            while let Some((at, key, value)) = ev.peek().copied().copied() {
                if at > t {
                    break;
                }
                out.extend(c.key(key, value, ms(t0, at)));
                ev.next();
            }
            out.extend(c.poll(ms(t0, t)));
            t += 20;
        }
        out
    }

    #[test]
    fn pair_within_100_ms_goes_home_without_a_volume_change() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        let out = run(
            &mut c,
            t0,
            &[
                (0, Key::Up, 1),
                (100, Key::Down, 1),
                (1500, Key::Down, 0),
                (1520, Key::Up, 0),
            ],
            2000,
        );
        assert_eq!(out, vec![Output::Home]);
        assert_eq!(c.deadline(), None);
    }

    #[test]
    fn presses_500_ms_apart_are_two_volume_steps() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        let out = run(
            &mut c,
            t0,
            &[
                (0, Key::Up, 1),
                (80, Key::Up, 0),
                (500, Key::Down, 1),
                (590, Key::Down, 0),
            ],
            1000,
        );
        assert_eq!(
            out,
            vec![Output::Volume(Key::Up), Output::Volume(Key::Down)]
        );
        assert_eq!(steps(&out), 0);
    }

    #[test]
    fn a_held_key_steps_then_repeats() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        assert!(c.key(Key::Down, 1, t0).is_empty());
        assert_eq!(c.deadline(), Some(ms(t0, 300)));
        assert!(c.poll(ms(t0, 200)).is_empty());
        assert_eq!(c.poll(ms(t0, 301)), vec![Output::Volume(Key::Down)]);
        // First repeat 400 ms after the press, but not sooner than 150 ms after the step.
        assert_eq!(c.deadline(), Some(ms(t0, 451)));
        assert!(c.poll(ms(t0, 450)).is_empty());
        assert_eq!(c.poll(ms(t0, 451)), vec![Output::Repeat(Key::Down)]);
        assert!(c.poll(ms(t0, 600)).is_empty());
        assert_eq!(c.poll(ms(t0, 601)), vec![Output::Repeat(Key::Down)]);
        // Late by a second: one step, not a burst.
        assert_eq!(c.poll(ms(t0, 1700)), vec![Output::Repeat(Key::Down)]);
        assert!(c.poll(ms(t0, 1750)).is_empty());
        // The kernel's own repeat is ignored.
        assert!(c.key(Key::Down, 2, ms(t0, 1760)).is_empty());
        assert!(c.key(Key::Down, 0, ms(t0, 1800)).is_empty());
        assert_eq!(c.deadline(), None);
        assert!(c.poll(ms(t0, 5000)).is_empty());
    }

    #[test]
    fn holding_for_a_second_repeats_in_the_timeline() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        let out = run(&mut c, t0, &[(0, Key::Up, 1), (1000, Key::Up, 0)], 1500);
        // The step at ~300 ms, repeats at ~460, ~610, ~760, ~910.
        assert_eq!(out[0], Output::Volume(Key::Up));
        assert!(out[1..].iter().all(|o| *o == Output::Repeat(Key::Up)));
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn the_other_key_late_is_its_own_press() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        c.key(Key::Down, 1, t0);
        assert_eq!(c.poll(ms(t0, 301)), vec![Output::Volume(Key::Down)]);
        assert!(c.key(Key::Up, 1, ms(t0, 420)).is_empty());
        assert_eq!(
            c.key(Key::Up, 0, ms(t0, 430)),
            vec![Output::Volume(Key::Up)]
        );
        assert!(c.key(Key::Down, 0, ms(t0, 440)).is_empty());
    }

    #[test]
    fn second_key_after_the_window_is_not_a_pair() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        c.key(Key::Up, 1, t0);
        // No poll in between: the late press still decides the first key first.
        assert_eq!(
            c.key(Key::Down, 1, ms(t0, 400)),
            vec![Output::Volume(Key::Up)]
        );
    }

    #[test]
    fn released_before_the_other_is_two_presses() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        c.key(Key::Up, 1, t0);
        assert_eq!(c.key(Key::Up, 0, ms(t0, 50)), vec![Output::Volume(Key::Up)]);
        assert!(c.key(Key::Down, 1, ms(t0, 100)).is_empty());
        assert_eq!(
            c.key(Key::Down, 0, ms(t0, 150)),
            vec![Output::Volume(Key::Down)]
        );
    }

    #[test]
    fn a_press_with_the_screen_off_only_wakes() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        assert!(c.key_screen(Key::Up, 1, t0, true).is_empty());
        assert_eq!(c.deadline(), None);
        assert!(c.poll(ms(t0, 2000)).is_empty());
        assert!(c.key_screen(Key::Up, 0, ms(t0, 2100), false).is_empty());
        // The next press, screen on: a step.
        c.key(Key::Up, 1, ms(t0, 3000));
        assert_eq!(
            c.key(Key::Up, 0, ms(t0, 3050)),
            vec![Output::Volume(Key::Up)]
        );
    }

    #[test]
    fn after_a_resume_presses_only_wake_for_a_second() {
        let t0 = Instant::now();
        let mut c = KeyCombo::default();
        // The press that woke the device is still down at resume.
        c.key(Key::Down, 1, t0);
        c.wake_only_for(ms(t0, 100), RESUME_QUIET);
        let out = run(&mut c, t0, &[(900, Key::Down, 0)], 1000);
        assert!(out.is_empty());
        // Delivered after the resume, within the quiet second: swallowed, held or not.
        let out = run(&mut c, t0, &[(1050, Key::Up, 1), (1800, Key::Up, 0)], 2000);
        assert!(out.is_empty());
        // After it: a step again.
        assert!(c.key(Key::Up, 1, ms(t0, 2500)).is_empty());
        assert_eq!(
            c.key(Key::Up, 0, ms(t0, 2550)),
            vec![Output::Volume(Key::Up)]
        );
    }

    #[test]
    fn pair_goes_home_in_casa_and_sleeps_elsewhere() {
        assert_eq!(pair_action(true), PairAction::Home);
        assert_eq!(pair_action(false), PairAction::Sleep);
    }
}
