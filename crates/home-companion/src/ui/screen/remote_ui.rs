// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The screen's side of the remote screen (crate::panel::remote): the switch in Settings →
//! Access from the browser, the sign «Controlled by …» with Stop, the frames (crate::ui::capture)
//! and the remote's touches, handed to the renderer's loop as touches of the screen
//! (renderer.rs takes them as it takes the panel's own, Home Assistant's view included).
//!
//! What is never sent nor touched ([`Ui::remote_hidden_now`]): the camera and the photos, the
//! pages with a code that lets in (Settings → Access from the browser and its one-time QR code,
//! Home Assistant's linking QR code), AirPlay mirroring (someone else's phone), the access
//! dialog (a remote must not let a new device in), the update's and Home Assistant unlink's
//! confirmations. The capture is told at every change ([`Ui::sync_remote`], from the renderer's
//! loop after its touches and from every page change), so a frame of such a page is never kept
//! nor sent.
//!
//! While a session lasts the screen stays awake (the sign must be seen). The remote's touches
//! carry the session's number: the renderer checks it, and whether the screen may be touched,
//! when it dispatches them (crate::ui::remote_input).

use std::sync::mpsc::Sender;

use super::{Screen, Ui, WakeSource, word};
use crate::panel::protocol;
use crate::panel::remote::{Controller, End, Touch, TouchKind};
use crate::panel::sessions::Now;
use crate::settings_model::Change;
use crate::ui::capture::Capture;
use crate::ui::touch::{TouchEvent, TouchKind as ScreenTouch};

/// What the screen keeps of the remote screen.
#[derive(Default)]
pub(in crate::ui::screen) struct RemoteUi {
    /// Settings → Access from the browser → Remote screen.
    pub on: bool,
    /// The sign's line ("" none).
    pub sign: String,
    /// The frames; None: this renderer cannot be read (the demos, the tests).
    pub capture: Option<Capture>,
    /// Where the remote's touches go: the renderer's loop, with the session's number.
    pub touches: Option<Sender<(u64, TouchEvent)>>,
    /// The session's number: a new one at every start and end (touches of another are dropped).
    pub epoch: u64,
    /// Ended while the sessions were busy answering: the next tick ends it for good.
    pub pending_end: Option<End>,
    /// The system is suspending: nothing is read.
    pub suspended: bool,
}

impl<S: Screen> Ui<S> {
    pub(super) fn wire_remote(self: &std::rc::Rc<Self>) {
        let ui = std::rc::Rc::downgrade(self);
        self.screen.on_remote_toggled(move |on| {
            if let Some(ui) = ui.upgrade() {
                crate::settings_model::commit(&*ui, Change::PanelScreen(on));
            }
        });
        let ui = std::rc::Rc::downgrade(self);
        self.screen.on_remote_stop(move || {
            if let Some(ui) = ui.upgrade() {
                ui.remote_end(End::Screen);
            }
        });
    }

    /// The renderer's capture and the way back for the remote's touches (renderer::run).
    pub fn set_remote_io(&self, capture: Capture, touches: Sender<(u64, TouchEvent)>) {
        let mut r = self.remote.borrow_mut();
        r.capture = Some(capture);
        r.touches = Some(touches);
    }

    /// The switch moved (settings_model::Change::PanelScreen).
    pub(super) fn remote_switched(&self, on: bool) {
        self.remote.borrow_mut().on = on;
        log::info!(event = "remote_screen", on = on; "remote screen {}", if on { "allowed" } else { "not allowed" });
        if !on {
            self.remote_end(End::Off);
        }
        self.refresh_remote();
    }

    /// Ends the session (if any): Stop on the sign, the switch or the panel off.
    pub fn remote_end(&self, why: End) {
        match self.panel_sessions.try_borrow_mut() {
            Ok(mut s) => protocol::remote_stop(self, &mut s, why, Now::system()),
            // Answering a request right now: the sign and the capture go at once, the session
            // at the next tick (no frame is served without the sign: remote_hidden_now).
            Err(_) => {
                self.remote.borrow_mut().pending_end = Some(why);
                self.remote_changed_now(None);
            }
        }
    }

    /// Every second: an end left pending, a session left without frame requests or whose
    /// device went; the screen kept awake while one lasts.
    pub(crate) fn tick_remote(&self) {
        let pending = self.remote.borrow_mut().pending_end.take();
        if let Ok(mut s) = self.panel_sessions.try_borrow_mut() {
            if let Some(why) = pending {
                protocol::remote_stop(self, &mut s, why, Now::system());
            }
            if s.remote.active() || !self.remote.borrow().sign.is_empty() {
                protocol::remote_sweep(self, &mut s, Now::system());
                if !s.remote.active() && !self.remote.borrow().sign.is_empty() {
                    self.remote_changed_now(None);
                }
            }
        } else if let Some(why) = pending {
            self.remote.borrow_mut().pending_end = Some(why);
        }
        if !self.remote.borrow().sign.is_empty() {
            self.wake(WakeSource::Signal);
        }
    }

    /// Tells the capture whether what is on screen now may be sent (cheap: two properties).
    pub(crate) fn sync_remote(&self) {
        let blocked = self.remote_hidden_now().is_some();
        if let Some(cap) = &self.remote.borrow().capture {
            cap.set_hidden(blocked);
        }
    }

    /// The system suspends (true) or is back (false): nothing is read meanwhile.
    pub(super) fn remote_suspended(&self, on: bool) {
        self.remote.borrow_mut().suspended = on;
        self.sync_remote();
    }

    /// The session's number and whether its touches may be dispatched now.
    pub fn remote_accepts(&self, epoch: u64) -> bool {
        let r = self.remote.borrow();
        r.epoch == epoch && !r.sign.is_empty() && r.pending_end.is_none() && {
            drop(r);
            self.remote_hidden_now().is_none()
        }
    }

    /// A session is on (its sign shown).
    pub fn remote_on_now(&self) -> bool {
        !self.remote.borrow().sign.is_empty()
    }

    /// The real finger's Down at (x, y) falls on the sign.
    pub fn remote_sign_hit(&self, x: f32, y: f32) -> bool {
        self.screen
            .remote_sign_rect()
            .is_some_and(|(sx, sy, w, h)| x >= sx && x < sx + w && y >= sy && y < sy + h)
    }

    /// The sign and the capture as the session is.
    pub(super) fn remote_changed_now(&self, c: Option<&Controller>) {
        let sign = match c {
            Some(c) => {
                let l = self.locale.borrow();
                let who = l
                    .text("panel_device_line")
                    .replace("{browser}", &c.agent.browser)
                    .replace("{os}", &c.agent.os);
                word(&l, "remote_controlled", &self.character.name).replace("{who}", &who)
            }
            None => String::new(),
        };
        let started = c.is_some() && self.remote.borrow().sign.is_empty();
        {
            let mut r = self.remote.borrow_mut();
            if started || c.is_none() {
                // Touches still queued from before belong to no session now.
                r.epoch += 1;
            }
            r.sign = sign;
            if let Some(cap) = &r.capture {
                if c.is_some() {
                    cap.restart();
                } else {
                    cap.stop();
                }
            }
        }
        if started {
            // Whoever is in the room sees the sign.
            self.wake(WakeSource::Signal);
        }
        self.refresh_remote();
        self.sync_remote();
    }

    pub(super) fn refresh_remote(&self) {
        let r = self.remote.borrow();
        self.screen.set_remote(r.on, &r.sign);
    }

    /// Why the screen must not be sent or touched now.
    pub(crate) fn remote_hidden_now(&self) -> Option<&'static str> {
        {
            let r = self.remote.borrow();
            // No sign, no frames (a session ending, or none).
            if r.sign.is_empty() || r.pending_end.is_some() {
                return Some("ended");
            }
            if r.suspended {
                return Some("suspended");
            }
        }
        let dialog = self.screen.get_dialog();
        if dialog == "panel-ask" {
            return Some("access");
        }
        // The dialogs a remote finger may see and answer, by name: any other (installing an
        // update, unlinking Home Assistant, forgetting the memories, clearing the
        // conversations, suspending, deleting a photo, revoking the devices) is for whoever is
        // in front of it, and so is a dialog added later until it is put here.
        if !remote_dialog_allowed(&dialog) {
            return Some("confirm");
        }
        // A Yes/No question of the house (the brain's, or a board tile's): never answered from
        // afar, never sent.
        if self.house.borrow().ask.is_some() || self.tile_board.borrow().ask.is_some() {
            return Some("confirm");
        }
        if self.mirror_shown() {
            return Some("mirror");
        }
        // Home Assistant's own view: logged in as whoever linked the device, with every
        // control Home Assistant has (locks and alarms too), outside the device's gates.
        if crate::ui::webview::visible() {
            return Some("ha_view");
        }
        remote_page(&self.screen.get_page())
    }

    pub(super) fn remote_frame_now(
        &self,
    ) -> Result<Option<crate::ui::capture::Frame>, &'static str> {
        let r = self.remote.borrow();
        let Some(cap) = &r.capture else {
            return Err("renderer");
        };
        cap.want();
        match cap.status() {
            crate::ui::capture::Status::Unavailable(why) => Err(why),
            _ => Ok(cap.latest()),
        }
    }

    /// A touch of the remote → a touch of the screen, in the window's logical pixels.
    pub(super) fn remote_touch_now(&self, t: Touch) {
        let r = self.remote.borrow();
        let Some(tx) = &r.touches else { return };
        let size = self
            .screen
            .window()
            .size()
            .to_logical(self.screen.window().scale_factor());
        let kind = match t.kind {
            TouchKind::Down => ScreenTouch::Down,
            TouchKind::Move => ScreenTouch::Move,
            TouchKind::Up => ScreenTouch::Up,
        };
        let _ = tx.send((
            r.epoch,
            TouchEvent {
                kind,
                x: (t.x * size.width).clamp(0.0, size.width - 1.0),
                y: (t.y * size.height).clamp(0.0, size.height - 1.0),
            },
        ));
        if let Some(cap) = &r.capture {
            cap.poke();
        }
    }

    /// The demos' sign (`--state remote-sign`) and switch (`settings-panel-remote`).
    pub(super) fn demo_remote(&self, sign: bool) {
        self.remote.borrow_mut().on = true;
        if sign {
            let c = Controller {
                hash: String::new(),
                agent: crate::panel::sessions::Agent::clean("Chrome", "macOS"),
                peer: "192.0.2.24".into(),
                since: std::time::Instant::now(),
                seen: std::time::Instant::now(),
            };
            self.remote_changed_now(Some(&c));
        } else {
            self.refresh_remote();
        }
    }
}

/// The dialogs the remote screen may show and touch (remote_hidden_now): none open, a new timer
/// or alarm, the result of an update. Everything else is hidden from it.
pub(crate) fn remote_dialog_allowed(dialog: &str) -> bool {
    matches!(dialog, "" | "new-timer" | "new-alarm" | "update-result")
}

/// Every page of the screen (companion.slint) and whether the remote screen may see and touch
/// it: None shown, Some(why) hidden. A page missing here is hidden ([`remote_page`]) and fails
/// a test, so a page added later is decided, not born visible.
const REMOTE_PAGES: &[(&str, Option<&str>)] = &[
    ("home", None),
    ("altro", None),
    // The board: its Yes/No questions are never sent nor touched (remote_hidden_now) and a
    // remote tap is one per round (renderer.rs), so a gated tile still asks the room.
    ("board", None),
    ("timers", None),
    ("reminders", None),
    ("shopping", None),
    ("notes", None),
    ("weather", None),
    ("city", None),
    ("trends", None),
    ("music", None),
    ("what", None),
    ("settings", None),
    ("language", None),
    ("screen", None),
    ("widgets", None),
    ("poses", None),
    ("usage", None),
    ("ha-lists", None),
    ("camera", Some("camera")),
    ("photo", Some("camera")),
    ("photos", Some("camera")),
    // What he remembers and what was said: read only on the device.
    ("memories", Some("private")),
    ("chat", Some("private")),
    // Settings → Access from the browser (the devices, the QR code that lets in, the remote
    // screen's own switches) and Home Assistant's linking QR code.
    ("panel", Some("code")),
    ("setup", Some("code")),
    // Home Assistant's link (Configure, Repair, Unlink) and its choices: in front of it.
    ("ha", Some("ha_setup")),
    ("ha-pick", Some("ha_setup")),
    // The first start: finding Home Assistant restarts the body with it.
    ("welcome", Some("setup")),
    ("find-ha", Some("setup")),
    // What runs on the device is chosen in front of it.
    ("update", Some("update")),
    ("mirror", Some("mirror")),
];

/// Why `page` must not be sent or touched (None: it may). A page not in [`REMOTE_PAGES`] is
/// hidden.
pub(crate) fn remote_page(page: &str) -> Option<&'static str> {
    match REMOTE_PAGES.iter().find(|(p, _)| *p == page) {
        Some((_, why)) => *why,
        None => Some("unclassified"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every page companion.slint draws, by the names it compares the page with.
    fn slint_pages() -> std::collections::BTreeSet<String> {
        let src = include_str!("../../../../spike-ui/ui/companion.slint");
        let mut pages = std::collections::BTreeSet::new();
        for pat in ["page == \"", "page = \""] {
            for (i, _) in src.match_indices(pat) {
                let rest = &src[i + pat.len()..];
                if let Some(end) = rest.find('"') {
                    pages.insert(rest[..end].to_string());
                }
            }
        }
        pages
    }

    #[test]
    fn every_page_is_classified_for_the_remote_screen() {
        let pages = slint_pages();
        assert!(pages.len() > 20, "{pages:?}");
        let missing: Vec<_> = pages
            .iter()
            .filter(|p| !REMOTE_PAGES.iter().any(|(q, _)| q == p))
            .collect();
        assert!(
            missing.is_empty(),
            "pages not classified in REMOTE_PAGES: {missing:?}"
        );
        let stale: Vec<_> = REMOTE_PAGES
            .iter()
            .filter(|(p, _)| !pages.contains(*p))
            .map(|(p, _)| p)
            .collect();
        assert!(stale.is_empty(), "no such page any more: {stale:?}");
    }

    #[test]
    fn the_pages_shown_and_hidden() {
        for p in ["home", "board", "settings", "timers"] {
            assert_eq!(remote_page(p), None, "{p}");
        }
        for (p, why) in [
            ("update", "update"),
            ("panel", "code"),
            ("ha", "ha_setup"),
            ("camera", "camera"),
            ("chat", "private"),
            ("a-page-added-later", "unclassified"),
        ] {
            assert_eq!(remote_page(p), Some(why), "{p}");
        }
    }
}
