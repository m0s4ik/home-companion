// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The board: grids of tiles, each a Home Assistant entity (a button, a sensor, a level), on
//! pages of their own (6 x 4 cells) and in custom widgets of the home (4 x 2 cells). Pure: the
//! file `board.json` (written by the body only, next to settings.toml), its validation, what a
//! tap does (a closed table from the entity's domain, [`action_for`], [`set_level`]), the
//! default icons and words, and the model the screen draws ([`view`], [`line`]).
//!
//! - **Never offered, never done**: locks, alarms and the covers (or relays) the gates never
//!   allow ([`safety::SafetyCfg::entity_gate`], the same table as the brain's voice) are dropped
//!   by [`validate`] and have no action even if they slipped in.
//! - **A yes first** for covers and valves by default ([`needs_confirm`]); a tile may turn it
//!   off or on.
//! - A bad tile is dropped with a reason, never the whole file.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ha::widgets::{Curve, Entity, State, States};

pub mod draw;
/// The icons' table (written by tools/make-board-icons.py) and the MDI version it holds.
pub mod icon_table;
pub mod icons;

/// The file's format.
pub const VERSION: u32 = 1;
/// Pages at most, tiles on a page at most, custom widgets at most, tiles in a widget at most.
pub const MAX_PAGES: usize = 6;
pub const MAX_PAGE_TILES: usize = 24;
pub const MAX_WIDGETS: usize = 4;
pub const MAX_WIDGET_TILES: usize = 8;
/// Entities followed for the whole board at most (one subscription with the widget's).
pub const MAX_ENTITIES: usize = 64;
/// A page's grid and a widget's, in cells (columns, rows).
pub const PAGE_GRID: (u8, u8) = (6, 4);
pub const WIDGET_GRID: (u8, u8) = (4, 2);
/// A canvas's or a tile's name at most, in characters; an icon's name; a canvas's id.
pub const MAX_NAME: usize = 40;
pub const MAX_ICON: usize = 64;
pub const MAX_ID: usize = 16;
/// An "Advanced" tile's `service_data`, as JSON, at most.
pub const MAX_SERVICE_DATA: usize = 1024;
/// After a tap the tile waits this long for Home Assistant's new state; a failure shows this
/// long.
pub const PENDING_FOR: Duration = Duration::from_secs(4);
pub const FAILED_FOR: Duration = Duration::from_secs(3);

/// What a tile is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TileKind {
    /// A tap does the domain's action ([`action_for`]).
    Button,
    /// A value (and its day when wide); a tap does nothing.
    Sensor,
    /// A bar to drag ([`set_level`]); a tap on the icon switches on and off.
    Level,
}

impl TileKind {
    /// The domains a tile of this kind may show.
    pub fn domains(self) -> &'static [&'static str] {
        match self {
            TileKind::Button => &[
                "light",
                "switch",
                "fan",
                "input_boolean",
                "automation",
                "scene",
                "script",
                "button",
                "input_button",
                "cover",
                "valve",
                "media_player",
            ],
            TileKind::Sensor => &["sensor", "binary_sensor", "input_number", "number"],
            TileKind::Level => &[
                "light",
                "media_player",
                "cover",
                "input_number",
                "number",
                "fan",
            ],
        }
    }

    /// The sizes (columns, rows) a tile of this kind may have.
    pub fn spans(self) -> &'static [(u8, u8)] {
        match self {
            TileKind::Button | TileKind::Sensor => &[(1, 1), (2, 1), (2, 2), (4, 1)],
            TileKind::Level => &[(2, 1), (4, 1)],
        }
    }
}

/// A tile: where it is (cells), what it is, which entity, and what the user changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tile {
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
    pub kind: TileKind,
    pub entity_id: String,
    /// "" = Home Assistant's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// An MDI icon's name without "mdi:" ("" = from the domain and the device class).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// Ask before doing it (None = the domain's default: [`needs_confirm`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
    /// "Advanced": `domain.service` instead of the domain's action (buttons only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_data: Option<Value>,
}

impl Tile {
    pub fn domain(&self) -> &str {
        self.entity_id.split('.').next().unwrap_or("")
    }
}

/// A page or a custom widget.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Canvas {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tiles: Vec<Tile>,
}

/// `board.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub version: u32,
    #[serde(default)]
    pub pages: Vec<Canvas>,
    #[serde(default)]
    pub widgets: Vec<Canvas>,
}

impl Default for Board {
    fn default() -> Self {
        Board {
            version: VERSION,
            pages: Vec::new(),
            widgets: Vec::new(),
        }
    }
}

/// A custom widget's page number from its canvas's id ("w2" → 2): its page of the home's turn
/// ("custom-2", widgets::Page). Ids that are not `w1`…`w4` have none ([`validate`] gives every
/// widget one).
pub fn widget_number(id: &str) -> Option<u8> {
    let n: u8 = id.strip_prefix('w')?.parse().ok()?;
    ((1..=MAX_WIDGETS as u8).contains(&n) && id == format!("w{n}")).then_some(n)
}

impl Board {
    /// The custom widget of page number `n` (its canvas `w<n>`).
    pub fn widget(&self, n: u8) -> Option<&Canvas> {
        self.widgets
            .iter()
            .find(|c| widget_number(&c.id) == Some(n))
    }

    fn tiles(&self) -> impl Iterator<Item = &Tile> {
        self.pages
            .iter()
            .chain(&self.widgets)
            .flat_map(|c| c.tiles.iter())
    }

    /// Every entity of the board, once, in order (what the hub follows).
    pub fn entities(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.tiles()
            .filter(|t| seen.insert(t.entity_id.as_str()))
            .map(|t| t.entity_id.clone())
            .take(MAX_ENTITIES)
            .collect()
    }

    /// The sensors whose day is drawn (a `sensor` tile at least two cells wide).
    pub fn curve_entities(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.tiles()
            .filter(|t| t.kind == TileKind::Sensor && t.w >= 2)
            .filter(|t| seen.insert(t.entity_id.as_str()))
            .map(|t| t.entity_id.clone())
            .collect()
    }

    /// The "Advanced" calls: (entity id, "domain.service").
    pub fn advanced(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .tiles()
            .filter_map(|t| Some((t.entity_id.clone(), t.service.clone()?)))
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

// ---- the file --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawBoard {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    pages: Vec<Value>,
    #[serde(default)]
    widgets: Vec<Value>,
}

#[derive(Deserialize)]
struct RawCanvas {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    tiles: Vec<Value>,
}

/// `board.json`'s text → the board as written, a tile or a canvas that cannot be read left out
/// (each with a reason); not validated ([`validate`]). Unreadable as a whole: an empty board.
pub fn parse(text: &str) -> (Board, Vec<String>) {
    let mut why = Vec::new();
    let raw: RawBoard = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => return (Board::default(), vec![format!("board.json: {e}")]),
    };
    if let Some(v) = raw.version.filter(|v| *v != VERSION) {
        why.push(format!("version {v}: read as version {VERSION}"));
    }
    let mut canvases = |list: Vec<Value>, what: &str| -> Vec<Canvas> {
        list.into_iter()
            .enumerate()
            .filter_map(|(i, v)| match serde_json::from_value::<RawCanvas>(v) {
                Ok(c) => Some(Canvas {
                    tiles: c
                        .tiles
                        .into_iter()
                        .enumerate()
                        .filter_map(|(j, t)| {
                            serde_json::from_value::<Tile>(t)
                                .map_err(|e| why.push(format!("{what} {}: tile {j}: {e}", c.id)))
                                .ok()
                        })
                        .collect(),
                    id: c.id,
                    name: c.name,
                }),
                Err(e) => {
                    why.push(format!("{what} {i}: {e}"));
                    None
                }
            })
            .collect()
    };
    let pages = canvases(raw.pages, "page");
    let widgets = canvases(raw.widgets, "widget");
    (
        Board {
            version: VERSION,
            pages,
            widgets,
        },
        why,
    )
}

/// The board as `board.json`'s text.
pub fn to_json(b: &Board) -> String {
    serde_json::to_string_pretty(b).unwrap_or_else(|_| "{}".into())
}

// ---- validation ------------------------------------------------------------------------------

fn clean_text(s: &str) -> String {
    crate::feeds::clean_name(s)
        .chars()
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

fn valid_icon(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ICON
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn clean_id(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(MAX_ID)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The keys of a service call's data that name a target: Home Assistant merges them with the
/// call's own target, so an "Advanced" tile's data could act on other entities. A tile's target
/// is always its own entity: these keys are stripped by [`validate`] and refused by the hub.
pub const TARGET_KEYS: [&str; 8] = [
    "entity_id",
    "device_id",
    "area_id",
    "floor_id",
    "label_id",
    "target",
    // scene.apply / scene.create: the entities the scene sets.
    "entities",
    "snapshot_entities",
];

/// `data` names a target of its own ([`TARGET_KEYS`]).
pub fn data_targets(data: &Value) -> bool {
    data.as_object()
        .is_some_and(|o| o.keys().any(|k| TARGET_KEYS.contains(&k.as_str())))
}

/// A `domain.service` name (lower case letters, digits, `_`).
fn split_service(s: &str) -> Option<(&str, &str)> {
    let (d, v) = s.split_once('.')?;
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 64
            && p.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    };
    (ok(d) && ok(v)).then_some((d, v))
}

/// A string anywhere in `v` that is an entity of a domain never controlled.
fn names_never(v: &Value, gates: &safety::SafetyCfg) -> bool {
    match v {
        Value::String(s) => s.split(|c: char| c == ',' || c.is_whitespace()).any(|id| {
            id.split_once('.')
                .is_some_and(|(d, _)| gates.never_domains.iter().any(|n| n == d))
        }),
        Value::Array(a) => a.iter().any(|x| names_never(x, gates)),
        Value::Object(o) => o.values().any(|x| names_never(x, gates)),
        _ => false,
    }
}

/// Why a tile cannot stay (None: it can), apart from its place on the canvas.
fn tile_problem(t: &Tile, catalog: &[Entity], gates: &safety::SafetyCfg) -> Option<String> {
    if !crate::settings::valid_entity(&t.entity_id, t.kind.domains()) {
        return Some(format!("{:?} cannot show {}", t.kind, t.entity_id).to_lowercase());
    }
    let known = catalog.iter().find(|e| e.id == t.entity_id);
    let (class, name) = known.map_or(("", ""), |e| (e.device_class.as_str(), e.name.as_str()));
    if gates.entity_gate(&t.entity_id, class, &format!("{} {name}", t.name)) == safety::Gate::Never
    {
        return Some(format!("{} is never controlled", t.entity_id));
    }
    if !t.kind.spans().contains(&(t.w, t.h)) {
        return Some(format!("{}x{} is not a size for this tile", t.w, t.h));
    }
    if let Some(s) = &t.service {
        let Some((d, _)) = split_service(s) else {
            return Some(format!("service {s:?} is not domain.service"));
        };
        if gates.never_domains.iter().any(|n| n == d) {
            return Some(format!("service {s} is never called"));
        }
        // Only a button keeps it (validate drops the rest's).
        if t.kind == TileKind::Button
            && let Some(why) = advanced_problem(&t.entity_id, s)
        {
            return Some(why);
        }
    }
    if t.service_data
        .as_ref()
        .is_some_and(|v| names_never(v, gates))
    {
        return Some("service_data names an entity never controlled".into());
    }
    None
}

/// The board as it may be used: canvases within the limits with ids of their own, tiles that
/// fit (inside, no overlap, a size and a domain of their kind, never a never-domain, at most
/// [`MAX_ENTITIES`] entities). What goes is said in the reasons, one line each; the rest stays.
/// `catalog` (Home Assistant's entities, may be empty) gives device classes and names to the
/// gates.
pub fn validate(b: Board, catalog: &[Entity]) -> (Board, Vec<String>) {
    let gates = safety::SafetyCfg::default();
    let mut why = Vec::new();
    let mut entities: HashSet<String> = HashSet::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut canvases = |list: Vec<Canvas>,
                        what: &str,
                        prefix: char,
                        max: usize,
                        max_tiles: usize,
                        (cols, rows): (u8, u8),
                        why: &mut Vec<String>|
     -> Vec<Canvas> {
        if list.len() > max {
            why.push(format!("{} {what}s: the first {max} kept", list.len()));
        }
        let mut out = Vec::new();
        for (i, c) in list.into_iter().take(max).enumerate() {
            let mut id = clean_id(&c.id);
            // A widget's id is its page's number in the home's turn: `w1`…`w4`.
            let bad = prefix == 'w' && widget_number(&id).is_none();
            if id.is_empty() || ids.contains(&id) || bad {
                // A widget takes the first number free (they are four at most).
                let from = if prefix == 'w' { 0 } else { i };
                id = (1..)
                    .map(|n| format!("{prefix}{}", n + from))
                    .find(|x| !ids.contains(x))
                    .unwrap_or_default();
            }
            ids.insert(id.clone());
            let mut taken = vec![false; cols as usize * rows as usize];
            let mut tiles = Vec::new();
            for mut t in c.tiles {
                let at = format!("{what} {id}: {} at {},{}", t.entity_id, t.x, t.y);
                if tiles.len() >= max_tiles {
                    why.push(format!("{at}: more than {max_tiles} tiles"));
                    continue;
                }
                if let Some(p) = tile_problem(&t, catalog, &gates) {
                    why.push(format!("{at}: {p}"));
                    continue;
                }
                let (x, y, w, h) = (t.x as usize, t.y as usize, t.w as usize, t.h as usize);
                if x + w > cols as usize || y + h > rows as usize {
                    why.push(format!("{at}: {w}x{h} does not fit"));
                    continue;
                }
                let cells: Vec<usize> = (y..y + h)
                    .flat_map(|r| (x..x + w).map(move |c| r * cols as usize + c))
                    .collect();
                if cells.iter().any(|&i| taken[i]) {
                    why.push(format!("{at}: over another tile"));
                    continue;
                }
                if !entities.contains(&t.entity_id) && entities.len() >= MAX_ENTITIES {
                    why.push(format!("{at}: more than {MAX_ENTITIES} entities"));
                    continue;
                }
                if t.service.is_some() && t.kind != TileKind::Button {
                    why.push(format!("{at}: a service only on a button (left out)"));
                    t.service = None;
                }
                if t.service.is_none() {
                    t.service_data = None;
                }
                if let Some(d) = &t.service_data
                    && (!d.is_object() || d.to_string().len() > MAX_SERVICE_DATA)
                {
                    why.push(format!(
                        "{at}: service_data not an object or too big (left out)"
                    ));
                    t.service_data = None;
                }
                if let Some(Value::Object(o)) = &mut t.service_data
                    && o.keys().any(|k| TARGET_KEYS.contains(&k.as_str()))
                {
                    o.retain(|k, _| !TARGET_KEYS.contains(&k.as_str()));
                    why.push(format!(
                        "{at}: service_data names a target (left out: the tile's own entity is the target)"
                    ));
                }
                if !t.icon.is_empty() && !valid_icon(&t.icon) {
                    why.push(format!("{at}: icon {:?} (left out)", t.icon));
                    t.icon.clear();
                }
                t.name = clean_text(&t.name);
                for i in cells {
                    taken[i] = true;
                }
                entities.insert(t.entity_id.clone());
                tiles.push(t);
            }
            out.push(Canvas {
                id,
                name: clean_text(&c.name),
                tiles,
            });
        }
        out
    };
    let pages = canvases(
        b.pages,
        "page",
        'p',
        MAX_PAGES,
        MAX_PAGE_TILES,
        PAGE_GRID,
        &mut why,
    );
    let widgets = canvases(
        b.widgets,
        "widget",
        'w',
        MAX_WIDGETS,
        MAX_WIDGET_TILES,
        WIDGET_GRID,
        &mut why,
    );
    (
        Board {
            version: VERSION,
            pages,
            widgets,
        },
        why,
    )
}

/// An entity the search may offer for a tile of `kind`: a domain of the kind, never one the
/// gates never allow.
pub fn offered(kind: TileKind, e: &Entity) -> bool {
    kind.domains().contains(&e.domain())
        && safety::SafetyCfg::default().entity_gate(&e.id, &e.device_class, &e.name)
            != safety::Gate::Never
}

// ---- actions ---------------------------------------------------------------------------------

/// A Home Assistant service call (`call_service`) on one entity.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub domain: String,
    pub service: String,
    pub entity_id: String,
    /// `service_data` without the target (an object; empty for none).
    pub data: Value,
}

impl Call {
    fn new(service: &str, entity_id: &str, data: Value) -> Call {
        let (d, s) = service.split_once('.').unwrap_or(("", service));
        Call {
            domain: d.into(),
            service: s.into(),
            entity_id: entity_id.into(),
            data,
        }
    }

    /// The hub's command for this call.
    pub fn into_cmd(self, id: u64) -> crate::ha::hub::HubCmd {
        crate::ha::hub::HubCmd::Call {
            id,
            domain: self.domain,
            service: self.service,
            entity_id: self.entity_id,
            data: self.data,
        }
    }
}

/// The services a tile may call without "Advanced": the domain's actions and the levels.
pub const SERVICES: [&str; 18] = [
    "homeassistant.toggle",
    "scene.turn_on",
    "script.turn_on",
    "button.press",
    "input_button.press",
    "cover.toggle",
    "valve.toggle",
    "media_player.media_play_pause",
    "light.turn_on",
    "media_player.volume_set",
    "cover.set_cover_position",
    "number.set_value",
    "input_number.set_value",
    "fan.set_percentage",
    // What a cover's or a valve's question binds to ([`bound_to_state`]).
    "cover.open_cover",
    "cover.close_cover",
    "valve.open_valve",
    "valve.close_valve",
];

/// A service of the closed list.
pub fn allowed(domain: &str, service: &str) -> bool {
    SERVICES
        .iter()
        .any(|s| s.split_once('.') == Some((domain, service)))
}

/// The domain's action of a tap (the plan's table).
fn domain_action(domain: &str) -> Option<&'static str> {
    Some(match domain {
        "light" | "switch" | "fan" | "input_boolean" | "automation" => "homeassistant.toggle",
        "scene" => "scene.turn_on",
        "script" => "script.turn_on",
        "button" => "button.press",
        "input_button" => "input_button.press",
        "cover" => "cover.toggle",
        "valve" => "valve.toggle",
        "media_player" => "media_player.media_play_pause",
        _ => return None,
    })
}

fn unavailable(st: Option<&State>) -> bool {
    st.is_none_or(|s| crate::ha::weather::is_unavailable(&s.state))
}

fn never(tile: &Tile, st: Option<&State>) -> bool {
    let (class, name) = st.map_or(("", ""), |s| (s.device_class.as_str(), s.name.as_str()));
    safety::SafetyCfg::default().entity_gate(
        &tile.entity_id,
        class,
        &format!("{} {name}", tile.name),
    ) == safety::Gate::Never
}

/// What a tap on the tile does (on a `level` tile: on its icon), None for nothing: a sensor, an
/// entity unavailable or not yet known, one the gates never allow, a number. An "Advanced"
/// service replaces the domain's action.
pub fn action_for(tile: &Tile, st: Option<&State>) -> Option<Call> {
    if tile.kind == TileKind::Sensor || unavailable(st) || never(tile, st) {
        return None;
    }
    if tile.kind == TileKind::Button
        && let Some(s) = &tile.service
    {
        split_service(s)?;
        let data = tile
            .service_data
            .clone()
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Default::default()));
        if data_targets(&data) || advanced_problem(&tile.entity_id, s).is_some() {
            return None;
        }
        return Some(Call::new(s, &tile.entity_id, data));
    }
    let service = domain_action(tile.domain())?;
    Some(Call::new(
        service,
        &tile.entity_id,
        Value::Object(Default::default()),
    ))
}

/// The call that sets a `level` tile to `percent` (0-100) of its range: a light's brightness,
/// a player's volume, a cover's position, a fan's speed, a number between its `min` and `max`
/// (on its `step`).
pub fn set_level(tile: &Tile, st: Option<&State>, percent: u8) -> Option<Call> {
    if tile.kind != TileKind::Level || unavailable(st) || never(tile, st) {
        return None;
    }
    let p = percent.min(100);
    let f = f64::from(p) / 100.0;
    let id = tile.entity_id.as_str();
    let (service, data) = match tile.domain() {
        "light" => ("light.turn_on", serde_json::json!({"brightness_pct": p})),
        "media_player" => (
            "media_player.volume_set",
            serde_json::json!({"volume_level": f}),
        ),
        "cover" => (
            "cover.set_cover_position",
            serde_json::json!({"position": p}),
        ),
        "fan" => ("fan.set_percentage", serde_json::json!({"percentage": p})),
        d @ ("input_number" | "number") => {
            let s = st?;
            let (min, max) = (s.min.unwrap_or(0.0), s.max.unwrap_or(100.0));
            if max <= min {
                return None;
            }
            let mut v = min + f * (max - min);
            if let Some(step) = s.step.filter(|x| *x > 0.0) {
                v = min + ((v - min) / step).round() * step;
            }
            let v = (v.clamp(min, max) * 1e6).round() / 1e6;
            let service = if d == "number" {
                "number.set_value"
            } else {
                "input_number.set_value"
            };
            (service, serde_json::json!({"value": v}))
        }
        _ => return None,
    };
    Some(Call::new(service, id, data))
}

/// The state is "on" for the tile's colours: a light on, a cover open, a player playing, a
/// script running; never for scenes and buttons.
pub fn is_on(domain: &str, state: &str) -> bool {
    match domain {
        "light" | "switch" | "fan" | "input_boolean" | "automation" | "script"
        | "binary_sensor" => state == "on",
        "cover" => matches!(state, "open" | "opening"),
        "valve" => matches!(state, "open" | "opening"),
        "media_player" => state == "playing",
        _ => false,
    }
}

/// A yes before the tile's action: the tile's choice, else the gates' default (covers and
/// valves, and the switches the gates read as a shutter, by the tile's name or Home
/// Assistant's, `ha_name`, as the voice reads it).
pub fn needs_confirm(tile: &Tile, device_class: &str, ha_name: &str) -> bool {
    tile.confirm.unwrap_or_else(|| {
        let g = safety::SafetyCfg::default();
        g.confirm_domain(tile.domain())
            || g.entity_gate(
                &tile.entity_id,
                device_class,
                &format!("{} {ha_name}", tile.name),
            ) == safety::Gate::Confirm
    })
}

/// The call a question binds to: for a cover or a valve the action that was asked ("Close
/// …?" closes), not a toggle, so a Yes after the state changed (someone used the wall switch
/// meanwhile) does not do the opposite. `state` is the one the question was asked from, as
/// [`ask_key`] reads it.
pub fn bound_to_state(call: Call, state: &str) -> Call {
    let open = matches!(state, "open" | "opening");
    let service = match (call.domain.as_str(), call.service.as_str()) {
        ("cover", "toggle") if open => "close_cover",
        ("cover", "toggle") => "open_cover",
        ("valve", "toggle") if open => "close_valve",
        ("valve", "toggle") => "open_valve",
        _ => return call,
    };
    Call {
        service: service.into(),
        ..call
    }
}

/// The percent a level call sets, as the bar showed it while dragging (`board_ask_level`'s
/// "{pct}"): a light's brightness, a cover's position, a fan's speed, a player's volume, a
/// number's place between its `min` and `max`. None for a call that sets no level.
pub fn ask_percent(call: &Call, st: Option<&State>) -> Option<f64> {
    let o = call.data.as_object()?;
    let num = |k: &str| o.get(k).and_then(Value::as_f64);
    let p = num("brightness_pct")
        .or_else(|| num("position"))
        .or_else(|| num("percentage"))
        .or_else(|| num("volume_level").map(|v| v * 100.0))
        .or_else(|| {
            let v = num("value")?;
            let s = st?;
            let (min, max) = (s.min.unwrap_or(0.0), s.max.unwrap_or(100.0));
            (max > min).then(|| (v - min) / (max - min) * 100.0)
        })?;
    Some(p.clamp(0.0, 100.0))
}

/// Why a tile's "Advanced" service cannot be (None: it can): only a service of the tile's own
/// domain or `homeassistant.turn_on/turn_off/toggle/update_entity` (both act on the tile's
/// entity, whose gate was looked at), and
/// of `script` only turning the tile's script on, off or over: `script.<name>` runs that
/// script, whatever the target, and its gate was never looked at.
pub fn advanced_problem(entity_id: &str, service: &str) -> Option<String> {
    let Some((d, s)) = split_service(service) else {
        return Some(format!("service {service:?} is not domain.service"));
    };
    let own = entity_id.split('.').next().unwrap_or("");
    if d == "homeassistant" {
        // The ones that act on the tile's entity; never restart, stop, reload…
        return (!matches!(s, "turn_on" | "turn_off" | "toggle" | "update_entity"))
            .then(|| format!("service {service} is not one a tile may call"));
    }
    if d != own {
        return Some(format!(
            "service {service} is not of the tile's domain ({own})"
        ));
    }
    if d == "script"
        && !matches!(s, "turn_on" | "turn_off" | "toggle")
        && format!("script.{s}") != entity_id
    {
        return Some(format!("service {service} runs another script"));
    }
    None
}

/// The locale key of a tile's question ("Close {name}?"): what the call does, from the state
/// it acts on (a cover open is closed, a light on is switched off). `board_ask_level` says
/// "{pct}".
pub fn ask_key(call: &Call, state: &str) -> &'static str {
    match (call.domain.as_str(), call.service.as_str()) {
        (_, "set_cover_position" | "volume_set" | "set_value" | "set_percentage") => {
            "board_ask_level"
        }
        ("light", "turn_on") if call.data.get("brightness_pct").is_some() => "board_ask_level",
        (_, "toggle") => {
            let domain = call.entity_id.split('.').next().unwrap_or("");
            match domain {
                "cover" | "valve" if matches!(state, "open" | "opening") => "board_ask_close",
                "cover" | "valve" => "board_ask_open",
                _ if state == "on" => "board_ask_off",
                _ => "board_ask_on",
            }
        }
        (_, "close_cover" | "close_valve") => "board_ask_close",
        (_, "open_cover" | "open_valve") => "board_ask_open",
        (_, "media_play_pause") if state == "playing" => "board_ask_pause",
        (_, "media_play_pause") => "board_ask_play",
        (_, "turn_on") if call.domain == "light" => "board_ask_on",
        _ => "board_ask_run",
    }
}

// ---- icons and words -------------------------------------------------------------------------

/// The default icon (an MDI name) of a domain and a device class.
pub fn default_icon(domain: &str, class: &str) -> &'static str {
    match (domain, class) {
        ("light", _) => "lightbulb",
        ("switch", "outlet") => "power-socket-eu",
        ("switch", _) => "toggle-switch",
        ("fan", _) => "fan",
        ("input_boolean", _) => "toggle-switch-outline",
        ("automation", _) => "robot",
        ("scene", _) => "palette",
        ("script", _) => "script-text",
        ("button" | "input_button", _) => "gesture-tap-button",
        ("cover", "blind") => "blinds",
        ("cover", "curtain") => "curtains",
        ("cover", "awning") => "awning-outline",
        ("cover", "shade") => "roller-shade",
        ("cover", "window") => "window-open-variant",
        ("cover", "garage") => "garage",
        ("cover", "gate") => "gate",
        ("cover", "door") => "door",
        ("cover", _) => "window-shutter",
        ("valve", "water") => "pipe-valve",
        ("valve", "gas") => "gas-burner",
        ("valve", _) => "valve",
        ("media_player", "tv") => "television",
        ("media_player", "receiver") => "audio-video",
        ("media_player", _) => "speaker",
        ("sensor", "temperature") => "thermometer",
        ("sensor", "humidity" | "moisture") => "water-percent",
        ("sensor", "power") => "flash",
        ("sensor", "energy") => "lightning-bolt",
        ("sensor", "battery") => "battery",
        ("sensor", "illuminance") => "brightness-5",
        ("sensor", "pressure" | "atmospheric_pressure") => "gauge",
        ("sensor", "carbon_dioxide") => "molecule-co2",
        ("sensor", "pm25" | "pm10" | "pm1") => "blur",
        ("sensor", "voltage") => "sine-wave",
        ("sensor", "current") => "current-ac",
        ("sensor", "gas") => "meter-gas",
        ("sensor", "water") => "water",
        ("sensor", "signal_strength") => "wifi",
        ("sensor", "wind_speed") => "weather-windy",
        ("sensor", "precipitation" | "precipitation_intensity") => "weather-rainy",
        ("sensor", "timestamp" | "duration") => "clock-outline",
        ("sensor", "monetary") => "cash",
        ("sensor", _) => "eye",
        ("binary_sensor", "door") => "door",
        ("binary_sensor", "window") => "window-closed",
        ("binary_sensor", "garage_door") => "garage",
        ("binary_sensor", "opening") => "square-outline",
        ("binary_sensor", "motion" | "moving") => "motion-sensor",
        ("binary_sensor", "occupancy" | "presence") => "home-account",
        ("binary_sensor", "smoke") => "smoke-detector",
        ("binary_sensor", "moisture") => "water-alert",
        ("binary_sensor", "connectivity") => "lan-connect",
        ("binary_sensor", "plug" | "power") => "power-plug",
        ("binary_sensor", "problem" | "safety") => "alert-circle",
        ("binary_sensor", "battery") => "battery-alert",
        ("binary_sensor", _) => "checkbox-blank-circle-outline",
        ("input_number" | "number", _) => "numeric",
        _ => "help-circle-outline",
    }
}

/// The words of the tiles' states (locales: `board_*`) and the number separators.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardWords {
    pub on: String,
    pub off: String,
    pub open: String,
    pub closed: String,
    /// A cover's or a valve's (in Italian a tapparella, a valvola: «Aperta»).
    pub cover_open: String,
    pub cover_closed: String,
    pub opening: String,
    pub closing: String,
    pub playing: String,
    pub paused: String,
    pub idle: String,
    pub running: String,
    pub detected: String,
    pub clear: String,
    pub unavailable: String,
    /// A scene's tile ("Scene"), a call that failed ("Didn't work").
    pub scene: String,
    pub failed: String,
    /// A wide sensor's day: "min {low} · max {high}".
    pub range: String,
    pub decimal: char,
    pub group: char,
}

impl BoardWords {
    /// From the screen's words (`text(key)`: the key itself when missing).
    pub fn new(text: impl Fn(&str) -> String) -> BoardWords {
        let ch = |k: &str, d: char| {
            let t = text(k);
            if t == k {
                d
            } else {
                t.chars().next().unwrap_or(d)
            }
        };
        BoardWords {
            on: text("board_on"),
            off: text("board_off"),
            open: text("board_open"),
            closed: text("board_closed"),
            cover_open: text("board_cover_open"),
            cover_closed: text("board_cover_closed"),
            opening: text("board_opening"),
            closing: text("board_closing"),
            playing: text("board_playing"),
            paused: text("board_paused"),
            idle: text("board_idle"),
            running: text("board_running"),
            detected: text("board_detected"),
            clear: text("board_clear"),
            unavailable: text("board_unavailable"),
            scene: text("board_label_scene"),
            failed: text("board_failed"),
            range: text("board_range"),
            decimal: ch("number_decimal", '.'),
            group: ch("number_group", ','),
        }
    }

    pub fn from_locale(l: &locale::Locale) -> BoardWords {
        BoardWords::new(|k| l.text(k).to_string())
    }
}

/// A state in words: by the domain (and a binary sensor's class); an unknown state as it is.
pub fn state_words(domain: &str, class: &str, state: &str, w: &BoardWords) -> String {
    let pick = |s: &String| s.clone();
    match (domain, state) {
        (_, s) if crate::ha::weather::is_unavailable(s) => pick(&w.unavailable),
        ("scene" | "button" | "input_button", _) => String::new(),
        ("script", "on") => pick(&w.running),
        ("binary_sensor", s) => {
            let on = s == "on";
            match class {
                "door" | "window" | "opening" | "garage_door" => {
                    pick(if on { &w.open } else { &w.closed })
                }
                "motion" | "moving" | "occupancy" | "presence" | "sound" | "vibration" => {
                    pick(if on { &w.detected } else { &w.clear })
                }
                _ => pick(if on { &w.on } else { &w.off }),
            }
        }
        (_, "on") => pick(&w.on),
        (_, "off") => pick(&w.off),
        ("cover" | "valve", "open") => pick(&w.cover_open),
        ("cover" | "valve", "closed") => pick(&w.cover_closed),
        ("cover" | "valve", "opening") => pick(&w.opening),
        ("cover" | "valve", "closing") => pick(&w.closing),
        ("media_player", "playing") => pick(&w.playing),
        ("media_player", "paused") => pick(&w.paused),
        ("media_player", "idle") => pick(&w.idle),
        ("media_player", "standby") => pick(&w.off),
        (_, s) => crate::feeds::clean_name(s).chars().take(24).collect(),
    }
}

/// "21.5 °C", "40%": a number with its unit (no space before "%" and "°…").
fn with_unit(n: String, unit: &str) -> String {
    if unit.is_empty() {
        n
    } else if unit == "%" || unit.starts_with('°') {
        format!("{n}{unit}")
    } else {
        format!("{n} {unit}")
    }
}

/// The digits after the separator of a state that is a number.
fn decimals_of(st: &State) -> u8 {
    let digits = |t: &str| t.split_once('.').map_or(0, |(_, f)| f.len().min(4) as u8);
    // A number's step says how fine it is ("30.0" on a step of 1 is 30); a sensor's state
    // has the digits Home Assistant gives it.
    match st.step.filter(|s| s.is_finite() && *s > 0.0) {
        Some(step) => {
            let t = format!("{:.4}", step);
            digits(t.trim_end_matches('0').trim_end_matches('.'))
        }
        None => digits(st.state.trim()),
    }
}

/// A state that is a number, in the screen's words, with its unit.
fn number_text(st: &State, w: &BoardWords) -> Option<String> {
    let v: f64 = st
        .state
        .trim()
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite())?;
    let n = crate::ui::widgets::format_number_sep(v, decimals_of(st), w.decimal, w.group);
    Some(with_unit(n, &st.unit))
}

/// A wide sensor's day in words: "min 8.1° · max 14.9°" (as many digits as its state).
fn range_text(st: &State, c: &Curve, w: &BoardWords) -> String {
    if !c.low.is_finite() || !c.high.is_finite() || w.range.is_empty() {
        return String::new();
    }
    let n = |v: f64| {
        let t =
            crate::ui::widgets::format_number_sep(v, decimals_of(st).max(1), w.decimal, w.group);
        // The unit's sign only ("°", "%"): the value above has the whole unit.
        match st.unit.as_str() {
            u if u.starts_with('°') => format!("{t}°"),
            "%" => format!("{t}%"),
            _ => t,
        }
    };
    w.range
        .replace("{low}", &n(c.low))
        .replace("{high}", &n(c.high))
}

/// A `level` tile's position, 0-100 (None when unknown).
pub fn level_of(domain: &str, st: &State) -> Option<u8> {
    let pct = |x: f64| Some(x.clamp(0.0, 100.0).round() as u8);
    match domain {
        "light" if st.state == "off" => Some(0),
        "light" => pct(st.brightness? / 255.0 * 100.0),
        "media_player" => pct(st.volume? * 100.0),
        "cover" => pct(st.position?),
        "fan" if st.state == "off" => Some(0),
        "fan" => pct(st.percentage?),
        "input_number" | "number" => {
            let v: f64 = st.state.trim().parse().ok()?;
            let (min, max) = (st.min.unwrap_or(0.0), st.max.unwrap_or(100.0));
            if max <= min {
                return None;
            }
            pct((v - min) / (max - min) * 100.0)
        }
        _ => None,
    }
}

// ---- what the screen draws -------------------------------------------------------------------

/// A tile's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileState {
    Off,
    On,
    /// Tapped, waiting for Home Assistant's new state (at most [`PENDING_FOR`]).
    Pending,
    Unavailable,
    /// The call failed ([`FAILED_FOR`]).
    Failed,
}

/// Taps waiting for their new state, and calls that failed.
#[derive(Debug, Clone, Default)]
pub struct Pending {
    marks: HashMap<String, Mark>,
}

#[derive(Debug, Clone)]
struct Mark {
    at: Instant,
    /// The state when tapped: another one is the answer.
    before: Option<State>,
    failed: bool,
}

impl Pending {
    /// A tap on the tile of `entity` sent its call.
    pub fn tapped(&mut self, entity: &str, states: &States, now: Instant) {
        self.marks.insert(
            entity.to_string(),
            Mark {
                at: now,
                before: states.by_id.get(entity).cloned(),
                failed: false,
            },
        );
    }

    /// The call of `entity` failed (Home Assistant refused it, or no link).
    pub fn failed(&mut self, entity: &str, now: Instant) {
        self.marks.insert(
            entity.to_string(),
            Mark {
                at: now,
                before: None,
                failed: true,
            },
        );
    }

    /// Pending or Failed now, else None.
    pub fn state_of(&self, entity: &str, states: &States, now: Instant) -> Option<TileState> {
        let m = self.marks.get(entity)?;
        let age = now.saturating_duration_since(m.at);
        if m.failed {
            return (age < FAILED_FOR).then_some(TileState::Failed);
        }
        (age < PENDING_FOR && states.by_id.get(entity) == m.before.as_ref())
            .then_some(TileState::Pending)
    }

    /// Marks that no longer show go.
    pub fn prune(&mut self, states: &States, now: Instant) {
        let keep: Vec<String> = self
            .marks
            .keys()
            .filter(|e| self.state_of(e, states, now).is_some())
            .cloned()
            .collect();
        self.marks.retain(|e, _| keep.contains(e));
    }

    /// When the next mark ends by its time (the screen redraws then), if any.
    pub fn next_change(&self) -> Option<Instant> {
        self.marks
            .values()
            .map(|m| m.at + if m.failed { FAILED_FOR } else { PENDING_FOR })
            .min()
    }
}

/// A tile as the screen draws it.
#[derive(Debug, Clone, PartialEq)]
pub struct TileView {
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
    pub kind: TileKind,
    pub entity_id: String,
    pub state: TileState,
    /// An MDI icon's name.
    pub icon: String,
    pub name: String,
    /// A sensor's or a level's value ("21.5 °C", "40%"); "" none.
    pub value: String,
    /// The state in words ("On", "Open", "On · 60%" for a dimmed light, "Didn't work"); ""
    /// none.
    pub status: String,
    /// A wide sensor's day in words ("min 8.1° · max 14.9°"); "" none.
    pub range: String,
    /// A `level` tile's position, 0-100.
    pub level: Option<u8>,
    /// A wide sensor's last day.
    pub curve: Option<Curve>,
    pub tappable: bool,
    /// A tap asks first.
    pub confirm: bool,
}

/// "living_room_lamp" → "Living room lamp": a name before Home Assistant's is known.
fn name_from_id(id: &str) -> String {
    let o = id.split_once('.').map_or(id, |(_, o)| o).replace('_', " ");
    let mut c = o.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The tiles of a canvas as the screen draws them, from the states the hub keeps, the curves
/// of the wide sensors, the taps waiting and the screen's words.
pub fn view(
    canvas: &Canvas,
    states: &States,
    curves: &HashMap<String, Curve>,
    pending: &Pending,
    w: &BoardWords,
    now: Instant,
) -> Vec<TileView> {
    canvas
        .tiles
        .iter()
        .map(|t| {
            let st = states.by_id.get(&t.entity_id);
            let domain = t.domain();
            let class = st.map_or("", |s| s.device_class.as_str());
            let gone = unavailable(st);
            let name = if !t.name.is_empty() {
                t.name.clone()
            } else if let Some(n) = st.map(|s| s.name.clone()).filter(|n| !n.is_empty()) {
                n
            } else {
                name_from_id(&t.entity_id)
            };
            let icon = if t.icon.is_empty() {
                default_icon(domain, class).to_string()
            } else {
                t.icon.clone()
            };
            let mut status = st.map_or_else(
                || w.unavailable.clone(),
                |s| state_words(domain, class, &s.state, w),
            );
            if domain == "scene" && !gone {
                status = w.scene.clone();
            }
            // A number's value is in its bar; its raw state beside the name said it twice.
            if t.kind == TileKind::Level && matches!(domain, "input_number" | "number") && !gone {
                status = String::new();
            }
            // A light on at less than full: how much ("On · 60%"); a level tile has its bar.
            if t.kind == TileKind::Button
                && domain == "light"
                && let Some(p) = st
                    .filter(|s| s.state == "on")
                    .and_then(|s| level_of(domain, s))
                    .filter(|p| *p < 100)
            {
                status = format!("{status} · {p}%");
            }
            let (value, level) = match (t.kind, st) {
                (_, None) => (String::new(), None),
                (TileKind::Sensor, Some(s)) if domain != "binary_sensor" => {
                    (number_text(s, w).unwrap_or_else(|| status.clone()), None)
                }
                (TileKind::Level, Some(s)) if !gone => {
                    let l = level_of(domain, s);
                    let v = match domain {
                        "input_number" | "number" => number_text(s, w).unwrap_or_default(),
                        _ => l.map(|p| format!("{p}%")).unwrap_or_default(),
                    };
                    (v, l)
                }
                _ => (String::new(), None),
            };
            let tappable = !gone
                && match t.kind {
                    TileKind::Sensor => false,
                    TileKind::Button => action_for(t, st).is_some(),
                    TileKind::Level => set_level(t, st, 0).is_some() || action_for(t, st).is_some(),
                };
            let state = if gone {
                TileState::Unavailable
            } else if let Some(p) = pending.state_of(&t.entity_id, states, now) {
                p
            } else if st.is_some_and(|s| is_on(domain, &s.state)) {
                TileState::On
            } else {
                TileState::Off
            };
            if state == TileState::Failed {
                status = w.failed.clone();
            }
            let curve = (t.kind == TileKind::Sensor && t.w >= 2)
                .then(|| curves.get(&t.entity_id).cloned())
                .flatten();
            let range = match (&curve, st) {
                (Some(c), Some(s)) if t.h >= 2 && !gone => range_text(s, c, w),
                _ => String::new(),
            };
            TileView {
                x: t.x,
                y: t.y,
                w: t.w,
                h: t.h,
                kind: t.kind,
                entity_id: t.entity_id.clone(),
                state,
                icon,
                name,
                value,
                status,
                range,
                level,
                curve,
                tappable,
                confirm: needs_confirm(t, class, st.map_or("", |s| s.name.as_str())),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
