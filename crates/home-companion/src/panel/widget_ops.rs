// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The browser panel's requests about the home's widget (all with a device let in): the pages'
//! order, the markets and the other cities, found by a search the body does.
//!
//! | `op` | parameters | answer |
//! |---|---|---|
//! | `widgets.get` | — | `{order:[{kind,on,label}], pages:[…], quotes:[{ref,name,detail}], cities:[…], currency, limits}` |
//! | `widgets.samples` | — | `{samples:[{kind, model}]}`: an example page of every kind (the gallery) |
//! | `undo` | — | puts back what this device removed last (`*.remove`, `player.set ""`) within 15 s; as `widgets.get`; `invalid` (nothing, too late) |
//! | `widgets.order` | `order:[kind…]` (every page once) | as `widgets.get` |
//! | `search.start` | `what: "quote"\|"city"`, `q` | `{id}`; `search_busy` (another search is going), `search_wait` (too many) |
//! | `search.status` | `id` | `{status:"waiting"\|"done"\|"failed", hits:[{ref,name,detail}]}` |
//! | `quotes.add` / `cities.add` | `search`, `ref` | as `widgets.get`; `list_full`, `exists`, `invalid` |
//! | `quotes.remove` / `cities.remove` | `ref` | as `widgets.get` |
//! | `quotes.move` / `cities.move` | `ref`, `to` | as `widgets.get` |
//! | `search.start` | `what: "sensor"\|"player"\|"person"`, `q` | answered at once from Home Assistant's catalogue (ha/widgets.rs) |
//! | `sensors.add` / `sensors.remove` / `sensors.move` | `search`+`ref` / `ref` / `ref`+`to` | as `widgets.get` |
//! | `player.set` | `search`+`ref`, or `ref: ""` (none) | as `widgets.get` |
//! | `people.set` | `refs:[person.*]` (empty: everyone) | as `widgets.get` |
//! | `notes.list` | — | `{notes:[{id,text}]}`; `no_brain` without the brain |
//! | `notes.add` / `notes.edit` / `notes.remove` | `text`, `id` | `{}` (the brain answers, the list follows); `invalid`, `list_full`, `no_brain` |
//!
//! Every answer "as `widgets.get`" carries `pages`: each page in the turn's order as
//! `{id, kind, label, on, status, model}`. `id` is opaque to the panel (a kind's key today);
//! `status` is a line about what it shows; `model` is the page as the screen draws it
//! ([`model`]: `{title, layout: "rows"|"hours", sample, windows:[{aside, rows}]}`, rows of text and
//! numbers, the windows the screen turns through with three free slots), or null when the
//! page has nothing to show (the screen skips it). The panel only draws it: numbers, words and
//! cuts are the screen's own code. A `*.remove` answers `undo: true` too.
//!
//! The panel never sends a name, a symbol or coordinates: it sends the `ref` of a hit of a
//! search this same device made in the last ten minutes, and the body writes what the source
//! said (cleaned). The words searched are checked here: 1-40 characters, no control characters;
//! for the markets letters, digits, spaces and `. - ^ = &`, for the cities letters, spaces and
//! `- ' .`. One search at a time for everyone, and per device one every 2 s and 40 an hour.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::PanelHost;
use super::sessions::{Now, Rng};
use crate::feeds::{
    Choice, CityChoice, MAX_CITIES, MAX_QUOTES, QuoteChoice, SearchState, Source, What,
};
use crate::settings_model::{self as model, Change};
use crate::ui::widgets::{self, Kind, WidgetRow, WidgetView};

/// The longest words a search takes.
pub const MAX_QUERY: usize = 40;
/// Per device: a search at most this often, and this many an hour.
pub const SEARCH_EVERY: Duration = Duration::from_secs(2);
pub const SEARCHES_PER_HOUR: usize = 40;

/// The ops of this file.
pub const OPS: &[&str] = &[
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
];

/// How long a removal can be put back with `undo`.
pub const UNDO_FOR: Duration = Duration::from_secs(15);
/// The free slots the previews are cut to (the normal home with no card).
pub const PREVIEW_SLOTS: usize = 3;

/// What a device removed last (one per device), for `undo`.
#[derive(Debug, Clone, PartialEq)]
pub enum Removed {
    Quote(QuoteChoice),
    City(CityChoice),
    Sensor(String),
    Player(String),
}

/// Notes on the board at most, and their length (the brain checks them again).
pub const MAX_NOTES: usize = 20;
pub const MAX_NOTE: usize = 280;

/// A note's text from the panel: one line (breaks and tabs become spaces), no control or
/// bidirectional characters, spaces squeezed, 1 to [`MAX_NOTE`] characters.
pub fn clean_note(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control()
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
        {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    (1..=MAX_NOTE).contains(&out.chars().count()).then_some(out)
}

/// The words of a search, trimmed, when they are fine for `what`.
pub fn clean_query(what: What, q: &str) -> Option<String> {
    let q = q.trim();
    let n = q.chars().count();
    if n == 0 || n > MAX_QUERY {
        return None;
    }
    let ok = |c: char| match what {
        What::Quote => c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '^' | '=' | '&'),
        What::City => c.is_alphabetic() || matches!(c, ' ' | '-' | '\'' | '.'),
        What::Sensor | What::Player | What::Person => {
            c.is_alphanumeric() || matches!(c, ' ' | '-' | '\'' | '.' | '_' | '(' | ')')
        }
    };
    q.chars().all(ok).then(|| q.to_string())
}

/// Who searched what, and how often.
#[derive(Debug, Default)]
pub struct SearchBook {
    /// Per device (its session's hash): the searches of the last hour.
    times: HashMap<String, Vec<Instant>>,
    /// Per search id: the device, what, when.
    owners: HashMap<String, (String, What, Instant)>,
    /// Per device: the last thing it removed, where it was, when.
    removed: HashMap<String, (Removed, usize, Instant)>,
}

impl SearchBook {
    /// Only the devices still let in keep their searches and counts (one revoked or expired is
    /// forgotten, and its searches can no longer be added from).
    pub fn keep_only(&mut self, alive: &[String]) {
        self.times.retain(|k, _| alive.contains(k));
        self.owners.retain(|_, (m, _, _)| alive.contains(m));
        self.removed.retain(|k, _| alive.contains(k));
    }

    fn remember(&mut self, me: &str, what: Removed, at: usize, now: Instant) {
        self.removed.insert(me.to_string(), (what, at, now));
    }

    /// What this device removed last, if within [`UNDO_FOR`] (taken: it can be put back once).
    fn take_removed(&mut self, me: &str, now: Instant) -> Option<(Removed, usize)> {
        let (r, at, when) = self.removed.remove(me)?;
        (now.duration_since(when) < UNDO_FOR).then_some((r, at))
    }

    fn allow(&mut self, me: &str, now: Instant) -> Result<(), &'static str> {
        let t = self.times.entry(me.to_string()).or_default();
        t.retain(|x| now.duration_since(*x) < Duration::from_secs(3600));
        if t.last()
            .is_some_and(|l| now.duration_since(*l) < SEARCH_EVERY)
            || t.len() >= SEARCHES_PER_HOUR
        {
            return Err("search_wait");
        }
        t.push(now);
        Ok(())
    }

    fn own(&mut self, id: &str, me: &str, what: What, now: Instant) {
        self.owners
            .retain(|_, (_, _, at)| now.duration_since(*at) < crate::feeds::SEARCH_KEPT);
        self.owners.insert(id.into(), (me.into(), what, now));
    }

    /// The search is this device's, for `what`, and not too old.
    fn owned(&self, id: &str, me: &str, what: Option<What>, now: Instant) -> bool {
        self.owners.get(id).is_some_and(|(m, w, at)| {
            m == me
                && what.is_none_or(|x| x == *w)
                && now.duration_since(*at) < crate::feeds::SEARCH_KEPT
        })
    }
}

fn err(code: &str) -> Value {
    json!({"ok": false, "error": code})
}

fn str_of<'a>(req: &'a Value, k: &str) -> &'a str {
    req.get(k).and_then(Value::as_str).unwrap_or("")
}

fn quote_detail(q: &QuoteChoice) -> String {
    match q.source {
        Source::Yahoo => q.id.clone(),
        Source::Coingecko => "Crypto".into(),
    }
}

fn city_detail(c: &CityChoice) -> String {
    [c.region.as_str(), c.country.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A row of a page for the panel: only the fields it has.
fn row_json(r: &WidgetRow) -> Value {
    let mut o = serde_json::Map::new();
    for (k, v) in [
        ("left", &r.left),
        ("text", &r.text),
        ("right", &r.right),
        ("note", &r.note),
        ("change", &r.change),
    ] {
        if !v.is_empty() {
            o.insert(k.into(), json!(v));
        }
    }
    if r.trend != 0 {
        o.insert("trend".into(), json!(r.trend));
    }
    if !r.curve.is_empty() {
        let c: Vec<f64> = r
            .curve
            .iter()
            .take(crate::ui::widgets::CURVE_POINTS)
            .map(|x| (f64::from(x.clamp(0.0, 1.0)) * 1000.0).round() / 1000.0)
            .collect();
        o.insert("curve".into(), json!(c));
    }
    if r.wait {
        o.insert("wait".into(), json!(true));
    }
    Value::Object(o)
}

/// A page as the panel draws it: its title, its layout ("rows", "hours", or "weather" with the
/// weather's big block in `hero`), the windows the screen turns through with [`PREVIEW_SLOTS`]
/// free (each with its header's right side and its rows), whether it is an example.
pub fn model(v: &WidgetView, sample: bool) -> Value {
    let windows: Vec<Value> = (0..widgets::windows(v, PREVIEW_SLOTS))
        .filter_map(|i| widgets::fit(v, PREVIEW_SLOTS, i as u32))
        .map(|f| json!({"aside": f.aside, "rows": f.rows.iter().map(row_json).collect::<Vec<_>>()}))
        .collect();
    let layout = if v.hero.is_some() {
        "weather"
    } else if v.columns {
        "hours"
    } else {
        "rows"
    };
    let mut m = json!({"title": v.title, "layout": layout, "sample": sample, "windows": windows});
    if let Some(h) = &v.hero {
        m["hero"] = json!({"label": h.label, "kind": h.kind, "temp": h.temp, "range": h.range,
            "tomorrow": h.tomorrow});
    }
    m
}

/// The line under a page: what it shows and where from.
fn status(host: &dyn PanelHost, kind: Kind, word: &dyn Fn(&str) -> String) -> String {
    let count = |n: usize, key: &str| {
        if n == 0 {
            word(&format!("panel_status_{key}_none"))
        } else {
            word(&format!("panel_status_{key}")).replace("{n}", &n.to_string())
        }
    };
    match kind {
        Kind::Quotes => count(host.widget_quotes().len(), "quotes"),
        Kind::Cities => count(host.widget_cities().len(), "cities"),
        Kind::Sensors => count(host.widget_ha().sensors.len(), "sensors"),
        Kind::Notes => count(host.notes().map_or(0, |n| n.len()), "notes"),
        Kind::Home => count(host.widget_ha().people.map_or(0, |p| p.len()), "home"),
        Kind::Playing => {
            let id = host.widget_ha().player;
            if id.is_empty() {
                word("panel_status_playing_none")
            } else {
                let name = host
                    .ha_catalog()
                    .into_iter()
                    .find(|e| e.id == id)
                    .map_or(id, |e| e.name);
                word("panel_status_playing").replace("{player}", &name)
            }
        }
        Kind::Shopping => {
            let names: Vec<String> = host.shopping().into_iter().map(|l| l.name).collect();
            if names.is_empty() {
                word("panel_status_shopping_none")
            } else {
                names.join(", ")
            }
        }
        Kind::Next => word("panel_status_next"),
        Kind::Weather => word("panel_status_weather"),
        Kind::Check => word("panel_status_check"),
    }
}

/// Every page in the turn's order, with its preview (`pages` of the answers).
pub fn pages(host: &dyn PanelHost, lang: &str) -> Value {
    let word = host.words(lang);
    let views = host.widget_views(lang);
    let list: Vec<Value> = host
        .widget_pages()
        .iter()
        .map(|(k, on)| {
            let m = views
                .iter()
                .find(|(x, _, _)| x == k)
                .and_then(|(_, v, sample)| v.as_ref().map(|v| model(v, *sample)));
            json!({"id": k.key(), "kind": k.key(), "on": on,
                   "label": word(&format!("widget_{}", k.key())),
                   "status": status(host, *k, &*word), "model": m})
        })
        .collect();
    Value::Array(list)
}

/// What `widgets.get` answers.
pub fn state(host: &dyn PanelHost, lang: &str) -> Value {
    let word = host.words(lang);
    let order: Vec<Value> = host
        .widget_pages()
        .iter()
        .map(|(k, on)| json!({"kind": k.key(), "on": on, "label": word(&format!("widget_{}", k.key()))}))
        .collect();
    let quotes: Vec<Value> = host
        .widget_quotes()
        .iter()
        .map(|q| json!({"ref": q.key(), "name": q.name, "detail": quote_detail(q)}))
        .collect();
    let cities: Vec<Value> = host
        .widget_cities()
        .iter()
        .map(|c| json!({"ref": c.key(), "name": c.name, "detail": city_detail(c)}))
        .collect();
    let currency = model::get(host, "widgets_currency").unwrap_or(json!("usd"));
    // Home Assistant's side: the entities chosen (with the catalogue's names), everyone who
    // could be shown. Only ids, names and units: nothing else of an entity leaves the body.
    host.ha_catalog_refresh();
    let catalog = host.ha_catalog();
    let named = |id: &str| -> Value {
        let e = catalog.iter().find(|e| e.id == id);
        json!({"ref": id, "name": e.map_or(id, |e| e.name.as_str()),
               "detail": e.map_or(String::new(), |e| e.unit.clone())})
    };
    let ha = host.widget_ha();
    let sensors: Vec<Value> = ha.sensors.iter().map(|id| named(id)).collect();
    let player = (!ha.player.is_empty()).then(|| named(&ha.player));
    let all_people: Vec<Value> = catalog
        .iter()
        .filter(|e| e.domain() == "person")
        .map(|e| json!({"ref": e.id, "name": e.name}))
        .collect();
    let showing = host.widget_showing().map(|k| k.key());
    json!({"ok": true, "order": order, "showing": showing, "pages": pages(host, lang), "quotes": quotes, "cities": cities, "currency": currency,
           "sensors": sensors, "player": player,
           "people": {"all": all_people, "chosen": ha.people.unwrap_or_default()},
           "ha": !catalog.is_empty(),
           "limits": {"quotes": MAX_QUOTES, "cities": MAX_CITIES, "sensors": crate::ha::widgets::MAX_SENSORS,
                      "query": MAX_QUERY}})
}

/// A search id: 16 random bytes in hex.
fn new_id(rng: Rng) -> Option<String> {
    let mut b = [0u8; 16];
    rng(&mut b).then(|| b.iter().map(|x| format!("{x:02x}")).collect())
}

/// An item moved to `to` (clamped), the others in their order.
fn moved<T: Clone>(list: &[T], from: usize, to: usize) -> Vec<T> {
    let mut v = list.to_vec();
    let x = v.remove(from);
    v.insert(to.min(v.len()), x);
    v
}

/// Who asks: the device's session hash, the reader's language, the time.
pub struct Caller<'a> {
    pub me: &'a str,
    pub lang: &'a str,
    pub now: Now,
}

/// One of this file's requests → its answer.
pub fn handle(
    host: &dyn PanelHost,
    book: &mut SearchBook,
    who: &Caller,
    op: &str,
    req: &Value,
    rng: Rng,
) -> Value {
    let (me, lang, t) = (who.me, who.lang, who.now.mono);
    match op {
        "widgets.get" => state(host, lang),
        "widgets.order" => {
            let Some(list) = req.get("order").and_then(Value::as_array) else {
                return err("invalid");
            };
            let mut order: Vec<Kind> = Vec::new();
            for v in list {
                match v.as_str().and_then(Kind::from_key) {
                    Some(k) if !order.contains(&k) => order.push(k),
                    _ => return err("invalid"),
                }
            }
            if order.len() != Kind::ALL.len() {
                return err("invalid");
            }
            model::commit(host, Change::WidgetOrder(order));
            state(host, lang)
        }
        "search.start" => {
            let what = match str_of(req, "what") {
                "quote" => What::Quote,
                "city" => What::City,
                "sensor" => What::Sensor,
                "player" => What::Player,
                "person" => What::Person,
                _ => return err("invalid"),
            };
            let Some(q) = clean_query(what, str_of(req, "q")) else {
                return err("invalid");
            };
            if let Err(e) = book.allow(me, t) {
                return err(e);
            }
            let Some(id) = new_id(rng) else {
                return err("error");
            };
            if !host.search_start(&id, what, &q, lang) {
                return err("search_busy");
            }
            book.own(&id, me, what, t);
            json!({"ok": true, "id": id})
        }
        "search.status" => {
            let id = str_of(req, "id");
            if !book.owned(id, me, None, t) {
                return err("invalid");
            }
            match host.search_state(id) {
                SearchState::Waiting => json!({"ok": true, "status": "waiting"}),
                SearchState::Done(found) => {
                    let rows = host.search_rows(id, lang);
                    let hits: Vec<Value> = found
                        .iter()
                        .map(|f| {
                            let mut h = json!({"ref": f.hit.key, "name": f.hit.name, "detail": f.hit.detail});
                            if let Some((_, r)) = rows
                                .as_ref()
                                .and_then(|r| r.iter().find(|(k, _)| *k == f.hit.key))
                            {
                                h["row"] = row_json(r);
                            }
                            h
                        })
                        .collect();
                    json!({"ok": true, "status": "done", "rows": rows.is_some(), "hits": hits})
                }
                SearchState::Failed | SearchState::Unknown => {
                    json!({"ok": true, "status": "failed"})
                }
            }
        }
        "quotes.add" | "cities.add" => {
            let city = op == "cities.add";
            let what = if city { What::City } else { What::Quote };
            let (search, key) = (str_of(req, "search"), str_of(req, "ref"));
            if !book.owned(search, me, Some(what), t) {
                return err("invalid");
            }
            match host.search_choice(search, key) {
                Some(Choice::Quote(q)) if !city => {
                    let mut list = host.widget_quotes();
                    if list.iter().any(|x| x.key() == q.key()) {
                        return err("exists");
                    }
                    if list.len() >= MAX_QUOTES {
                        return err("list_full");
                    }
                    if !q.valid() {
                        return err("invalid");
                    }
                    list.push(q);
                    model::commit(host, Change::Quotes(list));
                }
                Some(Choice::City(c)) if city => {
                    let mut list = host.widget_cities();
                    if list.iter().any(|x| x.id == c.id) {
                        return err("exists");
                    }
                    if list.len() >= MAX_CITIES {
                        return err("list_full");
                    }
                    if !c.valid() {
                        return err("invalid");
                    }
                    list.push(c);
                    model::commit(host, Change::Cities(list));
                }
                _ => return err("invalid"),
            }
            state(host, lang)
        }
        "quotes.remove" | "quotes.move" => {
            let list = host.widget_quotes();
            let Some(i) = list.iter().position(|q| q.key() == str_of(req, "ref")) else {
                return err("invalid");
            };
            let new = if op == "quotes.remove" {
                let mut l = list;
                book.remember(me, Removed::Quote(l.remove(i)), i, t);
                l
            } else {
                match req.get("to").and_then(Value::as_u64) {
                    Some(to) if (to as usize) < list.len() => moved(&list, i, to as usize),
                    _ => return err("invalid"),
                }
            };
            model::commit(host, Change::Quotes(new));
            removed(state(host, lang), op)
        }
        "cities.remove" | "cities.move" => {
            let list = host.widget_cities();
            let Some(i) = list.iter().position(|c| c.key() == str_of(req, "ref")) else {
                return err("invalid");
            };
            let new = if op == "cities.remove" {
                let mut l = list;
                book.remember(me, Removed::City(l.remove(i)), i, t);
                l
            } else {
                match req.get("to").and_then(Value::as_u64) {
                    Some(to) if (to as usize) < list.len() => moved(&list, i, to as usize),
                    _ => return err("invalid"),
                }
            };
            model::commit(host, Change::Cities(new));
            removed(state(host, lang), op)
        }
        "sensors.add" | "player.set" => {
            let what = if op == "sensors.add" {
                What::Sensor
            } else {
                What::Player
            };
            let mut ha = host.widget_ha();
            let key = str_of(req, "ref");
            let cleared = op == "player.set" && key.is_empty();
            if cleared {
                if ha.player.is_empty() {
                    return state(host, lang);
                }
                let old = std::mem::take(&mut ha.player);
                book.remember(me, Removed::Player(old), 0, t);
            } else {
                let search = str_of(req, "search");
                if !book.owned(search, me, Some(what), t) {
                    return err("invalid");
                }
                let Some(Choice::Entity(id)) = host.search_choice(search, key) else {
                    return err("invalid");
                };
                if op == "player.set" {
                    ha.player = id;
                } else if ha.sensors.contains(&id) {
                    return err("exists");
                } else if ha.sensors.len() >= crate::ha::widgets::MAX_SENSORS {
                    return err("list_full");
                } else {
                    ha.sensors.push(id);
                }
            }
            model::commit(host, Change::WidgetHa(ha));
            let st = state(host, lang);
            if cleared {
                removed(st, "player.clear")
            } else {
                st
            }
        }
        "sensors.remove" | "sensors.move" => {
            let mut ha = host.widget_ha();
            let Some(i) = ha.sensors.iter().position(|x| x == str_of(req, "ref")) else {
                return err("invalid");
            };
            if op == "sensors.remove" {
                book.remember(me, Removed::Sensor(ha.sensors.remove(i)), i, t);
            } else {
                match req.get("to").and_then(Value::as_u64) {
                    Some(to) if (to as usize) < ha.sensors.len() => {
                        ha.sensors = moved(&ha.sensors, i, to as usize)
                    }
                    _ => return err("invalid"),
                }
            }
            model::commit(host, Change::WidgetHa(ha));
            removed(state(host, lang), op)
        }
        "people.set" => {
            let Some(refs) = req.get("refs").and_then(Value::as_array) else {
                return err("invalid");
            };
            let catalog = host.ha_catalog();
            let mut chosen: Vec<String> = Vec::new();
            for r in refs {
                match r.as_str() {
                    Some(id)
                        if catalog.iter().any(|e| e.id == id && e.domain() == "person")
                            && !chosen.iter().any(|c| c == id) =>
                    {
                        chosen.push(id.to_string())
                    }
                    _ => return err("invalid"),
                }
            }
            chosen.truncate(crate::ha::widgets::MAX_PEOPLE);
            let mut ha = host.widget_ha();
            ha.people = Some(chosen);
            model::commit(host, Change::WidgetHa(ha));
            state(host, lang)
        }
        "notes.list" => match host.notes() {
            Some(n) => {
                json!({"ok": true, "notes": n.iter().map(|(id, text)| json!({"id": id, "text": text})).collect::<Vec<_>>()})
            }
            None => err("no_brain"),
        },
        "notes.add" | "notes.edit" | "notes.remove" => {
            let Some(notes) = host.notes() else {
                return err("no_brain");
            };
            let id = req.get("id").and_then(Value::as_i64);
            let known = id.is_some_and(|id| notes.iter().any(|(n, _)| *n == id));
            match op {
                "notes.remove" if known => host.note_remove(id.unwrap_or(0)),
                "notes.remove" => return err("invalid"),
                _ => {
                    let Some(text) = clean_note(str_of(req, "text")) else {
                        return err("invalid");
                    };
                    if op == "notes.add" {
                        if notes.len() >= MAX_NOTES {
                            return err("list_full");
                        }
                        host.note_add(&text);
                    } else if known {
                        host.note_edit(id.unwrap_or(0), &text);
                    } else {
                        return err("invalid");
                    }
                }
            }
            json!({"ok": true})
        }
        // Only the page on the screen now: cheap enough to ask every few seconds.
        "widgets.showing" => {
            json!({"ok": true, "showing": host.widget_showing().map(|k| k.key())})
        }
        "widgets.samples" => {
            let samples: Vec<Value> = host
                .widget_samples(lang)
                .iter()
                .map(|v| json!({"kind": v.kind.key(), "model": model(v, true)}))
                .collect();
            json!({"ok": true, "samples": samples})
        }
        "undo" => {
            let Some((what, at)) = book.take_removed(me, t) else {
                return err("invalid");
            };
            match what {
                Removed::Quote(q) => {
                    let mut list = host.widget_quotes();
                    if !list.iter().any(|x| x.key() == q.key()) {
                        if list.len() >= MAX_QUOTES {
                            return err("list_full");
                        }
                        list.insert(at.min(list.len()), q);
                        model::commit(host, Change::Quotes(list));
                    }
                }
                Removed::City(c) => {
                    let mut list = host.widget_cities();
                    if !list.iter().any(|x| x.id == c.id) {
                        if list.len() >= MAX_CITIES {
                            return err("list_full");
                        }
                        list.insert(at.min(list.len()), c);
                        model::commit(host, Change::Cities(list));
                    }
                }
                Removed::Sensor(id) => {
                    let mut ha = host.widget_ha();
                    if !ha.sensors.contains(&id) {
                        if ha.sensors.len() >= crate::ha::widgets::MAX_SENSORS {
                            return err("list_full");
                        }
                        let at = at.min(ha.sensors.len());
                        ha.sensors.insert(at, id);
                        model::commit(host, Change::WidgetHa(ha));
                    }
                }
                Removed::Player(id) => {
                    let mut ha = host.widget_ha();
                    ha.player = id;
                    model::commit(host, Change::WidgetHa(ha));
                }
            }
            state(host, lang)
        }
        _ => err("unknown_op"),
    }
}

/// An answer to a removal: it can be put back (`undo`).
fn removed(mut answer: Value, op: &str) -> Value {
    if answer["ok"] == true && (op.ends_with(".remove") || op == "player.clear") {
        answer["undo"] = json!(true);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_checked() {
        assert_eq!(
            clean_query(What::Quote, "  S&P 500 "),
            Some("S&P 500".into())
        );
        assert_eq!(clean_query(What::Quote, "^GSPC"), Some("^GSPC".into()));
        assert_eq!(
            clean_query(What::Quote, "EURUSD=X"),
            Some("EURUSD=X".into())
        );
        assert_eq!(
            clean_query(What::City, "Reggio nell'Emilia"),
            Some("Reggio nell'Emilia".into())
        );
        assert_eq!(clean_query(What::City, "Zürich"), Some("Zürich".into()));
        assert_eq!(clean_query(What::City, "東京"), Some("東京".into()));
        for bad in ["", "   ", "a\nb", "x/../y", "<script>", "a;b", "😀"] {
            assert!(clean_query(What::Quote, bad).is_none(), "{bad}");
            assert!(clean_query(What::City, bad).is_none(), "{bad}");
        }
        assert!(clean_query(What::City, "Berlin 2").is_none());
        assert!(clean_query(What::Quote, "\u{202e}AAPL").is_none());
        assert!(clean_query(What::Quote, &"a".repeat(40)).is_some());
        assert!(clean_query(What::Quote, &"a".repeat(41)).is_none());
        // An option-looking query stays words: it only ever travels encoded in --url.
        assert_eq!(clean_query(What::Quote, "-o x"), Some("-o x".into()));
    }

    #[test]
    fn searches_are_limited_per_device() {
        let mut b = SearchBook::default();
        let t0 = Instant::now();
        assert!(b.allow("a", t0).is_ok());
        assert_eq!(
            b.allow("a", t0 + Duration::from_secs(1)),
            Err("search_wait")
        );
        assert!(b.allow("b", t0 + Duration::from_secs(1)).is_ok());
        let mut t = t0;
        for _ in 1..SEARCHES_PER_HOUR {
            t += SEARCH_EVERY;
            assert!(b.allow("a", t).is_ok());
        }
        assert_eq!(b.allow("a", t + SEARCH_EVERY), Err("search_wait"));
        assert!(b.allow("a", t0 + Duration::from_secs(3601)).is_ok());
        b.own("s", "a", What::City, t0);
        b.own("t", "c", What::City, t0);
        b.keep_only(&["a".to_string()]);
        assert!(
            !b.owned("t", "c", None, t0),
            "a device revoked: its searches go"
        );
        assert!(!b.times.contains_key("b"));
        assert!(b.owned("s", "a", Some(What::City), t0));
        assert!(!b.owned("s", "b", None, t0));
        assert!(!b.owned("s", "a", Some(What::Quote), t0));
        assert!(!b.owned("s", "a", None, t0 + Duration::from_secs(601)));
    }

    #[test]
    fn notes_are_cleaned() {
        assert_eq!(
            clean_note(" Buy\nbread \t now "),
            Some("Buy bread now".into())
        );
        assert_eq!(clean_note("\u{202e}x"), Some("x".into()));
        assert!(clean_note("\n").is_none());
        assert!(clean_note(&"é".repeat(MAX_NOTE)).is_some());
        assert!(clean_note(&"é".repeat(MAX_NOTE + 1)).is_none());
    }

    #[test]
    fn moving() {
        assert_eq!(moved(&[1, 2, 3], 0, 2), [2, 3, 1]);
        assert_eq!(moved(&[1, 2, 3], 2, 0), [3, 1, 2]);
        assert_eq!(moved(&[1, 2, 3], 1, 1), [1, 2, 3]);
    }
}
