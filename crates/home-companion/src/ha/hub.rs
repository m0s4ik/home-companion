// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What Home Assistant pushes to the screen besides the weather, on one WebSocket connection of
//! its own thread (it reconnects with a growing pause and never touches the UI thread):
//!
//! - **cards**: the events `home_companion_card` and `home_companion_card_dismiss`
//!   ([`super::events`]), subscribed with `subscribe_events`;
//! - **the shopping lists**: one `todo/item/subscribe` per chosen `todo.*` entity (a change from
//!   the phone shows at once), and a tick on the screen is `todo.update_item`;
//! - **which lists and calendars exist**, for the page where the user chooses (a
//!   `render_template` subscription over the `todo` and `calendar` domains: a few hundred bytes
//!   even on a big installation, and it follows entities that come and go).
//!
//! - **the board's tiles** (ui/board.rs): their entities join the widget's one subscription,
//!   their states go to the screen within [`TILES_EVERY`], and a tap is a `call_service` from a
//!   closed list ([`check_call`]), answered with [`HubMsg::Called`]. A call made while the link
//!   is down is answered at once as failed, never sent later.
//!
//! Which lists are subscribed: the user's choice (Settings: `lists`), else every `todo.*`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::auth;
use super::events::{self, CardEvent};
use super::todo::{self, Item};
use super::ws::{Message, WebSocket};
use super::{Backoff, Endpoint, endpoint};

/// (entity id, friendly name).
pub type Named = (String, String);

/// The `todo.*` and `calendar.*` entities Home Assistant has.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entities {
    pub todo: Vec<Named>,
    pub calendar: Vec<Named>,
}

/// What the screen asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum HubCmd {
    /// Bought: the item of the list is completed in Home Assistant.
    Done { list: String, uid: String },
    /// A new item (the browser panel): `todo.add_item`.
    Add { list: String, text: String },
    /// An item taken away (the browser panel): `todo.remove_item`.
    Remove { list: String, uid: String },
    /// The user's choice of lists (empty = all of them).
    SetLists(Vec<String>),
    /// An event for Home Assistant's bus (`fire_event`, needs an admin user); `device` is added.
    Fire { event_type: String, data: Value },
    /// What the home's widget wants of Home Assistant (widgets.rs).
    SetWidgets(super::widgets::Want),
    /// The catalogue again (the browser panel opened its widget section): every five minutes
    /// at most.
    Catalog,
    /// The screen is off or it is night: no sensor's curve is fetched meanwhile.
    Quiet(bool),
    /// A tile's call (`call_service` on one entity), answered with [`HubMsg::Called`] and the
    /// same `id`: only an entity of the board's tiles, only a service of the closed list or the
    /// tile's own "Advanced" one ([`check_call`]).
    Call {
        id: u64,
        domain: String,
        service: String,
        entity_id: String,
        data: Value,
    },
}

/// What the screen gets.
#[derive(Debug, Clone, PartialEq)]
pub enum HubMsg {
    Card(CardEvent),
    /// Home Assistant asks the screen to wake (true) or sleep (false).
    Screen(bool),
    /// The lists and calendars that exist (the choosing page).
    Entities(Entities),
    /// The lists shown now, in order, with their names (a list left out disappears).
    Lists(Vec<Named>),
    /// The open items of a list.
    Items {
        list: String,
        items: Vec<Item>,
    },
    /// The connection to Home Assistant is up (true) or lost (false); `last_seen` is when the
    /// last message came (seconds since the epoch, 0 = never), for "last synced at".
    Link {
        up: bool,
        last_seen: i64,
    },
    /// The entities the widget may show (the browser panel searches them).
    Catalog(Vec<super::widgets::Entity>),
    /// The widget's data from Home Assistant, when it changed.
    Widgets(super::widgets::WidgetsData),
    /// A sensor's last 24 hours (None: none to draw).
    Curve(String, Option<super::widgets::Curve>),
    /// The states of the board's tiles' entities, when they changed.
    Tiles(super::widgets::States),
    /// The answer to [`HubCmd::Call`]: done, or why not.
    Called {
        id: u64,
        ok: bool,
        error: Option<String>,
    },
}

/// What the hub knows of its link across connections: when the last message came (seconds since
/// the epoch, 0 = never) and whether the screen was told it is up.
#[derive(Debug, Default)]
struct LinkState {
    seen: i64,
    told_up: bool,
}

/// Seconds since the epoch, now.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub struct HubConfig {
    pub url: String,
    pub auth: Arc<auth::Auth>,
    /// The character's name: cards addressed to another device are not ours.
    pub device: String,
    /// The lists to show at first (empty = all).
    pub lists: Vec<String>,
}

/// The thread's ends.
pub type HubLink = (Receiver<HubMsg>, Sender<HubCmd>, JoinHandle<()>);

pub fn spawn(cfg: HubConfig, stop: fn() -> bool) -> io::Result<HubLink> {
    let (tx, rx) = channel();
    let (cmd_tx, cmd_rx) = channel();
    let h = std::thread::Builder::new()
        .name("ha-hub".into())
        .spawn(move || run(cfg, tx, cmd_rx, stop))?;
    Ok((rx, cmd_tx, h))
}

// ---- pure parts ------------------------------------------------------------------------------

/// The template of the entities subscription: a JSON list of {id, name}.
pub const ENTITIES_TEMPLATE: &str = "{% set ns = namespace(r=[]) %}{% for s in (states.todo | list) + (states.calendar | list) %}{% set ns.r = ns.r + [{'id': s.entity_id, 'name': s.name}] %}{% endfor %}{{ ns.r | tojson }}";

/// The two `subscribe_events` messages for the card events, ids `first` and `first + 1`.
pub fn card_subscriptions(first: u64) -> [String; 2] {
    [events::CARD_EVENT, events::DISMISS_EVENT]
        .into_iter()
        .zip(first..)
        .map(|(t, id)| json!({"id": id, "type": "subscribe_events", "event_type": t}).to_string())
        .collect::<Vec<_>>()
        .try_into()
        .expect("two event types")
}

/// The `subscribe_events` message for the screen event.
pub fn screen_subscription(id: u64) -> String {
    json!({"id": id, "type": "subscribe_events", "event_type": events::SCREEN_EVENT}).to_string()
}

/// The `fire_event` message: `data` with this device's name as `device`.
pub fn fire_message(id: u64, event_type: &str, device: &str, data: &Value) -> Value {
    let mut d = data.clone();
    d["device"] = device.into();
    json!({"id": id, "type": "fire_event", "event_type": event_type, "event_data": d})
}

/// The entities of an `event` message of the entities subscription (`{"event": {"result": …}}`,
/// the result being the template's JSON text or, when Home Assistant parsed it, the list).
pub fn parse_entities(msg: &Value) -> Option<Entities> {
    let r = msg.pointer("/event/result")?;
    let parsed;
    let list = match r {
        Value::String(s) => {
            parsed = serde_json::from_str::<Value>(s).ok()?;
            &parsed
        }
        v => v,
    };
    let mut out = Entities::default();
    for e in list.as_array()? {
        let id = e.get("id")?.as_str()?;
        let name = e
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(id);
        let row = (id.to_string(), name.to_string());
        if id.starts_with("todo.") {
            out.todo.push(row);
        } else if id.starts_with("calendar.") {
            out.calendar.push(row);
        }
    }
    out.todo.sort_by_key(|(_, n)| n.to_lowercase());
    out.calendar.sort_by_key(|(_, n)| n.to_lowercase());
    Some(out)
}

/// The lists to show: the chosen ones that exist (in the user's order), or every `todo.*` when
/// nothing is chosen. Before Home Assistant told what exists (`known` is None) the chosen ones
/// are trusted under their ids.
pub fn active_lists(chosen: &[String], known: Option<&Entities>) -> Vec<Named> {
    match known {
        None => chosen.iter().map(|c| (c.clone(), c.clone())).collect(),
        Some(k) if chosen.is_empty() => k.todo.clone(),
        Some(k) => chosen
            .iter()
            .filter_map(|c| k.todo.iter().find(|(id, _)| id == c).cloned())
            .collect(),
    }
}

/// The `todo/item/subscribe` subscriptions: message id → list.
#[derive(Debug, Default)]
pub struct Subs {
    by_id: BTreeMap<u64, String>,
}

impl Subs {
    pub fn list_of(&self, id: u64) -> Option<&str> {
        self.by_id.get(&id).map(String::as_str)
    }

    /// Makes the subscriptions the wanted ones: the messages to send (new subscriptions,
    /// `unsubscribe_events` for the ones no longer wanted), with ids from `next_id`.
    pub fn reconcile(&mut self, wanted: &[Named], mut next_id: impl FnMut() -> u64) -> Vec<Value> {
        let mut out = Vec::new();
        let gone: Vec<u64> = self
            .by_id
            .iter()
            .filter(|(_, l)| !wanted.iter().any(|(w, _)| w == *l))
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.by_id.remove(&id);
            out.push(json!({"id": next_id(), "type": "unsubscribe_events", "subscription": id}));
        }
        for (list, _) in wanted {
            if !self.by_id.values().any(|l| l == list) {
                let id = next_id();
                self.by_id.insert(id, list.clone());
                out.push(json!({"id": id, "type": "todo/item/subscribe", "entity_id": list}));
            }
        }
        out
    }
}

/// `todo.update_item` completing an item.
pub fn tick_message(id: u64, list: &str, uid: &str) -> Value {
    json!({"id": id, "type": "call_service", "domain": "todo", "service": "update_item",
           "service_data": {"item": uid, "status": "completed"}, "target": {"entity_id": list}})
}

/// `todo.add_item`: a new item on a list.
pub fn add_message(id: u64, list: &str, text: &str) -> Value {
    json!({"id": id, "type": "call_service", "domain": "todo", "service": "add_item",
           "service_data": {"item": text}, "target": {"entity_id": list}})
}

/// `todo.remove_item`: an item gone from a list (by its uid).
pub fn remove_message(id: u64, list: &str, uid: &str) -> Value {
    json!({"id": id, "type": "call_service", "domain": "todo", "service": "remove_item",
           "service_data": {"item": uid}, "target": {"entity_id": list}})
}

/// A tile's call may go: an entity of the board's tiles, of a domain a tile may control, that
/// the gates allow (with the device class and name Home Assistant gave it), and a service of
/// the closed list ([`crate::ui::board::allowed`]) or the tile's "Advanced" one; `data` an
/// object. Err: why not.
pub fn check_call(
    want: &super::widgets::Want,
    states: &super::widgets::States,
    domain: &str,
    service: &str,
    entity_id: &str,
    data: &Value,
) -> Result<(), String> {
    use crate::ui::board::TileKind;
    if !want.tiles.iter().any(|t| t == entity_id) {
        return Err(format!("{entity_id} is not on the board"));
    }
    let controllable: Vec<&str> = TileKind::Button
        .domains()
        .iter()
        .chain(TileKind::Level.domains())
        .copied()
        .collect();
    if !crate::settings::valid_entity(entity_id, &controllable) {
        return Err(format!("{entity_id} is not controlled from a tile"));
    }
    let gates = safety::SafetyCfg::default();
    let st = states.by_id.get(entity_id);
    let (class, name) = st.map_or(("", ""), |s| (s.device_class.as_str(), s.name.as_str()));
    if gates.entity_gate(entity_id, class, name) == safety::Gate::Never
        || gates.never_domains.iter().any(|d| d == domain)
    {
        return Err(format!("{entity_id} is never controlled"));
    }
    let named = format!("{domain}.{service}");
    if !crate::ui::board::allowed(domain, service)
        && !want
            .advanced
            .iter()
            .any(|(e, s)| e == entity_id && *s == named)
    {
        return Err(format!("{named} is not allowed"));
    }
    if !crate::ui::board::allowed(domain, service)
        && let Some(why) = crate::ui::board::advanced_problem(entity_id, &named)
    {
        return Err(why);
    }
    if !data.is_object() {
        return Err("service data is not an object".into());
    }
    if crate::ui::board::data_targets(data) {
        return Err("service data names a target: a tile's target is its own entity".into());
    }
    Ok(())
}

/// The `call_service` message of a tile's call.
pub fn call_message(id: u64, domain: &str, service: &str, entity_id: &str, data: &Value) -> Value {
    json!({"id": id, "type": "call_service", "domain": domain, "service": service,
           "service_data": data, "target": {"entity_id": entity_id}})
}

// ---- the thread ------------------------------------------------------------------------------

fn pause(d: Duration, stop: fn() -> bool, offline: &mut dyn FnMut() -> bool) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if stop() || !offline() {
            return false;
        }
        std::thread::sleep((until - Instant::now()).min(Duration::from_millis(250)));
    }
    !stop()
}

/// While the link is down: a tile's call is answered as failed at once (never sent later, when
/// nobody waits for it any more); every other command waits in `backlog` for the next
/// connection. False: the screen is gone.
fn drain_offline(
    cmds: &Receiver<HubCmd>,
    backlog: &mut VecDeque<HubCmd>,
    tx: &Sender<HubMsg>,
) -> bool {
    loop {
        match cmds.try_recv() {
            Ok(HubCmd::Call { id, .. }) => {
                if tx
                    .send(HubMsg::Called {
                        id,
                        ok: false,
                        error: Some("Home Assistant is not connected".into()),
                    })
                    .is_err()
                {
                    return false;
                }
            }
            Ok(c) => backlog.push_back(c),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

fn run(cfg: HubConfig, tx: Sender<HubMsg>, cmds: Receiver<HubCmd>, stop: fn() -> bool) {
    let ep = match endpoint(&cfg.url) {
        Ok(e) => e,
        Err(e) => {
            log::warn!(event = "ha_hub"; "cards and lists off: home_assistant.url = {e}");
            return;
        }
    };
    log::info!(event = "ha_hub"; "cards and lists from {}:{}", ep.host, ep.port);
    let mut chosen = cfg.lists.clone();
    // What the widget wants, and what it learnt, across connections.
    let mut want = super::widgets::Want::default();
    let mut wd = WidgetSide::default();
    // When the last message came, and whether the screen was told the link is up.
    let mut link = LinkState::default();
    // The screen was told the link is lost (or was never made): the retries then stay short, so
    // the offline sign goes soon after Home Assistant is back (the backoff alone grows to 5 min).
    let mut told_down = false;
    let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(300));
    // Commands that came while the link was down (tiles' calls excepted: drain_offline).
    let mut backlog: VecDeque<HubCmd> = VecDeque::new();
    loop {
        if stop() || !drain_offline(&cmds, &mut backlog, &tx) {
            return;
        }
        // Checked again every minute: a link made on the screen is picked up by itself.
        let token = match cfg.auth.access_token() {
            Ok(t) => t,
            Err(e) => {
                log::debug!("cards and lists: {e}");
                if !pause(Duration::from_secs(60), stop, &mut || {
                    drain_offline(&cmds, &mut backlog, &tx)
                }) {
                    return;
                }
                continue;
            }
        };
        let started = Instant::now();
        match session(
            &cfg,
            &ep,
            &token,
            &tx,
            &cmds,
            &mut chosen,
            &mut want,
            &mut wd,
            &mut link,
            &mut backlog,
            stop,
        ) {
            Ok(()) => return,
            Err(why) => {
                if std::mem::take(&mut link.told_up) {
                    told_down = false;
                }
                if !told_down {
                    // Also when it never connected (Home Assistant away at start-up).
                    if tx
                        .send(HubMsg::Link {
                            up: false,
                            last_seen: link.seen,
                        })
                        .is_err()
                    {
                        return;
                    }
                    told_down = true;
                }
                if started.elapsed() >= Duration::from_secs(60) {
                    backoff.reset();
                }
                log::info!(event = "ha_hub"; "cards and lists: {why}, retrying");
            }
        }
        let delay = backoff.next_delay();
        let delay = if told_down {
            delay.min(Duration::from_secs(15))
        } else {
            delay
        };
        if !pause(delay, stop, &mut || drain_offline(&cmds, &mut backlog, &tx)) {
            return;
        }
    }
}

/// One connection; Ok = stop (or the UI is gone), Err = lost.
#[allow(clippy::too_many_arguments)]
fn session(
    cfg: &HubConfig,
    ep: &Endpoint,
    token: &str,
    tx: &Sender<HubMsg>,
    cmds: &Receiver<HubCmd>,
    chosen: &mut Vec<String>,
    want: &mut super::widgets::Want,
    wd: &mut WidgetSide,
    link: &mut LinkState,
    backlog: &mut VecDeque<HubCmd>,
    stop: fn() -> bool,
) -> Result<(), String> {
    let lost = |e: io::Error| format!("{}:{}: {e}", ep.host, ep.port);
    let mut ws: WebSocket<TcpStream> =
        WebSocket::connect(&ep.host, ep.port, &ep.path, Duration::from_secs(10)).map_err(lost)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let msg = match ws.read().map_err(lost)? {
            Some(Message::Text(t)) => t,
            Some(Message::Close) => return Err("closed during authentication".into()),
            None if stop() => return Ok(()),
            None if Instant::now() < deadline => continue,
            None => return Err("no answer to the authentication".into()),
        };
        let v: Value = serde_json::from_str(&msg).unwrap_or(Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("auth_required") => {
                let auth = json!({"type": "auth", "access_token": token}).to_string();
                ws.send_text(&auth).map_err(lost)?;
            }
            Some("auth_ok") => break,
            Some("auth_invalid") => return Err("token refused".into()),
            _ => {}
        }
    }
    let mut next_id = 1u64;
    let mut take_id = || {
        let id = next_id;
        next_id += 1;
        id
    };
    let entities_id = take_id();
    let first_card_id = take_id();
    let _second_card_id = take_id();
    let screen_id = take_id();
    let initial = [json!({"id": entities_id, "type": "render_template",
                          "template": ENTITIES_TEMPLATE, "report_errors": true})
    .to_string()];
    let screen_sub = [screen_subscription(screen_id)];
    for m in initial
        .iter()
        .chain(&card_subscriptions(first_card_id))
        .chain(&screen_sub)
    {
        ws.send_text(m).map_err(lost)?;
    }
    let mut known: Option<Entities> = None;
    let mut subs = Subs::default();
    let mut told_lists: Option<Vec<Named>> = None;
    let reconcile = |ws: &mut WebSocket<TcpStream>,
                     subs: &mut Subs,
                     known: &Option<Entities>,
                     chosen: &[String],
                     told: &mut Option<Vec<Named>>,
                     take_id: &mut dyn FnMut() -> u64|
     -> Result<bool, String> {
        let wanted = active_lists(chosen, known.as_ref());
        for m in subs.reconcile(&wanted, &mut *take_id) {
            ws.send_text(&m.to_string()).map_err(lost)?;
        }
        if told.as_ref() != Some(&wanted) {
            *told = Some(wanted.clone());
            return Ok(tx.send(HubMsg::Lists(wanted)).is_ok());
        }
        Ok(true)
    };
    if !reconcile(
        &mut ws,
        &mut subs,
        &known,
        chosen,
        &mut told_lists,
        &mut take_id,
    )? {
        ws.close();
        return Ok(());
    }
    log::info!(event = "ha_hub"; "cards and lists: connected to Home Assistant");
    link.seen = unix_now();
    link.told_up = true;
    if tx
        .send(HubMsg::Link {
            up: true,
            last_seen: link.seen,
        })
        .is_err()
    {
        ws.close();
        return Ok(());
    }
    // A command (an event to fire) waits at most this long for the next read to end.
    ws.set_read_timeout(Duration::from_millis(100))
        .map_err(lost)?;
    let mut fire_ids: Vec<u64> = Vec::new();
    let mut fire_refused = false;
    // Tiles' calls sent: message id → the screen's id.
    let mut calls: HashMap<u64, u64> = HashMap::new();
    wd.connected();
    if wd.catalog_due(want) {
        wd.ask_catalog(&mut ws, &mut take_id).map_err(lost)?;
    } else if wd.catalog.is_some() && !wd.refresh(&mut ws, want, &mut take_id).map_err(lost)? {
        ws.close();
        return Ok(());
    }
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    loop {
        if stop() {
            ws.close();
            return Ok(());
        }
        let mut changed_lists = false;
        loop {
            let next = match backlog.pop_front() {
                Some(c) => Ok(c),
                None => cmds.try_recv(),
            };
            match next {
                Ok(HubCmd::Done { list, uid }) => {
                    ws.send_text(&tick_message(take_id(), &list, &uid).to_string())
                        .map_err(lost)?;
                    log::info!(event = "ha_shopping"; "item ticked on the screen");
                }
                Ok(HubCmd::Add { list, text }) => {
                    ws.send_text(&add_message(take_id(), &list, &text).to_string())
                        .map_err(lost)?;
                    log::info!(event = "ha_shopping"; "item added from the browser panel");
                }
                Ok(HubCmd::Remove { list, uid }) => {
                    ws.send_text(&remove_message(take_id(), &list, &uid).to_string())
                        .map_err(lost)?;
                    log::info!(event = "ha_shopping"; "item removed from the browser panel");
                }
                Ok(HubCmd::Fire { event_type, data }) => {
                    let id = take_id();
                    fire_ids.push(id);
                    if fire_ids.len() > 16 {
                        fire_ids.remove(0);
                    }
                    ws.send_text(&fire_message(id, &event_type, &cfg.device, &data).to_string())
                        .map_err(lost)?;
                }
                Ok(HubCmd::SetWidgets(w)) => {
                    if *want != w {
                        *want = w;
                        if wd.catalog.is_none() && wd.catalog_due(want) {
                            wd.ask_catalog(&mut ws, &mut take_id).map_err(lost)?;
                        }
                        // With the catalogue on its way, the subscription waits for it.
                        if wd.catalog_id.is_none()
                            && !wd.refresh(&mut ws, want, &mut take_id).map_err(lost)?
                        {
                            ws.close();
                            return Ok(());
                        }
                    }
                }
                Ok(HubCmd::Catalog) => {
                    if wd.catalog_id.is_none()
                        && !wd.blocked()
                        && wd.catalog_at.is_none_or(|t| t.elapsed() >= CATALOG_ASKED)
                    {
                        wd.ask_catalog(&mut ws, &mut take_id).map_err(lost)?;
                    }
                }
                Ok(HubCmd::Quiet(q)) => wd.quiet = q,
                Ok(HubCmd::Call {
                    id,
                    domain,
                    service,
                    entity_id,
                    data,
                }) => match check_call(want, &wd.states, &domain, &service, &entity_id, &data) {
                    Ok(()) => {
                        let mid = take_id();
                        if calls.len() >= 32 {
                            // Never answered: the oldest are given up.
                            let oldest = calls.keys().min().copied().unwrap_or(mid);
                            calls.remove(&oldest);
                        }
                        calls.insert(mid, id);
                        ws.send_text(
                            &call_message(mid, &domain, &service, &entity_id, &data).to_string(),
                        )
                        .map_err(lost)?;
                        log::info!(event = "ha_board_call", entity = entity_id.as_str(); "tile: {domain}.{service} on {entity_id}");
                    }
                    Err(why) => {
                        log::warn!(event = "ha_board_call"; "tile: refused: {why}");
                        if tx
                            .send(HubMsg::Called {
                                id,
                                ok: false,
                                error: Some(why),
                            })
                            .is_err()
                        {
                            ws.close();
                            return Ok(());
                        }
                    }
                },
                Ok(HubCmd::SetLists(l)) => {
                    if *chosen != l {
                        *chosen = l;
                        changed_lists = true;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    ws.close();
                    return Ok(());
                }
            }
        }
        let read = ws.read().map_err(lost)?;
        // A message too big was skipped: the catalogue or a sensor's history (the only big
        // answers asked); not asked again for a while.
        if ws.take_dropped() > 0 {
            for entity in wd.dropped() {
                if tx.send(HubMsg::Curve(entity, None)).is_err() {
                    ws.close();
                    return Ok(());
                }
            }
        }
        if std::mem::take(&mut wd.resubscribe)
            && !wd.refresh(&mut ws, want, &mut take_id).map_err(lost)?
        {
            ws.close();
            return Ok(());
        }
        if !wd.flush(tx) {
            ws.close();
            return Ok(());
        }
        match read {
            Some(Message::Text(t)) => {
                last_rx = Instant::now();
                link.seen = unix_now();
                // The catalogue's answer can be megabytes: read through its own struct, never
                // as a JSON value (and only looked at while one is awaited).
                let mid = if wd.catalog_id.is_some() || !wd.curve_pending.is_empty() {
                    message_id(&t)
                } else {
                    None
                };
                if let Some((entity, _)) = mid.and_then(|m| wd.curve_pending.remove(&m)) {
                    let c = if t.len() > super::widgets::MAX_HISTORY {
                        wd.no_curve.insert(entity.clone(), Instant::now());
                        log::info!(event = "ha_widgets"; "widget: {entity}'s day is too big to draw: no curve for a day");
                        None
                    } else {
                        super::widgets::parse_curve(&t, &entity)
                    };
                    if tx.send(HubMsg::Curve(entity, c)).is_err() {
                        ws.close();
                        return Ok(());
                    }
                    continue;
                }
                if let Some(cid) = wd.catalog_id
                    && mid == Some(cid)
                {
                    wd.catalog_id = None;
                    wd.catalog_at = Some(Instant::now());
                    match super::widgets::parse_catalog(&t) {
                        Ok(c) => {
                            log::info!(event = "ha_widgets", entities = c.len(); "widget: {} entities it may show", c.len());
                            wd.catalog = Some(c.clone());
                            if tx.send(HubMsg::Catalog(c)).is_err()
                                || !wd.refresh(&mut ws, want, &mut take_id).map_err(lost)?
                            {
                                ws.close();
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            log::info!(event = "ha_widgets"; "widget: {e}");
                            wd.resubscribe = true;
                        }
                    }
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                if wd.sub_id.is_some() && v.get("id").and_then(Value::as_u64) == wd.sub_id {
                    if let Some(ev) = v.get("event")
                        && wd.states.apply(ev)
                    {
                        wd.publish(want);
                    }
                    continue;
                }
                let id = v.get("id").and_then(Value::as_u64);
                if v.get("type").and_then(Value::as_str) == Some("result")
                    && let Some(screen_id) = id.and_then(|i| calls.remove(&i))
                {
                    let ok = v.get("success").and_then(Value::as_bool) == Some(true);
                    let error = (!ok).then(|| {
                        v.pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("refused")
                            .to_string()
                    });
                    if let Some(e) = &error {
                        log::warn!(event = "ha_board_call"; "tile: Home Assistant refused the call: {e}");
                    }
                    if tx
                        .send(HubMsg::Called {
                            id: screen_id,
                            ok,
                            error,
                        })
                        .is_err()
                    {
                        ws.close();
                        return Ok(());
                    }
                    continue;
                }
                let is_event = v.get("type").and_then(Value::as_str) == Some("event");
                let failed = v.get("type").and_then(Value::as_str) == Some("result")
                    && v.get("success").and_then(Value::as_bool) == Some(false);
                if failed && id.is_some_and(|i| fire_ids.contains(&i)) {
                    // Firing an event needs an admin user: said once, the conversation never waits.
                    if !fire_refused {
                        fire_refused = true;
                        log::warn!(event = "ha_fire_refused"; "Home Assistant refused an event ({}): the QR link's user must be an administrator", v.pointer("/error/message").and_then(Value::as_str).unwrap_or("refused"));
                    }
                } else if failed {
                    let why = v
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("refused");
                    let what = match id {
                        Some(i) if i == entities_id => "the list of lists".to_string(),
                        Some(i) => subs
                            .list_of(i)
                            .map_or("a subscription".into(), String::from),
                        None => "a request".into(),
                    };
                    log::info!(event = "ha_hub"; "cards and lists: {what}: {why}");
                } else if is_event && id == Some(entities_id) {
                    if let Some(e) = parse_entities(&v)
                        && known.as_ref() != Some(&e)
                    {
                        known = Some(e.clone());
                        changed_lists = true;
                        if tx.send(HubMsg::Entities(e)).is_err() {
                            ws.close();
                            return Ok(());
                        }
                    }
                } else if is_event && let Some(list) = id.and_then(|i| subs.list_of(i)) {
                    if let Some(items) = todo::open_items(&v)
                        && tx
                            .send(HubMsg::Items {
                                list: list.to_string(),
                                items,
                            })
                            .is_err()
                    {
                        ws.close();
                        return Ok(());
                    }
                } else if is_event && let Some(on) = events::parse_screen(&v, &cfg.device) {
                    log::info!(event = "ha_screen", on; "screen from Home Assistant: {}", if on { "on" } else { "off" });
                    if tx.send(HubMsg::Screen(on)).is_err() {
                        ws.close();
                        return Ok(());
                    }
                } else if is_event && let Some(c) = events::parse(&v, &cfg.device) {
                    log::info!(event = "ha_card"; "card from Home Assistant: {c:?}");
                    if tx.send(HubMsg::Card(c)).is_err() {
                        ws.close();
                        return Ok(());
                    }
                }
            }
            Some(Message::Close) => return Err("Home Assistant closed the connection".into()),
            None => {}
        }
        if changed_lists
            && !reconcile(
                &mut ws,
                &mut subs,
                &known,
                chosen,
                &mut told_lists,
                &mut take_id,
            )?
        {
            ws.close();
            return Ok(());
        }
        if last_rx.elapsed() >= Duration::from_secs(90) {
            return Err("no message from Home Assistant".into());
        }
        if last_ping.elapsed() >= Duration::from_secs(30) {
            last_ping = Instant::now();
            ws.send_text(&json!({"id": take_id(), "type": "ping"}).to_string())
                .map_err(lost)?;
        }
        // A sensor's day, one request at a time, each again after half an hour.
        if let Some(m) = wd.next_curve(want, &mut take_id) {
            ws.send_text(&m).map_err(lost)?;
        }
        // The catalogue again every six hours (entities come and go).
        if wd.catalog.is_some() && wd.catalog_due(want) {
            wd.ask_catalog(&mut ws, &mut take_id).map_err(lost)?;
        }
    }
}

/// The catalogue is read again this often; asked by the browser panel, at most this often;
/// after an answer too big, not before this.
const CATALOG_EVERY: Duration = Duration::from_secs(6 * 3600);
const CATALOG_ASKED: Duration = Duration::from_secs(5 * 60);
const CATALOG_BLOCKED: Duration = Duration::from_secs(6 * 3600);
/// The widget's data goes to the screen at most this often; the tiles' states (a tap waits for
/// its answer) at most this often.
const WIDGETS_EVERY: Duration = Duration::from_millis(1500);
pub const TILES_EVERY: Duration = Duration::from_millis(300);

/// A message's `id` without reading the rest of it as JSON values.
fn message_id(text: &str) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct Head {
        id: Option<u64>,
    }
    serde_json::from_str::<Head>(text).ok()?.id
}

/// The widget's side of the connection: the catalogue, the one subscription, the states, the
/// curves. Kept across connections: the catalogue, what was too big, when each curve was asked.
#[derive(Default)]
struct WidgetSide {
    catalog: Option<Vec<super::widgets::Entity>>,
    catalog_id: Option<u64>,
    catalog_at: Option<Instant>,
    /// The catalogue's answer was too big: not asked again before then.
    catalog_blocked: Option<Instant>,
    sub_id: Option<u64>,
    sub_ids: Vec<String>,
    states: super::widgets::States,
    told: Option<super::widgets::WidgetsData>,
    /// Data waiting for [`WIDGETS_EVERY`], and when the last went.
    pending: Option<super::widgets::WidgetsData>,
    last_sent: Option<Instant>,
    /// The tiles' states told, waiting for [`TILES_EVERY`], when the last went.
    tiles_told: Option<super::widgets::States>,
    tiles_pending: Option<super::widgets::States>,
    tiles_sent: Option<Instant>,
    /// Curves asked (message id → entity, when) and when each sensor's was asked last.
    curve_pending: std::collections::HashMap<u64, (String, Instant)>,
    curve_asked: std::collections::HashMap<String, Instant>,
    last_curve: Option<Instant>,
    /// Sensors whose day was too big to read, and since when.
    no_curve: std::collections::HashMap<String, Instant>,
    /// The screen is off or it is night.
    quiet: bool,
    /// The catalogue will not come (too big, refused): subscribe to what is known anyway.
    resubscribe: bool,
}

/// A sensor's day is read again this often; two requests at least this far apart; one not
/// answered in this long is given up; one too big is not asked for a day.
const CURVE_EVERY: Duration = Duration::from_secs(30 * 60);
const CURVE_GAP: Duration = Duration::from_secs(5);
const CURVE_WAIT: Duration = Duration::from_secs(60);
const NO_CURVE_FOR: Duration = Duration::from_secs(24 * 3600);

impl WidgetSide {
    /// A new connection: what belonged to the old one goes.
    fn connected(&mut self) {
        self.catalog_id = None;
        self.sub_id = None;
        self.sub_ids.clear();
        self.states = Default::default();
        self.told = None;
        self.pending = None;
        self.tiles_told = None;
        self.tiles_pending = None;
        self.curve_pending.clear();
    }

    fn blocked(&self) -> bool {
        self.catalog_blocked
            .is_some_and(|t| t.elapsed() < CATALOG_BLOCKED)
    }

    /// The catalogue should be asked now: something wants it, none is on its way, it is
    /// missing or six hours old, and its last answer was not too big lately.
    fn catalog_due(&self, w: &super::widgets::Want) -> bool {
        *w != super::widgets::Want::default()
            && self.catalog_id.is_none()
            && !self.blocked()
            && self.catalog_at.is_none_or(|t| t.elapsed() >= CATALOG_EVERY)
    }

    /// A message too big was skipped: what was awaited is given up (the entities whose curve
    /// it was, to be told).
    fn dropped(&mut self) -> Vec<String> {
        if self.catalog_id.take().is_some() {
            self.resubscribe = true;
            self.catalog_blocked = Some(Instant::now());
            self.catalog_at = Some(Instant::now());
            log::warn!(event = "ha_widgets"; "widget: Home Assistant's list of entities is too big to read: tried again in six hours");
        }
        let gone: Vec<String> = self.curve_pending.drain().map(|(_, (e, _))| e).collect();
        for e in &gone {
            self.no_curve.insert(e.clone(), Instant::now());
        }
        gone
    }

    /// The next curve request due, if any: not while quiet, one at a time (one unanswered for a
    /// minute is given up), each sensor every half hour, none for a sensor too big.
    fn next_curve(
        &mut self,
        want: &super::widgets::Want,
        take_id: &mut dyn FnMut() -> u64,
    ) -> Option<String> {
        self.curve_pending
            .retain(|_, (_, at)| at.elapsed() < CURVE_WAIT);
        self.no_curve.retain(|_, at| at.elapsed() < NO_CURVE_FOR);
        if self.quiet
            || !self.curve_pending.is_empty()
            || self.last_curve.is_some_and(|t| t.elapsed() < CURVE_GAP)
        {
            return None;
        }
        let catalog = self.catalog.as_ref()?;
        let entity = want.sensors.iter().chain(&want.tile_curves).find(|s| {
            !self.no_curve.contains_key(*s)
                && self
                    .curve_asked
                    .get(*s)
                    .is_none_or(|t| t.elapsed() >= CURVE_EVERY)
        })?;
        let stats = catalog
            .iter()
            .find(|e| e.id == *entity)
            .is_some_and(|e| e.statistics);
        let id = take_id();
        self.curve_asked.insert(entity.clone(), Instant::now());
        self.curve_pending
            .insert(id, (entity.clone(), Instant::now()));
        self.last_curve = Some(Instant::now());
        let start = super::widgets::rfc3339(unix_now() - 24 * 3600);
        Some(super::widgets::curve_request(id, entity, stats, &start))
    }

    fn ask_catalog(
        &mut self,
        ws: &mut WebSocket<TcpStream>,
        take_id: &mut dyn FnMut() -> u64,
    ) -> io::Result<()> {
        let id = take_id();
        self.catalog_id = Some(id);
        self.catalog_at = Some(Instant::now());
        ws.send_text(&super::widgets::catalog_request(id))
    }

    /// The subscription made what is wanted now; the data computed (sent by [`Self::flush`]).
    fn refresh(
        &mut self,
        ws: &mut WebSocket<TcpStream>,
        want: &super::widgets::Want,
        take_id: &mut dyn FnMut() -> u64,
    ) -> io::Result<bool> {
        let ids = super::widgets::wanted(want, self.catalog.as_deref().unwrap_or(&[]));
        if ids != self.sub_ids {
            if let Some(old) = self.sub_id.take() {
                ws.send_text(&super::widgets::unsubscribe(take_id(), old))?;
            }
            self.states = Default::default();
            if !ids.is_empty() {
                let id = take_id();
                self.sub_id = Some(id);
                ws.send_text(&super::widgets::subscribe(id, &ids))?;
                log::info!(event = "ha_widgets", entities = ids.len(); "widget: {} entities followed", ids.len());
            }
            self.sub_ids = ids;
        }
        self.publish(want);
        Ok(true)
    }

    /// The data as it is now, to be sent when it changed.
    fn publish(&mut self, want: &super::widgets::Want) {
        let data = super::widgets::view(want, self.catalog.as_deref().unwrap_or(&[]), &self.states);
        if self.told.as_ref() != Some(&data) {
            self.pending = Some(data);
        } else {
            self.pending = None;
        }
        let tiles = super::widgets::States {
            by_id: want
                .tiles
                .iter()
                .filter_map(|id| Some((id.clone(), self.states.by_id.get(id)?.clone())))
                .collect(),
        };
        if self.tiles_told.as_ref() != Some(&tiles)
            && !(want.tiles.is_empty() && self.tiles_told.is_none())
        {
            self.tiles_pending = Some(tiles);
        } else {
            self.tiles_pending = None;
        }
    }

    /// Sends what waits, at most every [`WIDGETS_EVERY`]. False: the screen is gone.
    fn flush(&mut self, tx: &Sender<HubMsg>) -> bool {
        if self.tiles_pending.is_some()
            && self.tiles_sent.is_none_or(|t| t.elapsed() >= TILES_EVERY)
        {
            let tiles = self.tiles_pending.take().unwrap_or_default();
            self.tiles_told = Some(tiles.clone());
            self.tiles_sent = Some(Instant::now());
            if tx.send(HubMsg::Tiles(tiles)).is_err() {
                return false;
            }
        }
        if self.pending.is_none() || self.last_sent.is_some_and(|t| t.elapsed() < WIDGETS_EVERY) {
            return true;
        }
        let data = self.pending.take().unwrap_or_default();
        self.told = Some(data.clone());
        self.last_sent = Some(Instant::now());
        tx.send(HubMsg::Widgets(data)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use events::HaCard;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// The next message, the link's up and down left out.
    fn next(rx: &Receiver<HubMsg>) -> HubMsg {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                HubMsg::Link { .. } => {}
                m => return m,
            }
        }
    }

    /// The first message `f` takes, the others (and the link's) left out.
    fn wait<T>(rx: &Receiver<HubMsg>, f: impl Fn(HubMsg) -> Option<T>) -> T {
        loop {
            if let Some(t) = f(next(rx)) {
                return t;
            }
        }
    }

    fn named(v: &[(&str, &str)]) -> Vec<Named> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    /// The widget's side: too big is remembered, a curve unanswered is given up after a
    /// minute, none while quiet, the data at most every 1.5 s.
    #[test]
    fn the_widget_side_remembers_and_waits() {
        let want = super::super::widgets::Want {
            sensors: vec!["sensor.a".into(), "sensor.b".into()],
            ..Default::default()
        };
        let mut wd = WidgetSide {
            catalog: Some(vec![]),
            ..Default::default()
        };
        let mut n = 0u64;
        let mut take = || {
            n += 1;
            n
        };
        assert!(wd.catalog_due(&want));
        wd.catalog_id = Some(99);
        let first = wd.next_curve(&want, &mut take).unwrap();
        assert!(first.contains("sensor.a"));
        assert!(wd.next_curve(&want, &mut take).is_none(), "one at a time");
        // A message too big: the catalogue and the curve given up, remembered.
        assert_eq!(wd.dropped(), ["sensor.a"]);
        assert!(wd.blocked() && !wd.catalog_due(&want));
        assert!(wd.no_curve.contains_key("sensor.a"));
        wd.connected();
        assert!(
            !wd.catalog_due(&want),
            "a new connection does not ask again"
        );
        // An unanswered curve is given up after a minute.
        wd.last_curve = None;
        let id = take();
        wd.curve_pending.insert(
            id,
            ("sensor.b".into(), Instant::now() - Duration::from_secs(61)),
        );
        wd.curve_asked.insert(
            "sensor.b".into(),
            Instant::now() - Duration::from_secs(3600),
        );
        assert!(
            wd.next_curve(&want, &mut take)
                .unwrap()
                .contains("sensor.b")
        );
        // Quiet: nothing.
        wd.curve_pending.clear();
        wd.curve_asked.clear();
        wd.last_curve = None;
        wd.quiet = true;
        assert!(wd.next_curve(&want, &mut take).is_none());
        // The data: the first at once, the next after 1.5 s.
        let (tx, rx) = channel();
        wd.pending = Some(Default::default());
        assert!(wd.flush(&tx));
        assert!(rx.try_recv().is_ok());
        wd.pending = Some(super::super::widgets::WidgetsData {
            check: vec![],
            people: vec![crate::ui::widgets::Person {
                name: "x".into(),
                place: crate::ui::widgets::Place::Home,
            }],
            ..Default::default()
        });
        assert!(wd.flush(&tx));
        assert!(rx.try_recv().is_err());
        wd.last_sent = Some(Instant::now() - WIDGETS_EVERY);
        assert!(wd.flush(&tx));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn card_subscriptions_are_one_per_event_type() {
        let [a, b] = card_subscriptions(7);
        let (a, b): (Value, Value) = (
            serde_json::from_str(&a).unwrap(),
            serde_json::from_str(&b).unwrap(),
        );
        assert_eq!(
            a,
            json!({"id": 7, "type": "subscribe_events", "event_type": "home_companion_card"})
        );
        assert_eq!(
            b,
            json!({"id": 8, "type": "subscribe_events",
                   "event_type": "home_companion_card_dismiss"})
        );
    }

    #[test]
    fn entities_from_a_string_or_a_list() {
        let list = json!([{"id": "todo.shopping", "name": "Shopping"},
                          {"id": "calendar.family", "name": "Family"},
                          {"id": "todo.bring", "name": "bring"},
                          {"id": "calendar.work", "name": " "}]);
        let want = Entities {
            todo: named(&[("todo.bring", "bring"), ("todo.shopping", "Shopping")]),
            calendar: named(&[
                ("calendar.work", "calendar.work"),
                ("calendar.family", "Family"),
            ]),
        };
        let as_list = json!({"id": 1, "type": "event", "event": {"result": list}});
        assert_eq!(parse_entities(&as_list), Some(want.clone()));
        let as_text = json!({"id": 1, "type": "event", "event": {"result": list.to_string()}});
        assert_eq!(parse_entities(&as_text), Some(want));
        assert_eq!(parse_entities(&json!({"event": {"error": "x"}})), None);
        assert_eq!(
            parse_entities(&json!({"event": {"result": "not json"}})),
            None
        );
    }

    #[test]
    fn active_lists_choice_default_and_unknown() {
        let k = Entities {
            todo: named(&[("todo.a", "A"), ("todo.b", "B"), ("todo.c", "C")]),
            calendar: vec![],
        };
        assert_eq!(active_lists(&[], Some(&k)), k.todo);
        assert_eq!(
            active_lists(
                &["todo.c".into(), "todo.a".into(), "todo.gone".into()],
                Some(&k)
            ),
            named(&[("todo.c", "C"), ("todo.a", "A")])
        );
        assert_eq!(
            active_lists(&["todo.a".into()], None),
            named(&[("todo.a", "todo.a")])
        );
        assert!(active_lists(&[], None).is_empty());
    }

    #[test]
    fn reconcile_subscribes_and_unsubscribes() {
        let mut n = 10;
        let mut id = || {
            n += 1;
            n
        };
        let mut s = Subs::default();
        let m = s.reconcile(&named(&[("todo.a", "A"), ("todo.b", "B")]), &mut id);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0]["type"], "todo/item/subscribe");
        assert_eq!(s.list_of(11), Some("todo.a"));
        assert_eq!(s.list_of(12), Some("todo.b"));
        // Nothing changes: nothing is sent.
        assert!(
            s.reconcile(&named(&[("todo.a", "A"), ("todo.b", "B")]), &mut id)
                .is_empty()
        );
        // b goes, c comes.
        let m = s.reconcile(&named(&[("todo.a", "A"), ("todo.c", "C")]), &mut id);
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[0],
            json!({"id": 13, "type": "unsubscribe_events", "subscription": 12})
        );
        assert_eq!(m[1]["entity_id"], "todo.c");
        assert_eq!(s.list_of(12), None);
        assert_eq!(s.list_of(14), Some("todo.c"));
    }

    #[test]
    fn tick_is_todo_update_item() {
        let m = tick_message(9, "todo.a", "u1");
        assert_eq!(m["service"], "update_item");
        assert_eq!(
            m["service_data"],
            json!({"item": "u1", "status": "completed"})
        );
        assert_eq!(m["target"]["entity_id"], "todo.a");
    }

    fn read_frame(s: &mut TcpStream, buf: &mut Vec<u8>) -> Value {
        loop {
            if let Some(fr) = super::super::ws::decode_frame(buf).unwrap() {
                buf.drain(..fr.used);
                if fr.opcode == 1 {
                    return serde_json::from_slice(&fr.payload).unwrap();
                }
                continue;
            }
            let mut c = [0u8; 4096];
            let n = s.read(&mut c).unwrap();
            assert!(n > 0, "client closed");
            buf.extend_from_slice(&c[..n]);
        }
    }

    fn send(s: &mut TcpStream, v: &Value) {
        let text = v.to_string();
        let p = text.as_bytes();
        let mut f = vec![0x81u8];
        if p.len() < 126 {
            f.push(p.len() as u8);
        } else {
            f.push(126);
            f.extend_from_slice(&(p.len() as u16).to_be_bytes());
        }
        f.extend_from_slice(p);
        s.write_all(&f).unwrap();
    }

    fn no_stop() -> bool {
        false
    }

    #[test]
    fn cards_lists_and_a_tick_over_a_connection() {
        let dir = std::env::temp_dir().join(format!("home-companion-hub-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("ha-token"), "tok\n").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let auth = Arc::new(auth::Auth::new(&url, &dir.join("ha-token"), &dir));
        let cfg = HubConfig {
            url,
            auth,
            device: "Alfred".into(),
            lists: vec![],
        };
        let (rx, tx, _) = spawn(cfg, no_stop).unwrap();
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = Vec::new();
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut c = [0u8; 1024];
            let n = s.read(&mut c).unwrap();
            buf.extend_from_slice(&c[..n]);
        }
        s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            .unwrap();
        let mut buf = Vec::new();
        send(&mut s, &json!({"type": "auth_required"}));
        assert_eq!(read_frame(&mut s, &mut buf)["access_token"], "tok");
        send(&mut s, &json!({"type": "auth_ok"}));
        let t = read_frame(&mut s, &mut buf);
        assert_eq!(t["type"], "render_template");
        let c1 = read_frame(&mut s, &mut buf);
        assert_eq!(c1["event_type"], "home_companion_card");
        let c2 = read_frame(&mut s, &mut buf);
        assert_eq!(c2["event_type"], "home_companion_card_dismiss");
        let c3 = read_frame(&mut s, &mut buf);
        assert_eq!(c3["event_type"], "home_companion_screen");
        // Home Assistant tells what exists: every todo list is subscribed (nothing chosen).
        send(
            &mut s,
            &json!({"id": t["id"], "type": "event", "event": {"result": [
                {"id": "todo.spesa", "name": "Spesa"}, {"id": "todo.casa", "name": "Casa"},
                {"id": "calendar.famiglia", "name": "Famiglia"}]}}),
        );
        let mut sub_ids = BTreeMap::new();
        for _ in 0..2 {
            let m = read_frame(&mut s, &mut buf);
            assert_eq!(m["type"], "todo/item/subscribe");
            sub_ids.insert(
                m["entity_id"].as_str().unwrap().to_string(),
                m["id"].as_u64().unwrap(),
            );
        }
        let mut got = Vec::new();
        // First the empty list of lists (nothing known yet), then what exists, then the lists;
        // the link's "up" comes once the subscriptions are made.
        while got.len() < 3 {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                HubMsg::Link { up, last_seen } => {
                    assert!(up && last_seen > 0);
                }
                m => got.push(m),
            }
        }
        assert!(
            got.iter()
                .any(|m| matches!(m, HubMsg::Entities(e) if e.calendar.len() == 1))
        );
        let lists = got.iter().find_map(|m| match m {
            HubMsg::Lists(l) if !l.is_empty() => Some(l.clone()),
            _ => None,
        });
        assert_eq!(
            lists,
            Some(named(&[("todo.casa", "Casa"), ("todo.spesa", "Spesa")]))
        );
        // Items of one list, and a card.
        send(
            &mut s,
            &json!({"id": sub_ids["todo.spesa"], "type": "event", "event": {"items": [
                {"uid": "a", "summary": "Pane", "status": "needs_action"}]}}),
        );
        assert_eq!(
            next(&rx),
            HubMsg::Items {
                list: "todo.spesa".into(),
                items: vec![("a".into(), "Pane".into())]
            }
        );
        send(
            &mut s,
            &json!({"id": c1["id"], "type": "event", "event": {"event_type": "home_companion_card",
                    "data": {"id": "w", "text": "Done"}}}),
        );
        assert_eq!(
            next(&rx),
            HubMsg::Card(CardEvent::Show(HaCard {
                id: Some("w".into()),
                text: "Done".into(),
                ..Default::default()
            }))
        );
        // The screen is asked to sleep, and to wake.
        for (state, want) in [("off", false), ("on", true)] {
            send(
                &mut s,
                &json!({"id": c3["id"], "type": "event", "event": {
                    "event_type": "home_companion_screen", "data": {"state": state}}}),
            );
            assert_eq!(next(&rx), HubMsg::Screen(want));
        }
        // An event for Home Assistant's bus, with this device's name; a refusal is not fatal.
        tx.send(HubCmd::Fire {
            event_type: "home_companion_conversation".into(),
            data: json!({"state": "started", "trigger": "wake"}),
        })
        .unwrap();
        let fire = read_frame(&mut s, &mut buf);
        assert_eq!(fire["type"], "fire_event");
        assert_eq!(fire["event_type"], "home_companion_conversation");
        assert_eq!(
            fire["event_data"],
            json!({"state": "started", "trigger": "wake", "device": "Alfred"})
        );
        send(
            &mut s,
            &json!({"id": fire["id"], "type": "result", "success": false,
                    "error": {"code": "unauthorized", "message": "Unauthorized"}}),
        );
        tx.send(HubCmd::Fire {
            event_type: "home_companion_conversation".into(),
            data: json!({"state": "ended", "trigger": "wake", "handled_by": "reflex"}),
        })
        .unwrap();
        let fire2 = read_frame(&mut s, &mut buf);
        assert_eq!(fire2["event_data"]["handled_by"], "reflex");
        // A tick on the screen.
        tx.send(HubCmd::Done {
            list: "todo.spesa".into(),
            uid: "a".into(),
        })
        .unwrap();
        let call = read_frame(&mut s, &mut buf);
        assert_eq!(call["type"], "call_service");
        assert_eq!(call["target"]["entity_id"], "todo.spesa");
        // The user chooses one list: the other is unsubscribed.
        tx.send(HubCmd::SetLists(vec!["todo.casa".into()])).unwrap();
        let m = read_frame(&mut s, &mut buf);
        assert_eq!(m["type"], "unsubscribe_events");
        assert_eq!(m["subscription"], sub_ids["todo.spesa"]);
        assert_eq!(next(&rx), HubMsg::Lists(named(&[("todo.casa", "Casa")])));
        // The widget: the catalogue (read without a JSON value of the whole), one subscription
        // for what is wanted, the data when it changes.
        let fx = |n: &str| {
            std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("fixtures/ha")
                    .join(n),
            )
            .unwrap()
        };
        tx.send(HubCmd::SetWidgets(super::super::widgets::Want {
            sensors: vec!["sensor.outdoor_temperature".into()],
            player: String::new(),
            people: None,
            openings: true,
            batteries: false,
            ..Default::default()
        }))
        .unwrap();
        let get = read_frame(&mut s, &mut buf);
        assert_eq!(get["type"], "get_states");
        let mut states: Value = serde_json::from_str(&fx("widgets-get-states.json")).unwrap();
        states["id"] = get["id"].clone();
        send(&mut s, &states);
        let sub = read_frame(&mut s, &mut buf);
        assert_eq!(sub["type"], "subscribe_entities");
        assert_eq!(
            sub["entity_ids"],
            json!([
                "binary_sensor.front_door",
                "binary_sensor.kitchen_window",
                "sensor.outdoor_temperature"
            ])
        );
        assert!(
            matches!(next(&rx), HubMsg::Catalog(c) if c.iter().all(|e| !e.id.starts_with("lock.")))
        );
        assert_eq!(next(&rx), HubMsg::Widgets(Default::default()));
        let first: Value = serde_json::from_str(
            fx("widgets-subscribe-entities.jsonl")
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        send(
            &mut s,
            &json!({"id": sub["id"], "type": "event", "event": first["event"]}),
        );
        match next(&rx) {
            HubMsg::Widgets(d) => {
                assert_eq!(d.sensors.len(), 1);
                assert_eq!(d.check.len(), 1, "the kitchen window");
            }
            m => panic!("{m:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    fn board_want() -> super::super::widgets::Want {
        super::super::widgets::Want {
            tiles: vec![
                "light.living_room".into(),
                "cover.living_room".into(),
                "cover.garage".into(),
                "script.good_night".into(),
                "lock.front".into(),
            ],
            advanced: vec![("script.good_night".into(), "script.turn_off".into())],
            ..Default::default()
        }
    }

    #[test]
    fn a_tiles_call_only_from_the_closed_list() {
        use super::super::widgets::{State, States};
        let w = board_want();
        let mut s = States::default();
        s.by_id.insert(
            "cover.garage".into(),
            State {
                device_class: "garage".into(),
                ..Default::default()
            },
        );
        let obj = json!({});
        let ok = |d: &str, v: &str, e: &str| check_call(&w, &s, d, v, e, &obj);
        assert_eq!(ok("homeassistant", "toggle", "light.living_room"), Ok(()));
        assert_eq!(ok("light", "turn_on", "light.living_room"), Ok(()));
        assert_eq!(
            ok("cover", "set_cover_position", "cover.living_room"),
            Ok(())
        );
        assert_eq!(
            ok("script", "turn_off", "script.good_night"),
            Ok(()),
            "its Advanced service"
        );
        assert!(
            ok("script", "turn_off", "light.living_room").is_err(),
            "not this tile's"
        );
        assert!(
            ok("light", "turn_off", "light.living_room").is_err(),
            "not in the list"
        );
        assert!(
            ok("homeassistant", "toggle", "light.kitchen").is_err(),
            "not on the board"
        );
        assert!(
            ok("cover", "toggle", "cover.garage").is_err(),
            "a garage door, by its class"
        );
        assert!(ok("lock", "unlock", "lock.front").is_err());
        assert!(ok("homeassistant", "toggle", "lock.front").is_err());
        let mut sneaky = board_want();
        sneaky
            .advanced
            .push(("light.living_room".into(), "script.open_gate".into()));
        assert!(
            check_call(
                &sneaky,
                &s,
                "script",
                "open_gate",
                "light.living_room",
                &obj
            )
            .is_err(),
            "another script by Advanced: its gate was never looked at"
        );
        assert_eq!(ok("cover", "close_cover", "cover.living_room"), Ok(()));
        assert!(check_call(&w, &s, "light", "turn_on", "light.living_room", &json!([1])).is_err());
        for key in crate::ui::board::TARGET_KEYS {
            let redirect = json!({ key: "lock.front", "variables": {} });
            assert!(
                check_call(&w, &s, "script", "turn_off", "script.good_night", &redirect).is_err(),
                "{key} redirects the call"
            );
        }
        assert_eq!(
            check_call(
                &w,
                &s,
                "script",
                "turn_off",
                "script.good_night",
                &json!({"variables": {"entity_id": "x"}})
            ),
            Ok(()),
            "only the data's own keys are a target"
        );
        assert_eq!(
            call_message(
                5,
                "light",
                "turn_on",
                "light.living_room",
                &json!({"brightness_pct": 40})
            ),
            json!({"id": 5, "type": "call_service", "domain": "light", "service": "turn_on",
                   "service_data": {"brightness_pct": 40},
                   "target": {"entity_id": "light.living_room"}})
        );
    }

    #[test]
    fn a_call_while_offline_fails_at_once_and_the_rest_waits() {
        let (cmd_tx, cmd_rx) = channel();
        let (tx, rx) = channel();
        cmd_tx.send(HubCmd::Quiet(true)).unwrap();
        cmd_tx
            .send(HubCmd::Call {
                id: 3,
                domain: "homeassistant".into(),
                service: "toggle".into(),
                entity_id: "light.a".into(),
                data: json!({}),
            })
            .unwrap();
        cmd_tx.send(HubCmd::Catalog).unwrap();
        let mut backlog = VecDeque::new();
        assert!(drain_offline(&cmd_rx, &mut backlog, &tx));
        assert!(matches!(
            rx.try_recv(),
            Ok(HubMsg::Called {
                id: 3,
                ok: false,
                ..
            })
        ));
        assert_eq!(backlog, [HubCmd::Quiet(true), HubCmd::Catalog]);
        drop(cmd_tx);
        assert!(
            !drain_offline(&cmd_rx, &mut backlog, &tx),
            "the screen is gone"
        );
    }

    /// The fake Home Assistant's side up to the first subscriptions (render_template, the
    /// card and screen events).
    fn accept_ha(l: &TcpListener) -> (TcpStream, Vec<u8>) {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = Vec::new();
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut c = [0u8; 1024];
            let n = s.read(&mut c).unwrap();
            buf.extend_from_slice(&c[..n]);
        }
        s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            .unwrap();
        let mut buf = Vec::new();
        send(&mut s, &json!({"type": "auth_required"}));
        assert_eq!(read_frame(&mut s, &mut buf)["access_token"], "tok");
        send(&mut s, &json!({"type": "auth_ok"}));
        for _ in 0..4 {
            read_frame(&mut s, &mut buf);
        }
        (s, buf)
    }

    #[test]
    fn tiles_are_followed_and_called_over_a_connection() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-hub-tiles-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("ha-token"), "tok\n").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let auth = Arc::new(auth::Auth::new(&url, &dir.join("ha-token"), &dir));
        let cfg = HubConfig {
            url,
            auth,
            device: "Alfred".into(),
            lists: vec![],
        };
        let (rx, tx, _) = spawn(cfg, no_stop).unwrap();
        // A call before the board is known (or the link is up): refused, never sent.
        tx.send(HubCmd::Call {
            id: 1,
            domain: "homeassistant".into(),
            service: "toggle".into(),
            entity_id: "light.living_room".into(),
            data: json!({}),
        })
        .unwrap();
        let (mut s, mut buf) = accept_ha(&l);
        assert!(matches!(
            wait(&rx, |m| matches!(m, HubMsg::Called { .. }).then_some(m)),
            HubMsg::Called {
                id: 1,
                ok: false,
                error: Some(_)
            }
        ));
        let want = super::super::widgets::Want {
            tiles: vec!["light.living_room".into(), "cover.living_room".into()],
            ..Default::default()
        };
        tx.send(HubCmd::SetWidgets(want)).unwrap();
        // The catalogue first, then one subscription with the tiles' entities.
        let get = loop {
            let m = read_frame(&mut s, &mut buf);
            if m["type"] == "get_states" {
                break m;
            }
        };
        send(
            &mut s,
            &json!({"id": get["id"], "type": "result", "success": true, "result": [
                {"entity_id": "light.living_room", "state": "off",
                 "attributes": {"friendly_name": "Living room"}}]}),
        );
        let sub = read_frame(&mut s, &mut buf);
        assert_eq!(sub["type"], "subscribe_entities");
        assert_eq!(
            sub["entity_ids"],
            json!(["cover.living_room", "light.living_room"])
        );
        send(
            &mut s,
            &json!({"id": sub["id"], "type": "event", "event": {"a": {
                "light.living_room": {"s": "off", "a": {"friendly_name": "Living room"}},
                "cover.living_room": {"s": "open", "a": {"device_class": "shutter",
                                                         "current_position": 100}}}}}),
        );
        // (First the empty states of the new subscription, then the event's, 300 ms later.)
        let tiles = wait(&rx, |m| match m {
            HubMsg::Tiles(t) if !t.by_id.is_empty() => Some(t),
            _ => None,
        });
        assert_eq!(tiles.by_id["cover.living_room"].position, Some(100.0));
        // A tap: call_service, Home Assistant says yes; a refused one says why.
        for (id, ok) in [(7u64, true), (8, false)] {
            tx.send(HubCmd::Call {
                id,
                domain: "light".into(),
                service: "turn_on".into(),
                entity_id: "light.living_room".into(),
                data: json!({"brightness_pct": 40}),
            })
            .unwrap();
            let call = read_frame(&mut s, &mut buf);
            assert_eq!(call["type"], "call_service");
            assert_eq!(call["service_data"], json!({"brightness_pct": 40}));
            assert_eq!(call["target"]["entity_id"], "light.living_room");
            let answer = if ok {
                json!({"id": call["id"], "type": "result", "success": true, "result": {}})
            } else {
                json!({"id": call["id"], "type": "result", "success": false,
                       "error": {"code": "not_found", "message": "Service not found"}})
            };
            send(&mut s, &answer);
            let got = wait(&rx, |m| matches!(m, HubMsg::Called { .. }).then_some(m));
            let error = (!ok).then(|| "Service not found".to_string());
            assert_eq!(got, HubMsg::Called { id, ok, error });
        }
        // Not on the closed list: refused here, never sent.
        tx.send(HubCmd::Call {
            id: 9,
            domain: "light".into(),
            service: "turn_off".into(),
            entity_id: "light.living_room".into(),
            data: json!({}),
        })
        .unwrap();
        assert!(matches!(
            wait(&rx, |m| matches!(m, HubMsg::Called { .. }).then_some(m)),
            HubMsg::Called {
                id: 9,
                ok: false,
                ..
            }
        ));
        // The new state comes within TILES_EVERY.
        send(
            &mut s,
            &json!({"id": sub["id"], "type": "event", "event": {"c": {
                "light.living_room": {"+": {"s": "on", "a": {"brightness": 102}}}}}}),
        );
        let at = Instant::now();
        let t = wait(&rx, |m| match m {
            HubMsg::Tiles(t) => Some(t),
            _ => None,
        });
        assert_eq!(t.by_id["light.living_room"].brightness, Some(102.0));
        assert!(at.elapsed() < Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_and_remove_messages() {
        assert_eq!(
            add_message(4, "todo.shopping", "Eggs"),
            json!({"id": 4, "type": "call_service", "domain": "todo", "service": "add_item",
                   "service_data": {"item": "Eggs"}, "target": {"entity_id": "todo.shopping"}})
        );
        assert_eq!(
            remove_message(5, "todo.shopping", "u1")["service_data"],
            json!({"item": "u1"})
        );
    }
}
