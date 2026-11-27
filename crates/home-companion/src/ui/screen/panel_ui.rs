// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The screen's side of the browser panel: the requests of `panel.sock` answered on this thread
//! ([`crate::panel::protocol`]), the access dialog, Settings → Access from the browser (the
//! switch, the address and its QR code with a one-time ticket, the devices with Revoke) and the
//! home's card after too many unanswered requests.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Screen, Ui, WakeSource, local_hm, qr_image, short_date, word};
use crate::core::{Phase, UiEvent};
use crate::ha::hub::HubCmd;
use crate::panel::PanelCfg;
use crate::panel::protocol::{self, PanelHost};
use crate::panel::sessions::{self, Request, Sessions};
use crate::panel::socket::{self, PanelSocket};
use crate::settings_model::Change;
use crate::ui::lists::{self, ShopList};

/// The access dialog's buttons answer only after it has been on screen this long.
pub(super) const ASK_READY_AFTER: Duration = Duration::from_millis(700);
/// A Revoke touched once waits this long for its second touch.
const ARMED_FOR: Duration = Duration::from_secs(3);

/// The address and the ticket of the demos (`--state settings-panel`).
const DEMO_ADDRESS: &str = "192.0.2.5";
const DEMO_TICKET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// What the screen keeps about the panel.
#[derive(Default)]
pub(super) struct PanelUi {
    socket: Option<PanelSocket>,
    pub(super) cfg: PanelCfg,
    /// Settings → Access from the browser is on.
    pub(super) on: bool,
    /// The request the dialog shows.
    asking: Option<String>,
    /// The ticket the QR code on screen holds.
    pub(super) ticket_shown: Option<String>,
    /// The device whose Revoke was touched once, and when.
    armed: Option<(String, Instant)>,
    /// The home's card is up (requests stopped for a while).
    pub(super) blocked: bool,
    /// The devices changed while the sessions were busy: draw the page again.
    changed: bool,
    /// The device's address on the home network, while the page shows it.
    address: Option<String>,
    /// Sample devices for the demos.
    demo: Vec<(String, String, String)>,
    /// The screen was put to sleep with Vol+ and Vol− together: nothing is asked until it wakes.
    pub(super) keys_asleep: bool,
    /// Makes the dialog's buttons answer after [`ASK_READY_AFTER`].
    ask_timer: slint::Timer,
    /// The panel's words per language, loaded once.
    words: std::collections::HashMap<String, Rc<locale::Locale>>,
}

impl PanelUi {
    /// The panel's state at start: on as the settings say, its socket listening (`cfg` None:
    /// no socket, as in the tests and the demos).
    pub(super) fn new(cfg: Option<PanelCfg>, on: bool) -> PanelUi {
        let socket = cfg.as_ref().and_then(|c| {
            socket::start(std::path::Path::new(&c.socket))
                .map_err(|e| log::warn!(event = "panel"; "browser panel socket {}: {e}", c.socket))
                .ok()
        });
        PanelUi {
            socket,
            cfg: cfg.unwrap_or_default(),
            on,
            ..Default::default()
        }
    }
}

pub(super) fn load_sessions(dir: Option<&std::path::Path>) -> RefCell<Sessions> {
    RefCell::new(dir.map(Sessions::load).unwrap_or_default())
}

fn now() -> sessions::Now {
    sessions::Now::system()
}

impl<S: Screen> Ui<S> {
    pub(super) fn wire_panel(self: &Rc<Self>) {
        let ui = Rc::downgrade(self);
        self.screen.on_panel_toggled(move |on| {
            if let Some(ui) = ui.upgrade() {
                crate::settings_model::commit(&*ui, Change::Panel(on));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_panel_answered(move |yes| {
            if let Some(ui) = ui.upgrade() {
                ui.panel_answered(yes);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_panel_revoke(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.panel_revoke(&id);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_panel_revoke_all(move || {
            if let Some(ui) = ui.upgrade() {
                if let Ok(mut s) = ui.panel_sessions.try_borrow_mut() {
                    s.revoke_all();
                }
                ui.refresh_panel();
            }
        });
    }

    /// Switched on or off on the screen (settings_model::Change::Panel).
    pub(super) fn panel_switched(&self, on: bool) {
        self.panel.borrow_mut().on = on;
        log::info!(event = "panel", on = on; "access from the browser {}", if on { "on" } else { "off" });
        if !on {
            self.remote_end(crate::panel::remote::End::Off);
            self.close_panel_ask();
            if let Ok(mut s) = self.panel_sessions.try_borrow_mut() {
                s.drop_ticket();
                // Its browser learns it was refused; nobody is counted for it.
                s.cancel_waiting(now());
            }
        }
        self.refresh_panel();
    }

    /// The requests the panel's process made since the last poll, answered.
    pub(super) fn poll_panel(&self) {
        let calls: Vec<socket::Call> = match &self.panel.borrow().socket {
            Some(s) => s.calls.try_iter().collect(),
            None => Vec::new(),
        };
        for c in calls {
            let answer = self.panel_call(&c.request);
            let _ = c.reply.send(answer);
        }
        self.refresh_panel_if_changed();
    }

    /// The page drawn again only when the devices really changed.
    fn refresh_panel_if_changed(&self) {
        let listed = self
            .panel_sessions
            .try_borrow_mut()
            .is_ok_and(|mut s| std::mem::take(&mut s.changed));
        let asked = std::mem::take(&mut self.panel.borrow_mut().changed);
        if listed || asked {
            self.refresh_panel();
        }
    }

    /// One request of the panel → its answer (the tests call this directly).
    pub(super) fn panel_call(&self, request: &Value) -> Value {
        match self.panel_sessions.try_borrow_mut() {
            Ok(mut s) => {
                protocol::handle(self, &mut s, request, now(), &mut sessions::system_random)
            }
            Err(_) => serde_json::json!({"ok": false, "error": "busy"}),
        }
    }

    /// Every second: requests that waited too long, the home's card, the QR code's ticket, a
    /// Revoke that was not confirmed.
    pub(super) fn tick_panel(&self) {
        let t = now();
        let (waiting, blocked, ticket) = {
            let Ok(mut s) = self.panel_sessions.try_borrow_mut() else {
                return;
            };
            s.expire(t);
            let shown = self.panel.borrow().on && self.screen.get_page() == "panel";
            if shown {
                // Someone is looking at the page: requests may come again.
                s.unblock();
            } else {
                s.drop_ticket();
            }
            (
                s.waiting().map(|r| r.id.clone()),
                s.blocked(t),
                shown.then(|| s.ticket(t, &mut sessions::system_random).map(String::from)),
            )
        };
        let asking = self.panel.borrow().asking.clone();
        if asking.is_some() && asking != waiting {
            log::info!(event = "panel"; "browser access: the request was not answered in time");
            self.close_panel_ask();
        }
        if blocked != self.panel.borrow().blocked {
            self.panel.borrow_mut().blocked = blocked;
            self.refresh_home();
        }
        let disarm = self
            .panel
            .borrow()
            .armed
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= ARMED_FOR);
        let new_ticket = ticket.is_some_and(|t| t != self.panel.borrow().ticket_shown);
        if disarm {
            self.panel.borrow_mut().armed = None;
        }
        if disarm || new_ticket {
            self.refresh_panel();
        } else {
            self.refresh_panel_if_changed();
        }
    }

    fn close_panel_ask(&self) {
        self.panel.borrow_mut().asking = None;
        self.panel.borrow().ask_timer.stop();
        if self.screen.get_dialog() == "panel-ask" {
            self.screen.set_dialog("");
        }
    }

    #[cfg(test)]
    pub(super) fn panel_answered_for_test(&self, yes: bool) {
        self.panel_answered(yes);
    }

    /// Refuse or Allow in the dialog.
    fn panel_answered(&self, yes: bool) {
        let Some(id) = self.panel.borrow_mut().asking.take() else {
            return;
        };
        if let Ok(mut s) = self.panel_sessions.try_borrow_mut() {
            s.answer(&id, yes, now(), &mut sessions::system_random);
        }
        log::info!(event = "panel", allowed = yes; "browser access {}", if yes { "allowed" } else { "refused" });
        self.refresh_panel();
    }

    /// A touch on a device's Revoke: the first arms it, the second (within 3 s) revokes it.
    fn panel_revoke(&self, id: &str) {
        let armed = self
            .panel
            .borrow()
            .armed
            .as_ref()
            .is_some_and(|(a, at)| a == id && at.elapsed() < ARMED_FOR);
        if armed {
            self.panel.borrow_mut().armed = None;
            if let Ok(mut s) = self.panel_sessions.try_borrow_mut() {
                s.revoke(id);
            }
        } else {
            self.panel.borrow_mut().armed = Some((id.to_string(), Instant::now()));
        }
        self.refresh_panel();
    }

    /// The device's address on the home network: the interface toward Home Assistant (or the
    /// default route).
    fn panel_address(&self) -> String {
        if self.opts.demo.is_some() {
            return DEMO_ADDRESS.into();
        }
        if let Some(a) = self.panel.borrow().address.clone() {
            return a;
        }
        let host = self
            .opts
            .ha_host
            .clone()
            .unwrap_or_else(|| "192.0.2.1".into());
        let a = match crate::ha::link::local_address(&host, 80) {
            Ok(std::net::IpAddr::V6(v6)) => format!("[{v6}]"),
            Ok(ip) => ip.to_string(),
            Err(_) => String::new(),
        };
        self.panel.borrow_mut().address = Some(a.clone());
        a
    }

    /// Settings → Access from the browser and the Settings row, as they are.
    pub(super) fn refresh_panel(&self) {
        let l = self.locale.borrow();
        let on = self.panel.borrow().on;
        let summary = l.text(if on {
            "panel_access_on"
        } else {
            "panel_access_off"
        });
        let port = self.panel.borrow().cfg.port;
        let host = if on {
            self.panel_address()
        } else {
            String::new()
        };
        let address = if host.is_empty() {
            String::new()
        } else {
            format!("http://{host}:{port}/")
        };
        let (devices, ticket) = if self.opts.demo.is_some() {
            (
                self.panel.borrow().demo.clone(),
                on.then(|| DEMO_TICKET.to_string()),
            )
        } else {
            let Ok(mut s) = self.panel_sessions.try_borrow_mut() else {
                self.panel.borrow_mut().changed = true;
                return;
            };
            let t = now();
            let ticket = (on && self.screen.get_page() == "panel")
                .then(|| s.ticket(t, &mut sessions::system_random).map(String::from))
                .flatten();
            let devices = s
                .list(t)
                .iter()
                .map(|d| {
                    let line = l
                        .text("panel_device_line")
                        .replace("{browser}", &d.agent.browser)
                        .replace("{os}", &d.agent.os);
                    let when = format!(
                        "{} {}",
                        short_date(&l, d.last_used as i64),
                        local_hm(d.last_used as i64).0
                    );
                    let detail = l
                        .text("panel_device_detail")
                        .replace("{peer}", &d.peer)
                        .replace("{when}", &when);
                    (d.id().to_string(), line, detail)
                })
                .collect();
            (devices, ticket)
        };
        let qr = match &ticket {
            Some(t) if self.panel.borrow().ticket_shown.as_ref() != Some(t) && !host.is_empty() => {
                qr_image(&format!("http://{host}:{port}/q/{t}"))
            }
            _ => None,
        };
        self.panel.borrow_mut().ticket_shown = ticket;
        let armed = self
            .panel
            .borrow()
            .armed
            .as_ref()
            .map(|(a, _)| a.clone())
            .unwrap_or_default();
        self.screen
            .set_panel(on, &address, qr, &devices, &armed, summary);
    }

    /// The demos' devices and dialog (`--state settings-panel`, `panel-ask`).
    pub(super) fn demo_panel(&self, ask: bool) {
        let l = self.locale.borrow();
        let line = |b: &str, o: &str| {
            l.text("panel_device_line")
                .replace("{browser}", b)
                .replace("{os}", o)
        };
        let detail = |peer: &str, when: &str| {
            l.text("panel_device_detail")
                .replace("{peer}", peer)
                .replace("{when}", when)
        };
        {
            let mut p = self.panel.borrow_mut();
            p.on = true;
            p.demo = vec![
                (
                    "0123456789abcdef".into(),
                    line("Safari", "iPhone"),
                    detail("192.0.2.31", "14:02"),
                ),
                (
                    "fedcba9876543210".into(),
                    line("Firefox", "Linux"),
                    detail("192.0.2.24", "13:40"),
                ),
            ];
        }
        if ask {
            let who = format!("{} · 192.0.2.24", line("Chrome", "macOS"));
            self.screen.set_panel_ask(&who, "4827");
            self.screen.set_dialog("panel-ask");
        }
        drop(l);
        self.refresh_panel();
    }

    /// Whether a question for whoever is in front of the screen may open now: not at night,
    /// with do not disturb, in a conversation, over a ringing timer, another dialog, mirroring,
    /// nor (unless `over_view`) Home Assistant's view.
    fn quiet_for_a_question(&self, over_view: bool) -> bool {
        if self.panel.borrow().keys_asleep {
            return false;
        }
        let d = self.desk.borrow();
        let busy = !matches!(self.view.get(), Phase::Home | Phase::Offline);
        !d.night_now
            && !self.dnd_on()
            && !busy
            && d.ringing.is_empty()
            && self.screen.get_dialog().is_empty()
            && self.asking.get().is_none()
            && (over_view || !super::super::webview::visible())
            && !self.mirror_shown()
    }

    /// The home's card while requests are stopped.
    pub(super) fn panel_card(&self) -> Option<(String, String)> {
        let l = self.locale.borrow();
        let name = &self.character.name;
        self.panel.borrow().blocked.then(|| {
            (
                word(&l, "panel_blocked_title", name),
                word(&l, "panel_blocked_text", name),
            )
        })
    }
}

impl<S: Screen> PanelHost for Ui<S> {
    fn panel_on(&self) -> bool {
        self.panel.borrow().on
    }

    fn remote_enabled(&self) -> bool {
        self.remote.borrow().on && self.panel.borrow().on
    }

    fn remote_hidden(&self) -> Option<&'static str> {
        self.remote_hidden_now()
    }

    fn remote_frame(&self) -> Result<Option<crate::ui::capture::Frame>, &'static str> {
        self.remote_frame_now()
    }

    fn remote_touches(&self, t: &[crate::panel::remote::Touch]) -> bool {
        self.remote_touches_now(t)
    }

    fn remote_home(&self) -> Result<(), crate::panel::remote::Refusal> {
        self.remote_home_now()
    }

    fn remote_changed(&self, c: Option<&crate::panel::remote::Controller>) {
        self.remote_changed_now(c);
    }

    fn remote_confirm(&self) -> bool {
        self.remote.borrow().confirm
    }

    /// As for the access dialog, but over Home Assistant's view too: the question closes it.
    fn remote_can_ask(&self) -> bool {
        self.quiet_for_a_question(true)
    }

    fn remote_ask(&self, c: &crate::panel::remote::Controller) {
        self.remote_ask_now(c);
    }

    fn can_ask(&self) -> bool {
        self.quiet_for_a_question(false)
    }

    fn name(&self) -> String {
        self.character.name.clone()
    }

    fn words(&self, lang: &str) -> Box<dyn Fn(&str) -> String + '_> {
        let name = self.character.name.clone();
        match self.panel_locale(lang) {
            Some(l) => Box::new(move |k| word(&l, k, &name)),
            None => Box::new(|k| k.to_string()),
        }
    }

    fn languages(&self) -> Vec<String> {
        locale::available(&self.opts.locales)
    }

    fn ask(&self, r: &Request) {
        let line = {
            let l = self.locale.borrow();
            let agent = l
                .text("panel_device_line")
                .replace("{browser}", &r.agent.browser)
                .replace("{os}", &r.agent.os);
            if r.agent.browser.is_empty() {
                r.peer.clone()
            } else {
                format!("{agent} · {}", r.peer)
            }
        };
        self.panel.borrow_mut().asking = Some(r.id.clone());
        self.screen.set_panel_ask(&line, &r.code);
        // A tap meant for what was on screen a moment ago does not answer it.
        self.screen.set_panel_ask_ready(false);
        if let Some(me) = self.this.borrow().upgrade() {
            let w = Rc::downgrade(&me);
            self.panel.borrow().ask_timer.start(
                slint::TimerMode::SingleShot,
                ASK_READY_AFTER,
                move || {
                    if let Some(ui) = w.upgrade() {
                        ui.screen.set_panel_ask_ready(true);
                    }
                },
            );
        }
        self.screen.set_dialog("panel-ask");
        self.wake(WakeSource::Setup);
        self.send(UiEvent::Tap);
    }

    fn shopping(&self) -> Vec<ShopList> {
        self.desk.borrow().shopping.clone().unwrap_or_default()
    }

    fn shopping_add(&self, list: &str, text: &str) {
        if let Some(tx) = self.hub_tx.borrow().as_ref() {
            let _ = tx.send(HubCmd::Add {
                list: list.into(),
                text: text.into(),
            });
        }
    }

    fn shopping_done(&self, list: &str, uid: &str) {
        if let Some(tx) = self.hub_tx.borrow().as_ref() {
            let _ = tx.send(HubCmd::Done {
                list: list.into(),
                uid: uid.into(),
            });
        }
        self.panel_item_gone(list, uid);
    }

    fn shopping_remove(&self, list: &str, uid: &str) {
        if let Some(tx) = self.hub_tx.borrow().as_ref() {
            let _ = tx.send(HubCmd::Remove {
                list: list.into(),
                uid: uid.into(),
            });
        }
        self.panel_item_gone(list, uid);
    }

    fn sessions_changed(&self) {
        self.panel.borrow_mut().changed = true;
    }
    fn board(&self) -> crate::ui::board::Board {
        self.tile_board.borrow().board.clone()
    }
    fn board_replace(&self, b: crate::ui::board::Board) -> Vec<String> {
        log::info!(event = "board"; "the board changed from the browser");
        self.replace_board(b)
    }
    fn board_states(&self) -> crate::ha::widgets::States {
        self.tile_board.borrow().states.clone()
    }
    fn board_curves(&self) -> std::collections::HashMap<String, crate::ha::widgets::Curve> {
        self.ha_curves.borrow().clone()
    }
    fn board_icons(&self) -> crate::ui::board::icons::Icons {
        self.tile_board.borrow().icons.clone()
    }
    fn board_icons_file(&self) -> Option<std::path::PathBuf> {
        Some(crate::ui::board::icons::file_beside(&self.opts.locales))
    }
    fn widget_pages(&self) -> Vec<(crate::ui::widgets::Page, bool)> {
        let on = self.widget_on.borrow();
        self.widget_order
            .borrow()
            .iter()
            .map(|k| (*k, on.contains(k)))
            .collect()
    }
    fn widget_showing(&self) -> Option<crate::ui::widgets::Page> {
        self.widget.borrow().page()
    }
    fn widget_quotes(&self) -> crate::ui::widgets::Lists<crate::feeds::QuoteChoice> {
        self.widget_quotes.borrow().clone()
    }
    fn widget_cities(&self) -> crate::ui::widgets::Lists<crate::feeds::CityChoice> {
        self.widget_cities.borrow().clone()
    }
    fn widget_sensors(&self) -> crate::ui::widgets::Lists<String> {
        self.widget_sensors.borrow().clone()
    }
    fn widget_names(&self) -> Vec<(crate::ui::widgets::Page, String)> {
        self.widget_names.borrow().clone()
    }
    fn search_start(&self, id: &str, what: crate::feeds::What, q: &str, lang: &str) -> bool {
        use crate::feeds::SearchState;
        if what.from_catalogue() {
            // Answered at once from the catalogue in memory.
            let found = crate::ha::widgets::search(&self.ha_catalog.borrow(), what, q);
            self.feeds.borrow().put_search(id, SearchState::Done(found));
            return true;
        }
        self.feeds.borrow_mut().search(id, what, q, lang).is_some()
    }
    fn widget_ha(&self) -> crate::ha::widgets::Want {
        self.widget_ha.borrow().clone()
    }
    fn ha_catalog(&self) -> Vec<crate::ha::widgets::Entity> {
        self.ha_catalog.borrow().clone()
    }
    /// At most every five minutes, however often the panel reads its state.
    fn ha_catalog_refresh(&self) {
        if self
            .catalog_asked
            .get()
            .is_some_and(|t| t.elapsed() < Duration::from_secs(300))
        {
            return;
        }
        self.catalog_asked.set(Some(Instant::now()));
        if let Some(tx) = self.hub_tx.borrow().as_ref() {
            let _ = tx.send(crate::ha::hub::HubCmd::Catalog);
        }
    }
    fn search_state(&self, id: &str) -> crate::feeds::SearchState {
        self.feeds.borrow().search_state(id)
    }
    fn search_choice(&self, id: &str, key: &str) -> Option<crate::feeds::Choice> {
        self.feeds.borrow().search_choice(id, key)
    }
    fn notes(&self) -> Option<Vec<(i64, String)>> {
        self.brain.as_ref()?;
        Some(
            self.board
                .borrow()
                .iter()
                .map(|n| (n.id, n.text.clone()))
                .collect(),
        )
    }
    fn note_add(&self, text: &str) {
        log::info!(event = "notes"; "a note from the browser");
        self.brain_do(crate::brain_link::note_add(text));
    }
    fn note_edit(&self, id: i64, text: &str) {
        log::info!(event = "notes", id = id; "note {id} changed from the browser");
        self.brain_do(crate::brain_link::note_edit(id, text));
    }
    fn note_remove(&self, id: i64) {
        log::info!(event = "notes", id = id; "note {id} removed from the browser");
        self.brain_do(crate::brain_link::note_delete(id));
    }
    fn widget_views(
        &self,
        lang: &str,
    ) -> Vec<(
        crate::ui::widgets::Page,
        Option<crate::ui::widgets::WidgetView>,
        bool,
    )> {
        use crate::ui::widgets;
        let Some(l) = self.panel_locale(lang) else {
            return Vec::new();
        };
        let w = super::widget_words(&l);
        let now = self.now() as i64;
        let today = if self.opts.demo.is_some() {
            super::DEMO_DATE.to_string()
        } else {
            crate::ha::local_date()
        };
        let (columns, hero) = self
            .weather
            .borrow()
            .as_ref()
            .map(|wt| {
                (
                    crate::ui::weather::widget_columns(wt, &l, &today, now, &|t| local_hm(t).0),
                    Some(crate::ui::weather::hero(wt, &l, &today, now, &local_hm)),
                )
            })
            .unwrap_or_default();
        let cities = self.city_rows(now);
        let ha = self.widget_ha.borrow().clone();
        let catalog = self.ha_catalog.borrow().clone();
        let name_of = |id: &str| {
            catalog
                .iter()
                .find(|e| e.id == id)
                .map_or_else(|| id.to_string(), |e| e.name.clone())
        };
        let picks = self.widget_picks(&l, &name_of);
        let custom = self.custom_pages();
        let d = self.desk.borrow();
        let next = crate::ui::next::merge(&d.upcoming, &d.events, now);
        let data = self.widget_data.borrow();
        let real = widgets::Sources {
            next: &next,
            weather: columns,
            weather_hero: hero,
            shopping: d.shopping.as_deref(),
            quotes: &data.quotes,
            cities: if cities.is_empty() {
                &data.cities
            } else {
                &cities
            },
            notes: &data.notes,
            sensors: &data.sensors,
            check: &data.check,
            playing: data.playing.as_ref(),
            people: &data.people,
            custom: &custom,
            picks: &picks,
        };
        let samples = super::samples::samples(&l);
        let player = if ha.player.is_empty() {
            String::new()
        } else {
            name_of(&ha.player)
        };
        let check = ["check_openings", "check_batteries"]
            .iter()
            .any(|k| crate::settings_model::get(self, k) == Some(serde_json::json!(true)));
        let chosen = widgets::Chosen {
            check,
            player: &player,
        };
        let order: Vec<widgets::Page> = self.widget_order.borrow().clone();
        widgets::panel_pages(
            &real,
            &samples.sources(),
            &chosen,
            &order,
            &w,
            now,
            super::samples::NOW,
            &local_hm,
        )
    }
    fn widget_samples(&self, lang: &str) -> Vec<crate::ui::widgets::WidgetView> {
        let Some(l) = self.panel_locale(lang) else {
            return Vec::new();
        };
        let w = super::widget_words(&l);
        let s = super::samples::samples(&l);
        let src = s.sources();
        crate::ui::widgets::Kind::ALL
            .into_iter()
            .filter_map(|k| crate::ui::widgets::page(&src, k, &w, super::samples::NOW, &local_hm))
            .collect()
    }
    fn search_rows(
        &self,
        id: &str,
        lang: &str,
    ) -> Option<Vec<(String, crate::ui::widgets::WidgetRow)>> {
        let l = self.panel_locale(lang)?;
        let w = super::widget_words(&l);
        let rows = self.feeds.borrow().search_rows(id)?;
        Some(super::samples::hit_rows(&rows, &w, self.now() as i64))
    }
}

impl<S: Screen> Ui<S> {
    /// The words of `lang` for the panel, loaded once (`lang` is one of `languages()`,
    /// protocol::pick_language: a handful at most).
    fn panel_locale(&self, lang: &str) -> Option<Rc<locale::Locale>> {
        if let Some(l) = self.panel.borrow().words.get(lang).cloned() {
            return Some(l);
        }
        let l = Rc::new(locale::load_or_english(&self.opts.locales, lang).ok()?);
        self.panel
            .borrow_mut()
            .words
            .insert(lang.to_string(), l.clone());
        Some(l)
    }

    /// An item ticked or removed from the panel: gone from the screen at once (Home Assistant's
    /// own update follows).
    fn panel_item_gone(&self, list: &str, uid: &str) {
        if let Some(l) = self.desk.borrow_mut().shopping.as_mut() {
            lists::remove(l, list, uid);
        }
        self.refresh_home();
        self.render_shopping();
    }
}
