// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the browser panel's process may ask the body, over `panel.sock`: one JSON object per
//! line, one answer per line. A closed list: an `op` not here does not exist for the panel.
//!
//! Without a device let in (`token`) only these pass: `hello` (the character's name, whether the
//! panel is on, whether `token` is good), `strings` (the panel's words), `pair.request`,
//! `pair.status`, `pair.qr`. With one: `session.end`, `schema`, `state`, `set`,
//! `shopping.list|add|done|remove`, `sessions.list|revoke`, and the widget's (widget_ops.rs:
//! `widgets.get|order|samples`, `search.start|status`, `quotes|cities|sensors.add|remove|move`,
//! `player.set`, `people.set`, `notes.list|add|edit|remove`, `undo`; their answers carry the pages'
//! previews as the screen draws them, `pages`), and the remote screen's (remote.rs:
//! `screen.status|start|frame|input|home|stop`). With the panel switched off every
//! op but `hello` and `strings` answers `off`.
//!
//! Every answer is built here from what the body chooses to say: settings through
//! [`crate::settings_model`] (only the `panel` items), the shopping lists' items, the devices'
//! names. Never the Home Assistant token or address, keys, photos, recordings, memories,
//! conversations or logs (a test puts such things in the state and reads every answer).
//!
//! Requests carry `peer` (the browser's address) and `agent` (`{browser, os}`), as the panel's
//! process saw them: shown on the screen, never trusted for anything else.

use serde_json::{Value, json};

use super::remote::{Confirm, Controller, End, Refusal as RemoteRefusal, Start, parse_events};
use super::sessions::{Agent, Now, Refusal, Rng, Sessions, Status};
use crate::settings_model::{self as model, SetError};
use crate::ui::lists::ShopList;

/// The longest item a shopping list gets from the panel.
pub const MAX_ITEM: usize = 200;

/// What the protocol needs from the body (the screen's thread; a fake in the tests).
pub trait PanelHost: model::Host {
    /// Settings → Access from the browser is on.
    fn panel_on(&self) -> bool;
    /// The screen may show the access dialog now (not at night, with do not disturb, during a
    /// conversation, a ring, another dialog…).
    fn can_ask(&self) -> bool;
    /// The character's name.
    fn name(&self) -> String;
    /// The words of `lang` (falling back to English key by key).
    fn words(&self, lang: &str) -> Box<dyn Fn(&str) -> String + '_>;
    /// The language codes there are words for.
    fn languages(&self) -> Vec<String>;
    /// Shows the dialog for a request just made.
    fn ask(&self, request: &super::sessions::Request);
    /// The shopping lists shown on the screen, with their open items.
    fn shopping(&self) -> Vec<ShopList>;
    /// Adds an item to a list (Home Assistant's `todo.add_item`).
    fn shopping_add(&self, list: &str, text: &str);
    /// Ticks an item off (`todo.update_item` completed: Home Assistant keeps it).
    fn shopping_done(&self, list: &str, uid: &str);
    /// Removes an item (`todo.remove_item`).
    fn shopping_remove(&self, list: &str, uid: &str);
    /// The device list changed (the screen's page redraws it).
    fn sessions_changed(&self);
    /// The widget's pages in the turn's order, each on or off.
    fn widget_pages(&self) -> Vec<(crate::ui::widgets::Page, bool)>;
    /// The page of the widget on the screen now (None: none, or not known).
    fn widget_showing(&self) -> Option<crate::ui::widgets::Page> {
        None
    }
    /// The markets, the other cities and the sensors chosen, page by page.
    fn widget_quotes(&self) -> crate::ui::widgets::Lists<crate::feeds::QuoteChoice>;
    fn widget_cities(&self) -> crate::ui::widgets::Lists<crate::feeds::CityChoice>;
    fn widget_sensors(&self) -> crate::ui::widgets::Lists<String>;
    /// The names given to pages.
    fn widget_names(&self) -> Vec<(crate::ui::widgets::Page, String)>;
    /// Starts a search on the feeds' thread (false: another one is still going).
    fn search_start(&self, id: &str, what: crate::feeds::What, q: &str, lang: &str) -> bool;
    fn search_state(&self, id: &str) -> crate::feeds::SearchState;
    /// What hit `key` of search `id` becomes (None: not one of its hits).
    fn search_choice(&self, id: &str, key: &str) -> Option<crate::feeds::Choice>;
    /// What the widget asks of Home Assistant (the user's choices, whether its pages are on or
    /// not), the catalogue of entities it may show, and a fresh catalogue asked for.
    fn widget_ha(&self) -> crate::ha::widgets::Want;
    fn ha_catalog(&self) -> Vec<crate::ha::widgets::Entity>;
    fn ha_catalog_refresh(&self);
    /// The board's notes (id, text); None: no brain to keep them.
    fn notes(&self) -> Option<Vec<(i64, String)>>;
    /// Asks the brain to write, change, remove a note (already checked).
    fn note_add(&self, text: &str);
    fn note_edit(&self, id: i64, text: &str);
    fn note_remove(&self, id: i64);
    /// Every page of the widget as the screen would draw it now, in `lang`'s words, in the
    /// turn's order (`widgets::panel_pages`): the page (None: nothing to show) and whether it
    /// is an example.
    fn widget_views(
        &self,
        lang: &str,
    ) -> Vec<(
        crate::ui::widgets::Page,
        Option<crate::ui::widgets::WidgetView>,
        bool,
    )>;
    /// An example page of every kind for the gallery, in `lang`'s words.
    fn widget_samples(&self, lang: &str) -> Vec<crate::ui::widgets::WidgetView>;
    /// The rows a search's hits will become (hit key → row) once the second half of the search
    /// came back (None: not yet, or never: the hits show without it).
    fn search_rows(
        &self,
        id: &str,
        lang: &str,
    ) -> Option<Vec<(String, crate::ui::widgets::WidgetRow)>> {
        let _ = (id, lang);
        None
    }
    /// The board (ui/board.rs) as it is in use.
    fn board(&self) -> crate::ui::board::Board {
        crate::ui::board::Board::default()
    }
    /// Replaces the board: validated against Home Assistant's catalogue, saved, followed and
    /// drawn (`Ui::replace_board`); the reasons of what was left out.
    fn board_replace(&self, b: crate::ui::board::Board) -> Vec<String> {
        let _ = b;
        vec!["no board here".into()]
    }
    /// The board's entities as the hub last told them, and the wide sensors' days.
    fn board_states(&self) -> crate::ha::widgets::States {
        Default::default()
    }
    fn board_curves(&self) -> std::collections::HashMap<String, crate::ha::widgets::Curve> {
        Default::default()
    }
    /// The icons in use (built in, and those read from the bundle's file).
    fn board_icons(&self) -> crate::ui::board::icons::Icons {
        Default::default()
    }
    /// The bundle's icons file (`icons/mdi.json`), for the icons' search (None: the built-in
    /// ones only).
    fn board_icons_file(&self) -> Option<std::path::PathBuf> {
        None
    }
    /// Settings → Access from the browser → Remote screen is on (on the device only).
    fn remote_enabled(&self) -> bool {
        false
    }
    /// Why the screen must not be sent or touched now ("camera", "qr"…); None: it may.
    fn remote_hidden(&self) -> Option<&'static str> {
        None
    }
    /// The latest frame of the screen, the capture kept going (Ok(None): none yet); Err: this
    /// screen cannot be read (why).
    fn remote_frame(&self) -> Result<Option<crate::ui::capture::Frame>, &'static str> {
        Err("renderer")
    }
    /// The remote's touches, in the picture's 0-1 coordinates: all queued, or none (false: the
    /// screen's queue is full).
    fn remote_touches(&self, touches: &[super::remote::Touch]) -> bool {
        let _ = touches;
        true
    }
    /// The remote asks for the home (out of a page that is not sent, where it cannot see to
    /// touch its way back); Err: not now (remote::home_allowed).
    fn remote_home(&self) -> Result<(), super::remote::Refusal> {
        Ok(())
    }
    /// Settings → Access from the browser → Ask on the screen first is on (on the device only).
    fn remote_confirm(&self) -> bool {
        false
    }
    /// The screen can show the question «… wants to control the screen» now.
    fn remote_can_ask(&self) -> bool {
        true
    }
    /// The screen asks whoever is in front of it to allow `controller` (Deny or Allow come back
    /// through [`remote_answer`]).
    fn remote_ask(&self, controller: &super::remote::Controller) {
        let _ = controller;
    }
    /// The session began (Some) or ended (None): the sign on the screen, the capture.
    fn remote_changed(&self, controller: Option<&super::remote::Controller>) {
        let _ = controller;
    }
}

fn err(code: &str) -> Value {
    json!({"ok": false, "error": code})
}

fn ok(mut v: Value) -> Value {
    v["ok"] = json!(true);
    v
}

fn str_of<'a>(req: &'a Value, k: &str) -> &'a str {
    req.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The first of `wanted` (the browser's languages, best first) there are words for; English
/// otherwise.
pub fn pick_language(wanted: &[String], have: &[String]) -> String {
    wanted
        .iter()
        .map(|w| {
            w.split(['-', '_'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        })
        .find(|w| have.iter().any(|h| h == w))
        .unwrap_or_else(|| locale::DEFAULT.to_string())
}

fn lang_of(host: &dyn PanelHost, req: &Value) -> String {
    let wanted: Vec<String> = req
        .get("lang")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .take(8)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    pick_language(&wanted, &host.languages())
}

/// The panel's words in `lang`: every `panel_*` key of `[text]`, plus the few shared ones.
fn strings(host: &dyn PanelHost, lang: &str, keys: &[&str]) -> Value {
    let word = host.words(lang);
    let mut out = serde_json::Map::new();
    for k in keys {
        out.insert(
            (*k).to_string(),
            json!(word(k).replace("{name}", &host.name())),
        );
    }
    Value::Object(out)
}

/// The keys of the panel's words.
pub const STRING_KEYS: &[&str] = &[
    "panel_ask",
    "panel_refused",
    "panel_expired",
    "panel_busy",
    "panel_blocked",
    "panel_wait",
    "panel_cannot_ask",
    "panel_full",
    "panel_bad_ticket",
    "panel_off",
    "panel_error",
    "panel_saved",
    "panel_shopping_empty",
    "panel_shopping_none",
    "panel_devices_this",
    "panel_logout",
    "panel_device_line",
    "panel_search_quote",
    "panel_search_city",
    "panel_searching",
    "panel_search_none",
    "panel_search_failed",
    "panel_search_busy",
    "panel_search_wait",
    "panel_add",
    "panel_quotes_empty",
    "panel_cities_empty",
    "panel_list_full",
    "panel_exists",
    "panel_invalid",
    "panel_feeds_credit",
    "panel_notes",
    "panel_notes_placeholder",
    "panel_notes_empty",
    "panel_notes_hint",
    "panel_no_brain",
    "panel_edit",
    "panel_save",
    "panel_search_sensor",
    "panel_sensors_empty",
    "panel_search_player",
    "panel_people_hint",
    "panel_no_ha",
    "panel_tabs",
    "panel_tab_shopping",
    "panel_tab_settings",
    "panel_move",
    "panel_moved",
    "panel_order_saved",
    "panel_empty_next",
    "panel_empty_quotes",
    "panel_empty_weather",
    "panel_empty_cities",
    "panel_empty_shopping",
    "panel_empty_notes",
    "panel_empty_sensors",
    "panel_empty_check",
    "panel_empty_playing",
    "panel_empty_home",
    "panel_empty_custom",
    "panel_preview_sample",
    "panel_rows_prev",
    "panel_rows_next",
    "panel_shown_quotes",
    "panel_shown_cities",
    "panel_shown_sensors",
    "panel_already",
    "panel_choose",
    "panel_added",
    "panel_removed",
    "panel_undo",
    "panel_remove_x",
    "panel_check_what",
    "panel_which_player",
    "panel_player_chosen",
    "panel_who",
    "panel_what_next",
    "panel_what_quotes",
    "panel_src_quotes",
    "panel_what_weather",
    "panel_what_cities",
    "panel_src_cities",
    "panel_what_shopping",
    "panel_what_notes",
    "panel_what_sensors",
    "panel_src_sensors",
    "panel_what_check",
    "panel_what_playing",
    "panel_what_home",
    "panel_what_custom",
    "panel_bought",
    "panel_bought_label",
    "panel_add_to",
    "panel_shopping_hint",
    "panel_revoke_x",
    "panel_revoke_sure_x",
    "panel_revoke_sure",
    "panel_access_new_hint",
    "panel_last_used",
    "panel_no_widgets",
    "panel_finish",
    "panel_all_on",
    "panel_add_home",
    "panel_now_on",
    "panel_removed_home",
    "panel_add_another",
    "panel_page_name",
    "panel_page_name_help",
    "panel_remove_page",
    "panel_page_removed",
    "panel_help_weather",
    "panel_help_check",
    "panel_help_playing",
    "panel_check_openings",
    "panel_check_batteries",
    "panel_add_quote",
    "panel_add_city",
    "panel_add_sensor",
    "panel_choose_player",
    "panel_note_new",
    "panel_note_add",
    "panel_nothing_yet",
    "panel_ask_line",
    "panel_code_title",
    "panel_code_text",
    "panel_cancel",
    "panel_off_title",
    "panel_qr_old_title",
    "panel_pair_title",
    "panel_tab_screen",
    "panel_screen_title",
    "panel_screen_lead",
    "panel_screen_start",
    "panel_screen_stop",
    "panel_screen_off",
    "panel_screen_busy",
    "panel_screen_stopped",
    "panel_screen_ended",
    "panel_screen_hidden",
    "panel_screen_unavailable",
    "panel_screen_home",
    "panel_tab_home",
    "panel_home_sub",
    "panel_status_on",
    "panel_device_preview",
    "panel_preview_now",
    "panel_not_on_home",
    "panel_would_look",
    "panel_rotation",
    "panel_every",
    "panel_can_add",
    "panel_pos_of",
    "panel_pos_in",
    "panel_move_hint",
    "panel_showing_now",
    "panel_on_home",
    "panel_off_home_line",
    "panel_position",
    "panel_before",
    "panel_after",
    "panel_reorder",
    "panel_all",
    "panel_only_these",
    "panel_keep_one",
    "panel_lists_edit_in",
    "panel_rotation_in",
    "panel_night_hours",
    "panel_from",
    "panel_to",
    "panel_shopping_sub",
    "panel_to_buy",
    "panel_screen_ro",
    "panel_revoke",
    "panel_revoked",
    "panel_retry",
    "panel_screen_sub",
    "panel_screen_ready_title",
    "panel_screen_ready_text",
    "panel_screen_sign",
    "panel_screen_off_title",
    "panel_screen_private_title",
    "panel_screen_busy_title",
    "panel_screen_busy_by",
    "panel_screen_stopped_title",
    "panel_screen_lost_title",
    "panel_screen_lost",
    "panel_screen_state_ready",
    "panel_screen_state_live",
    "panel_screen_state_mine",
    "panel_screen_state_paused",
    "panel_screen_state_private",
    "panel_screen_state_off",
    "panel_screen_state_busy",
    "panel_screen_state_stopped",
    "panel_screen_state_lost",
    "panel_screen_state_waiting",
    "panel_screen_state_denied",
    "panel_screen_state_unanswered",
    "panel_screen_state_cannotask",
    "panel_screen_will_ask",
    "panel_screen_waiting_title",
    "panel_screen_waiting",
    "panel_screen_denied_title",
    "panel_screen_denied",
    "panel_screen_unanswered_title",
    "panel_screen_unanswered",
    "panel_screen_cannotask_title",
    "panel_screen_cannotask",
    "panel_screen_fps",
    "panel_screen_home_done",
    "panel_screen_home_here",
    "panel_screen_in_use",
    "panel_slow_down",
    "panel_screen_stopped_here",
    "panel_fact_tap",
    "panel_fact_tap_text",
    "panel_fact_seen",
    "panel_fact_seen_text",
    "panel_fact_private",
    "panel_fact_private_text",
    "panel_access",
    "panel_access_on",
    "panel_access_off",
    "panel_screen",
    "panel_tab_board",
    "panel_board_sub",
    "panel_board_pages",
    "panel_board_widgets",
    "panel_board_add_page",
    "panel_board_add_widget",
    "panel_board_empty",
    "panel_board_cell_add",
    "panel_board_move_here",
    "panel_board_tile_of",
    "panel_board_tile",
    "panel_board_pick",
    "panel_board_done",
    "panel_board_never",
    "panel_board_add_title",
    "panel_board_at",
    "panel_board_search",
    "panel_board_change",
    "panel_board_name",
    "panel_board_icon",
    "panel_board_icon_default",
    "panel_board_icon_reset",
    "panel_board_icons_search",
    "panel_board_icons_some",
    "panel_board_size",
    "panel_board_kind",
    "panel_board_kind_button",
    "panel_board_kind_sensor",
    "panel_board_kind_level",
    "panel_board_confirm",
    "panel_board_confirm_hint",
    "panel_board_confirm_off",
    "panel_board_advanced",
    "panel_board_advanced_hint",
    "panel_board_service",
    "panel_board_data",
    "panel_board_apply",
    "panel_board_bad_json",
    "panel_board_bad_service",
    "panel_board_move",
    "panel_board_move_hint",
    "panel_board_moved",
    "panel_board_delete",
    "panel_board_no_room",
    "panel_board_dropped",
    "panel_board_page_name",
    "panel_board_widget_name",
    "panel_board_page_hint",
    "panel_board_widget_hint",
    "panel_board_remove_page",
    "panel_board_remove_widget",
    "panel_board_edit_in",
    "ha_short",
    "talk",
    "more",
];

fn shopping_json(lists: &[ShopList]) -> Value {
    json!({"lists": lists.iter().map(|l| json!({
        "entity": l.entity,
        "name": l.name,
        "items": l.items.iter().map(|(uid, text)| json!({"uid": uid, "text": text})).collect::<Vec<_>>(),
    })).collect::<Vec<_>>()})
}

/// An item's text from the panel: one line, trimmed, at most [`MAX_ITEM`] characters, none of
/// them control characters.
pub fn clean_item(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > MAX_ITEM || t.chars().any(char::is_control) {
        return None;
    }
    Some(t.to_string())
}

/// One request → its answer.
pub fn handle(
    host: &dyn PanelHost,
    sessions: &mut Sessions,
    req: &Value,
    now: Now,
    rng: Rng,
) -> Value {
    let op = str_of(req, "op");
    let token = str_of(req, "token");
    let peer = str_of(req, "peer");
    let agent = req
        .get("agent")
        .map(|a| Agent::clean(str_of(a, "browser"), str_of(a, "os")))
        .unwrap_or_default();
    // Whatever needs no device let in is limited for everyone together: a flood from many
    // addresses never keeps the screen's thread busy.
    let open_op = matches!(
        op,
        "hello" | "strings" | "pair.request" | "pair.status" | "pair.qr"
    );
    if open_op && !sessions.open_call(now) {
        return err("busy");
    }
    match op {
        "hello" => {
            let session = host.panel_on() && sessions.check(token, now).is_some();
            return ok(json!({"name": host.name(), "on": host.panel_on(), "session": session}));
        }
        "strings" => {
            let lang = lang_of(host, req);
            return ok(json!({"lang": lang, "strings": strings(host, &lang, STRING_KEYS)}));
        }
        _ => {}
    }
    if !host.panel_on() {
        return err("off");
    }
    let refusal = |r: Refusal| err(r.key());
    match op {
        "pair.request" => {
            // While someone controls the screen from a browser no access dialog opens: the remote
            // could answer it.
            let can = host.can_ask() && !sessions.remote.active();
            return match sessions.request(peer, agent, can, now, rng) {
                Ok(r) => {
                    host.ask(&r);
                    log::info!(event = "panel", peer = peer; "browser access asked from {peer}");
                    ok(json!({"id": r.id, "code": r.code}))
                }
                Err(r) => refusal(r),
            };
        }
        "pair.status" => {
            return match sessions.status(str_of(req, "id"), now) {
                Status::Waiting => ok(json!({"status": "waiting"})),
                Status::Refused => ok(json!({"status": "refused"})),
                Status::Expired => ok(json!({"status": "expired"})),
                Status::Unknown => ok(json!({"status": "unknown"})),
                Status::Allowed(t) => {
                    host.sessions_changed();
                    ok(json!({"status": "allowed", "token": t}))
                }
            };
        }
        "pair.qr" => {
            return match sessions.redeem(str_of(req, "ticket"), peer, agent, now, rng) {
                Ok(t) => {
                    host.sessions_changed();
                    ok(json!({"token": t}))
                }
                Err(r) => refusal(r),
            };
        }
        _ => {}
    }
    let Some((me, my_agent)) = sessions
        .check(token, now)
        .map(|s| (s.hash.clone(), s.agent.clone()))
    else {
        return err("no_session");
    };
    if op.starts_with("screen.") {
        return screen_op(host, sessions, &me, &my_agent, peer, op, req, now);
    }
    if super::board_ops::OPS.contains(&op) {
        let alive: Vec<String> = sessions.list(now).iter().map(|s| s.hash.clone()).collect();
        sessions.searches.keep_only(&alive);
        sessions.board.keep_only(&alive);
        let lang = lang_of(host, req);
        let who = super::widget_ops::Caller {
            me: &me,
            lang: &lang,
            now,
        };
        return super::board_ops::handle(
            host,
            &mut sessions.board,
            &mut sessions.searches,
            &who,
            op,
            req,
        );
    }
    if super::widget_ops::OPS.contains(&op) {
        let alive: Vec<String> = sessions.list(now).iter().map(|s| s.hash.clone()).collect();
        sessions.searches.keep_only(&alive);
        let lang = lang_of(host, req);
        let who = super::widget_ops::Caller {
            me: &me,
            lang: &lang,
            now,
        };
        return super::widget_ops::handle(host, &mut sessions.searches, &who, op, req, rng);
    }
    match op {
        "session.end" => {
            sessions.end(token);
            remote_sweep(host, sessions, now);
            host.sessions_changed();
            ok(json!({}))
        }
        "schema" => {
            let lang = lang_of(host, req);
            let word = host.words(&lang);
            ok(model::panel_schema(host, &*word))
        }
        "state" => ok(json!({"values": model::panel_state(host)})),
        "set" => match model::panel_set(
            host,
            str_of(req, "key"),
            req.get("value").unwrap_or(&Value::Null),
        ) {
            Ok(()) => {
                let key = str_of(req, "key");
                let mut a = json!({"values": model::panel_state(host)});
                // What the previews show changed with it: the pages come along.
                if key.starts_with("widgets")
                    || key.starts_with("check_")
                    || matches!(key, "lists" | "calendars")
                {
                    a["pages"] = super::widget_ops::pages(host, &lang_of(host, req));
                }
                ok(a)
            }
            Err(SetError::Unknown) => err("unknown_key"),
            Err(SetError::Invalid) => err("invalid"),
        },
        "shopping.list" => ok(shopping_json(&host.shopping())),
        "shopping.add" => {
            let list = str_of(req, "list");
            let Some(text) = clean_item(str_of(req, "text")) else {
                return err("invalid");
            };
            if !host.shopping().iter().any(|l| l.entity == list) {
                return err("invalid");
            }
            host.shopping_add(list, &text);
            ok(json!({}))
        }
        "shopping.done" | "shopping.remove" => {
            let (list, uid) = (str_of(req, "list"), str_of(req, "uid"));
            let known = host
                .shopping()
                .iter()
                .any(|l| l.entity == list && l.items.iter().any(|(u, _)| u == uid));
            if !known {
                return err("invalid");
            }
            if op == "shopping.done" {
                host.shopping_done(list, uid);
            } else {
                host.shopping_remove(list, uid);
            }
            ok(shopping_json(&host.shopping()))
        }
        "sessions.list" => {
            let list: Vec<Value> = sessions
                .list(now)
                .iter()
                .map(|s| {
                    json!({"id": s.id(), "browser": s.agent.browser, "os": s.agent.os,
                           "peer": s.peer, "created": s.created, "last_used": s.last_used,
                           "current": s.hash == me})
                })
                .collect();
            ok(json!({"sessions": list}))
        }
        "sessions.revoke" => {
            let gone = sessions.revoke(str_of(req, "id"));
            remote_sweep(host, sessions, now);
            host.sessions_changed();
            if gone { ok(json!({})) } else { err("invalid") }
        }
        _ => err("unknown_op"),
    }
}

/// Ends the remote screen's session when its device went quiet or is no longer let in (the
/// screen calls it every second too).
pub fn remote_sweep(host: &dyn PanelHost, sessions: &mut Sessions, now: Now) {
    let alive: Vec<String> = sessions.list(now).iter().map(|s| s.hash.clone()).collect();
    let gone = match sessions.remote.keep_only(&alive) {
        Some(c) => Some((c, End::Revoked)),
        None => sessions.remote.expire(now.mono),
    };
    if let Some((c, why)) = gone {
        remote_ended(host, &c, why);
    }
}

/// Ends the remote screen's session (if any) for `why`: Stop on the screen, the switch off.
pub fn remote_stop(host: &dyn PanelHost, sessions: &mut Sessions, why: End, now: Now) {
    if let Some(c) = sessions.remote.stop(why, now.mono) {
        remote_ended(host, &c, why);
    }
}

/// Deny or Allow on the screen's question: the session it asked for ends, or is in control.
pub fn remote_answer(host: &dyn PanelHost, sessions: &mut Sessions, yes: bool, now: Now) {
    if sessions.remote.asking().is_none() {
        return;
    }
    if !yes {
        remote_stop(host, sessions, End::Denied, now);
        return;
    }
    if let Some(c) = sessions.remote.allow(now.mono) {
        log::info!(event = "remote_screen", action = "allowed", peer = c.peer.as_str();
                   "remote screen: {} on {} ({}) allowed on the screen, in control",
                   c.agent.browser, c.agent.os, c.peer);
        host.remote_changed(Some(&c));
    }
}

fn remote_ended(host: &dyn PanelHost, c: &Controller, why: End) {
    log::info!(event = "remote_screen", action = "stop", why = why.key(), peer = c.peer.as_str(),
               secs = c.since.elapsed().as_secs();
               "remote screen: {} on {} ({}) stopped: {}", c.agent.browser, c.agent.os, c.peer, why.key());
    host.remote_changed(None);
}

/// `screen.*`: the remote screen (remote.rs), for a device let in (`me`).
#[allow(clippy::too_many_arguments)]
fn screen_op(
    host: &dyn PanelHost,
    sessions: &mut Sessions,
    me: &str,
    agent: &Agent,
    peer: &str,
    op: &str,
    req: &Value,
    now: Now,
) -> Value {
    remote_sweep(host, sessions, now);
    let t = now.mono;
    let refused = |r: RemoteRefusal| {
        if matches!(
            r,
            RemoteRefusal::Off
                | RemoteRefusal::Busy
                | RemoteRefusal::Stopped
                | RemoteRefusal::Denied
                | RemoteRefusal::Unanswered
                | RemoteRefusal::CannotAsk
        ) {
            log::info!(event = "remote_screen", action = "denied", why = r.key(), peer = peer;
                       "remote screen: {} on {} ({peer}) refused: {}", agent.browser, agent.os, r.key());
        }
        err(r.key())
    };
    match op {
        "screen.status" => {
            let c = sessions.remote.controller.as_ref();
            // Who is in control, when it is another device: what the screen's sign says
            // («Controlled by Safari on iPhone»), never its address.
            let by = c
                .filter(|c| c.hash != me)
                .map(|c| json!({"browser": c.agent.browser, "os": c.agent.os}));
            ok(json!({
                "enabled": host.remote_enabled(),
                "confirm": host.remote_confirm(),
                "active": c.is_some(),
                "mine": c.is_some_and(|c| c.hash == me),
                "waiting": c.is_some_and(|c| c.hash == me && !c.allowed),
                "by": by,
            }))
        }
        "screen.start" => {
            let confirm = if !host.remote_confirm() {
                Confirm::No
            } else if host.remote_can_ask() {
                Confirm::Ask
            } else {
                Confirm::CannotAsk
            };
            match sessions
                .remote
                .start(me, agent, peer, host.remote_enabled(), confirm, t)
            {
                Ok(Start::Live) => {
                    log::info!(event = "remote_screen", action = "start", peer = peer;
                               "remote screen: {} on {} ({peer}) in control", agent.browser, agent.os);
                    host.remote_changed(sessions.remote.controller.as_ref());
                    ok(json!({}))
                }
                Ok(Start::Replaced) => {
                    host.remote_changed(sessions.remote.controller.as_ref());
                    ok(json!({}))
                }
                Ok(Start::Ask) => {
                    log::info!(event = "remote_screen", action = "ask", peer = peer;
                               "remote screen: {} on {} ({peer}) asks, the screen asks the room",
                               agent.browser, agent.os);
                    if let Some(c) = sessions.remote.asking() {
                        host.remote_ask(c);
                    }
                    ok(json!({"waiting": true}))
                }
                Ok(Start::Asking) => ok(json!({"waiting": true})),
                Err(r) => refused(r),
            }
        }
        "screen.stop" => {
            // Also while the screen still asks (Cancel in the browser): then as if nobody
            // answered, so a loop of start and stop cannot keep putting the question up.
            if sessions.remote.owns(me) {
                let why = if sessions.remote.asking().is_some() {
                    End::Unanswered
                } else {
                    End::Browser
                };
                remote_stop(host, sessions, why, now);
            }
            ok(json!({}))
        }
        "screen.frame" => {
            match sessions.remote.check(me, t) {
                Ok(()) => {}
                // Kept alive while the screen asks: nothing to see yet.
                Err(RemoteRefusal::Waiting) => return ok(json!({"waiting": true})),
                Err(r) => return refused(r),
            }
            if let Some(why) = host.remote_hidden() {
                return ok(json!({"hidden": why}));
            }
            let after = req.get("after").and_then(Value::as_u64).unwrap_or(0);
            match host.remote_frame() {
                Ok(Some(f)) if f.seq != after => ok(json!({
                    "seq": f.seq,
                    "width": f.width,
                    "height": f.height,
                    "jpeg": f.b64.as_str(),
                })),
                Ok(_) => ok(json!({"same": true})),
                Err(why) => json!({"ok": false, "error": "screen_unavailable", "why": why}),
            }
        }
        "screen.home" => {
            if let Err(r) = sessions.remote.check(me, t) {
                return refused(r);
            }
            match host.remote_home() {
                Ok(()) => ok(json!({})),
                Err(r) => refused(r),
            }
        }
        "screen.input" => {
            if let Err(r) = sessions.remote.check(me, t) {
                return refused(r);
            }
            if host.remote_hidden().is_some() {
                return err("screen_hidden");
            }
            let Some(events) = parse_events(req.get("events").unwrap_or(&Value::Null)) else {
                return err("invalid");
            };
            if !sessions.remote.allow_events(events.len(), t) {
                return refused(RemoteRefusal::SlowDown);
            }
            if !host.remote_touches(&events) {
                return refused(RemoteRefusal::SlowDown);
            }
            ok(json!({}))
        }
        _ => err("unknown_op"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_model::tests::FakeHost;
    use std::cell::{Cell, RefCell};

    struct Host {
        model: FakeHost,
        on: Cell<bool>,
        can: Cell<bool>,
        asked: RefCell<Vec<String>>,
        lists: RefCell<Vec<ShopList>>,
        calls: RefCell<Vec<String>>,
        searches: RefCell<std::collections::HashMap<String, crate::feeds::SearchState>>,
        notes: RefCell<Option<Vec<(i64, String)>>>,
        remote_on: Cell<bool>,
        remote_confirm: Cell<bool>,
        remote_can_ask: Cell<bool>,
        remote_asks: RefCell<Vec<String>>,
        hidden: Cell<Option<&'static str>>,
        touches: RefCell<Vec<crate::panel::remote::Touch>>,
        signs: RefCell<Vec<Option<String>>>,
        board: RefCell<crate::ui::board::Board>,
    }

    impl model::Host for Host {
        fn settings(&self) -> Option<&crate::settings::Settings> {
            self.model.settings()
        }
        fn live(&self, key: &str) -> Option<Value> {
            self.model.live(key)
        }
        fn dynamic_options(&self, key: &str) -> Vec<(String, String)> {
            self.model.dynamic_options(key)
        }
        fn apply(&self, change: model::Change) {
            self.model.apply(change)
        }
    }

    impl PanelHost for Host {
        fn panel_on(&self) -> bool {
            self.on.get()
        }
        fn can_ask(&self) -> bool {
            self.can.get()
        }
        fn name(&self) -> String {
            "Alfred".into()
        }
        fn words(&self, lang: &str) -> Box<dyn Fn(&str) -> String + '_> {
            let l = locale::load_or_english(&locale::repo_locales(), lang).unwrap();
            Box::new(move |k| l.text(k).to_string())
        }
        fn languages(&self) -> Vec<String> {
            vec!["en".into(), "it".into()]
        }
        fn ask(&self, r: &crate::panel::sessions::Request) {
            self.asked.borrow_mut().push(r.code.clone());
        }
        fn shopping(&self) -> Vec<ShopList> {
            self.lists.borrow().clone()
        }
        fn shopping_add(&self, list: &str, text: &str) {
            self.calls.borrow_mut().push(format!("add {list} {text}"));
        }
        fn shopping_done(&self, list: &str, uid: &str) {
            self.calls.borrow_mut().push(format!("done {list} {uid}"));
            crate::ui::lists::remove(&mut self.lists.borrow_mut(), list, uid);
        }
        fn shopping_remove(&self, list: &str, uid: &str) {
            self.calls.borrow_mut().push(format!("remove {list} {uid}"));
            crate::ui::lists::remove(&mut self.lists.borrow_mut(), list, uid);
        }
        fn sessions_changed(&self) {}
        fn board(&self) -> crate::ui::board::Board {
            self.board.borrow().clone()
        }
        fn board_replace(&self, b: crate::ui::board::Board) -> Vec<String> {
            let (b, why) = crate::ui::board::validate(b, &self.ha_catalog());
            *self.board.borrow_mut() = b;
            why
        }
        fn remote_enabled(&self) -> bool {
            self.remote_on.get()
        }
        fn remote_confirm(&self) -> bool {
            self.remote_confirm.get()
        }
        fn remote_can_ask(&self) -> bool {
            self.remote_can_ask.get()
        }
        fn remote_ask(&self, c: &crate::panel::remote::Controller) {
            self.remote_asks
                .borrow_mut()
                .push(format!("{} {}", c.agent.browser, c.agent.os));
        }
        fn remote_hidden(&self) -> Option<&'static str> {
            self.hidden.get()
        }
        fn remote_frame(&self) -> Result<Option<crate::ui::capture::Frame>, &'static str> {
            Ok(Some(crate::ui::capture::Frame {
                seq: 3,
                width: 2,
                height: 1,
                jpeg: std::sync::Arc::new(b"jpeg".to_vec()),
                b64: std::sync::Arc::new("anBlZw==".into()),
            }))
        }
        fn remote_touches(&self, t: &[crate::panel::remote::Touch]) -> bool {
            self.touches.borrow_mut().extend_from_slice(t);
            true
        }
        fn remote_changed(&self, c: Option<&crate::panel::remote::Controller>) {
            self.signs
                .borrow_mut()
                .push(c.map(|c| format!("{} {}", c.agent.browser, c.agent.os)));
        }
        fn widget_pages(&self) -> Vec<(crate::ui::widgets::Page, bool)> {
            let order = model::widget_order(self);
            let on = self.model.settings.widgets().0;
            order.iter().map(|k| (*k, on.contains(k))).collect()
        }
        fn widget_showing(&self) -> Option<crate::ui::widgets::Page> {
            Some(crate::ui::widgets::Kind::Quotes.into())
        }
        fn widget_quotes(&self) -> crate::ui::widgets::Lists<crate::feeds::QuoteChoice> {
            self.model.settings.quotes()
        }
        fn widget_cities(&self) -> crate::ui::widgets::Lists<crate::feeds::CityChoice> {
            self.model.settings.cities()
        }
        fn widget_sensors(&self) -> crate::ui::widgets::Lists<String> {
            self.model.settings.sensors()
        }
        fn widget_names(&self) -> Vec<(crate::ui::widgets::Page, String)> {
            self.model.settings.widget_names()
        }
        fn search_start(&self, id: &str, what: crate::feeds::What, q: &str, _lang: &str) -> bool {
            use crate::feeds::*;
            let mut s = self.searches.borrow_mut();
            if s.values().any(|x| *x == SearchState::Waiting) {
                return false;
            }
            // A canned answer: the words searched become a hit's name (they are a source's
            // text now).
            if what.from_catalogue() {
                let f = crate::ha::widgets::search(&self.ha_catalog(), what, q);
                s.insert(id.into(), SearchState::Done(f));
                return true;
            }
            let found = match what {
                What::Quote => vec![Found {
                    hit: Hit {
                        key: "yahoo:AAPL".into(),
                        name: q.into(),
                        detail: "AAPL · NASDAQ".into(),
                    },
                    choice: Choice::Quote(QuoteChoice {
                        source: Source::Yahoo,
                        id: "AAPL".into(),
                        name: clean_name(q),
                    }),
                }],
                _ => vec![Found {
                    hit: Hit {
                        key: "city:7".into(),
                        name: q.into(),
                        detail: "Region, XX".into(),
                    },
                    choice: Choice::City(CityChoice {
                        id: 7,
                        name: clean_name(q),
                        region: "Region".into(),
                        country: "XX".into(),
                        lat: 1.5,
                        lon: 2.5,
                    }),
                }],
            };
            s.insert(id.into(), SearchState::Done(found));
            true
        }
        fn search_state(&self, id: &str) -> crate::feeds::SearchState {
            self.searches
                .borrow()
                .get(id)
                .cloned()
                .unwrap_or(crate::feeds::SearchState::Unknown)
        }
        fn notes(&self) -> Option<Vec<(i64, String)>> {
            self.notes.borrow().clone()
        }
        fn widget_ha(&self) -> crate::ha::widgets::Want {
            model::widget_ha(self)
        }
        /// The catalogue of the fixture: it holds a lock, an alarm, a camera with a token.
        fn ha_catalog(&self) -> Vec<crate::ha::widgets::Entity> {
            let text = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("fixtures/ha/widgets-get-states.json"),
            )
            .unwrap();
            crate::ha::widgets::parse_catalog(&text).unwrap()
        }
        fn ha_catalog_refresh(&self) {}
        fn note_add(&self, text: &str) {
            self.calls.borrow_mut().push(format!("note add {text}"));
        }
        fn note_edit(&self, id: i64, text: &str) {
            self.calls
                .borrow_mut()
                .push(format!("note edit {id} {text}"));
        }
        fn note_remove(&self, id: i64) {
            self.calls.borrow_mut().push(format!("note remove {id}"));
        }
        /// The samples for the pages that wait or show at times; the notes are the host's own.
        fn widget_views(
            &self,
            lang: &str,
        ) -> Vec<(
            crate::ui::widgets::Page,
            Option<crate::ui::widgets::WidgetView>,
            bool,
        )> {
            use crate::ui::screen::samples;
            let l = locale::load_or_english(&locale::repo_locales(), lang).unwrap();
            let w = samples::words(&l);
            let s = samples::samples(&l);
            let notes: Vec<String> = self
                .notes
                .borrow()
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|n| n.1)
                .collect();
            let order: Vec<_> = self.widget_pages().into_iter().map(|x| x.0).collect();
            let picks = crate::ui::widgets::picks_of(
                &order,
                &crate::ui::widgets::AllLists {
                    quotes: &self.widget_quotes(),
                    cities: &self.widget_cities(),
                    sensors: &self.widget_sensors(),
                    names: &self.widget_names(),
                },
                &|id| id.to_string(),
                &|k| l.text(k).to_string(),
            );
            let real = crate::ui::widgets::Sources {
                notes: &notes,
                picks: &picks,
                ..Default::default()
            };
            let chosen = crate::ui::widgets::Chosen {
                check: true,
                ..Default::default()
            };
            crate::ui::widgets::panel_pages(
                &real,
                &s.sources(),
                &chosen,
                &order,
                &w,
                samples::NOW,
                samples::NOW,
                &samples::local,
            )
        }
        fn widget_samples(&self, lang: &str) -> Vec<crate::ui::widgets::WidgetView> {
            use crate::ui::screen::samples;
            let l = locale::load_or_english(&locale::repo_locales(), lang).unwrap();
            let s = samples::samples(&l);
            crate::ui::widgets::Kind::ALL
                .into_iter()
                .filter_map(|k| {
                    crate::ui::widgets::page(
                        &s.sources(),
                        k,
                        &samples::words(&l),
                        samples::NOW,
                        &samples::local,
                    )
                })
                .collect()
        }
        fn search_rows(
            &self,
            _id: &str,
            _lang: &str,
        ) -> Option<Vec<(String, crate::ui::widgets::WidgetRow)>> {
            Some(vec![(
                "yahoo:AAPL".into(),
                crate::ui::widgets::WidgetRow {
                    right: "189.20 $".into(),
                    curve: vec![0.0, 1.0],
                    ..Default::default()
                },
            )])
        }
        fn search_choice(&self, id: &str, key: &str) -> Option<crate::feeds::Choice> {
            match self.searches.borrow().get(id) {
                Some(crate::feeds::SearchState::Done(f)) => f
                    .iter()
                    .find(|x| x.hit.key == key)
                    .map(|x| x.choice.clone()),
                _ => None,
            }
        }
    }

    fn host(name: &str) -> Host {
        Host {
            model: FakeHost::new(name),
            on: Cell::new(true),
            can: Cell::new(true),
            asked: RefCell::new(Vec::new()),
            lists: RefCell::new(vec![ShopList {
                entity: "todo.shopping".into(),
                name: "Shopping".into(),
                items: vec![("u1".into(), "Bread".into()), ("u2".into(), "Milk".into())],
            }]),
            calls: RefCell::new(Vec::new()),
            searches: RefCell::new(Default::default()),
            notes: RefCell::new(Some(vec![(1, "Water the plants".into())])),
            remote_on: Cell::new(false),
            remote_confirm: Cell::new(false),
            remote_can_ask: Cell::new(true),
            remote_asks: RefCell::new(Vec::new()),
            hidden: Cell::new(None),
            touches: RefCell::new(Vec::new()),
            signs: RefCell::new(Vec::new()),
            board: RefCell::new(Default::default()),
        }
    }

    use crate::panel::sessions::tests::t as at;

    /// Random bytes that never repeat over a test run (a fresh counter would hand two devices
    /// the same token).
    fn rng(out: &mut [u8]) -> bool {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(1);
        for chunk in out.chunks_mut(8) {
            let n = N
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_mul(0x9e37_79b9_7f4a_7c15);
            chunk.copy_from_slice(&n.to_le_bytes()[..chunk.len()]);
        }
        true
    }

    fn call(h: &Host, s: &mut Sessions, req: Value) -> Value {
        handle(h, s, &req, at(0), &mut rng)
    }

    /// A device let in through the screen's dialog: (token).
    fn pair(h: &Host, s: &mut Sessions) -> String {
        let r = call(
            h,
            s,
            json!({"op": "pair.request", "peer": "192.0.2.24",
            "agent": {"browser": "Chrome", "os": "macOS"}}),
        );
        let id = r["id"].as_str().unwrap().to_string();
        assert_eq!(
            h.asked.borrow().last().unwrap(),
            r["code"].as_str().unwrap()
        );
        let waiting = s.waiting().unwrap().id.clone();
        s.answer(&waiting, true, at(0), &mut rng);
        let st = call(h, s, json!({"op": "pair.status", "id": id}));
        assert_eq!(st["status"], "allowed");
        st["token"].as_str().unwrap().to_string()
    }

    /// Every op the panel knows.
    const SESSION_OPS: &[&str] = &[
        "schema",
        "state",
        "set",
        "shopping.list",
        "shopping.add",
        "shopping.done",
        "shopping.remove",
        "sessions.list",
        "sessions.revoke",
        "widgets.get",
        "widgets.order",
        "search.start",
        "search.status",
        "quotes.add",
        "quotes.remove",
        "quotes.move",
        "cities.add",
        "cities.remove",
        "cities.move",
        "sensors.add",
        "sensors.remove",
        "sensors.move",
        "player.set",
        "people.set",
        "notes.list",
        "notes.add",
        "notes.edit",
        "notes.remove",
        "widgets.samples",
        "widgets.showing",
        "undo",
        "board.get",
        "board.set",
        "board.page.add",
        "board.page.remove",
        "board.page.name",
        "board.page.order",
        "board.widget.add",
        "board.widget.remove",
        "board.widget.name",
        "board.search",
        "board.icons",
        "board.undo",
        "screen.status",
        "screen.start",
        "screen.frame",
        "screen.input",
        "screen.home",
        "screen.stop",
        "session.end",
    ];

    #[test]
    fn without_a_session_only_pairing_passes() {
        let h = host("nosession");
        let mut s = Sessions::default();
        let hello = call(&h, &mut s, json!({"op": "hello"}));
        assert_eq!(
            hello,
            json!({"ok": true, "name": "Alfred", "on": true, "session": false})
        );
        for op in SESSION_OPS {
            for token in ["", "abc", &"f".repeat(64)] {
                let r = call(
                    &h,
                    &mut s,
                    json!({"op": op, "token": token, "key": "home_compact",
                    "value": true, "list": "todo.shopping", "uid": "u1", "text": "x"}),
                );
                assert_eq!(r, json!({"ok": false, "error": "no_session"}), "{op}");
            }
        }
        assert!(h.model.applied.borrow().is_empty());
        assert!(h.calls.borrow().is_empty());
        assert_eq!(
            call(&h, &mut s, json!({"op": "shell", "token": ""}))["error"],
            "no_session"
        );
        // Revoked: nothing either.
        let t = pair(&h, &mut s);
        assert_eq!(
            call(&h, &mut s, json!({"op": "hello", "token": t}))["session"],
            true
        );
        let id = s.list(at(0))[0].id().to_string();
        assert!(s.revoke(&id));
        let r = call(
            &h,
            &mut s,
            json!({"op": "set", "token": t, "key": "home_compact", "value": true}),
        );
        assert_eq!(r["error"], "no_session");
    }

    #[test]
    fn off_answers_off() {
        let h = host("off");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        h.on.set(false);
        for op in ["pair.request", "pair.qr", "pair.status", "state", "set"] {
            assert_eq!(
                call(&h, &mut s, json!({"op": op, "token": t}))["error"],
                "off"
            );
        }
        assert_eq!(
            call(&h, &mut s, json!({"op": "hello", "token": t}))["session"],
            false
        );
        assert_eq!(call(&h, &mut s, json!({"op": "strings"}))["ok"], true);
        // Switching off keeps the devices.
        h.on.set(true);
        assert_eq!(
            call(&h, &mut s, json!({"op": "hello", "token": t}))["session"],
            true
        );
    }

    #[test]
    fn settings_and_shopping_with_a_session() {
        let h = host("ops");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        let r = call(
            &h,
            &mut s,
            json!({"op": "set", "token": t, "key": "home_compact", "value": true}),
        );
        assert_eq!(r["values"]["home_compact"], true);
        assert_eq!(
            h.model.applied.borrow().last(),
            Some(&model::Change::HomeCompact(true))
        );
        let r = call(
            &h,
            &mut s,
            json!({"op": "set", "token": t, "key": "update_prerelease", "value": true}),
        );
        assert_eq!(r["error"], "unknown_key");
        let r = call(
            &h,
            &mut s,
            json!({"op": "set", "token": t, "key": "volume", "value": 7}),
        );
        assert_eq!(r["error"], "invalid");
        let schema = call(
            &h,
            &mut s,
            json!({"op": "schema", "token": t, "lang": ["it-IT", "en"]}),
        );
        assert_eq!(schema["groups"][1]["label"], "Schermo");
        let r = call(
            &h,
            &mut s,
            json!({"op": "shopping.add", "token": t, "list": "todo.shopping",
            "text": "  Eggs  "}),
        );
        assert_eq!(r["ok"], true);
        for (list, text) in [
            ("todo.other", "x"),
            ("todo.shopping", ""),
            ("todo.shopping", "a\nb"),
        ] {
            let r = call(
                &h,
                &mut s,
                json!({"op": "shopping.add", "token": t, "list": list, "text": text}),
            );
            assert_eq!(r["error"], "invalid");
        }
        let long = "x".repeat(MAX_ITEM + 1);
        let r = call(
            &h,
            &mut s,
            json!({"op": "shopping.add", "token": t, "list": "todo.shopping", "text": long}),
        );
        assert_eq!(r["error"], "invalid");
        let r = call(
            &h,
            &mut s,
            json!({"op": "shopping.done", "token": t, "list": "todo.shopping", "uid": "u1"}),
        );
        assert_eq!(
            r["lists"][0]["items"],
            json!([{"uid": "u2", "text": "Milk"}])
        );
        let r = call(
            &h,
            &mut s,
            json!({"op": "shopping.remove", "token": t, "list": "todo.shopping", "uid": "u1"}),
        );
        assert_eq!(r["error"], "invalid");
        call(
            &h,
            &mut s,
            json!({"op": "shopping.remove", "token": t, "list": "todo.shopping", "uid": "u2"}),
        );
        assert_eq!(
            *h.calls.borrow(),
            [
                "add todo.shopping Eggs",
                "done todo.shopping u1",
                "remove todo.shopping u2"
            ]
        );
        // The devices: this one marked, revoked by id, then logged out.
        let list = call(&h, &mut s, json!({"op": "sessions.list", "token": t}));
        assert_eq!(list["sessions"][0]["current"], true);
        assert_eq!(list["sessions"][0]["browser"], "Chrome");
        let r = call(&h, &mut s, json!({"op": "session.end", "token": t}));
        assert_eq!(r["ok"], true);
        assert_eq!(
            call(&h, &mut s, json!({"op": "state", "token": t}))["error"],
            "no_session"
        );
    }

    /// The pages as the screen draws them, in every answer that changes them; the rows a search
    /// will become; a removal put back once, by the device that made it, within 15 s.
    #[test]
    fn previews_rows_and_undo() {
        let h = host("previews");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        let w = call(
            &h,
            &mut s,
            json!({"op": "widgets.get", "token": t, "lang": ["it"]}),
        );
        let pages = w["pages"].as_array().unwrap();
        assert_eq!(pages.len(), crate::ui::widgets::Kind::ALL.len());
        let page = |w: &Value, k: &str| -> Value {
            w["pages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["kind"] == k)
                .cloned()
                .unwrap()
        };
        assert_eq!(page(&w, "next")["label"], "Prossimi");
        // The host's own note, as the screen writes it.
        let notes = page(&w, "notes");
        assert_eq!(
            notes["model"]["windows"][0]["rows"][0]["text"],
            "Water the plants"
        );
        assert_eq!(notes["model"]["sample"], false);
        // Nothing chosen: no page; what to check shows an example.
        assert_eq!(page(&w, "quotes")["model"], Value::Null);
        assert_eq!(page(&w, "check")["model"]["sample"], true);
        // The weather (an example here): its big block and the next hours under it.
        let weather = &page(&w, "weather")["model"];
        assert_eq!(weather["layout"], "weather");
        // The hour is the sample's in the machine's own time zone (CI runs in UTC).
        let label = weather["hero"]["label"].as_str().unwrap();
        assert!(
            label.starts_with("Nuvoloso, pioggia dalle ") && label.ends_with(":00"),
            "{label}"
        );
        assert_eq!(weather["hero"]["range"], "Oggi tra 8° e 15°");
        assert_eq!(weather["windows"][0]["rows"].as_array().unwrap().len(), 4);
        // A quote added: its row waits for the data.
        let r = call(
            &h,
            &mut s,
            json!({"op": "search.start", "token": t, "what": "quote", "q": "Apple"}),
        );
        let id = r["id"].as_str().unwrap().to_string();
        let st = call(
            &h,
            &mut s,
            json!({"op": "search.status", "token": t, "id": id}),
        );
        assert_eq!(st["rows"], true);
        assert_eq!(
            st["hits"][0]["row"],
            json!({"right": "189.20 $", "curve": [0.0, 1.0]})
        );
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.add", "token": t, "search": id, "ref": "yahoo:AAPL"}),
        );
        let q = page(&r, "quotes");
        assert_eq!(
            q["model"]["windows"][0]["rows"][0],
            json!({"text": "Apple", "right": "…", "wait": true})
        );
        assert_eq!(q["status"], "1 chosen · Yahoo Finance, CoinGecko");
        // Removed, then put back where it was; only once.
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.remove", "token": t, "ref": "yahoo:AAPL"}),
        );
        assert_eq!(
            (r["undo"].clone(), r["lists"]["quotes"].clone()),
            (json!(true), json!([]))
        );
        let r = call(&h, &mut s, json!({"op": "undo", "token": t}));
        assert_eq!(r["lists"]["quotes"][0]["ref"], "yahoo:AAPL");
        assert_eq!(
            call(&h, &mut s, json!({"op": "undo", "token": t}))["error"],
            "invalid"
        );
        // Another device cannot put back what this one removed; nor after 15 s.
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.remove", "token": t, "ref": "yahoo:AAPL"}),
        );
        assert_eq!(r["undo"], true);
        let qr = s.ticket(at(0), &mut rng).unwrap().to_string();
        let other = call(
            &h,
            &mut s,
            json!({"op": "pair.qr", "ticket": qr, "peer": "192.0.2.30"}),
        )["token"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            call(&h, &mut s, json!({"op": "undo", "token": other}))["error"],
            "invalid"
        );
        let late = handle(
            &h,
            &mut s,
            &json!({"op": "undo", "token": t}),
            at(16),
            &mut rng,
        );
        assert_eq!(late["error"], "invalid");
        assert_eq!(late.get("quotes"), None);
        // The gallery: an example of every kind.
        let g = call(
            &h,
            &mut s,
            json!({"op": "widgets.samples", "token": t, "lang": ["en"]}),
        );
        let kinds: Vec<&str> = g["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds.len(), crate::ui::widgets::Kind::ALL.len());
        assert!(
            g["samples"]
                .as_array()
                .unwrap()
                .iter()
                .all(|x| x["model"]["sample"] == true)
        );
    }

    /// Several pages of a kind: another markets' page added (on, empty, at the end of the turn),
    /// its own list edited, moved in the turn, named, removed and put back with Undo; the
    /// first page untouched all along; at most MAX_SAME of a kind, and only of the kinds that
    /// repeat.
    #[test]
    fn several_pages_of_a_kind() {
        let h = host("several");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        let q = |h: &Host, s: &mut Sessions, v: Value| call(h, s, v);
        // Apple on the first page.
        let r = q(
            &h,
            &mut s,
            json!({"op": "search.start", "token": t, "what": "quote", "q": "Apple"}),
        );
        let id = r["id"].as_str().unwrap().to_string();
        let r = q(
            &h,
            &mut s,
            json!({"op": "quotes.add", "token": t, "search": id, "ref": "yahoo:AAPL"}),
        );
        assert_eq!(r["lists"]["quotes"][0]["ref"], "yahoo:AAPL");
        assert!(
            r["more"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["kind"] == "quotes")
        );
        for bad in ["next", "weather", "stocks", ""] {
            let r = q(
                &h,
                &mut s,
                json!({"op": "widgets.add", "token": t, "kind": bad}),
            );
            assert_eq!(r["error"], "invalid", "{bad}");
        }
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.add", "token": t, "kind": "quotes"}),
        );
        assert_eq!(r["added"], "quotes-2");
        assert_eq!(r["lists"]["quotes-2"], json!([]));
        let p2 = r["pages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "quotes-2")
            .unwrap()
            .clone();
        assert_eq!(
            (p2["kind"].as_str(), p2["n"].as_u64(), p2["on"].as_bool()),
            (Some("quotes"), Some(2), Some(true))
        );
        assert_eq!(p2["label"], "Markets 2");
        assert_eq!(
            h.model.settings.widgets().0.last().map(|p| p.key()),
            Some("quotes-2".into())
        );
        // Its own list: Apple there too (the feeds fetch it once), the first page unchanged.
        let r = q(
            &h,
            &mut s,
            json!({"op": "quotes.add", "token": t, "search": id, "ref": "yahoo:AAPL", "page": "quotes-2"}),
        );
        assert_eq!(r["lists"]["quotes-2"][0]["ref"], "yahoo:AAPL");
        assert_eq!(r["lists"]["quotes"].as_array().unwrap().len(), 1);
        assert_eq!(h.model.settings.quotes().union(|q| q.key()).len(), 1);
        // A page that is not one of the turn, or of another kind: invalid.
        for page in ["quotes-3", "cities", "quotes-9", "x"] {
            let r = q(
                &h,
                &mut s,
                json!({"op": "quotes.remove", "token": t, "ref": "yahoo:AAPL", "page": page}),
            );
            assert_eq!(r["error"], "invalid", "{page}");
        }
        // Its own place in the turn.
        let order: Vec<String> = r["order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_string())
            .collect();
        let mut moved = order.clone();
        let i = moved.iter().position(|x| x == "quotes-2").unwrap();
        let x = moved.remove(i);
        moved.insert(0, x);
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.order", "token": t, "order": moved}),
        );
        assert_eq!(r["order"][0]["id"], "quotes-2");
        // The old order (without it) is not every page any more.
        let without: Vec<&String> = order.iter().filter(|x| *x != "quotes-2").collect();
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.order", "token": t, "order": without}),
        );
        assert_eq!(r["error"], "invalid");
        // A name, cleaned; too long is refused; "" gives the usual one back.
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.name", "token": t, "page": "quotes-2", "name": " Crypto\n"}),
        );
        assert_eq!(r["order"][0]["label"], "Crypto");
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.name", "token": t, "page": "quotes-2", "name": "x".repeat(40)}),
        );
        assert_eq!(r["error"], "invalid");
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.name", "token": t, "page": "next", "name": "x"}),
        );
        assert_eq!(r["error"], "invalid");
        // Removed with its list and its name; Undo puts it back where it was.
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.remove", "token": t, "page": "quotes"}),
        );
        assert_eq!(r["error"], "invalid", "the first page is only turned off");
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.remove", "token": t, "page": "quotes-2"}),
        );
        assert_eq!(r["undo"], true);
        assert!(r["lists"].get("quotes-2").is_none());
        assert!(!h.model.settings.widget_order().iter().any(|p| p.n == 2));
        let file = std::fs::read_to_string(&h.model.settings.path).unwrap();
        assert!(!file.contains("quotes-2"), "{file}");
        let r = q(&h, &mut s, json!({"op": "undo", "token": t}));
        assert_eq!(r["order"][0]["id"], "quotes-2");
        assert_eq!(r["order"][0]["label"], "Crypto");
        assert_eq!(r["order"][0]["on"], true);
        assert_eq!(r["lists"]["quotes-2"][0]["ref"], "yahoo:AAPL");
        // At most four markets' pages.
        for n in 3..=4 {
            let r = q(
                &h,
                &mut s,
                json!({"op": "widgets.add", "token": t, "kind": "quotes"}),
            );
            assert_eq!(r["added"], format!("quotes-{n}"));
        }
        let r = q(
            &h,
            &mut s,
            json!({"op": "widgets.add", "token": t, "kind": "quotes"}),
        );
        assert_eq!(r["error"], "list_full");
        let r = q(&h, &mut s, json!({"op": "widgets.get", "token": t}));
        assert!(
            !r["more"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["kind"] == "quotes")
        );
        // The first page as it was.
        assert_eq!(h.model.settings.quotes().get(1).len(), 1);
    }

    /// The widget's ops: the order, a search, adding only one of its own hits, the limits.
    #[test]
    fn widgets_from_the_browser() {
        let h = host("widgets");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        let w = call(
            &h,
            &mut s,
            json!({"op": "widgets.get", "token": t, "lang": ["it"]}),
        );
        assert_eq!(
            w["order"][0],
            json!({"id": "next", "kind": "next", "on": true, "label": "Prossimi"})
        );
        assert_eq!(w["pages"][0]["kind"], "next");
        assert_eq!(w["order"][9]["on"], false);
        assert_eq!(w["lists"]["quotes"], json!([]));
        assert_eq!(w["limits"]["quotes"], 8);
        // The order: a permutation of every page, nothing else.
        let mut order: Vec<&str> = crate::ui::widgets::Kind::ALL
            .iter()
            .map(|k| k.key())
            .collect();
        order.reverse();
        let r = call(
            &h,
            &mut s,
            json!({"op": "widgets.order", "token": t, "order": order}),
        );
        assert_eq!(r["order"][0]["kind"], "custom");
        for bad in [
            json!(["next"]),
            json!(["next", "next"]),
            json!("next"),
            json!([
                "next", "quotes", "weather", "cities", "shopping", "notes", "sensors", "check",
                "playing", "stocks"
            ]),
        ] {
            let r = call(
                &h,
                &mut s,
                json!({"op": "widgets.order", "token": t, "order": bad}),
            );
            assert_eq!(r["error"], "invalid", "{bad}");
        }
        // A search, then its hit added: the name is the body's (cleaned), not the browser's.
        for bad in ["", "a\nb", "<b>", &"x".repeat(41)] {
            let r = call(
                &h,
                &mut s,
                json!({"op": "search.start", "token": t, "what": "quote", "q": bad}),
            );
            assert_eq!(r["error"], "invalid", "{bad}");
        }
        let r = call(
            &h,
            &mut s,
            json!({"op": "search.start", "token": t, "what": "stock", "q": "x"}),
        );
        assert_eq!(r["error"], "invalid");
        let r = call(
            &h,
            &mut s,
            json!({"op": "search.start", "token": t, "what": "quote", "q": "Apple"}),
        );
        let id = r["id"].as_str().unwrap().to_string();
        assert_eq!(id.len(), 32);
        let st = call(
            &h,
            &mut s,
            json!({"op": "search.status", "token": t, "id": id}),
        );
        assert_eq!(st["status"], "done");
        assert_eq!(st["hits"][0]["ref"], "yahoo:AAPL");
        // Not one of its hits, or another search: invalid.
        for (search, r#ref) in [(id.as_str(), "yahoo:MSFT"), ("feed", "yahoo:AAPL")] {
            let r = call(
                &h,
                &mut s,
                json!({"op": "quotes.add", "token": t, "search": search, "ref": r#ref}),
            );
            assert_eq!(r["error"], "invalid");
        }
        let r = call(
            &h,
            &mut s,
            json!({"op": "cities.add", "token": t, "search": id, "ref": "yahoo:AAPL"}),
        );
        assert_eq!(r["error"], "invalid", "a markets' search adds no city");
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.add", "token": t, "search": id, "ref": "yahoo:AAPL",
                                        "name": "<img src=x onerror=alert(1)>"}),
        );
        assert_eq!(
            r["lists"]["quotes"],
            json!([{"ref": "yahoo:AAPL", "name": "Apple", "detail": "AAPL"}])
        );
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.add", "token": t, "search": id, "ref": "yahoo:AAPL"}),
        );
        assert_eq!(r["error"], "exists");
        assert!(
            matches!(h.model.applied.borrow().last(), Some(model::Change::Quotes(1, q)) if q.len() == 1)
        );
        // Too soon after the last one; another device's search is not this one's.
        let r = call(
            &h,
            &mut s,
            json!({"op": "search.start", "token": t, "what": "city", "q": "Berlin"}),
        );
        assert_eq!(r["error"], "search_wait");
        let qr = s.ticket(at(0), &mut rng).unwrap().to_string();
        let other = call(
            &h,
            &mut s,
            json!({"op": "pair.qr", "ticket": qr, "peer": "192.0.2.31"}),
        )["token"]
            .as_str()
            .unwrap()
            .to_string();
        let st = call(
            &h,
            &mut s,
            json!({"op": "search.status", "token": other, "id": id}),
        );
        assert_eq!(st["error"], "invalid");
        // Moved and removed by ref.
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.move", "token": t, "ref": "yahoo:AAPL", "to": 3}),
        );
        assert_eq!(r["error"], "invalid");
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.remove", "token": t, "ref": "yahoo:AAPL"}),
        );
        assert_eq!(r["lists"]["quotes"], json!([]));
        let r = call(
            &h,
            &mut s,
            json!({"op": "quotes.remove", "token": t, "ref": "yahoo:AAPL"}),
        );
        assert_eq!(r["error"], "invalid");
        // The coins' currency is a setting of the panel.
        let r = call(
            &h,
            &mut s,
            json!({"op": "set", "token": t, "key": "widgets_currency", "value": "chf"}),
        );
        assert_eq!(r["values"]["widgets_currency"], "chf");
    }

    /// Home Assistant's entities: only through a search of the catalogue (never a lock, an
    /// alarm, a camera), the player set and cleared, the people from the catalogue only.
    #[test]
    fn home_assistant_entities_from_the_browser() {
        let h = host("ha-entities");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        let w = call(&h, &mut s, json!({"op": "widgets.get", "token": t}));
        assert_eq!(w["ha"], true);
        assert_eq!(w["people"]["all"].as_array().unwrap().len(), 2);
        // The page on the screen now (the panel's green dot), also alone.
        assert_eq!(w["showing"], "quotes");
        let only = call(&h, &mut s, json!({"op": "widgets.showing", "token": t}));
        assert_eq!(only, json!({"ok": true, "showing": "quotes"}));
        let a = w.to_string();
        for bad in [
            "lock.",
            "alarm_control_panel",
            "camera.",
            "device_tracker",
            "latitude",
        ] {
            assert!(!a.contains(bad), "{bad} in {a}");
        }
        let r = call(
            &h,
            &mut s,
            json!({"op": "search.start", "token": t, "what": "sensor", "q": "lock"}),
        );
        let st = call(
            &h,
            &mut s,
            json!({"op": "search.status", "token": t, "id": r["id"]}),
        );
        assert_eq!(st["hits"], json!([]));
        let mut at_s = 10;
        let mut search = |h: &Host, s: &mut Sessions, what: &str, q: &str| -> String {
            at_s += 3;
            let r = handle(
                h,
                s,
                &json!({"op": "search.start", "token": t, "what": what, "q": q}),
                at(at_s),
                &mut rng,
            );
            r["id"].as_str().unwrap().to_string()
        };
        let id = search(&h, &mut s, "sensor", "outdoor");
        let r = call(
            &h,
            &mut s,
            json!({"op": "sensors.add", "token": t, "search": id,
                                        "ref": "sensor.outdoor_temperature"}),
        );
        assert_eq!(
            r["lists"]["sensors"],
            json!([{"ref": "sensor.outdoor_temperature", "name": "Outdoor temperature", "detail": "°C"}])
        );
        let r = call(
            &h,
            &mut s,
            json!({"op": "sensors.add", "token": t, "search": id, "ref": "lock.front"}),
        );
        assert_eq!(r["error"], "invalid");
        let id = search(&h, &mut s, "player", "speaker");
        let r = call(
            &h,
            &mut s,
            json!({"op": "player.set", "token": t, "search": id, "ref": "media_player.speaker"}),
        );
        assert_eq!(r["player"]["name"], "Speaker");
        let r = call(
            &h,
            &mut s,
            json!({"op": "player.set", "token": t, "ref": ""}),
        );
        assert_eq!(r["player"], Value::Null);
        let r = call(
            &h,
            &mut s,
            json!({"op": "people.set", "token": t, "refs": ["person.bob"]}),
        );
        assert_eq!(r["people"]["chosen"], json!(["person.bob"]));
        for bad in [
            json!(["device_tracker.phone_one"]),
            json!(["person.zed"]),
            json!("person.bob"),
        ] {
            let r = call(
                &h,
                &mut s,
                json!({"op": "people.set", "token": t, "refs": bad}),
            );
            assert_eq!(r["error"], "invalid", "{bad}");
        }
        let r = call(
            &h,
            &mut s,
            json!({"op": "sensors.remove", "token": t, "ref": "sensor.outdoor_temperature"}),
        );
        assert_eq!(r["lists"]["sensors"], json!([]));
        let r = call(
            &h,
            &mut s,
            json!({"op": "set", "token": t, "key": "check_openings", "value": false}),
        );
        assert_eq!(r["values"]["check_openings"], false);
        assert!(!h.model.settings.widget_ha().openings);
    }

    #[test]
    fn notes_from_the_browser() {
        let h = host("notes");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        let r = call(&h, &mut s, json!({"op": "notes.list", "token": t}));
        assert_eq!(r["notes"], json!([{"id": 1, "text": "Water the plants"}]));
        assert_eq!(
            call(
                &h,
                &mut s,
                json!({"op": "notes.add", "token": t, "text": " Pizza\ntonight "})
            )["ok"],
            true
        );
        assert_eq!(
            call(
                &h,
                &mut s,
                json!({"op": "notes.edit", "token": t, "id": 1, "text": "Water the basil"})
            )["ok"],
            true
        );
        assert_eq!(
            call(
                &h,
                &mut s,
                json!({"op": "notes.edit", "token": t, "id": 9, "text": "x"})
            )["error"],
            "invalid"
        );
        assert_eq!(
            call(
                &h,
                &mut s,
                json!({"op": "notes.remove", "token": t, "id": 9})
            )["error"],
            "invalid"
        );
        assert_eq!(
            call(
                &h,
                &mut s,
                json!({"op": "notes.remove", "token": t, "id": 1})
            )["ok"],
            true
        );
        assert_eq!(
            call(
                &h,
                &mut s,
                json!({"op": "notes.add", "token": t, "text": "x".repeat(281)})
            )["error"],
            "invalid"
        );
        assert_eq!(
            *h.calls.borrow(),
            [
                "note add Pizza tonight",
                "note edit 1 Water the basil",
                "note remove 1"
            ]
        );
        *h.notes.borrow_mut() = Some((0..20).map(|i| (i, format!("n{i}"))).collect());
        assert_eq!(
            call(
                &h,
                &mut s,
                json!({"op": "notes.add", "token": t, "text": "one more"})
            )["error"],
            "list_full"
        );
        *h.notes.borrow_mut() = None;
        assert_eq!(
            call(&h, &mut s, json!({"op": "notes.list", "token": t}))["error"],
            "no_brain"
        );
    }

    #[test]
    fn languages() {
        let have = vec!["de".to_string(), "en".into(), "it".into()];
        assert_eq!(pick_language(&["it-IT".into(), "en".into()], &have), "it");
        assert_eq!(pick_language(&["fr".into(), "de-CH".into()], &have), "de");
        assert_eq!(pick_language(&["fr".into()], &have), "en");
        let h = host("strings");
        let mut s = Sessions::default();
        let r = call(&h, &mut s, json!({"op": "strings", "lang": ["it"]}));
        for k in STRING_KEYS {
            let v = r["strings"][k].as_str().unwrap();
            assert!(
                !v.is_empty() && v != *k,
                "{k} missing in it: {v:?} {}",
                r["lang"]
            );
        }
    }

    /// The panel builds `panel_empty_<kind>` and `panel_what_<kind>` from the widget's kind
    /// (panel.js): every kind has both, sent to the panel and written in en and it.
    #[test]
    fn every_widget_kind_has_its_panel_words() {
        let repo = locale::repo_locales();
        for code in ["en", "it"] {
            let l = locale::load(&repo, code).unwrap();
            for kind in crate::ui::widgets::Kind::ALL {
                for k in [
                    format!("panel_empty_{}", kind.key()),
                    format!("panel_what_{}", kind.key()),
                ] {
                    assert!(STRING_KEYS.contains(&k.as_str()), "{k} not sent");
                    let v = l.text(&k);
                    assert!(!v.is_empty() && v != k, "{k} missing in {code}");
                }
            }
        }
    }

    /// The canary: secrets of every kind put where the body keeps them; no answer to any op,
    /// with or without a device let in, ever contains one.
    #[test]
    fn no_answer_ever_holds_a_secret() {
        let h = host("canary");
        let dir = h.model.dir.clone();
        std::fs::write(
            dir.join("ha-auth.json"),
            r#"{"access_token":"CANARY-ACCESS","refresh_token":"CANARY-REFRESH","client_id":"http://CANARY-URL/"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("settings.toml"),
            "ha_url = \"http://CANARY-HA.example:8123\"\n[assistant]\nai = \"CANARY-AI\"\ntts = \"CANARY-TTS\"\n",
        )
        .unwrap();
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        // Another device's token must not show to this one either.
        let qr = s.ticket(at(0), &mut rng).unwrap().to_string();
        let other = call(
            &h,
            &mut s,
            json!({"op": "pair.qr", "ticket": qr, "peer": "192.0.2.30"}),
        )["token"]
            .as_str()
            .unwrap()
            .to_string();
        let mut secrets: Vec<String> = vec!["CANARY".into(), other.clone()];
        secrets.extend(s.list(at(0)).iter().map(|x| x.hash.clone()));
        let ops = ["hello", "strings", "pair.request", "pair.status", "pair.qr"]
            .iter()
            .chain(SESSION_OPS.iter())
            .chain(["nonsense"].iter());
        for op in ops {
            for tok in ["", t.as_str()] {
                for extra in [
                    json!({}),
                    json!({"key": "home_compact", "value": false}),
                    json!({"key": "ha_url", "value": "x"}),
                    json!({"list": "todo.shopping", "uid": "u1", "text": "Tea"}),
                    json!({"id": "", "ticket": ""}),
                    json!({"what": "quote", "q": "Apple", "order": ["next"], "ref": "yahoo:AAPL",
                           "search": "", "to": 0}),
                ] {
                    let mut req = extra.clone();
                    req["op"] = json!(op);
                    req["token"] = json!(tok);
                    req["peer"] = json!("192.0.2.40");
                    let a = call(&h, &mut s, req).to_string();
                    for secret in &secrets {
                        assert!(!a.contains(secret.as_str()), "{op}: {a}");
                    }
                }
            }
        }
        // The other device is still in, and this one too (session.end was last, for "").
        assert!(s.check(&other, at(0)).is_some());
    }

    #[test]
    fn the_remote_screen_only_when_switched_on_and_for_one_device() {
        let h = host("remote");
        let mut s = Sessions::default();
        let a = pair(&h, &mut s);
        s.skip_pause();
        let b = pair(&h, &mut s);
        let ask = |s: &mut Sessions, t: &str, op: &str, extra: Value| {
            let mut r = json!({"op": op, "token": t, "peer": "192.0.2.24"});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            call(&h, s, r)
        };
        // Off on the device: nothing.
        assert_eq!(
            ask(&mut s, &a, "screen.start", json!({}))["error"],
            "screen_off"
        );
        assert_eq!(
            ask(&mut s, &a, "screen.frame", json!({}))["error"],
            "screen_ended"
        );
        h.remote_on.set(true);
        assert_eq!(ask(&mut s, &a, "screen.start", json!({}))["ok"], true);
        assert_eq!(
            h.signs.borrow().last().unwrap().as_deref(),
            Some("Chrome macOS")
        );
        // One at a time.
        assert_eq!(
            ask(&mut s, &b, "screen.start", json!({}))["error"],
            "screen_busy"
        );
        assert_eq!(
            ask(&mut s, &b, "screen.frame", json!({}))["error"],
            "screen_ended"
        );
        // The other device is told who is in control (as the sign says it), never where from.
        let st = ask(&mut s, &b, "screen.status", json!({}));
        assert_eq!(
            st,
            json!({"ok": true, "enabled": true, "confirm": false, "active": true, "mine": false,
                   "waiting": false, "by": {"browser": "Chrome", "os": "macOS"}})
        );
        assert!(!st.to_string().contains("192.0.2"));
        let st = ask(&mut s, &a, "screen.status", json!({}));
        assert_eq!(
            (st["mine"].clone(), st["by"].clone()),
            (json!(true), Value::Null)
        );
        // The frame, then nothing new for the same seq.
        let f = ask(&mut s, &a, "screen.frame", json!({"after": 0}));
        assert_eq!(f["seq"], 3);
        assert_eq!(f["jpeg"], "anBlZw==");
        assert!(f.get("token").is_none());
        assert_eq!(
            ask(&mut s, &a, "screen.frame", json!({"after": 3}))["same"],
            true
        );
        // Touches: checked whole, then passed on.
        let ev =
            json!({"events": [{"k": "d", "x": 0.5, "y": 0.5}, {"k": "u", "x": 0.5, "y": 0.5}]});
        assert_eq!(ask(&mut s, &a, "screen.input", ev.clone())["ok"], true);
        assert_eq!(h.touches.borrow().len(), 2);
        let bad = json!({"events": [{"k": "d", "x": 2, "y": 0.5}]});
        assert_eq!(ask(&mut s, &a, "screen.input", bad)["error"], "invalid");
        assert_eq!(
            ask(&mut s, &b, "screen.input", ev.clone())["error"],
            "screen_ended"
        );
        assert_eq!(h.touches.borrow().len(), 2);
        // A private page: no picture, no touch.
        h.hidden.set(Some("camera"));
        assert_eq!(
            ask(&mut s, &a, "screen.frame", json!({}))["hidden"],
            "camera"
        );
        assert_eq!(
            ask(&mut s, &a, "screen.input", ev.clone())["error"],
            "screen_hidden"
        );
        assert_eq!(h.touches.borrow().len(), 2);
        h.hidden.set(None);
        // Revoked: the session ends with it.
        let a_id = {
            let me = s.check(&a, at(0)).unwrap().hash.clone();
            s.list(at(0))
                .iter()
                .find(|x| x.hash == me)
                .unwrap()
                .id()
                .to_string()
        };
        assert_eq!(
            ask(&mut s, &b, "sessions.revoke", json!({"id": a_id}))["ok"],
            true
        );
        assert!(!s.remote.active());
        assert_eq!(h.signs.borrow().last().unwrap(), &None);
        // Now the other may; Stop in the browser ends it.
        assert_eq!(ask(&mut s, &b, "screen.start", json!({}))["ok"], true);
        assert_eq!(ask(&mut s, &b, "screen.stop", json!({}))["ok"], true);
        assert!(!s.remote.active());
        // Stop on the screen: nobody for a while.
        assert_eq!(ask(&mut s, &b, "screen.start", json!({}))["ok"], true);
        remote_stop(&h, &mut s, End::Screen, at(0));
        assert_eq!(
            ask(&mut s, &b, "screen.start", json!({}))["error"],
            "screen_stopped"
        );
    }

    /// «Ask on the screen first»: a start only asks the room; nothing is sent nor touched until
    /// Allow; Deny and silence are told to the browser.
    #[test]
    fn the_remote_screen_asks_the_room_first() {
        let h = host("remote-confirm");
        let mut s = Sessions::default();
        let a = pair(&h, &mut s);
        s.skip_pause();
        let b = pair(&h, &mut s);
        h.remote_on.set(true);
        h.remote_confirm.set(true);
        let ask = |s: &mut Sessions, t: &str, op: &str, extra: Value| {
            let mut r = json!({"op": op, "token": t, "peer": "192.0.2.24"});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            call(&h, s, r)
        };
        // The screen cannot ask now (a conversation, a dialog...): refused, nothing kept.
        h.remote_can_ask.set(false);
        assert_eq!(
            ask(&mut s, &a, "screen.start", json!({}))["error"],
            "screen_cannot_ask"
        );
        assert!(!s.remote.active());
        h.remote_can_ask.set(true);
        let st = ask(&mut s, &a, "screen.start", json!({}));
        assert_eq!(
            (st["ok"].clone(), st["waiting"].clone()),
            (json!(true), json!(true))
        );
        assert_eq!(*h.remote_asks.borrow(), ["Chrome macOS"]);
        assert!(
            h.signs.borrow().is_empty(),
            "no sign, no capture before Allow"
        );
        // Waiting: no picture, no touch, no home; asking again does not ask twice.
        assert_eq!(ask(&mut s, &a, "screen.frame", json!({}))["waiting"], true);
        let ev = json!({"events": [{"k": "d", "x": 0.5, "y": 0.5}]});
        assert_eq!(
            ask(&mut s, &a, "screen.input", ev.clone())["error"],
            "screen_waiting"
        );
        assert_eq!(
            ask(&mut s, &a, "screen.home", json!({}))["error"],
            "screen_waiting"
        );
        assert!(h.touches.borrow().is_empty());
        assert_eq!(ask(&mut s, &a, "screen.start", json!({}))["waiting"], true);
        assert_eq!(h.remote_asks.borrow().len(), 1);
        assert_eq!(ask(&mut s, &a, "screen.status", json!({}))["waiting"], true);
        // Nobody else meanwhile.
        assert_eq!(
            ask(&mut s, &b, "screen.start", json!({}))["error"],
            "screen_busy"
        );
        // Allow: the sign, the frames, the touches.
        remote_answer(&h, &mut s, true, at(0));
        assert_eq!(
            h.signs.borrow().last().unwrap().as_deref(),
            Some("Chrome macOS")
        );
        assert_eq!(ask(&mut s, &a, "screen.frame", json!({}))["seq"], 3);
        assert_eq!(ask(&mut s, &a, "screen.input", ev.clone())["ok"], true);
        assert_eq!(ask(&mut s, &a, "screen.stop", json!({}))["ok"], true);
        // Cancel in the browser while the screen asks: the question goes, and it counts as
        // unanswered (a loop of start and stop cannot keep asking the room).
        ask(&mut s, &a, "screen.start", json!({}));
        assert!(s.remote.asking().is_some());
        let asks = h.remote_asks.borrow().len();
        assert_eq!(ask(&mut s, &a, "screen.stop", json!({}))["ok"], true);
        assert!(!s.remote.active());
        assert_eq!(h.signs.borrow().last().unwrap(), &None);
        assert_eq!(
            ask(&mut s, &a, "screen.start", json!({}))["error"],
            "screen_unanswered"
        );
        assert_eq!(h.remote_asks.borrow().len(), asks, "no new question");
        // Deny: the browser is told; that device stays out.
        ask(&mut s, &b, "screen.start", json!({}));
        remote_answer(&h, &mut s, false, at(0));
        assert!(!s.remote.active());
        assert_eq!(
            ask(&mut s, &b, "screen.frame", json!({}))["error"],
            "screen_denied"
        );
        assert_eq!(
            ask(&mut s, &b, "screen.start", json!({}))["error"],
            "screen_denied"
        );
        // An answer with nothing asked does nothing.
        remote_answer(&h, &mut s, true, at(0));
        assert!(!s.remote.active());
    }

    /// No access dialog while a browser controls the screen (it could answer it).
    #[test]
    fn no_access_dialog_during_a_remote_session() {
        let h = host("remote-ask");
        let mut s = Sessions::default();
        let a = pair(&h, &mut s);
        h.remote_on.set(true);
        let start = json!({"op": "screen.start", "token": a, "peer": "192.0.2.24"});
        assert_eq!(call(&h, &mut s, start)["ok"], true);
        s.skip_pause();
        let asked = h.asked.borrow().len();
        let r = call(
            &h,
            &mut s,
            json!({"op": "pair.request", "peer": "192.0.2.30"}),
        );
        assert_eq!(r["error"], "cannot_ask");
        assert_eq!(h.asked.borrow().len(), asked, "no dialog");
        call(&h, &mut s, json!({"op": "screen.stop", "token": a}));
        s.skip_pause();
        let r = call(
            &h,
            &mut s,
            json!({"op": "pair.request", "peer": "192.0.2.30"}),
        );
        assert_eq!(r["ok"], true);
    }

    /// The board from the browser: pages and widgets added, a tile placed, what the body never
    /// keeps (a lock, a gate, a never-domain service) dropped with its reason, the preview as
    /// the screen draws it, the home's turn following the custom widgets, undo once.
    #[test]
    fn the_board_from_the_browser() {
        let h = host("board");
        let mut s = Sessions::default();
        let t = pair(&h, &mut s);
        let mut ask = |op: &str, extra: Value| {
            let mut r = json!({"op": op, "token": t, "lang": ["en"]});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            call(&h, &mut s, r)
        };
        let g = ask("board.get", json!({}));
        assert_eq!(g["canvases"], json!([]));
        assert_eq!(g["grids"]["page"], json!([6, 4]));
        let a = ask("board.page.add", json!({"name": "Living room"}));
        assert_eq!(a["added"], "p1");
        assert_eq!(a["canvases"][0]["label"], "Living room");
        // A tile of each: a shutter (asks first by default), a lock and a gate (never), a
        // light 3x1 (no size), an Advanced service of a never-domain.
        let tiles = json!([
            {"x": 0, "y": 0, "w": 1, "h": 1, "kind": "button", "entity_id": "cover.living_room"},
            {"x": 1, "y": 0, "w": 1, "h": 1, "kind": "button", "entity_id": "lock.front"},
            {"x": 2, "y": 0, "w": 1, "h": 1, "kind": "button", "entity_id": "cover.garage"},
            {"x": 0, "y": 1, "w": 3, "h": 1, "kind": "button", "entity_id": "light.lamp"},
            {"x": 0, "y": 2, "w": 2, "h": 1, "kind": "level", "entity_id": "light.lamp", "icon": "ceiling-light"},
            {"x": 3, "y": 3, "w": 1, "h": 1, "kind": "button", "entity_id": "scene.movie_night",
             "service": "lock.unlock", "service_data": {"entity_id": "lock.front"}},
        ]);
        let r = ask("board.set", json!({"canvas": {"id": "p1", "tiles": tiles}}));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["undo"], true);
        let dropped = r["dropped"].as_array().unwrap();
        assert_eq!(dropped.len(), 4, "{dropped:?}");
        let kept = &r["canvases"][0]["tiles"];
        assert_eq!(kept.as_array().unwrap().len(), 2);
        assert_eq!(kept[0]["tile"]["entity_id"], "cover.living_room");
        assert_eq!(kept[0]["confirm_default"], true);
        assert_eq!(kept[0]["draw"]["confirm"], true);
        assert_eq!(
            kept[0]["allowed"],
            json!({"button": [[1, 1], [2, 1], [2, 2], [4, 1]],
                                              "level": [[2, 1], [4, 1]]})
        );
        assert_eq!(kept[1]["draw"]["icon"], "ceiling-light");
        assert!(kept[1]["draw"]["path"].as_str().unwrap().starts_with('M'));
        assert_eq!(kept[1]["draw"]["state"], "unavailable", "no state yet");
        // Out of a canvas's limits, an unknown canvas, a tile that is no tile: refused whole.
        let many: Vec<Value> = (0..25)
            .map(|i| {
                json!({"x": i % 6, "y": i / 6, "w": 1, "h": 1,
            "kind": "button", "entity_id": format!("light.l{i}")})
            })
            .collect();
        assert_eq!(
            ask("board.set", json!({"canvas": {"id": "p1", "tiles": many}}))["error"],
            "list_full"
        );
        assert_eq!(
            ask("board.set", json!({"canvas": {"id": "p9", "tiles": []}}))["error"],
            "invalid"
        );
        assert_eq!(
            ask(
                "board.set",
                json!({"canvas": {"id": "p1", "tiles": [{"x": 0}]}})
            )["error"],
            "invalid"
        );
        // Undo: as before the last change; once.
        let u = ask("board.undo", json!({}));
        assert_eq!(u["canvases"][0]["tiles"], json!([]));
        assert_eq!(ask("board.undo", json!({}))["error"], "invalid");
        // Pages: a second one, renamed, reordered, removed.
        assert_eq!(ask("board.page.add", json!({}))["added"], "p2");
        ask("board.page.name", json!({"canvas": "p2", "name": "Night"}));
        let o = ask("board.page.order", json!({"order": ["p2", "p1"]}));
        assert_eq!(o["canvases"][0]["name"], "Night");
        assert_eq!(
            ask("board.page.order", json!({"order": ["p2"]}))["error"],
            "invalid"
        );
        assert_eq!(
            ask(
                "board.page.name",
                json!({"canvas": "p2", "name": "x".repeat(60)})
            )["error"],
            "invalid"
        );
        let r = ask("board.page.remove", json!({"canvas": "p2"}));
        assert_eq!(r["canvases"].as_array().unwrap().len(), 1);
        assert_eq!(
            ask("board.widget.remove", json!({"canvas": "p1"}))["error"],
            "invalid"
        );
        // A custom widget: a page of the home's turn, on, named with it.
        let custom2 = crate::ui::widgets::Page::new(crate::ui::widgets::Kind::Custom, 2);
        let w = ask("board.widget.add", json!({"name": "Lights"}));
        assert_eq!(w["added"], "w1");
        let w = ask("board.widget.add", json!({}));
        assert_eq!(w["added"], "w2");
        assert_eq!(w["canvases"][2]["page"], "custom-2");
        assert_eq!(w["canvases"][2]["on"], true);
        assert!(h.widget_pages().contains(&(custom2, true)));
        ask(
            "board.widget.name",
            json!({"canvas": "w2", "name": "Climate"}),
        );
        assert!(
            h.model
                .settings
                .widget_names()
                .contains(&(custom2, "Climate".into()))
        );
        ask("board.widget.remove", json!({"canvas": "w2"}));
        assert!(!h.widget_pages().iter().any(|(p, _)| *p == custom2));
        // Undo brings it back, with its page.
        ask("board.undo", json!({}));
        assert!(h.widget_pages().contains(&(custom2, true)));
        for _ in 0..2 {
            ask("board.widget.add", json!({}));
        }
        assert_eq!(ask("board.widget.add", json!({}))["error"], "list_full");
        // The search offers what a tile may be, never a lock or a gate; typed words count.
        let all = ask("board.search", json!({"q": ""}));
        let ids: Vec<&str> = all["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["entity_id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&"light.lamp") && ids.contains(&"cover.living_room"),
            "{ids:?}"
        );
        for never in [
            "lock.front",
            "cover.garage",
            "switch.relay_2",
            "alarm_control_panel.house",
            "camera.garden",
        ] {
            assert!(!ids.contains(&never), "{never}");
        }
        let shutter = ask("board.search", json!({"q": "shutter"}));
        let hit = &shutter["hits"][0];
        assert_eq!(
            (
                hit["entity_id"].as_str(),
                hit["kind"].as_str(),
                hit["confirm"].as_bool()
            ),
            (Some("cover.living_room"), Some("button"), Some(true))
        );
        assert_eq!(hit["icon"], "window-shutter");
        assert_eq!(
            ask("board.search", json!({"q": "lamp"}))["error"],
            "search_wait"
        );
        assert_eq!(ask("board.search", json!({"q": "<x>"}))["error"], "invalid");
        // The icons: the built-in ones without the bundle's file.
        let i = ask("board.icons", json!({"q": "lightbulb"}));
        assert_eq!(i["all"], false);
        assert_eq!(i["icons"][0]["name"], "lightbulb");
        assert_eq!(
            ask("board.icons", json!({"q": "lamp"}))["error"],
            "search_wait"
        );
        assert_eq!(
            ask("board.icons", json!({"q": "<svg>"}))["error"],
            "invalid"
        );
    }
}
