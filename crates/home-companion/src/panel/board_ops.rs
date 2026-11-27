// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The browser panel's requests about the board (crate::ui::board), all with a device let in:
//! its pages and custom widgets, their tiles, a search for the tiles' entities and one for the
//! icons.
//!
//! | `op` | parameters | answer |
//! |---|---|---|
//! | `board.get` | — | [`state`]: every canvas with its tiles as the screen draws them now |
//! | `board.set` | `canvas: {id, tiles:[Tile]}` (one whole canvas) | as `board.get` with `dropped: [reason]` (what the body left out), `undo: true` |
//! | `board.page.add` | `name` ("" none) | as `board.get` with `added: id`; `list_full` |
//! | `board.page.remove` | `canvas` | as `board.get`, `undo: true` |
//! | `board.page.name` | `canvas`, `name` | as `board.get` |
//! | `board.page.order` | `order: [id…]` (every page once) | as `board.get` |
//! | `board.widget.add` | `name` | as `board.get` with `added: id`: a custom widget, a page of the home's turn, on; `list_full` |
//! | `board.widget.remove` | `canvas` | as `board.get`, `undo: true` (its page leaves the turn) |
//! | `board.widget.name` | `canvas`, `name` (its page's name in the turn too) | as `board.get` |
//! | `board.search` | `q` (may be empty), `kind` ("" every kind) | `{hits:[{entity_id, name, detail, kind, icon, path, confirm, allowed}]}`; `search_wait` |
//! | `board.icons` | `q` | `{icons:[{name, path}], all}` (≤ [`MAX_ICON_HITS`]; `all`: the bundle's file was read); `search_wait` |
//! | `board.undo` | — | puts back the board as it was before this device's last change (within 15 s, if nobody changed it since); as `board.get`; `invalid` |
//!
//! **What the body keeps to itself.** A tile comes as the panel sent it, but the board that is
//! saved is [`board::validate`]'s: a never-domain entity, a lock, an alarm, a garage door or a
//! gate never stays, a size or a kind that is not the tile's goes, an "Advanced" service of a
//! never-domain or data naming a target is left out. The search offers only what
//! [`board::offered`] allows, with the existing search's limits (one search every 2 s per
//! device, 40 an hour; the empty one, the first list, is free: it reads the catalogue in
//! memory). A custom widget is a page of the home's turn ("custom-2" is the canvas `w2`): adding
//! or removing it, or naming it, changes the turn with it ([`turn_changes`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::protocol::PanelHost;
use super::widget_ops::{Caller, SearchBook};
use crate::feeds::What;
use crate::settings_model::{self as model, Change};
use crate::ui::board::{self, Board, BoardWords, Canvas, Pending, Tile, TileKind, draw, icons};
use crate::ui::widgets::{Kind as WidgetKind, MAX_PAGE_NAME, Page};

/// The ops of this file.
pub const OPS: &[&str] = &[
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
];

/// How long a change can be put back with `board.undo`.
pub const UNDO_FOR: Duration = Duration::from_secs(15);
/// Icons a search answers at most.
pub const MAX_ICON_HITS: usize = 40;
/// Per device: an icons' search at most this often (typed letter by letter, the panel waits
/// 250 ms after the last key).
pub const ICONS_EVERY: Duration = Duration::from_millis(200);
/// The longest words an icons' search takes.
pub const MAX_ICON_QUERY: usize = 32;

/// Per device: the board before and after its last change (for `board.undo`), when; the last
/// icons' search.
#[derive(Debug, Default)]
pub struct BoardBook {
    last: HashMap<String, (Board, Board, Instant)>,
    icons_at: HashMap<String, Instant>,
}

impl BoardBook {
    /// Only the devices still let in are remembered.
    pub fn keep_only(&mut self, alive: &[String]) {
        self.last.retain(|k, _| alive.contains(k));
        self.icons_at.retain(|k, _| alive.contains(k));
    }
}

fn err(code: &str) -> Value {
    json!({"ok": false, "error": code})
}

fn str_of<'a>(req: &'a Value, k: &str) -> &'a str {
    req.get(k).and_then(Value::as_str).unwrap_or("")
}

fn kind_key(k: TileKind) -> &'static str {
    draw::kind_key(k)
}

fn kind_from(s: &str) -> Option<TileKind> {
    match s {
        "button" => Some(TileKind::Button),
        "sensor" => Some(TileKind::Sensor),
        "level" => Some(TileKind::Level),
        _ => None,
    }
}

/// The kinds in the order a new tile takes the first one allowed.
const KINDS: [TileKind; 3] = [TileKind::Button, TileKind::Sensor, TileKind::Level];

/// The kinds a tile of `entity_id` may be: what the search would offer (Home Assistant's
/// catalogue gives the device class and the name to the gates), or by its domain when the
/// catalogue does not know it.
pub fn allowed_kinds(entity_id: &str, catalog: &[crate::ha::widgets::Entity]) -> Vec<TileKind> {
    let e = catalog.iter().find(|e| e.id == entity_id);
    KINDS
        .into_iter()
        .filter(|k| match e {
            Some(e) => board::offered(*k, e),
            None => {
                let domain = entity_id.split('.').next().unwrap_or("");
                k.domains().contains(&domain)
                    && safety::SafetyCfg::default().entity_gate(entity_id, "", "")
                        != safety::Gate::Never
            }
        })
        .collect()
}

/// `{kind: [[w, h]…]}` of the kinds allowed.
fn allowed_json(kinds: &[TileKind]) -> Value {
    let mut o = serde_json::Map::new();
    for k in kinds {
        let spans: Vec<Value> = k.spans().iter().map(|(w, h)| json!([w, h])).collect();
        o.insert(kind_key(*k).into(), Value::Array(spans));
    }
    Value::Object(o)
}

/// The device class of an entity, as the hub or the catalogue knows it.
fn class_of(entity: &str, host: &dyn PanelHost, catalog: &[crate::ha::widgets::Entity]) -> String {
    host.board_states()
        .by_id
        .get(entity)
        .map(|s| s.device_class.clone())
        .filter(|c| !c.is_empty())
        .or_else(|| {
            catalog
                .iter()
                .find(|e| e.id == entity)
                .map(|e| e.device_class.clone())
        })
        .unwrap_or_default()
}

/// A canvas with its tiles as the screen draws them now (the preview), each with what the
/// editor needs: the tile as saved, the kinds and sizes its entity allows, the default of "Ask
/// first".
fn canvas_json(
    host: &dyn PanelHost,
    c: &Canvas,
    widget: bool,
    words: &BoardWords,
    catalog: &[crate::ha::widgets::Entity],
    label: &str,
) -> Value {
    let states = host.board_states();
    let curves = host.board_curves();
    let icons = host.board_icons();
    let views = board::view(
        c,
        &states,
        &curves,
        &Pending::default(),
        words,
        Instant::now(),
    );
    let cells = if widget { draw::WIDGET } else { draw::PAGE };
    let drawn = draw::draws(&views, cells, &icons, false);
    let tiles: Vec<Value> = c
        .tiles
        .iter()
        .zip(views.iter().zip(drawn.iter()))
        .enumerate()
        .map(|(i, (t, (v, d)))| {
            let class = class_of(&t.entity_id, host, catalog);
            let entity_name = catalog
                .iter()
                .find(|e| e.id == t.entity_id)
                .map(|e| e.name.clone())
                .or_else(|| states.by_id.get(&t.entity_id).map(|s| s.name.clone()))
                .filter(|n| !n.is_empty())
                .unwrap_or_default();
            let default = board::needs_confirm(
                &Tile {
                    confirm: None,
                    ..t.clone()
                },
                &class,
                &entity_name,
            );
            json!({
                "index": i,
                "tile": t,
                "entity_name": entity_name,
                "allowed": allowed_json(&allowed_kinds(&t.entity_id, catalog)),
                "confirm_default": default,
                "draw": {
                    "state": d.state, "icon": v.icon, "path": d.icon, "name": d.name,
                    "value": d.value, "status": d.status, "range": d.range, "level": d.level,
                    "curve": d.curve, "tappable": d.tappable, "confirm": v.confirm,
                },
            })
        })
        .collect();
    let mut o = json!({"id": c.id, "type": if widget { "widget" } else { "page" },
                       "name": c.name, "label": label, "tiles": tiles});
    if widget && let Some(n) = board::widget_number(&c.id) {
        let p = Page::new(WidgetKind::Custom, n);
        let on = host
            .widget_pages()
            .iter()
            .find(|(x, _)| *x == p)
            .map(|(_, on)| *on);
        o["page"] = json!(p.key());
        o["on"] = json!(on);
    }
    o
}

/// What `board.get` answers: every canvas (pages, then custom widgets) with its tiles, the
/// grids and the cells (in the device's pixels), the kinds, the limits.
pub fn state(host: &dyn PanelHost, lang: &str) -> Value {
    let b = host.board();
    let word = host.words(lang);
    let words = BoardWords::new(|k| word(k));
    host.ha_catalog_refresh();
    let catalog = host.ha_catalog();
    let mut canvases: Vec<Value> = Vec::new();
    for (i, c) in b.pages.iter().enumerate() {
        let label = if c.name.is_empty() {
            format!("{} {}", word("board_title"), i + 1)
        } else {
            c.name.clone()
        };
        canvases.push(canvas_json(host, c, false, &words, &catalog, &label));
    }
    for c in &b.widgets {
        let n = board::widget_number(&c.id).unwrap_or(1);
        let label = Page::new(WidgetKind::Custom, n).label(&word("widget_custom"), &c.name);
        canvases.push(canvas_json(host, c, true, &words, &catalog, &label));
    }
    let cells = |c: draw::Cells| json!({"width": c.width, "height": c.height, "gap": c.gap});
    let kinds: serde_json::Map<String, Value> = KINDS
        .iter()
        .map(|k| {
            (
                kind_key(*k).to_string(),
                json!({"domains": k.domains(), "spans": k.spans()}),
            )
        })
        .collect();
    json!({"ok": true, "canvases": canvases,
           "grids": {"page": board::PAGE_GRID, "widget": board::WIDGET_GRID},
           "cells": {"page": cells(draw::PAGE), "widget": cells(draw::WIDGET)},
           "kinds": kinds, "ha": !catalog.is_empty(),
           "limits": {"pages": board::MAX_PAGES, "widgets": board::MAX_WIDGETS,
                      "page_tiles": board::MAX_PAGE_TILES, "widget_tiles": board::MAX_WIDGET_TILES,
                      "name": board::MAX_NAME, "widget_name": MAX_PAGE_NAME,
                      "data": board::MAX_SERVICE_DATA, "icon": board::MAX_ICON,
                      "query": super::widget_ops::MAX_QUERY}})
}

/// The changes to the home's turn that follow the board's custom widgets from `before` to
/// `after`: a widget added is a page added (on; the first one, "custom", is always in the turn
/// and is switched on), a widget removed is its page removed (the first one's stays, with
/// nothing to show), a widget renamed is its page renamed.
pub fn turn_changes(
    before: &Board,
    after: &Board,
    pages: &[(Page, bool)],
) -> (Vec<Change>, Option<Page>) {
    let mut out = Vec::new();
    let mut switch_on = None;
    for n in 1..=board::MAX_WIDGETS as u8 {
        let p = Page::new(WidgetKind::Custom, n);
        let in_turn = pages.iter().find(|(x, _)| *x == p);
        match (before.widget(n), after.widget(n)) {
            (None, Some(c)) => {
                if n == 1 {
                    if in_turn.is_some_and(|(_, on)| !on) {
                        switch_on = Some(p);
                    }
                } else if in_turn.is_none() {
                    out.push(Change::WidgetAdded(p, None, true));
                }
                if !c.name.is_empty() {
                    out.push(Change::WidgetName(p, c.name.clone()));
                }
            }
            (Some(_), None) => {
                if n > 1 {
                    out.push(Change::WidgetRemoved(p));
                } else {
                    out.push(Change::WidgetName(p, String::new()));
                }
            }
            (Some(a), Some(b)) if a.name != b.name => {
                out.push(Change::WidgetName(p, b.name.clone()))
            }
            _ => {}
        }
    }
    (out, switch_on)
}

/// Replaces the board; the home's turn follows its widgets; the device's change remembered for
/// `board.undo`. The reasons of what the body left out.
fn replace(
    host: &dyn PanelHost,
    book: &mut BoardBook,
    me: &str,
    b: Board,
    now: Instant,
) -> Vec<String> {
    let before = host.board();
    let why = host.board_replace(b);
    let after = host.board();
    follow_turn(host, &before, &after);
    book.last.insert(me.to_string(), (before, after, now));
    why
}

fn follow_turn(host: &dyn PanelHost, before: &Board, after: &Board) {
    let (changes, switch_on) = turn_changes(before, after, &host.widget_pages());
    for c in changes {
        model::commit(host, c);
    }
    if let Some(p) = switch_on {
        let mut keys: Vec<String> = host
            .widget_pages()
            .iter()
            .filter(|(_, on)| *on)
            .map(|(x, _)| x.key())
            .collect();
        keys.push(p.key());
        let _ = model::set(host, "widgets", &json!(keys));
    }
}

/// A name from the panel: at most `max` characters once cleaned (the body cleans it again).
fn name_of(req: &Value, max: usize) -> Option<String> {
    let raw = str_of(req, "name");
    let clean = crate::feeds::clean_name(raw).trim().to_string();
    (raw.chars().count() <= max + 8 && clean.chars().count() <= max).then_some(clean)
}

/// A canvas by its id: (a widget?, its index).
fn find(b: &Board, id: &str) -> Option<(bool, usize)> {
    if id.is_empty() {
        return None;
    }
    b.pages
        .iter()
        .position(|c| c.id == id)
        .map(|i| (false, i))
        .or_else(|| b.widgets.iter().position(|c| c.id == id).map(|i| (true, i)))
}

/// The icons whose names hold the words of `q` ("" none), best first: the name itself, then
/// those that start with it, then a part that does ("ceiling-light" for "light"), then the rest;
/// shorter names first.
pub fn rank_icons<'a>(names: impl Iterator<Item = &'a str>, q: &str) -> Vec<&'a str> {
    let q = q.trim().to_ascii_lowercase();
    let words: Vec<&str> = q.split([' ', '-']).filter(|w| !w.is_empty()).collect();
    let mut found: Vec<(u8, usize, &str)> = names
        .filter_map(|n| {
            if words.is_empty() {
                return Some((3, n.len(), n));
            }
            let k = icons::key(n);
            if !words.iter().all(|w| k.contains(&icons::key(w))) {
                return None;
            }
            let qk = icons::key(&q);
            let score = if k == qk {
                0
            } else if k.starts_with(&qk) {
                1
            } else if n.split('-').any(|p| p.starts_with(words[0])) {
                2
            } else {
                3
            };
            Some((score, n.len(), n))
        })
        .collect();
    found.sort();
    found.dedup_by(|a, b| icons::key(a.2) == icons::key(b.2));
    found.into_iter().map(|x| x.2).collect()
}

/// `board.icons`: the bundle's file when there is one, else the icons built in.
fn icons_found(host: &dyn PanelHost, q: &str) -> (Vec<(String, String)>, bool) {
    #[derive(serde::Deserialize)]
    struct File<'a> {
        #[serde(borrow)]
        icons: HashMap<&'a str, &'a str>,
    }
    let text = host
        .board_icons_file()
        .and_then(|p| std::fs::read_to_string(p).ok());
    if let Some(f) = text
        .as_deref()
        .and_then(|t| serde_json::from_str::<File>(t).ok())
    {
        let hits = rank_icons(f.icons.keys().copied(), q)
            .into_iter()
            .take(MAX_ICON_HITS)
            .map(|n| (n.to_string(), f.icons[n].to_string()))
            .collect();
        return (hits, true);
    }
    let hits = rank_icons(icons::builtin_names(), q)
        .into_iter()
        .take(MAX_ICON_HITS)
        .map(|n| (n.to_string(), icons::builtin(n).unwrap_or("").to_string()))
        .collect();
    (hits, false)
}

/// One of this file's requests → its answer.
pub fn handle(
    host: &dyn PanelHost,
    book: &mut BoardBook,
    searches: &mut SearchBook,
    who: &Caller,
    op: &str,
    req: &Value,
) -> Value {
    let (me, lang, t) = (who.me, who.lang, who.now.mono);
    let b = host.board();
    let changed = |book: &mut BoardBook, nb: Board, extra: Option<(&str, Value)>| -> Value {
        let why = replace(host, book, me, nb, t);
        let mut a = state(host, lang);
        a["dropped"] = json!(why);
        a["undo"] = json!(true);
        if let Some((k, v)) = extra {
            a[k] = v;
        }
        a
    };
    match op {
        "board.get" => state(host, lang),
        "board.set" => {
            let Some(c) = req.get("canvas") else {
                return err("invalid");
            };
            let Some((widget, i)) = find(&b, str_of(c, "id")) else {
                return err("invalid");
            };
            let Some(list) = c.get("tiles").and_then(Value::as_array) else {
                return err("invalid");
            };
            let max = if widget {
                board::MAX_WIDGET_TILES
            } else {
                board::MAX_PAGE_TILES
            };
            if list.len() > max {
                return err("list_full");
            }
            let mut tiles = Vec::new();
            for v in list {
                match serde_json::from_value::<Tile>(v.clone()) {
                    Ok(tile) => tiles.push(tile),
                    Err(_) => return err("invalid"),
                }
            }
            let mut nb = b.clone();
            if widget {
                nb.widgets[i].tiles = tiles;
            } else {
                nb.pages[i].tiles = tiles;
            }
            changed(book, nb, None)
        }
        "board.page.add" | "board.widget.add" => {
            let widget = op == "board.widget.add";
            let max_name = if widget {
                MAX_PAGE_NAME
            } else {
                board::MAX_NAME
            };
            let Some(name) = name_of(req, max_name) else {
                return err("invalid");
            };
            let mut nb = b.clone();
            let id = if widget {
                if nb.widgets.len() >= board::MAX_WIDGETS {
                    return err("list_full");
                }
                let Some(n) = (1..=board::MAX_WIDGETS as u8).find(|n| b.widget(*n).is_none())
                else {
                    return err("list_full");
                };
                format!("w{n}")
            } else {
                if nb.pages.len() >= board::MAX_PAGES {
                    return err("list_full");
                }
                (1..)
                    .map(|n| format!("p{n}"))
                    .find(|id| find(&b, id).is_none())
                    .unwrap_or_default()
            };
            let c = Canvas {
                id: id.clone(),
                name,
                tiles: Vec::new(),
            };
            if widget {
                nb.widgets.push(c);
            } else {
                nb.pages.push(c);
            }
            changed(book, nb, Some(("added", json!(id))))
        }
        "board.page.remove" | "board.widget.remove" => {
            let widget = op == "board.widget.remove";
            match find(&b, str_of(req, "canvas")) {
                Some((w, i)) if w == widget => {
                    let mut nb = b.clone();
                    if widget {
                        nb.widgets.remove(i);
                    } else {
                        nb.pages.remove(i);
                    }
                    changed(book, nb, None)
                }
                _ => err("invalid"),
            }
        }
        "board.page.name" | "board.widget.name" => {
            let widget = op == "board.widget.name";
            let max_name = if widget {
                MAX_PAGE_NAME
            } else {
                board::MAX_NAME
            };
            let Some(name) = name_of(req, max_name) else {
                return err("invalid");
            };
            match find(&b, str_of(req, "canvas")) {
                Some((w, i)) if w == widget => {
                    let mut nb = b.clone();
                    if widget {
                        nb.widgets[i].name = name;
                    } else {
                        nb.pages[i].name = name;
                    }
                    changed(book, nb, None)
                }
                _ => err("invalid"),
            }
        }
        "board.page.order" => {
            let Some(list) = req.get("order").and_then(Value::as_array) else {
                return err("invalid");
            };
            let mut pages: Vec<Canvas> = Vec::new();
            for v in list {
                match v
                    .as_str()
                    .and_then(|id| b.pages.iter().find(|c| c.id == id))
                {
                    Some(c) if !pages.iter().any(|p| p.id == c.id) => pages.push(c.clone()),
                    _ => return err("invalid"),
                }
            }
            if pages.len() != b.pages.len() {
                return err("invalid");
            }
            let mut nb = b.clone();
            nb.pages = pages;
            changed(book, nb, None)
        }
        "board.search" => {
            let q = str_of(req, "q").trim();
            let kinds: Vec<TileKind> = match str_of(req, "kind") {
                "" => KINDS.to_vec(),
                k => match kind_from(k) {
                    Some(k) => vec![k],
                    None => return err("invalid"),
                },
            };
            let q = if q.is_empty() {
                String::new()
            } else {
                let Some(q) = super::widget_ops::clean_query(What::Tile(TileKind::Button), q)
                else {
                    return err("invalid");
                };
                // Typed words count against the device's searches; the first list is free.
                if let Err(e) = searches.allow(me, t) {
                    return err(e);
                }
                q
            };
            let catalog = host.ha_catalog();
            let mut hits: Vec<Value> = Vec::new();
            let mut seen: Vec<String> = Vec::new();
            for k in &kinds {
                for f in crate::ha::widgets::search(&catalog, What::Tile(*k), &q) {
                    let id = f.hit.key.clone();
                    if seen.contains(&id) {
                        continue;
                    }
                    seen.push(id.clone());
                    let allowed = allowed_kinds(&id, &catalog);
                    let Some(kind) = allowed.first().copied() else {
                        continue;
                    };
                    let class = class_of(&id, host, &catalog);
                    let domain = id.split('.').next().unwrap_or("");
                    let icon = board::default_icon(domain, &class);
                    let tile = Tile {
                        x: 0,
                        y: 0,
                        w: 1,
                        h: 1,
                        kind,
                        entity_id: id.clone(),
                        name: String::new(),
                        icon: String::new(),
                        confirm: None,
                        service: None,
                        service_data: None,
                    };
                    hits.push(json!({
                        "entity_id": id, "name": f.hit.name, "detail": f.hit.detail,
                        "kind": kind_key(kind), "icon": icon,
                        "path": icons::builtin(icon).unwrap_or(""),
                        "confirm": board::needs_confirm(&tile, &class, &f.hit.name),
                        "allowed": allowed_json(&allowed),
                    }));
                }
            }
            json!({"ok": true, "hits": hits, "ha": !catalog.is_empty()})
        }
        "board.icons" => {
            let q = str_of(req, "q").trim().to_ascii_lowercase();
            let ok = q.chars().count() <= MAX_ICON_QUERY
                && q.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ' ');
            if !ok {
                return err("invalid");
            }
            if book
                .icons_at
                .get(me)
                .is_some_and(|at| t.duration_since(*at) < ICONS_EVERY)
            {
                return err("search_wait");
            }
            book.icons_at.insert(me.to_string(), t);
            let (hits, all) = icons_found(host, &q);
            let list: Vec<Value> = hits
                .into_iter()
                .map(|(name, path)| json!({"name": name, "path": path}))
                .collect();
            json!({"ok": true, "icons": list, "all": all})
        }
        "board.undo" => {
            let Some((before, after, at)) = book.last.remove(me) else {
                return err("invalid");
            };
            if t.duration_since(at) >= UNDO_FOR || host.board() != after {
                return err("invalid");
            }
            host.board_replace(before.clone());
            let now = host.board();
            follow_turn(host, &after, &now);
            state(host, lang)
        }
        _ => err("unknown_op"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::widgets::Entity;

    fn entity(id: &str, name: &str, class: &str) -> Entity {
        Entity {
            id: id.into(),
            name: name.into(),
            unit: String::new(),
            device_class: class.into(),
            statistics: false,
        }
    }

    #[test]
    fn the_kinds_an_entity_allows() {
        let cat = [
            entity("light.lamp", "Lamp", ""),
            entity("cover.garage", "Garage", "garage"),
            entity("cover.living_room", "Shutter", "shutter"),
        ];
        use TileKind::*;
        assert_eq!(allowed_kinds("light.lamp", &cat), [Button, Level]);
        assert!(allowed_kinds("cover.garage", &cat).is_empty());
        assert_eq!(allowed_kinds("cover.living_room", &cat), [Button, Level]);
        assert_eq!(allowed_kinds("sensor.t", &cat), [Sensor]);
        assert_eq!(allowed_kinds("input_number.x", &cat), [Sensor, Level]);
        assert!(allowed_kinds("lock.front", &cat).is_empty());
        assert!(allowed_kinds("alarm_control_panel.home", &cat).is_empty());
        assert!(allowed_kinds("camera.door", &cat).is_empty());
        assert_eq!(allowed_json(&[Level]), json!({"level": [[2, 1], [4, 1]]}));
    }

    #[test]
    fn icons_rank_by_name() {
        let names = [
            "ceiling-light",
            "lightbulb",
            "light-switch",
            "lightbulb-group",
            "flash",
            "lamp",
        ];
        let r = rank_icons(names.iter().copied(), "lightbulb");
        assert_eq!(r, ["lightbulb", "lightbulb-group"]);
        let r = rank_icons(names.iter().copied(), "light");
        assert_eq!(
            r,
            [
                "lightbulb",
                "light-switch",
                "lightbulb-group",
                "ceiling-light"
            ]
        );
        assert_eq!(
            rank_icons(names.iter().copied(), "ceiling light"),
            ["ceiling-light"]
        );
        assert_eq!(rank_icons(names.iter().copied(), "").len(), names.len());
        assert!(rank_icons(names.iter().copied(), "zzz").is_empty());
    }

    fn canvas(id: &str, name: &str) -> Canvas {
        Canvas {
            id: id.into(),
            name: name.into(),
            tiles: Vec::new(),
        }
    }

    #[test]
    fn the_turn_follows_the_widgets() {
        let custom = |n| Page::new(WidgetKind::Custom, n);
        let before = Board::default();
        let after = Board {
            widgets: vec![canvas("w1", "Lights"), canvas("w2", "")],
            ..Board::default()
        };
        // "custom" is in the turn, off: switched on; "custom-2" added, on.
        let pages = [(custom(1), false)];
        let (c, on) = turn_changes(&before, &after, &pages);
        assert_eq!(on, Some(custom(1)));
        assert_eq!(
            c,
            [
                Change::WidgetName(custom(1), "Lights".into()),
                Change::WidgetAdded(custom(2), None, true),
            ]
        );
        // Renamed, then removed: the second's page goes, the first's stays without its name.
        let renamed = Board {
            widgets: vec![canvas("w1", "Lamps"), canvas("w2", "")],
            ..Board::default()
        };
        let pages = [(custom(1), true), (custom(2), true)];
        let (c, on) = turn_changes(&after, &renamed, &pages);
        assert_eq!(
            (c, on),
            (vec![Change::WidgetName(custom(1), "Lamps".into())], None)
        );
        let (c, _) = turn_changes(&renamed, &Board::default(), &pages);
        assert_eq!(
            c,
            [
                Change::WidgetName(custom(1), String::new()),
                Change::WidgetRemoved(custom(2)),
            ]
        );
    }

    #[test]
    fn widget_numbers() {
        assert_eq!(board::widget_number("w1"), Some(1));
        assert_eq!(board::widget_number("w4"), Some(4));
        assert_eq!(board::widget_number("w5"), None);
        assert_eq!(board::widget_number("w01"), None);
        assert_eq!(board::widget_number("custom"), None);
    }
}
