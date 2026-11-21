// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The remote screen: a device let into the browser panel sees the screen (JPEG frames, made
//! only while it watches: crate::ui::capture) and touches it (taps and drags that the screen
//! takes as touches). Who may, and when:
//!
//! - off unless Settings → Access from the browser → Remote screen is on, on the device itself
//!   (`[panel] screen` in settings.toml; the panel cannot switch it);
//! - only a device let in (its session's token, checked on every request by the protocol);
//! - one controller at a time: another device's `screen.start` is refused while the controller
//!   asked for a frame within [`IDLE`]; the same device (another tab) takes over;
//! - the session ends after [`IDLE`] without frame requests, when the device is revoked, when
//!   the switch or the panel is turned off, and at once with Stop on the screen's sign
//!   («Controlled by …»); after Stop on the screen nobody may start for [`COOLDOWN`];
//! - touches: at most [`MAX_EVENTS`] in a request and [`EVENTS_PER_SECOND`], coordinates
//!   between 0 and 1 of the picture (anything else refuses the whole request);
//! - some pages are never sent and never touched (the camera, the photos, Settings → Access from
//!   the browser with its one-time QR code, Home Assistant's linking QR code, AirPlay mirroring,
//!   the access dialog): the screen decides ([`super::protocol::PanelHost::remote_hidden`]).
//!
//! Pure: the time comes from the caller.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::sessions::Agent;

/// Without a frame request this long, the session ends.
pub const IDLE: Duration = Duration::from_secs(10);
/// After Stop on the screen, no session starts for this long.
pub const COOLDOWN: Duration = Duration::from_secs(60);
pub const MAX_EVENTS: usize = 32;
pub const EVENTS_PER_SECOND: u32 = 120;

/// The device in control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controller {
    /// Its session's hash (crate::panel::sessions::Session::hash).
    pub hash: String,
    /// What it said it is ("Chrome", "macOS"), for the sign.
    pub agent: Agent,
    /// Its address, for the log.
    pub peer: String,
    pub since: Instant,
    pub seen: Instant,
}

/// Why a request was refused (the key the browser gets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The switch on the device is off.
    Off,
    /// Another device is in control.
    Busy,
    /// Stopped on the screen a moment ago.
    Stopped,
    /// This device is not in control (start first, or it ended).
    NotController,
    /// Too many touches.
    SlowDown,
    Invalid,
}

impl Refusal {
    pub fn key(self) -> &'static str {
        match self {
            Refusal::Off => "screen_off",
            Refusal::Busy => "screen_busy",
            Refusal::Stopped => "screen_stopped",
            Refusal::NotController => "screen_ended",
            Refusal::SlowDown => "slow_down",
            Refusal::Invalid => "invalid",
        }
    }
}

/// Why a session ended (for the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Stop in the browser.
    Browser,
    /// Stop on the screen.
    Screen,
    /// The switch (or the whole panel) turned off.
    Off,
    /// No frame request for [`IDLE`].
    Idle,
    /// The device was revoked or logged out.
    Revoked,
    /// The same device started again from another tab.
    Replaced,
}

impl End {
    pub fn key(self) -> &'static str {
        match self {
            End::Browser => "browser",
            End::Screen => "screen",
            End::Off => "off",
            End::Idle => "idle",
            End::Revoked => "revoked",
            End::Replaced => "replaced",
        }
    }
}

/// A touch from the browser, in the picture's 0-1 coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    pub kind: TouchKind,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchKind {
    Down,
    Move,
    Up,
}

#[derive(Debug, Default)]
pub struct Remote {
    pub controller: Option<Controller>,
    stopped_at: Option<Instant>,
    window: Option<(Instant, u32)>,
}

impl Remote {
    /// A device asks for control. Ok(Some(end)): it replaced its own older session.
    pub fn start(
        &mut self,
        hash: &str,
        agent: &Agent,
        peer: &str,
        enabled: bool,
        now: Instant,
    ) -> Result<Option<End>, Refusal> {
        if !enabled {
            return Err(Refusal::Off);
        }
        if self
            .stopped_at
            .is_some_and(|t| now.duration_since(t) < COOLDOWN)
        {
            return Err(Refusal::Stopped);
        }
        self.expire(now);
        let replaced = match &self.controller {
            Some(c) if c.hash != hash => return Err(Refusal::Busy),
            Some(_) => Some(End::Replaced),
            None => None,
        };
        self.controller = Some(Controller {
            hash: hash.to_string(),
            agent: agent.clone(),
            peer: peer.to_string(),
            since: now,
            seen: now,
        });
        self.window = None;
        Ok(replaced)
    }

    /// The device asking is the one in control (its use noted).
    pub fn check(&mut self, hash: &str, now: Instant) -> Result<(), Refusal> {
        self.expire(now);
        match &mut self.controller {
            Some(c) if c.hash == hash => {
                c.seen = now;
                Ok(())
            }
            _ => Err(Refusal::NotController),
        }
    }

    /// `n` touches more now: within the limit?
    pub fn allow_events(&mut self, n: usize, now: Instant) -> bool {
        let w = match self.window {
            Some((t, c)) if now.duration_since(t) < Duration::from_secs(1) => (t, c),
            _ => (now, 0),
        };
        let total = w.1.saturating_add(n as u32);
        self.window = Some((w.0, total));
        total <= EVENTS_PER_SECOND
    }

    /// Ends the session (if any): the one that ended.
    pub fn stop(&mut self, why: End, now: Instant) -> Option<Controller> {
        if why == End::Screen {
            self.stopped_at = Some(now);
        }
        self.controller.take()
    }

    /// Ends a session left without frame requests.
    pub fn expire(&mut self, now: Instant) -> Option<Controller> {
        if self
            .controller
            .as_ref()
            .is_some_and(|c| now.duration_since(c.seen) >= IDLE)
        {
            return self.controller.take();
        }
        None
    }

    /// Ends the session if its device is not among `alive` (session hashes) any more.
    pub fn keep_only(&mut self, alive: &[String]) -> Option<Controller> {
        if self
            .controller
            .as_ref()
            .is_some_and(|c| !alive.contains(&c.hash))
        {
            return self.controller.take();
        }
        None
    }

    pub fn active(&self) -> bool {
        self.controller.is_some()
    }
}

/// `events` of a `screen.input` request: `[{k: "d"|"m"|"u", x, y}]`, x and y in 0-1. None if
/// anything is off (the whole request is refused).
pub fn parse_events(v: &Value) -> Option<Vec<Touch>> {
    let a = v.as_array()?;
    if a.is_empty() || a.len() > MAX_EVENTS {
        return None;
    }
    a.iter()
        .map(|e| {
            let kind = match e.get("k")?.as_str()? {
                "d" => TouchKind::Down,
                "m" => TouchKind::Move,
                "u" => TouchKind::Up,
                _ => return None,
            };
            let c = |k: &str| -> Option<f32> {
                let f = e.get(k)?.as_f64()?;
                (f.is_finite() && (0.0..=1.0).contains(&f)).then_some(f as f32)
            };
            Some(Touch {
                kind,
                x: c("x")?,
                y: c("y")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn off_unless_switched_on() {
        let mut r = Remote::default();
        let t = Instant::now();
        assert_eq!(
            r.start("a", &Agent::default(), "", false, t),
            Err(Refusal::Off)
        );
        assert_eq!(r.start("a", &Agent::default(), "", true, t), Ok(None));
        assert!(r.active());
    }

    #[test]
    fn one_controller_at_a_time() {
        let mut r = Remote::default();
        let t = Instant::now();
        r.start("a", &Agent::default(), "", true, t).unwrap();
        assert_eq!(
            r.start("b", &Agent::default(), "", true, t),
            Err(Refusal::Busy)
        );
        assert_eq!(r.check("b", t), Err(Refusal::NotController));
        // The same device from another tab takes over.
        assert_eq!(
            r.start("a", &Agent::default(), "", true, t),
            Ok(Some(End::Replaced))
        );
        // Gone quiet: the other may.
        let later = t + IDLE;
        assert_eq!(r.start("b", &Agent::default(), "", true, later), Ok(None));
        assert_eq!(r.check("a", later), Err(Refusal::NotController));
    }

    #[test]
    fn frames_keep_it_alive_silence_ends_it() {
        let mut r = Remote::default();
        let t = Instant::now();
        r.start("a", &Agent::default(), "", true, t).unwrap();
        let mut now = t;
        for _ in 0..5 {
            now += Duration::from_secs(5);
            assert!(r.check("a", now).is_ok());
        }
        assert!(r.expire(now + IDLE).is_some());
        assert!(!r.active());
    }

    #[test]
    fn stop_on_the_screen_holds_everyone_off_a_while() {
        let mut r = Remote::default();
        let t = Instant::now();
        r.start("a", &Agent::default(), "", true, t).unwrap();
        assert!(r.stop(End::Screen, t).is_some());
        assert_eq!(
            r.start("a", &Agent::default(), "", true, t + Duration::from_secs(5)),
            Err(Refusal::Stopped)
        );
        assert_eq!(
            r.start("b", &Agent::default(), "", true, t + Duration::from_secs(5)),
            Err(Refusal::Stopped)
        );
        assert!(
            r.start("a", &Agent::default(), "", true, t + COOLDOWN)
                .is_ok()
        );
        // Stop in the browser: no wait.
        r.stop(End::Browser, t + COOLDOWN);
        assert!(
            r.start("a", &Agent::default(), "", true, t + COOLDOWN)
                .is_ok()
        );
    }

    #[test]
    fn a_revoked_device_loses_control() {
        let mut r = Remote::default();
        let t = Instant::now();
        r.start("a", &Agent::default(), "", true, t).unwrap();
        assert!(r.keep_only(&["a".into(), "b".into()]).is_none());
        assert!(r.keep_only(&["b".into()]).is_some());
        assert!(!r.active());
    }

    #[test]
    fn touches_are_limited() {
        let mut r = Remote::default();
        let t = Instant::now();
        assert!(r.allow_events(100, t));
        assert!(r.allow_events(20, t + Duration::from_millis(500)));
        assert!(!r.allow_events(1, t + Duration::from_millis(600)));
        assert!(r.allow_events(30, t + Duration::from_millis(1100)));
    }

    #[test]
    fn events_are_checked_whole() {
        let ok =
            parse_events(&json!([{"k": "d", "x": 0.5, "y": 0.25}, {"k": "u", "x": 1, "y": 0}]))
                .unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].kind, TouchKind::Down);
        assert_eq!(ok[1].x, 1.0);
        for bad in [
            json!([]),
            json!({"k": "d"}),
            json!([{"k": "d", "x": 1.5, "y": 0}]),
            json!([{"k": "d", "x": -0.1, "y": 0}]),
            json!([{"k": "x", "x": 0, "y": 0}]),
            json!([{"k": "d", "x": "0", "y": 0}]),
            json!([{"k": "d", "x": 0}]),
            Value::Array(vec![json!({"k": "m", "x": 0, "y": 0}); MAX_EVENTS + 1]),
        ] {
            assert!(parse_events(&bad).is_none(), "{bad}");
        }
    }
}
