// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Backlight and the awake period. The home screen is dim (`idle_brightness`) and the character
//! still; a touch, a volume key, moving the device or the end of a conversation wakes it: full
//! brightness (`active_brightness`) for `awake_seconds`, extended by each new event. Every other
//! screen (listening, thinking, speaking, error, ringing, start-up) is at full brightness.
//!
//! Vol+ and Vol− together on any screen but "Casa" put the screen to sleep at once (Andrea,
//! 2026-11-04): backlight 0 and the character still, until the next wake (touch, key, motion,
//! the wake word, a conversation); home-companion keeps running and listening (not a system suspend).
//! Movement right after the pair is ignored for [`SLEEP_QUIET`] (pressing the keys shakes the
//! device). While a conversation or a ringing timer is on screen the sleep ends: they need it.
//!
//! The panel is never switched off through DRM (on the ThinkSmart View it does not come back);
//! brightness 0 is fine. The body runs as a user in `video`: packaging/99-home-companion-backlight.rules
//! lets that group write `brightness`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// After the pair puts the screen to sleep, movement does not wake it for this long.
pub const SLEEP_QUIET: Duration = Duration::from_millis(1500);

static SLEEP_REQUESTED: AtomicBool = AtomicBool::new(false);
static SCREEN_OFF: AtomicBool = AtomicBool::new(false);

/// The backlight is at 0 (the UI loop, every time it sets the level): a volume key then only
/// wakes the screen (keycombo.rs).
pub fn set_screen_off(off: bool) {
    SCREEN_OFF.store(off, Ordering::Relaxed);
}

pub fn screen_off() -> bool {
    SCREEN_OFF.load(Ordering::Relaxed)
}

/// Screen to sleep now (from any thread: Vol+ and Vol− together, keycombo.rs).
pub fn request_sleep() {
    SLEEP_REQUESTED.store(true, Ordering::Relaxed);
}

/// A sleep was requested since the last call (the UI loop, after the wake events).
pub fn take_sleep_request() -> bool {
    SLEEP_REQUESTED.swap(false, Ordering::Relaxed)
}

/// Why the screen woke up (for the debug log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeSource {
    Touch,
    /// A volume key (evdev key code).
    Key(u16),
    Motion,
    /// The end of a conversation or of an announcement.
    Conversation,
    /// SIGUSR1 (`systemctl kill -s USR1 home-companion`): a test hook.
    Signal,
    Start,
    /// Back from a system suspend (suspend.rs).
    Resume,
    /// Home Assistant's link or setup is on screen or moved on by itself (the QR code being
    /// scanned, linked, a setup step): the user is looking, the screen stays bright.
    Setup,
}

/// Until when the screen is awake.
#[derive(Debug, Clone)]
pub struct Awake {
    seconds: Duration,
    until: Option<Instant>,
    /// Put to sleep (backlight 0) at this time, until the next wake.
    asleep: Option<Instant>,
}

impl Awake {
    pub fn new(seconds: f32) -> Awake {
        Awake {
            seconds: Duration::from_secs_f32(seconds.max(0.0)),
            until: None,
            asleep: None,
        }
    }

    /// Awake for `awake_seconds` from now (never shortens a longer awake time); ends a sleep.
    pub fn wake(&mut self, now: Instant) {
        let until = now + self.seconds;
        self.until = Some(self.until.map_or(until, |u| u.max(until)));
        self.asleep = None;
    }

    /// Whether `why` wakes the screen now: not movement right after the pair put it to sleep.
    pub fn accepts(&self, why: WakeSource, now: Instant) -> bool {
        !(why == WakeSource::Motion
            && self
                .asleep
                .is_some_and(|t| now.saturating_duration_since(t) < SLEEP_QUIET))
    }

    /// To sleep now: not awake, backlight 0 until the next [`Awake::wake`].
    pub fn sleep(&mut self, now: Instant) {
        self.until = None;
        self.asleep = Some(now);
    }

    /// The sleep ends without waking (a screen that needs the backlight).
    pub fn end_sleep(&mut self) {
        self.asleep = None;
    }

    pub fn is_asleep(&self) -> bool {
        self.asleep.is_some()
    }

    pub fn is_awake(&self, now: Instant) -> bool {
        self.until.is_some_and(|u| now < u)
    }
}

/// How long the backlight takes to go from one level to another (not to or from 0: a screen
/// put to sleep goes dark at once and wakes at once).
pub const FADE: Duration = Duration::from_millis(1500);

/// A fade between two levels, even to the eye (on a log scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    pub from: u32,
    pub to: u32,
    pub start: Instant,
}

impl Fade {
    /// The level at `now` and whether the fade is over.
    pub fn at(&self, now: Instant) -> (u32, bool) {
        let t = now.saturating_duration_since(self.start).as_secs_f32() / FADE.as_secs_f32();
        if t >= 1.0 || self.from == 0 || self.to == 0 || self.from == self.to {
            return (self.to, true);
        }
        let (a, b) = ((self.from as f32).ln(), (self.to as f32).ln());
        ((a + (b - a) * t).exp().round() as u32, false)
    }
}

/// The two levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels {
    pub idle: u32,
    pub active: u32,
}

impl Levels {
    /// Each level at most `cap`'s (night mode).
    pub fn min(self, cap: Levels) -> Levels {
        Levels {
            idle: self.idle.min(cap.idle),
            active: self.active.min(cap.active),
        }
    }

    /// Full brightness when awake or when the screen is not the dim home.
    pub fn level(&self, dim_home: bool, awake: bool) -> u32 {
        if dim_home && !awake {
            self.idle
        } else {
            self.active
        }
    }
}

/// The backlight's `brightness` file, written only when the level changes. Without one (the
/// simulator, a missing device) the level is only logged.
pub struct Backlight {
    path: Option<PathBuf>,
    max: u32,
    current: Option<u32>,
}

impl Backlight {
    /// `setting`: "" = none, "auto" = the first /sys/class/backlight/*, else a directory.
    pub fn open(setting: &str) -> Backlight {
        let dir = match setting {
            "" => None,
            "auto" => first_backlight(Path::new("/sys/class/backlight")),
            d => Some(PathBuf::from(d)),
        };
        let none = Backlight {
            path: None,
            max: u32::MAX,
            current: None,
        };
        let Some(dir) = dir else {
            log::info!("backlight: none (levels are only logged)");
            return none;
        };
        let read = |name: &str| -> Option<u32> {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .and_then(|s| s.trim().parse().ok())
        };
        let path = dir.join("brightness");
        let (Some(current), Some(max)) = (read("brightness"), read("max_brightness")) else {
            log::warn!(
                "backlight {}: cannot read it (levels are only logged)",
                dir.display()
            );
            return none;
        };
        if let Err(e) = std::fs::OpenOptions::new().write(true).open(&path) {
            log::warn!(
                "backlight {}: {e} (install packaging/99-home-companion-backlight.rules; levels are only logged)",
                path.display()
            );
            return none;
        }
        log::info!("backlight {}: {current} of {max}", dir.display());
        Backlight {
            path: Some(path),
            max,
            current: Some(current),
        }
    }

    /// The level now written (None until the first [`Backlight::set`] without a device).
    pub fn current(&self) -> Option<u32> {
        self.current
    }

    pub fn set(&mut self, level: u32) {
        self.write(level, true);
    }

    /// A step of a fade: written, logged at debug level only.
    pub fn set_quiet(&mut self, level: u32) {
        self.write(level, false);
    }

    fn write(&mut self, level: u32, loud: bool) {
        let level = level.min(self.max);
        if self.current == Some(level) {
            return;
        }
        let from = self.current.map_or(-1, |c| c as i64);
        self.current = Some(level);
        match &self.path {
            Some(p) => {
                if let Err(e) = std::fs::write(p, level.to_string()) {
                    log::warn!("backlight {}: {e}", p.display());
                    return;
                }
                if loud {
                    log::info!(event = "backlight", level = level as u64, from = from; "backlight {level}");
                } else {
                    log::debug!("backlight {level}");
                }
            }
            None => {
                log::info!(event = "backlight", level = level as u64, from = from, simulated = true; "backlight {level} (no device)")
            }
        }
    }
}

fn first_backlight(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    v.sort();
    v.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awake_is_extended_by_each_event() {
        let t0 = Instant::now();
        let mut a = Awake::new(16.0);
        assert!(!a.is_awake(t0));
        a.wake(t0);
        assert!(a.is_awake(t0 + Duration::from_secs(15)));
        assert!(!a.is_awake(t0 + Duration::from_secs(16)));
        a.wake(t0 + Duration::from_secs(10));
        assert!(a.is_awake(t0 + Duration::from_secs(25)));
        assert!(!a.is_awake(t0 + Duration::from_secs(26)));
        // An earlier event does not shorten it.
        a.wake(t0);
        assert!(a.is_awake(t0 + Duration::from_secs(25)));
        // awake_seconds = 0: never awake.
        let mut z = Awake::new(0.0);
        z.wake(t0);
        assert!(!z.is_awake(t0));
    }

    #[test]
    fn sleep_until_the_next_wake() {
        let t0 = Instant::now();
        let s = |ms: u64| t0 + Duration::from_millis(ms);
        let mut a = Awake::new(16.0);
        a.wake(t0);
        a.sleep(s(100));
        assert!(a.is_asleep());
        assert!(!a.is_awake(s(200)));
        // Movement from pressing the keys does not wake it; a touch or a key does.
        assert!(!a.accepts(WakeSource::Motion, s(1000)));
        assert!(a.accepts(WakeSource::Touch, s(1000)));
        assert!(a.accepts(WakeSource::Key(115), s(1000)));
        assert!(a.accepts(WakeSource::Motion, s(1600)));
        a.wake(s(2000));
        assert!(!a.is_asleep());
        assert!(a.is_awake(s(2000)));
        assert!(a.accepts(WakeSource::Motion, s(2001)));
        a.sleep(s(3000));
        a.end_sleep();
        assert!(!a.is_asleep());
        assert!(!a.is_awake(s(3000)));
    }

    #[test]
    fn fades_on_a_log_scale_but_not_to_dark() {
        let t0 = Instant::now();
        let f = Fade {
            from: 400,
            to: 3600,
            start: t0,
        };
        assert_eq!(f.at(t0), (400, false));
        assert_eq!(f.at(t0 + FADE / 2), (1200, false));
        assert_eq!(f.at(t0 + FADE), (3600, true));
        let off = Fade {
            from: 600,
            to: 0,
            start: t0,
        };
        assert_eq!(off.at(t0), (0, true));
    }

    #[test]
    fn levels() {
        let l = Levels {
            idle: 600,
            active: 4095,
        };
        assert_eq!(l.level(true, false), 600);
        assert_eq!(l.level(true, true), 4095);
        assert_eq!(l.level(false, false), 4095);
    }

    #[test]
    fn writes_only_changes_and_clamps() {
        let d =
            std::env::temp_dir().join(format!("home-companion-backlight-{}", std::process::id()));
        let dev = d.join("panel");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("brightness"), "600\n").unwrap();
        std::fs::write(dev.join("max_brightness"), "4095\n").unwrap();
        let mut b = Backlight::open(dev.to_str().unwrap());
        assert_eq!(b.current(), Some(600));
        b.set(9999);
        assert_eq!(
            std::fs::read_to_string(dev.join("brightness")).unwrap(),
            "4095"
        );
        std::fs::write(dev.join("brightness"), "x").unwrap();
        b.set(4095); // unchanged: not written
        assert_eq!(
            std::fs::read_to_string(dev.join("brightness")).unwrap(),
            "x"
        );
        assert_eq!(first_backlight(&d), Some(dev.clone()));
        let mut none = Backlight::open("");
        none.set(600);
        assert_eq!(none.current(), Some(600));
        std::fs::remove_dir_all(d).unwrap();
    }
}
