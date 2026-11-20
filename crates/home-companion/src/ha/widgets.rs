// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Home Assistant's data for the home's widget: the sensors chosen from the browser panel, what
//! to check (doors and windows open, batteries low), what a chosen media player plays, who is
//! home. Pure: the hub (hub.rs) asks and subscribes on its connection, this file reads.
//!
//! - **The catalogue** ([`parse_catalog`]): `get_states`, read at each connection, every six
//!   hours and when the panel opens its widget section (once a minute at most). Only the
//!   entities of a few domains are kept, and of each only its id, name, unit and classes, read
//!   through a struct of those fields (never the whole answer as a JSON value: a big
//!   installation's answer is megabytes, issue #1).
//! - **One subscription** (`subscribe_entities` with `entity_ids`) for every entity the widget
//!   needs ([`wanted`]): the compact format `{"a": {id: {"s", "a"}}, "c": {id: {"+", "-"}},
//!   "r": [id]}` is read by [`States::apply`]; when the set changes, the old subscription is
//!   dropped (`unsubscribe_events`) and a new one made.
//! - [`view`] turns the states into the widget's rows.
//!
//! Nothing here names a room or an entity: the user's choices come from settings.toml.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ui::widgets::{CheckRow, Person, Place, Playing};

/// Sensors at most, people at most, entities to check at most.
pub const MAX_SENSORS: usize = 8;
pub const MAX_PEOPLE: usize = 8;
pub const MAX_CHECK: usize = 96;
/// A battery under this (percent) is to check.
pub const LOW_BATTERY: f64 = 20.0;
/// The `binary_sensor` classes that are an opening.
pub const OPENINGS: [&str; 4] = ["door", "window", "opening", "garage_door"];

/// What the catalogue keeps of an entity.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entity {
    pub id: String,
    /// The friendly name (the id without one), cleaned.
    pub name: String,
    pub unit: String,
    pub device_class: String,
    /// Has long-term statistics (`state_class`).
    pub statistics: bool,
}

impl Entity {
    pub fn domain(&self) -> &str {
        self.id.split('.').next().unwrap_or("")
    }

    /// A number the sensors' page can show: a `sensor` whose state is a number, or an
    /// `input_number`.
    pub fn numeric(&self) -> bool {
        matches!(self.domain(), "sensor" | "input_number")
    }

    pub fn opening(&self) -> bool {
        self.domain() == "binary_sensor" && OPENINGS.contains(&self.device_class.as_str())
    }

    pub fn battery(&self) -> bool {
        self.domain() == "sensor" && self.device_class == "battery" && self.unit == "%"
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    result: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    entity_id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    attributes: Attrs,
}

#[derive(Deserialize, Default)]
struct Attrs {
    #[serde(default)]
    friendly_name: Option<String>,
    #[serde(default)]
    unit_of_measurement: Option<String>,
    #[serde(default)]
    device_class: Option<String>,
    #[serde(default)]
    state_class: Option<String>,
}

/// The domains the widget may ever show; anything else (locks, alarms, trackers, cameras…)
/// never enters the catalogue, so it can never reach the browser panel either.
pub const DOMAINS: [&str; 5] = [
    "sensor",
    "input_number",
    "binary_sensor",
    "media_player",
    "person",
];

/// A `get_states` answer (its text) → the catalogue, by name. A sensor counts only when its
/// state is a number (or unavailable now); a `binary_sensor` only when it is an opening.
pub fn parse_catalog(text: &str) -> Result<Vec<Entity>, String> {
    let a: Answer = serde_json::from_str(text).map_err(|e| format!("get_states: {e}"))?;
    if !a.success {
        return Err("get_states refused".into());
    }
    let mut out: Vec<Entity> = a
        .result
        .into_iter()
        .filter(|r| DOMAINS.contains(&r.entity_id.split('.').next().unwrap_or("")))
        .filter_map(|r| {
            let e = Entity {
                name: crate::feeds::clean_name(
                    r.attributes
                        .friendly_name
                        .as_deref()
                        .unwrap_or(&r.entity_id),
                ),
                unit: crate::feeds::clean_name(
                    r.attributes.unit_of_measurement.as_deref().unwrap_or(""),
                ),
                device_class: r.attributes.device_class.unwrap_or_default(),
                statistics: r.attributes.state_class.is_some(),
                id: r.entity_id,
            };
            let ok = match e.domain() {
                "sensor" => {
                    r.state.parse::<f64>().is_ok() || super::weather::is_unavailable(&r.state)
                }
                "binary_sensor" => e.opening(),
                _ => true,
            };
            ok.then_some(e)
        })
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    Ok(out)
}

/// The `get_states` request.
pub fn catalog_request(id: u64) -> String {
    json!({"id": id, "type": "get_states"}).to_string()
}

/// What the widget asks of Home Assistant (settings.toml `[widgets]`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Want {
    pub sensors: Vec<String>,
    /// The media player shown ("" none).
    pub player: String,
    /// The people shown (empty: every `person.*`); None: who is home is off.
    pub people: Option<Vec<String>>,
    /// What to check: openings, batteries (each may be off).
    pub openings: bool,
    pub batteries: bool,
}

/// Every entity the widget needs now, sorted (the subscription's `entity_ids`).
pub fn wanted(w: &Want, catalog: &[Entity]) -> Vec<String> {
    let mut out: Vec<String> = w.sensors.iter().take(MAX_SENSORS).cloned().collect();
    if !w.player.is_empty() {
        out.push(w.player.clone());
    }
    match &w.people {
        Some(p) if !p.is_empty() => out.extend(p.iter().take(MAX_PEOPLE).cloned()),
        Some(_) => out.extend(
            catalog
                .iter()
                .filter(|e| e.domain() == "person")
                .take(MAX_PEOPLE)
                .map(|e| e.id.clone()),
        ),
        None => {}
    }
    let check: Vec<String> = catalog
        .iter()
        .filter(|e| (w.openings && e.opening()) || (w.batteries && e.battery()))
        .map(|e| e.id.clone())
        .collect();
    if check.len() > MAX_CHECK {
        log::info!(event = "ha_widgets"; "to check: {} entities, the first {MAX_CHECK} by name", check.len());
    }
    out.extend(check.into_iter().take(MAX_CHECK));
    out.sort();
    out.dedup();
    out
}

/// The subscription of `ids`.
pub fn subscribe(id: u64, ids: &[String]) -> String {
    json!({"id": id, "type": "subscribe_entities", "entity_ids": ids}).to_string()
}

pub fn unsubscribe(id: u64, subscription: u64) -> String {
    json!({"id": id, "type": "unsubscribe_events", "subscription": subscription}).to_string()
}

/// An entity's state as the widget needs it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    pub state: String,
    pub name: String,
    pub unit: String,
    pub title: String,
    pub artist: String,
}

/// The states of the subscribed entities.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct States {
    pub by_id: HashMap<String, State>,
}

fn attr(a: &Value, k: &str) -> Option<String> {
    a.get(k)
        .and_then(Value::as_str)
        .map(crate::feeds::clean_name)
}

impl States {
    /// An event of the subscription; true when something changed.
    pub fn apply(&mut self, ev: &Value) -> bool {
        let before = self.by_id.clone();
        if let Some(all) = ev.get("a").and_then(Value::as_object) {
            for (id, e) in all {
                let a = e.get("a").cloned().unwrap_or(Value::Null);
                self.by_id.insert(
                    id.clone(),
                    State {
                        state: e.get("s").and_then(Value::as_str).unwrap_or("").into(),
                        name: attr(&a, "friendly_name").unwrap_or_default(),
                        unit: attr(&a, "unit_of_measurement").unwrap_or_default(),
                        title: attr(&a, "media_title").unwrap_or_default(),
                        artist: attr(&a, "media_artist").unwrap_or_default(),
                    },
                );
            }
        }
        if let Some(changes) = ev.get("c").and_then(Value::as_object) {
            for (id, c) in changes {
                let st = self.by_id.entry(id.clone()).or_default();
                if let Some(plus) = c.get("+") {
                    if let Some(s) = plus.get("s").and_then(Value::as_str) {
                        st.state = s.into();
                    }
                    if let Some(a) = plus.get("a") {
                        if let Some(v) = attr(a, "friendly_name") {
                            st.name = v;
                        }
                        if let Some(v) = attr(a, "unit_of_measurement") {
                            st.unit = v;
                        }
                        if let Some(v) = attr(a, "media_title") {
                            st.title = v;
                        }
                        if let Some(v) = attr(a, "media_artist") {
                            st.artist = v;
                        }
                    }
                }
                if let Some(gone) = c.pointer("/-/a").and_then(Value::as_array) {
                    for k in gone.iter().filter_map(Value::as_str) {
                        match k {
                            "media_title" => st.title.clear(),
                            "media_artist" => st.artist.clear(),
                            _ => {}
                        }
                    }
                }
            }
        }
        if let Some(r) = ev.get("r").and_then(Value::as_array) {
            for id in r.iter().filter_map(Value::as_str) {
                self.by_id.remove(id);
            }
        }
        self.by_id != before
    }
}

/// A sensor's value now.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorNow {
    pub id: String,
    pub name: String,
    pub value: f64,
    /// Digits after the separator, as Home Assistant wrote the state.
    pub decimals: u8,
    pub unit: String,
}

/// The widget's data from Home Assistant.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WidgetsData {
    pub sensors: Vec<SensorNow>,
    pub check: Vec<CheckRow>,
    pub playing: Option<Playing>,
    pub people: Vec<Person>,
}

/// The digits after the separator of a state ("21.50" → 2).
fn decimals_of(state: &str) -> u8 {
    state
        .split_once('.')
        .map_or(0, |(_, f)| f.len().min(4) as u8)
}

/// The states as the widget's rows: the chosen sensors in their order (those with a number);
/// the openings open and the batteries under 20%; the player while it plays; the people.
pub fn view(w: &Want, catalog: &[Entity], s: &States) -> WidgetsData {
    let name = |id: &str, st: &State| {
        if !st.name.is_empty() {
            return st.name.clone();
        }
        catalog
            .iter()
            .find(|e| e.id == id)
            .map_or_else(|| id.to_string(), |e| e.name.clone())
    };
    let sensors = w
        .sensors
        .iter()
        .filter_map(|id| {
            let st = s.by_id.get(id)?;
            let value: f64 = st
                .state
                .trim()
                .parse()
                .ok()
                .filter(|v: &f64| v.is_finite())?;
            let unit = if st.unit.is_empty() {
                catalog
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| e.unit.clone())
                    .unwrap_or_default()
            } else {
                st.unit.clone()
            };
            Some(SensorNow {
                id: id.clone(),
                name: name(id, st),
                value,
                decimals: decimals_of(&st.state),
                unit,
            })
        })
        .collect();
    let mut check: Vec<CheckRow> = Vec::new();
    for e in catalog {
        let Some(st) = s.by_id.get(&e.id) else {
            continue;
        };
        if w.openings && e.opening() && st.state == "on" {
            check.push(CheckRow {
                name: name(&e.id, st),
                battery: None,
            });
        } else if w.batteries
            && e.battery()
            && let Ok(p) = st.state.parse::<f64>()
            && p < LOW_BATTERY
        {
            check.push(CheckRow {
                name: name(&e.id, st),
                battery: Some(p.clamp(0.0, 100.0).round() as u8),
            });
        }
    }
    let playing = (!w.player.is_empty())
        .then(|| s.by_id.get(&w.player))
        .flatten()
        .filter(|st| st.state == "playing" && !st.title.is_empty())
        .map(|st| Playing {
            title: st.title.clone(),
            artist: st.artist.clone(),
            player: name(&w.player, st),
        });
    let people = match &w.people {
        None => Vec::new(),
        Some(chosen) => {
            let ids: Vec<String> = if chosen.is_empty() {
                catalog
                    .iter()
                    .filter(|e| e.domain() == "person")
                    .map(|e| e.id.clone())
                    .collect()
            } else {
                chosen.clone()
            };
            ids.iter()
                .take(MAX_PEOPLE)
                .filter_map(|id| {
                    let st = s.by_id.get(id)?;
                    let place = match st.state.as_str() {
                        "home" => Place::Home,
                        "not_home" => Place::Away,
                        s if super::weather::is_unavailable(s) || s.is_empty() => return None,
                        zone => Place::Zone(crate::feeds::clean_name(zone)),
                    };
                    Some(Person {
                        name: name(id, st),
                        place,
                    })
                })
                .collect()
        }
    };
    WidgetsData {
        sensors,
        check,
        playing,
        people,
    }
}

/// The catalogue's entities for a search from the browser panel: those of the role whose name
/// or id holds every word of `q`, by name, at most six.
pub fn search(catalog: &[Entity], role: crate::feeds::What, q: &str) -> Vec<crate::feeds::Found> {
    use crate::feeds::{Choice, Found, Hit, What};
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    catalog
        .iter()
        .filter(|e| match role {
            What::Sensor => e.numeric(),
            What::Player => e.domain() == "media_player",
            What::Person => e.domain() == "person",
            _ => false,
        })
        .filter(|e| {
            let hay = format!("{} {}", e.name.to_lowercase(), e.id);
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .take(crate::feeds::MAX_HITS)
        .map(|e| Found {
            hit: Hit {
                key: e.id.clone(),
                name: e.name.clone(),
                detail: if e.unit.is_empty() {
                    e.id.clone()
                } else {
                    format!("{} · {}", e.id, e.unit)
                },
            },
            choice: Choice::Entity(e.id.clone()),
        })
        .collect()
}

/// A sensor's last day.
#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    /// At most 32 points.
    pub points: Vec<f32>,
    /// The first value of the day (the change is against it), the lowest, the highest.
    pub first: f64,
    pub low: f64,
    pub high: f64,
}

/// An answer this big is not read (a sensor that changes every second): no curve.
pub const MAX_HISTORY: usize = 1 << 20;

/// The request for a sensor's last 24 hours: its 5-minute statistics when it has them, else its
/// history (numbers only, no attributes). `start` is RFC 3339.
pub fn curve_request(id: u64, entity: &str, statistics: bool, start: &str) -> String {
    if statistics {
        json!({"id": id, "type": "recorder/statistics_during_period", "start_time": start,
               "statistic_ids": [entity], "period": "5minute", "types": ["mean", "state"]})
        .to_string()
    } else {
        json!({"id": id, "type": "history/history_during_period", "start_time": start,
               "entity_ids": [entity], "minimal_response": true, "no_attributes": true,
               "significant_changes_only": false})
        .to_string()
    }
}

/// An answer to [`curve_request`] (its text) → the curve; None without two numbers.
pub fn parse_curve(text: &str, entity: &str) -> Option<Curve> {
    if text.len() > MAX_HISTORY {
        return None;
    }
    let v: Value = serde_json::from_str(text).ok()?;
    let rows = v.get("result")?.get(entity)?.as_array()?;
    let values: Vec<f64> = rows
        .iter()
        .filter_map(|r| {
            let x = r
                .get("mean")
                .or_else(|| r.get("state"))
                .or_else(|| r.get("s"))?;
            match x {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse().ok(),
                _ => None,
            }
        })
        .filter(|x: &f64| x.is_finite())
        .collect();
    if values.len() < 2 {
        return None;
    }
    let low = values.iter().copied().fold(f64::INFINITY, f64::min);
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(Curve {
        points: crate::ui::widgets::downsample(&values, crate::ui::widgets::CURVE_POINTS),
        first: values[0],
        low,
        high,
    })
}

/// "2026-11-19T14:25:00Z" for a unix time.
pub fn rfc3339(t: i64) -> String {
    let days = t.div_euclid(86400);
    let s = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/ha")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn the_catalogue_keeps_only_what_the_widget_may_show() {
        let c = parse_catalog(&fixture("widgets-get-states.json")).unwrap();
        let ids: Vec<&str> = c.iter().map(|e| e.id.as_str()).collect();
        for gone in [
            "lock.front",
            "alarm_control_panel.house",
            "device_tracker.phone_one",
            "sensor.status_text",
            "binary_sensor.motion_hall",
            "camera.garden",
        ] {
            assert!(!ids.contains(&gone), "{gone}");
        }
        for kept in [
            "sensor.outdoor_temperature",
            "sensor.remote_battery",
            "input_number.target",
            "binary_sensor.kitchen_window",
            "media_player.speaker",
            "person.ada",
            "sensor.power",
        ] {
            assert!(ids.contains(&kept), "{kept}");
        }
        let t = c
            .iter()
            .find(|e| e.id == "sensor.outdoor_temperature")
            .unwrap();
        assert_eq!(
            (t.name.as_str(), t.unit.as_str(), t.statistics),
            ("Outdoor temperature", "°C", true)
        );
        assert!(
            c.iter()
                .find(|e| e.id == "input_number.target")
                .unwrap()
                .numeric()
        );
        // Sorted by name.
        let names: Vec<String> = c.iter().map(|e| e.name.to_lowercase()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(parse_catalog(r#"{"id":1,"type":"result","success":false}"#).is_err());
        assert!(parse_catalog("[").is_err());
    }

    fn want() -> Want {
        Want {
            sensors: vec![
                "sensor.outdoor_temperature".into(),
                "input_number.target".into(),
            ],
            player: "media_player.speaker".into(),
            people: Some(vec![]),
            openings: true,
            batteries: true,
        }
    }

    #[test]
    fn one_subscription_for_everything() {
        let c = parse_catalog(&fixture("widgets-get-states.json")).unwrap();
        let ids = wanted(&want(), &c);
        for id in [
            "sensor.outdoor_temperature",
            "input_number.target",
            "media_player.speaker",
            "person.ada",
            "person.bob",
            "binary_sensor.kitchen_window",
            "binary_sensor.front_door",
            "sensor.remote_battery",
        ] {
            assert!(ids.contains(&id.to_string()), "{id}");
        }
        assert!(!ids.contains(&"sensor.power".to_string()), "not chosen");
        let off = Want {
            people: None,
            openings: false,
            batteries: false,
            ..want()
        };
        assert_eq!(wanted(&off, &c).len(), 3);
        assert_eq!(
            subscribe(4, &ids[..1]),
            r#"{"entity_ids":["binary_sensor.front_door"],"id":4,"type":"subscribe_entities"}"#
        );
    }

    #[test]
    fn states_become_rows() {
        let c = parse_catalog(&fixture("widgets-get-states.json")).unwrap();
        let mut s = States::default();
        let events: Vec<Value> = fixture("widgets-subscribe-entities.jsonl")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(s.apply(&events[0]["event"]));
        let v = view(&want(), &c, &s);
        assert_eq!(v.sensors.len(), 2);
        assert_eq!(v.sensors[0].name, "Outdoor temperature");
        assert_eq!(
            (
                v.sensors[0].value,
                v.sensors[0].decimals,
                v.sensors[0].unit.as_str()
            ),
            (12.5, 1, "°C")
        );
        // Open: the kitchen window; low: the remote; the front door is closed, the hall sensor
        // unavailable.
        let check: Vec<(&str, Option<u8>)> = v
            .check
            .iter()
            .map(|r| (r.name.as_str(), r.battery))
            .collect();
        assert_eq!(
            check,
            [("Kitchen window", None), ("Remote battery", Some(12))]
        );
        // Paused: no page.
        assert_eq!(v.playing, None);
        let people: Vec<(&str, &Place)> = v
            .people
            .iter()
            .map(|p| (p.name.as_str(), &p.place))
            .collect();
        assert_eq!(
            people,
            [("Ada", &Place::Home), ("Bob", &Place::Zone("Work".into()))]
        );
        // Changes: it plays, the window closes, a sensor goes.
        for e in &events[1..] {
            s.apply(&e["event"]);
        }
        let v = view(&want(), &c, &s);
        assert_eq!(
            v.playing,
            Some(Playing {
                title: "Song".into(),
                artist: "Band".into(),
                player: "Speaker".into()
            })
        );
        assert_eq!(v.check.len(), 1);
        assert_eq!(v.sensors.len(), 1, "the input number was removed");
        // The same event again: nothing changes.
        assert!(!s.apply(&events[1]["event"]));
    }

    #[test]
    fn searching_the_catalogue() {
        use crate::feeds::What;
        let c = parse_catalog(&fixture("widgets-get-states.json")).unwrap();
        let keys = |role, q| {
            search(&c, role, q)
                .into_iter()
                .map(|f| f.hit.key)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(What::Sensor, "outdoor"),
            ["sensor.outdoor_temperature"]
        );
        assert_eq!(keys(What::Sensor, "TARGET"), ["input_number.target"]);
        assert!(
            keys(What::Sensor, "window").is_empty(),
            "an opening is not a sensor's number"
        );
        assert_eq!(keys(What::Player, "speak"), ["media_player.speaker"]);
        assert_eq!(keys(What::Person, ""), ["person.ada", "person.bob"]);
        assert!(keys(What::Sensor, "lock").is_empty() && keys(What::Person, "lock").is_empty());
    }

    #[test]
    fn curves_from_statistics_or_history() {
        let stats = r#"{"id":9,"type":"result","success":true,"result":{"sensor.t":[
            {"start":1,"end":2,"mean":10.0,"state":10.0},{"start":2,"end":3,"mean":null,"state":11.0},
            {"start":3,"end":4,"mean":12.5}]}}"#;
        let c = parse_curve(stats, "sensor.t").unwrap();
        assert_eq!((c.first, c.low, c.high), (10.0, 10.0, 12.5));
        assert_eq!(c.points, vec![10.0, 12.5]);
        let hist = r#"{"id":9,"type":"result","success":true,"result":{"input_number.x":[
            {"s":"20","lu":1},{"s":"unavailable","lu":2},{"s":"21.5","lu":3}]}}"#;
        let c = parse_curve(hist, "input_number.x").unwrap();
        assert_eq!((c.first, c.high), (20.0, 21.5));
        assert!(parse_curve(hist, "sensor.other").is_none());
        assert!(parse_curve(r#"{"result":{"sensor.t":[{"s":"1"}]}}"#, "sensor.t").is_none());
        let big = format!(
            r#"{{"result":{{"sensor.t":[{}{{"s":"1"}}]}}}}"#,
            r#"{"s":"1"},"#.repeat(110_000)
        );
        assert!(
            parse_curve(&big, "sensor.t").is_none(),
            "over a mebibyte: no curve"
        );
        assert_eq!(rfc3339(1_793_715_900), "2026-11-03T14:25:00Z");
        let r: Value = serde_json::from_str(&curve_request(3, "sensor.t", true, "x")).unwrap();
        assert_eq!(r["type"], "recorder/statistics_during_period");
        assert_eq!(r["period"], "5minute");
        let r: Value =
            serde_json::from_str(&curve_request(3, "input_number.x", false, "x")).unwrap();
        assert_eq!(r["type"], "history/history_during_period");
        assert_eq!(r["no_attributes"], true);
    }
}
