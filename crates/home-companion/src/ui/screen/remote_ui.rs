// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The screen's side of the remote screen (crate::panel::remote): the switch in Settings →
//! Access from the browser, the sign «Controlled by …» with Stop, the frames (crate::ui::capture)
//! and the remote's touches, handed to the renderer's loop as touches of the screen
//! (renderer.rs takes them as it takes the panel's own, Home Assistant's view included).
//!
//! What is never sent nor touched ([`Ui::remote_hidden_now`]): the camera and the photos, the
//! pages with a code that lets in (Settings → Access from the browser and its one-time QR code,
//! Home Assistant's linking QR code), AirPlay mirroring (someone else's phone), and the access
//! dialog (a remote must not let a new device in).

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
    /// Where the remote's touches go: the renderer's loop.
    pub touches: Option<Sender<TouchEvent>>,
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
    pub fn set_remote_io(&self, capture: Capture, touches: Sender<TouchEvent>) {
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
    pub(super) fn remote_end(&self, why: End) {
        match self.panel_sessions.try_borrow_mut() {
            Ok(mut s) => protocol::remote_stop(self, &mut s, why, Now::system()),
            // Answering a request right now: the sign goes at once, the session at the next
            // tick (the request answers from a stopped capture meanwhile).
            Err(_) => self.remote_changed_now(None),
        }
    }

    /// Every second: a session left without frame requests, or whose device went, ends.
    pub(super) fn tick_remote(&self) {
        if let Ok(mut s) = self.panel_sessions.try_borrow_mut() {
            if s.remote.active() || !self.remote.borrow().sign.is_empty() {
                protocol::remote_sweep(self, &mut s, Now::system());
                if !s.remote.active() && !self.remote.borrow().sign.is_empty() {
                    self.remote_changed_now(None);
                }
            }
        }
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
    }

    pub(super) fn refresh_remote(&self) {
        let r = self.remote.borrow();
        self.screen.set_remote(r.on, &r.sign);
    }

    /// Why the screen must not be sent or touched now.
    pub(super) fn remote_hidden_now(&self) -> Option<&'static str> {
        if self.screen.get_dialog() == "panel-ask" {
            return Some("access");
        }
        if self.mirror_shown() {
            return Some("mirror");
        }
        match self.screen.get_page().as_str() {
            "camera" | "photo" | "photos" => Some("camera"),
            "panel" | "setup" => Some("code"),
            "mirror" => Some("mirror"),
            _ => None,
        }
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
        let _ = tx.send(TouchEvent {
            kind,
            x: (t.x * size.width).clamp(0.0, size.width - 1.0),
            y: (t.y * size.height).clamp(0.0, size.height - 1.0),
        });
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
