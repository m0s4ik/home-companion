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
//! Which lists are subscribed: the user's choice (Settings: `lists`), else every `todo.*`.

use std::collections::BTreeMap;
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
    /// The user's choice of lists (empty = all of them).
    SetLists(Vec<String>),
    /// An event for Home Assistant's bus (`fire_event`, needs an admin user); `device` is added.
    Fire { event_type: String, data: Value },
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

// ---- the thread ------------------------------------------------------------------------------

fn pause(d: Duration, stop: fn() -> bool) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if stop() {
            return false;
        }
        std::thread::sleep((until - Instant::now()).min(Duration::from_millis(250)));
    }
    !stop()
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
    let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(300));
    loop {
        if stop() {
            return;
        }
        // Checked again every minute: a link made on the screen is picked up by itself.
        let token = match cfg.auth.access_token() {
            Ok(t) => t,
            Err(e) => {
                log::debug!("cards and lists: {e}");
                if !pause(Duration::from_secs(60), stop) {
                    return;
                }
                continue;
            }
        };
        let started = Instant::now();
        match session(&cfg, &ep, &token, &tx, &cmds, &mut chosen, stop) {
            Ok(()) => return,
            Err(why) => {
                if started.elapsed() >= Duration::from_secs(60) {
                    backoff.reset();
                }
                log::info!(event = "ha_hub"; "cards and lists: {why}, retrying");
            }
        }
        if !pause(backoff.next_delay(), stop) {
            return;
        }
    }
}

/// One connection; Ok = stop (or the UI is gone), Err = lost.
fn session(
    cfg: &HubConfig,
    ep: &Endpoint,
    token: &str,
    tx: &Sender<HubMsg>,
    cmds: &Receiver<HubCmd>,
    chosen: &mut Vec<String>,
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
    // A command (an event to fire) waits at most this long for the next read to end.
    ws.set_read_timeout(Duration::from_millis(100))
        .map_err(lost)?;
    let mut fire_ids: Vec<u64> = Vec::new();
    let mut fire_refused = false;
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    loop {
        if stop() {
            ws.close();
            return Ok(());
        }
        let mut changed_lists = false;
        loop {
            match cmds.try_recv() {
                Ok(HubCmd::Done { list, uid }) => {
                    ws.send_text(&tick_message(take_id(), &list, &uid).to_string())
                        .map_err(lost)?;
                    log::info!(event = "ha_shopping"; "item ticked on the screen");
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
        match ws.read().map_err(lost)? {
            Some(Message::Text(t)) => {
                last_rx = Instant::now();
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                let id = v.get("id").and_then(Value::as_u64);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use events::HaCard;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn named(v: &[(&str, &str)]) -> Vec<Named> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
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
        // First the empty list of lists (nothing known yet), then what exists, then the lists.
        while got.len() < 3 {
            got.push(rx.recv_timeout(Duration::from_secs(5)).unwrap());
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
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
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
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
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
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                HubMsg::Screen(want)
            );
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
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            HubMsg::Lists(named(&[("todo.casa", "Casa")]))
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
