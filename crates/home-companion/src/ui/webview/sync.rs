// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The sign on the home button that the dashboard is catching up (Andrea, 2026-11-15).
//!
//! Shown back after a while hidden, Home Assistant's page draws for some seconds (graphs and
//! cameras fetched again, cards laid out): the picture is not the present one yet. Two signs, on
//! the home button itself so nothing else covers the dashboard:
//!
//! - **catching up**: a gold arc turns around the button from the moment the view is shown (after
//!   at least [`AFTER_HIDDEN`] hidden) until the page is quiet: no fresh frame for [`QUIET`], at
//!   least [`MIN`] on screen, at most [`MAX`]; again when Home Assistant comes back;
//! - **offline**: while the connection to Home Assistant is lost, the button stays shown with a
//!   small house, a cloud whose sync arrows turn, and the time of the last data (amber).
//!
//! Pure logic: the caller passes the clock and whether Home Assistant is reachable.

use std::time::{Duration, Instant};

/// Hidden at least this long: shown again with the sign.
pub const AFTER_HIDDEN: Duration = Duration::from_secs(30);
/// No fresh frame for this long: the page is quiet.
pub const QUIET: Duration = Duration::from_millis(1500);
/// On screen at least this long (no flash), at most this long (a page that never stops drawing).
pub const MIN: Duration = Duration::from_millis(600);
pub const MAX: Duration = Duration::from_secs(15);

/// Whether the offline sign's arrows turn, `offline_for` after Home Assistant was lost: the first
/// 10 s, then 2 s every 20 s (a redraw 10 times a second, for hours, would cost a core's share).
pub fn offline_turning(offline_for: Duration) -> bool {
    let s = offline_for.as_millis();
    s < 10_000 || s % 20_000 < 2_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    None,
    CatchingUp,
    Offline,
}

impl Sign {
    /// The name Slint's `CasaBridge.sync` takes.
    pub fn name(self) -> &'static str {
        match self {
            Sign::None => "",
            Sign::CatchingUp => "catchup",
            Sign::Offline => "offline",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SyncSign {
    /// Catching up since then (`None` = not catching up).
    since: Option<Instant>,
    /// The last fresh frame since `since`.
    last_frame: Option<Instant>,
    /// Home Assistant was reachable at the last [`SyncSign::sign`].
    was_up: Option<bool>,
}

impl SyncSign {
    /// The view is shown after `hidden_for` hidden.
    pub fn shown(&mut self, now: Instant, hidden_for: Duration) {
        if hidden_for >= AFTER_HIDDEN {
            self.start(now);
        }
    }

    /// The view is hidden: no sign until it is shown again.
    pub fn hidden(&mut self) {
        self.since = None;
        self.last_frame = None;
    }

    /// A fresh frame of the page reached the screen.
    pub fn frame(&mut self, now: Instant) {
        if self.since.is_some() {
            self.last_frame = Some(now);
        }
    }

    fn start(&mut self, now: Instant) {
        self.since = Some(now);
        self.last_frame = None;
    }

    /// The sign now, while the view is shown. `ha_up`: the connection to Home Assistant is up
    /// (or not known yet: then it counts as up). Coming back up starts a catch-up.
    pub fn sign(&mut self, now: Instant, ha_up: bool) -> Sign {
        if self.was_up == Some(false) && ha_up {
            self.start(now);
        }
        self.was_up = Some(ha_up);
        if !ha_up {
            return Sign::Offline;
        }
        let Some(since) = self.since else {
            return Sign::None;
        };
        let on = now.saturating_duration_since(since);
        let still = now.saturating_duration_since(self.last_frame.unwrap_or(since));
        if on >= MAX || (on >= MIN && still >= QUIET) {
            self.since = None;
            self.last_frame = None;
            return Sign::None;
        }
        Sign::CatchingUp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_short_hide_has_no_sign() {
        let t0 = Instant::now();
        let mut s = SyncSign::default();
        s.shown(t0, Duration::from_secs(10));
        assert_eq!(s.sign(t0 + ms(100), true), Sign::None);
    }

    #[test]
    fn catching_up_until_the_page_is_quiet() {
        let t0 = Instant::now();
        let mut s = SyncSign::default();
        s.shown(t0, Duration::from_secs(48_896));
        assert_eq!(s.sign(t0, true), Sign::CatchingUp);
        // Frames for 9 s (as on 2026-11-15), then quiet.
        for k in 1..=90 {
            s.frame(t0 + ms(k * 100));
            assert_eq!(s.sign(t0 + ms(k * 100), true), Sign::CatchingUp);
        }
        assert_eq!(s.sign(t0 + ms(9000 + 1400), true), Sign::CatchingUp);
        assert_eq!(s.sign(t0 + ms(9000 + 1500), true), Sign::None);
        // Over: a later frame does not bring it back.
        s.frame(t0 + ms(11_000));
        assert_eq!(s.sign(t0 + ms(11_000), true), Sign::None);
    }

    #[test]
    fn a_quiet_page_still_shows_it_a_moment() {
        let t0 = Instant::now();
        let mut s = SyncSign::default();
        s.shown(t0, Duration::from_secs(60));
        // No frame at all: over after QUIET (more than MIN).
        assert_eq!(s.sign(t0 + ms(599), true), Sign::CatchingUp);
        assert_eq!(s.sign(t0 + ms(1499), true), Sign::CatchingUp);
        assert_eq!(s.sign(t0 + ms(1500), true), Sign::None);
    }

    #[test]
    fn a_page_that_never_stops_drawing_ends_at_max() {
        let t0 = Instant::now();
        let mut s = SyncSign::default();
        s.shown(t0, Duration::from_secs(60));
        for k in 1..150 {
            s.frame(t0 + ms(k * 100));
        }
        assert_eq!(s.sign(t0 + ms(14_900), true), Sign::CatchingUp);
        assert_eq!(s.sign(t0 + MAX, true), Sign::None);
    }

    #[test]
    fn offline_wins_and_coming_back_catches_up() {
        let t0 = Instant::now();
        let mut s = SyncSign::default();
        s.shown(t0, Duration::from_secs(5));
        assert_eq!(s.sign(t0, true), Sign::None);
        assert_eq!(s.sign(t0 + ms(100), false), Sign::Offline);
        assert_eq!(s.sign(t0 + ms(60_000), false), Sign::Offline);
        // Back: the arc until the page is quiet.
        assert_eq!(s.sign(t0 + ms(61_000), true), Sign::CatchingUp);
        s.frame(t0 + ms(62_000));
        assert_eq!(s.sign(t0 + ms(63_400), true), Sign::CatchingUp);
        assert_eq!(s.sign(t0 + ms(63_500), true), Sign::None);
    }

    #[test]
    fn hidden_stops_the_catch_up() {
        let t0 = Instant::now();
        let mut s = SyncSign::default();
        s.shown(t0, Duration::from_secs(60));
        s.hidden();
        assert_eq!(s.sign(t0 + ms(100), true), Sign::None);
    }

    #[test]
    fn offline_arrows_turn_only_at_times() {
        assert!(offline_turning(ms(0)) && offline_turning(ms(9_999)));
        assert!(!offline_turning(ms(10_000)) && !offline_turning(ms(19_999)));
        assert!(offline_turning(ms(20_000)) && offline_turning(ms(21_999)));
        assert!(!offline_turning(ms(22_000)));
    }

    #[test]
    fn names_for_slint() {
        assert_eq!(Sign::None.name(), "");
        assert_eq!(Sign::CatchingUp.name(), "catchup");
        assert_eq!(Sign::Offline.name(), "offline");
    }
}
