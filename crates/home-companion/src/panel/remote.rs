// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The remote screen: a device let into the browser panel sees the screen (JPEG frames, made
//! only while it watches: crate::ui::capture) and touches it (taps and drags that the screen
//! takes as touches). Who may, and when:
//!
//! - off unless Settings → Access from the browser → Remote screen is on, on the device itself
//!   (`[panel] screen` in settings.toml; the panel cannot switch it);
//! - with «Ask on the screen first» (`[panel] screen_confirm`, on unless switched off on the
//!   device, after a warning) a start only asks: the screen shows who wants control with Deny
//!   and Allow, and nothing is sent nor touched until Allow; Deny holds that device out as Stop
//!   does, nobody answering in [`ASK_FOR`] holds it out for [`ASK_AGAIN_AFTER`];
//! - only a device let in (its session's token, checked on every request by the protocol);
//! - one controller at a time: another device's `screen.start` is refused while the controller
//!   asked for a frame within [`IDLE`]; the same device (another tab) takes over;
//! - the session ends after [`IDLE`] without frame requests, when the device is revoked, when
//!   the switch or the panel is turned off, and at once with Stop on the screen's sign
//!   («Controlled by …»); after Stop on the screen nobody may start for [`COOLDOWN`], and the
//!   device that was stopped not at all until the switch is turned off (and on again) on the
//!   device: whoever is in front of it said no, and the browser must not just try again;
//! - touches: at most [`MAX_EVENTS`] in a request and [`EVENTS_PER_SECOND`], coordinates
//!   between 0 and 1 of the picture (anything else refuses the whole request);
//! - some pages are never sent and never touched (the camera, the photos, the memories and the
//!   conversations, Settings → Access from the browser with its one-time QR code, Home
//!   Assistant's linking QR code, AirPlay mirroring, the access dialog and every confirmation
//!   but a few named ones): the screen decides ([`super::protocol::PanelHost::remote_hidden`]).
//!
//! Pure: the time comes from the caller.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::sessions::Agent;

/// Without a frame request this long, the session ends.
pub const IDLE: Duration = Duration::from_secs(10);
/// After Stop on the screen, no session starts for this long.
pub const COOLDOWN: Duration = Duration::from_secs(60);
/// A start asked on the screen is refused when nobody answers in this long.
pub const ASK_FOR: Duration = Duration::from_secs(30);
/// After nobody answered, the same device may ask again after this long.
pub const ASK_AGAIN_AFTER: Duration = Duration::from_secs(60);
pub const MAX_EVENTS: usize = 32;
pub const EVENTS_PER_SECOND: u32 = 120;
/// `screen.home` waits this long after a real touch on the device: someone is using it.
pub const HOME_AFTER_REAL: Duration = Duration::from_secs(3);
/// At most one `screen.home` in this long.
pub const HOME_EVERY: Duration = Duration::from_secs(2);

/// Whether the remote may send the screen home now: never off `page`s that are for whoever is
/// in front of it (Settings → Access from the browser: pushed off it, a Revoke touched once
/// would not get its second touch), not while someone is touching the device, not in a row.
pub fn home_allowed(
    page: &str,
    real_touch: Option<Instant>,
    last_home: Option<Instant>,
    now: Instant,
) -> Result<(), Refusal> {
    if page == "panel" {
        return Err(Refusal::HomeHere);
    }
    if real_touch.is_some_and(|t| now.saturating_duration_since(t) < HOME_AFTER_REAL) {
        return Err(Refusal::InUse);
    }
    if last_home.is_some_and(|t| now.saturating_duration_since(t) < HOME_EVERY) {
        return Err(Refusal::SlowDown);
    }
    Ok(())
}

/// The device in control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controller {
    /// Its session's hash (crate::panel::sessions::Session::hash).
    pub hash: String,
    /// What it said it is ("Chrome", "macOS"), for the sign.
    pub agent: Agent,
    /// Its address, for the log.
    pub peer: String,
    /// Since when it is in control (while it waits: since it asked).
    pub since: Instant,
    pub seen: Instant,
    /// Allowed on the screen (or no need to ask): false while the screen asks.
    pub allowed: bool,
}

/// What a start does about asking on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// «Ask on the screen first» is off.
    No,
    /// It is on and the screen can show the question now.
    Ask,
    /// It is on and the screen cannot ask now (a conversation, a dialog, the night…).
    CannotAsk,
}

/// How a start went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// In control now.
    Live,
    /// In control now, in place of its own older session (another tab).
    Replaced,
    /// The screen is to ask (show the question now).
    Ask,
    /// The screen is asking already (the same device again).
    Asking,
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
    /// Too many touches (or homes).
    SlowDown,
    /// The screen asks whoever is in front of it: nothing yet.
    Waiting,
    /// Refused on the screen: held out until the switch goes off.
    Denied,
    /// Nobody answered on the screen: again after [`ASK_AGAIN_AFTER`].
    Unanswered,
    /// The screen cannot ask now.
    CannotAsk,
    Invalid,
    /// `screen.home` on a page that is for whoever is in front of the device.
    HomeHere,
    /// Someone touched the device a moment ago.
    InUse,
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
            Refusal::Waiting => "screen_waiting",
            Refusal::Denied => "screen_denied",
            Refusal::Unanswered => "screen_unanswered",
            Refusal::CannotAsk => "screen_cannot_ask",
            Refusal::HomeHere => "screen_home_here",
            Refusal::InUse => "screen_in_use",
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
    /// Deny on the screen's question.
    Denied,
    /// Nobody answered the screen's question in [`ASK_FOR`].
    Unanswered,
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
            End::Denied => "denied",
            End::Unanswered => "unanswered",
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
    /// Devices (session hashes) stopped or denied on the screen: refused until the switch goes
    /// off.
    shut_out: Vec<(String, End)>,
    /// The device whose question nobody answered, and when.
    unanswered: Option<(String, Instant)>,
    /// How the last session asked on the screen ended (denied, unanswered), for its device.
    last: Option<(String, End)>,
    window: Option<(Instant, u32)>,
}

impl Remote {
    /// A device asks for control.
    pub fn start(
        &mut self,
        hash: &str,
        agent: &Agent,
        peer: &str,
        enabled: bool,
        confirm: Confirm,
        now: Instant,
    ) -> Result<Start, Refusal> {
        if !enabled {
            return Err(Refusal::Off);
        }
        if let Some((_, why)) = self.shut_out.iter().find(|(h, _)| h == hash) {
            return Err(if *why == End::Denied {
                Refusal::Denied
            } else {
                Refusal::Stopped
            });
        }
        if self
            .stopped_at
            .is_some_and(|t| now.duration_since(t) < COOLDOWN)
        {
            return Err(Refusal::Stopped);
        }
        if self
            .unanswered
            .as_ref()
            .is_some_and(|(h, t)| h == hash && now.duration_since(*t) < ASK_AGAIN_AFTER)
        {
            return Err(Refusal::Unanswered);
        }
        self.expire(now);
        let replaced = match &mut self.controller {
            Some(c) if c.hash != hash => return Err(Refusal::Busy),
            Some(c) if !c.allowed => {
                c.seen = now;
                return Ok(Start::Asking);
            }
            Some(_) => true,
            None => false,
        };
        // Another tab of a device already allowed is not asked again.
        let allowed = replaced || confirm == Confirm::No;
        if !allowed && confirm == Confirm::CannotAsk {
            return Err(Refusal::CannotAsk);
        }
        self.controller = Some(Controller {
            hash: hash.to_string(),
            agent: agent.clone(),
            peer: peer.to_string(),
            since: now,
            seen: now,
            allowed,
        });
        self.window = None;
        if self.last.as_ref().is_some_and(|(h, _)| h == hash) {
            self.last = None;
        }
        Ok(if replaced {
            Start::Replaced
        } else if allowed {
            Start::Live
        } else {
            Start::Ask
        })
    }

    /// Allow on the screen's question: the session it asked for, now in control (None: nothing
    /// was asked).
    pub fn allow(&mut self, now: Instant) -> Option<Controller> {
        let c = self.controller.as_mut().filter(|c| !c.allowed)?;
        c.allowed = true;
        c.since = now;
        c.seen = now;
        Some(c.clone())
    }

    /// The screen is asking for this device now.
    pub fn asking(&self) -> Option<&Controller> {
        self.controller.as_ref().filter(|c| !c.allowed)
    }

    /// The device holds the session (in control, or waiting for the screen's answer).
    pub fn owns(&self, hash: &str) -> bool {
        self.controller.as_ref().is_some_and(|c| c.hash == hash)
    }

    /// The device asking is the one in control (its use noted).
    pub fn check(&mut self, hash: &str, now: Instant) -> Result<(), Refusal> {
        self.expire(now);
        match &mut self.controller {
            Some(c) if c.hash == hash => {
                c.seen = now;
                if c.allowed {
                    Ok(())
                } else {
                    Err(Refusal::Waiting)
                }
            }
            _ => Err(match &self.last {
                Some((h, End::Denied)) if h == hash => Refusal::Denied,
                Some((h, End::Unanswered)) if h == hash => Refusal::Unanswered,
                _ => Refusal::NotController,
            }),
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
        match why {
            End::Screen | End::Denied => {
                self.stopped_at = Some(now);
                if let Some(c) = &self.controller {
                    if !self.shut_out.iter().any(|(h, _)| *h == c.hash) {
                        self.shut_out.push((c.hash.clone(), why));
                    }
                }
            }
            End::Unanswered => {
                if let Some(c) = &self.controller {
                    self.unanswered = Some((c.hash.clone(), now));
                }
            }
            // The switch (or the panel) off: whoever was stopped may come back once it is on.
            End::Off => {
                self.shut_out.clear();
                self.unanswered = None;
                self.last = None;
            }
            _ => {}
        }
        if matches!(why, End::Denied | End::Unanswered) {
            if let Some(c) = &self.controller {
                self.last = Some((c.hash.clone(), why));
            }
        }
        self.controller.take()
    }

    /// Ends a session left without frame requests, or whose question nobody answered.
    pub fn expire(&mut self, now: Instant) -> Option<(Controller, End)> {
        let c = self.controller.as_ref()?;
        if !c.allowed && now.duration_since(c.since) >= ASK_FOR {
            return self
                .stop(End::Unanswered, now)
                .map(|c| (c, End::Unanswered));
        }
        if now.duration_since(c.seen) >= IDLE {
            return self.controller.take().map(|c| (c, End::Idle));
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
            r.start("a", &Agent::default(), "", false, Confirm::No, t),
            Err(Refusal::Off)
        );
        assert_eq!(
            r.start("a", &Agent::default(), "", true, Confirm::No, t),
            Ok(Start::Live)
        );
        assert!(r.active());
    }

    #[test]
    fn one_controller_at_a_time() {
        let mut r = Remote::default();
        let t = Instant::now();
        r.start("a", &Agent::default(), "", true, Confirm::No, t)
            .unwrap();
        assert_eq!(
            r.start("b", &Agent::default(), "", true, Confirm::No, t),
            Err(Refusal::Busy)
        );
        assert_eq!(r.check("b", t), Err(Refusal::NotController));
        // The same device from another tab takes over.
        assert_eq!(
            r.start("a", &Agent::default(), "", true, Confirm::No, t),
            Ok(Start::Replaced)
        );
        // Gone quiet: the other may.
        let later = t + IDLE;
        assert_eq!(
            r.start("b", &Agent::default(), "", true, Confirm::No, later),
            Ok(Start::Live)
        );
        assert_eq!(r.check("a", later), Err(Refusal::NotController));
    }

    #[test]
    fn frames_keep_it_alive_silence_ends_it() {
        let mut r = Remote::default();
        let t = Instant::now();
        r.start("a", &Agent::default(), "", true, Confirm::No, t)
            .unwrap();
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
        let at = |s: u64| t + Duration::from_secs(s);
        r.start("a", &Agent::default(), "", true, Confirm::No, t)
            .unwrap();
        assert!(r.stop(End::Screen, t).is_some());
        assert_eq!(
            r.start("a", &Agent::default(), "", true, Confirm::No, at(5)),
            Err(Refusal::Stopped)
        );
        assert_eq!(
            r.start("b", &Agent::default(), "", true, Confirm::No, at(5)),
            Err(Refusal::Stopped)
        );
        // After the cooldown another device may, the one stopped may not: not by trying again.
        assert!(
            r.start("b", &Agent::default(), "", true, Confirm::No, t + COOLDOWN)
                .is_ok()
        );
        r.stop(End::Browser, t + COOLDOWN);
        assert_eq!(
            r.start(
                "a",
                &Agent::default(),
                "",
                true,
                Confirm::No,
                t + COOLDOWN * 10
            ),
            Err(Refusal::Stopped)
        );
        // The switch off and on again on the device lets it back.
        r.stop(End::Off, t + COOLDOWN * 10);
        assert!(
            r.start(
                "a",
                &Agent::default(),
                "",
                true,
                Confirm::No,
                t + COOLDOWN * 10
            )
            .is_ok()
        );
        // Stop in the browser: no wait.
        r.stop(End::Browser, t + COOLDOWN * 10);
        assert!(
            r.start(
                "a",
                &Agent::default(),
                "",
                true,
                Confirm::No,
                t + COOLDOWN * 10
            )
            .is_ok()
        );
    }

    #[test]
    fn a_revoked_device_loses_control() {
        let mut r = Remote::default();
        let t = Instant::now();
        r.start("a", &Agent::default(), "", true, Confirm::No, t)
            .unwrap();
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
    fn asked_on_the_screen_nothing_before_allow() {
        let mut r = Remote::default();
        let t = Instant::now();
        let at = |s: u64| t + Duration::from_secs(s);
        let a = Agent::default();
        // The screen cannot ask now: refused, nothing kept.
        assert_eq!(
            r.start("a", &a, "", true, Confirm::CannotAsk, t),
            Err(Refusal::CannotAsk)
        );
        assert!(!r.active());
        assert_eq!(r.start("a", &a, "", true, Confirm::Ask, t), Ok(Start::Ask));
        assert!(r.active());
        assert_eq!(r.asking().map(|c| c.hash.as_str()), Some("a"));
        // Waiting: no frame, no touch; asking again does not ask twice; nobody else meanwhile.
        assert_eq!(r.check("a", at(1)), Err(Refusal::Waiting));
        assert_eq!(
            r.start("a", &a, "", true, Confirm::Ask, at(2)),
            Ok(Start::Asking)
        );
        assert_eq!(
            r.start("b", &a, "", true, Confirm::Ask, at(2)),
            Err(Refusal::Busy)
        );
        // Allow: in control from now.
        let c = r.allow(at(5)).unwrap();
        assert!(c.allowed);
        assert_eq!(c.since, at(5));
        assert!(r.check("a", at(6)).is_ok());
        assert!(r.allow(at(6)).is_none(), "nothing to allow any more");
        // Another tab of the device allowed: not asked again.
        assert_eq!(
            r.start("a", &a, "", true, Confirm::Ask, at(7)),
            Ok(Start::Replaced)
        );
        assert!(r.check("a", at(7)).is_ok());
    }

    #[test]
    fn denied_or_unanswered_on_the_screen() {
        let mut r = Remote::default();
        let t = Instant::now();
        let at = |s: u64| t + Duration::from_secs(s);
        let a = Agent::default();
        // Deny: that device is told, and held out until the switch goes off; the others for
        // the cooldown, as after Stop.
        r.start("a", &a, "", true, Confirm::Ask, t).unwrap();
        assert!(r.stop(End::Denied, at(3)).is_some());
        assert_eq!(r.check("a", at(4)), Err(Refusal::Denied));
        assert_eq!(
            r.start("a", &a, "", true, Confirm::Ask, at(3) + COOLDOWN * 5),
            Err(Refusal::Denied)
        );
        assert_eq!(
            r.start("b", &a, "", true, Confirm::Ask, at(4)),
            Err(Refusal::Stopped)
        );
        assert_eq!(
            r.start("b", &a, "", true, Confirm::Ask, at(3) + COOLDOWN),
            Ok(Start::Ask)
        );
        // Nobody answers b: it ends after ASK_FOR even while b keeps asking for frames.
        let asked = at(3) + COOLDOWN;
        for s in [8, 15] {
            assert_eq!(
                r.check("b", asked + Duration::from_secs(s)),
                Err(Refusal::Waiting)
            );
        }
        assert!(
            r.expire(asked + ASK_FOR / 2 + Duration::from_secs(5))
                .is_none()
        );
        let (c, why) = r.expire(asked + ASK_FOR).unwrap();
        assert_eq!((c.hash.as_str(), why), ("b", End::Unanswered));
        assert_eq!(r.check("b", asked + ASK_FOR), Err(Refusal::Unanswered));
        // It may ask again a minute later; meanwhile not.
        assert_eq!(
            r.start("b", &a, "", true, Confirm::Ask, asked + ASK_FOR),
            Err(Refusal::Unanswered)
        );
        assert_eq!(
            r.start(
                "b",
                &a,
                "",
                true,
                Confirm::Ask,
                asked + ASK_FOR + ASK_AGAIN_AFTER
            ),
            Ok(Start::Ask)
        );
        // The switch off and on lets the denied device ask again.
        r.stop(End::Off, asked + ASK_FOR + ASK_AGAIN_AFTER);
        assert_eq!(
            r.start(
                "a",
                &a,
                "",
                true,
                Confirm::Ask,
                asked + ASK_FOR + ASK_AGAIN_AFTER
            ),
            Ok(Start::Ask)
        );
    }

    #[test]
    fn home_not_off_the_access_page_nor_over_a_finger_nor_in_a_row() {
        let t = Instant::now();
        let s = |n: u64| Duration::from_millis(n);
        assert_eq!(home_allowed("settings", None, None, t), Ok(()));
        assert_eq!(home_allowed("panel", None, None, t), Err(Refusal::HomeHere));
        assert_eq!(
            home_allowed("camera", Some(t), None, t + s(2900)),
            Err(Refusal::InUse)
        );
        assert_eq!(home_allowed("camera", Some(t), None, t + s(3000)), Ok(()));
        assert_eq!(
            home_allowed("camera", None, Some(t), t + s(1500)),
            Err(Refusal::SlowDown)
        );
        assert_eq!(home_allowed("camera", None, Some(t), t + s(2000)), Ok(()));
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
