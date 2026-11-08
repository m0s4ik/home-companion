// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The weather entity as Home Assistant's WebSocket API reports it, reduced to what the home
//! screen shows. Pure (no I/O): [`WeatherModel::handle`] takes the decoded messages of the two
//! subscriptions and [`WeatherModel::view`] gives what is on screen, `None` when there is no
//! real data (entity missing, `unavailable`, `unknown`, no temperature): the box is hidden then.
//!
//! Messages (https://developers.home-assistant.io/docs/api/websocket):
//! - `subscribe_entities` with `entity_ids: [entity]`: first an event `{"a": {id: {"s": state,
//!   "a": attributes, ...}}}` with the current state (empty if the entity does not exist), then
//!   diffs `{"c": {id: {"+": {"s": ..., "a": {changed attributes}}, "-": {"a": [removed]}}}}`
//!   and `{"r": [id]}` when it is removed.
//! - `weather/subscribe_forecast` with `forecast_type: "daily"`: events `{"type": "daily",
//!   "forecast": [{"datetime", "condition", "temperature", "templow"}, ...]}` (`forecast` may be
//!   null while the integration has no data).

use serde_json::{Map, Value};

/// A day of the daily forecast.
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastDay {
    /// "2026-11-04" (the date part of the forecast's `datetime`).
    pub date: String,
    pub condition: String,
    pub high: Option<f64>,
    pub low: Option<f64>,
    /// Expected precipitation (the integration's unit, usually mm), when it says.
    pub precipitation: Option<f64>,
    /// Chance of precipitation, percent.
    pub precipitation_probability: Option<f64>,
}

/// What the home screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    /// Home Assistant's condition (`sunny`, `rainy`, ...).
    pub condition: String,
    pub temperature: f64,
    /// `temperature_unit`, "°C" if missing.
    pub unit: String,
    /// Today's forecast, if Home Assistant has one.
    pub today: Option<ForecastDay>,
    /// The following days (for the dashboard to come).
    pub next_days: Vec<ForecastDay>,
}

impl Weather {
    /// The simulator's and the golden screenshots' sample.
    pub fn sample() -> Weather {
        Weather {
            condition: "cloudy".into(),
            temperature: 13.7,
            unit: "°C".into(),
            today: Some(ForecastDay {
                date: String::new(),
                condition: "cloudy".into(),
                high: Some(15.0),
                low: Some(8.0),
                precipitation: None,
                precipitation_probability: None,
            }),
            next_days: Vec::new(),
        }
    }
}

impl Weather {
    /// A week for the demos: `today` ("2026-11-03") and the six days after it.
    pub fn sample_week(today: &str) -> Weather {
        let d: u32 = today[8..10].parse().unwrap_or(3);
        // Days stay inside one month for the sample (the 3rd plus six days).
        let day = |i: u32, condition: &str, high: f64, low: f64, rain: Option<f64>| ForecastDay {
            date: format!("{}-{:02}", &today[..7], d + i),
            condition: condition.into(),
            high: Some(high),
            low: Some(low),
            precipitation: rain,
            precipitation_probability: None,
        };
        let mut w = Weather::sample();
        w.today = Some(day(0, "cloudy", 15.0, 8.0, None));
        w.next_days = vec![
            day(1, "rainy", 12.0, 9.0, Some(2.5)),
            day(2, "sunny", 14.0, 6.0, None),
            day(3, "partlycloudy", 13.0, 7.0, None),
            day(4, "rainy", 11.0, 8.0, Some(6.0)),
            day(5, "pouring", 10.0, 8.0, Some(12.0)),
            day(6, "cloudy", 12.0, 7.0, None),
        ];
        w
    }
}

/// The states that mean "no data".
pub fn is_unavailable(state: &str) -> bool {
    matches!(state, "unavailable" | "unknown" | "")
}

/// The entity's state as the subscription keeps it.
#[derive(Debug, Clone, Default, PartialEq)]
struct EntityState {
    state: String,
    attributes: Map<String, Value>,
}

/// What a message changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Handled {
    /// The view may have changed.
    pub changed: bool,
    /// The forecast subscription should be sent again (it failed while the entity was missing
    /// or unavailable, and the entity is back).
    pub resubscribe_forecast: bool,
    /// A subscription was refused: (id, Home Assistant's error message).
    pub error: Option<(u64, String)>,
}

#[derive(Debug, Clone)]
pub struct WeatherModel {
    pub entity_id: String,
    /// The `subscribe_entities` message id.
    pub entities_id: u64,
    /// The current `weather/subscribe_forecast` message id.
    pub forecast_id: u64,
    entity: Option<EntityState>,
    forecast: Vec<ForecastDay>,
    forecast_failed: bool,
}

fn number(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// The date part of an ISO 8601 date-time.
fn date_of(datetime: &str) -> String {
    datetime.get(..10).unwrap_or(datetime).to_string()
}

pub fn parse_forecast(v: &Value) -> Vec<ForecastDay> {
    let Some(list) = v.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|d| {
            Some(ForecastDay {
                date: date_of(d.get("datetime")?.as_str()?),
                condition: d
                    .get("condition")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                high: number(d.get("temperature")),
                low: number(d.get("templow")),
                precipitation: number(d.get("precipitation")),
                precipitation_probability: number(d.get("precipitation_probability")),
            })
        })
        .collect()
}

impl WeatherModel {
    pub fn new(entity_id: &str, entities_id: u64, forecast_id: u64) -> WeatherModel {
        WeatherModel {
            entity_id: entity_id.to_string(),
            entities_id,
            forecast_id,
            entity: None,
            forecast: Vec::new(),
            forecast_failed: false,
        }
    }

    fn available(&self) -> bool {
        self.entity
            .as_ref()
            .is_some_and(|e| !is_unavailable(&e.state))
    }

    /// The entity exists in Home Assistant (after the first event of the subscription).
    pub fn entity_known(&self) -> bool {
        self.entity.is_some()
    }

    /// The entity's state ("" if missing), for the log.
    pub fn state(&self) -> &str {
        self.entity.as_ref().map_or("", |e| e.state.as_str())
    }

    /// One decoded message of the connection (after authentication).
    pub fn handle(&mut self, msg: &Value) -> Handled {
        let mut out = Handled::default();
        let id = msg.get("id").and_then(Value::as_u64);
        match msg.get("type").and_then(Value::as_str) {
            Some("result") => {
                if msg.get("success").and_then(Value::as_bool) == Some(false) {
                    let why = msg
                        .pointer("/error/message")
                        .or_else(|| msg.pointer("/error/code"))
                        .and_then(Value::as_str)
                        .unwrap_or("refused")
                        .to_string();
                    if id == Some(self.forecast_id) {
                        self.forecast_failed = true;
                        if !self.forecast.is_empty() {
                            self.forecast.clear();
                            out.changed = true;
                        }
                    }
                    if let Some(id) =
                        id.filter(|i| *i == self.forecast_id || *i == self.entities_id)
                    {
                        out.error = Some((id, why));
                    }
                }
            }
            Some("event") if id == Some(self.entities_id) => {
                let was_available = self.available();
                if let Some(ev) = msg.get("event") {
                    out.changed = self.apply_entities(ev);
                }
                if self.forecast_failed && !was_available && self.available() {
                    self.forecast_failed = false;
                    out.resubscribe_forecast = true;
                }
            }
            Some("event") if id == Some(self.forecast_id) => {
                let days = msg
                    .pointer("/event/forecast")
                    .map(parse_forecast)
                    .unwrap_or_default();
                if days != self.forecast {
                    self.forecast = days;
                    out.changed = true;
                }
            }
            _ => {}
        }
        out
    }

    /// A `subscribe_entities` event; true if something changed.
    fn apply_entities(&mut self, ev: &Value) -> bool {
        let before = self.entity.clone();
        if let Some(e) = ev.get("a").and_then(|a| a.get(&self.entity_id)) {
            self.entity = Some(EntityState {
                state: e.get("s").and_then(Value::as_str).unwrap_or("").to_string(),
                attributes: e
                    .get("a")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        if let Some(c) = ev.get("c").and_then(|c| c.get(&self.entity_id)) {
            let e = self.entity.get_or_insert_with(EntityState::default);
            if let Some(plus) = c.get("+") {
                if let Some(s) = plus.get("s").and_then(Value::as_str) {
                    e.state = s.to_string();
                }
                if let Some(a) = plus.get("a").and_then(Value::as_object) {
                    for (k, v) in a {
                        e.attributes.insert(k.clone(), v.clone());
                    }
                }
            }
            if let Some(gone) = c.pointer("/-/a").and_then(Value::as_array) {
                for k in gone.iter().filter_map(Value::as_str) {
                    e.attributes.remove(k);
                }
            }
        }
        if let Some(r) = ev.get("r").and_then(Value::as_array) {
            if r.iter()
                .any(|x| x.as_str() == Some(self.entity_id.as_str()))
            {
                self.entity = None;
            }
        }
        self.entity != before
    }

    /// What the screen shows; `today` is the local date ("2026-11-04"): forecast days before it
    /// are stale and skipped.
    pub fn view(&self, today: &str) -> Option<Weather> {
        let e = self.entity.as_ref()?;
        if is_unavailable(&e.state) {
            return None;
        }
        let temperature = number(e.attributes.get("temperature"))?;
        let unit = e
            .attributes
            .get("temperature_unit")
            .and_then(Value::as_str)
            .unwrap_or("°C")
            .to_string();
        let mut days = self
            .forecast
            .iter()
            .filter(|d| d.date.as_str() >= today)
            .cloned();
        let first = days.next();
        // Today's line only if the first day is today (else every day is in the future).
        let (today_fc, next_days) = match first {
            Some(d) if d.date == today => (Some(d), days.collect()),
            Some(d) => (None, std::iter::once(d).chain(days).collect()),
            None => (None, Vec::new()),
        };
        Some(Weather {
            condition: e.state.clone(),
            temperature,
            unit,
            today: today_fc,
            next_days,
        })
    }
}

/// Material Design Icons path data (Pictogrammers, Apache-2.0; the same set as the rest of the
/// screen) for Home Assistant's weather conditions, as Home Assistant's own frontend maps them.
pub fn condition_icon(condition: &str) -> &'static str {
    match condition {
        "sunny" => icons::SUNNY,
        "clear-night" => icons::NIGHT,
        "partlycloudy" => icons::PARTLY_CLOUDY,
        "cloudy" => icons::CLOUDY,
        "rainy" => icons::RAINY,
        "pouring" => icons::POURING,
        "snowy" => icons::SNOWY,
        "snowy-rainy" => icons::SNOWY_RAINY,
        "fog" => icons::FOG,
        "hail" => icons::HAIL,
        "lightning" => icons::LIGHTNING,
        "lightning-rainy" => icons::LIGHTNING_RAINY,
        "windy" | "windy-variant" => icons::WINDY,
        // "exceptional" and anything new.
        _ => icons::EXCEPTIONAL,
    }
}

/// mdi-weather-* and mdi-alert-circle-outline from @mdi/js 7.4.47.
pub mod icons {
    pub const SUNNY: &str = "M12,7A5,5 0 0,1 17,12A5,5 0 0,1 12,17A5,5 0 0,1 7,12A5,5 0 0,1 12,7M12,9A3,3 0 0,0 9,12A3,3 0 0,0 12,15A3,3 0 0,0 15,12A3,3 0 0,0 12,9M12,2L14.39,5.42C13.65,5.15 12.84,5 12,5C11.16,5 10.35,5.15 9.61,5.42L12,2M3.34,7L7.5,6.65C6.9,7.16 6.36,7.78 5.94,8.5C5.5,9.24 5.25,10 5.11,10.79L3.34,7M3.36,17L5.12,13.23C5.26,14 5.53,14.78 5.95,15.5C6.37,16.24 6.91,16.86 7.5,17.37L3.36,17M20.65,7L18.88,10.79C18.74,10 18.47,9.23 18.05,8.5C17.63,7.78 17.1,7.15 16.5,6.64L20.65,7M20.64,17L16.5,17.36C17.09,16.85 17.62,16.22 18.04,15.5C18.46,14.77 18.73,14 18.87,13.21L20.64,17M12,22L9.59,18.56C10.33,18.83 11.14,19 12,19C12.82,19 13.63,18.83 14.37,18.56L12,22Z";
    pub const NIGHT: &str = "M17.75,4.09L15.22,6.03L16.13,9.09L13.5,7.28L10.87,9.09L11.78,6.03L9.25,4.09L12.44,4L13.5,1L14.56,4L17.75,4.09M21.25,11L19.61,12.25L20.2,14.23L18.5,13.06L16.8,14.23L17.39,12.25L15.75,11L17.81,10.95L18.5,9L19.19,10.95L21.25,11M18.97,15.95C19.8,15.87 20.69,17.05 20.16,17.8C19.84,18.25 19.5,18.67 19.08,19.07C15.17,23 8.84,23 4.94,19.07C1.03,15.17 1.03,8.83 4.94,4.93C5.34,4.53 5.76,4.17 6.21,3.85C6.96,3.32 8.14,4.21 8.06,5.04C7.79,7.9 8.75,10.87 10.95,13.06C13.14,15.26 16.1,16.22 18.97,15.95M17.33,17.97C14.5,17.81 11.7,16.64 9.53,14.5C7.36,12.31 6.2,9.5 6.04,6.68C3.23,9.82 3.34,14.64 6.35,17.66C9.37,20.67 14.19,20.78 17.33,17.97Z";
    pub const PARTLY_CLOUDY: &str = "M12.74,5.47C15.1,6.5 16.35,9.03 15.92,11.46C17.19,12.56 18,14.19 18,16V16.17C18.31,16.06 18.65,16 19,16A3,3 0 0,1 22,19A3,3 0 0,1 19,22H6A4,4 0 0,1 2,18A4,4 0 0,1 6,14H6.27C5,12.45 4.6,10.24 5.5,8.26C6.72,5.5 9.97,4.24 12.74,5.47M11.93,7.3C10.16,6.5 8.09,7.31 7.31,9.07C6.85,10.09 6.93,11.22 7.41,12.13C8.5,10.83 10.16,10 12,10C12.7,10 13.38,10.12 14,10.34C13.94,9.06 13.18,7.86 11.93,7.3M13.55,3.64C13,3.4 12.45,3.23 11.88,3.12L14.37,1.82L15.27,4.71C14.76,4.29 14.19,3.93 13.55,3.64M6.09,4.44C5.6,4.79 5.17,5.19 4.8,5.63L4.91,2.82L7.87,3.5C7.25,3.71 6.65,4.03 6.09,4.44M18,9.71C17.91,9.12 17.78,8.55 17.59,8L19.97,9.5L17.92,11.73C18.03,11.08 18.05,10.4 18,9.71M3.04,11.3C3.11,11.9 3.24,12.47 3.43,13L1.06,11.5L3.1,9.28C3,9.93 2.97,10.61 3.04,11.3M19,18H16V16A4,4 0 0,0 12,12A4,4 0 0,0 8,16H6A2,2 0 0,0 4,18A2,2 0 0,0 6,20H19A1,1 0 0,0 20,19A1,1 0 0,0 19,18Z";
    pub const CLOUDY: &str = "M6,19A5,5 0 0,1 1,14A5,5 0 0,1 6,9C7,6.65 9.3,5 12,5C15.43,5 18.24,7.66 18.5,11.03L19,11A4,4 0 0,1 23,15A4,4 0 0,1 19,19H6M19,13H17V12A5,5 0 0,0 12,7C9.5,7 7.45,8.82 7.06,11.19C6.73,11.07 6.37,11 6,11A3,3 0 0,0 3,14A3,3 0 0,0 6,17H19A2,2 0 0,0 21,15A2,2 0 0,0 19,13Z";
    pub const RAINY: &str = "M6,14.03A1,1 0 0,1 7,15.03C7,15.58 6.55,16.03 6,16.03C3.24,16.03 1,13.79 1,11.03C1,8.27 3.24,6.03 6,6.03C7,3.68 9.3,2.03 12,2.03C15.43,2.03 18.24,4.69 18.5,8.06L19,8.03A4,4 0 0,1 23,12.03C23,14.23 21.21,16.03 19,16.03H18C17.45,16.03 17,15.58 17,15.03C17,14.47 17.45,14.03 18,14.03H19A2,2 0 0,0 21,12.03A2,2 0 0,0 19,10.03H17V9.03C17,6.27 14.76,4.03 12,4.03C9.5,4.03 7.45,5.84 7.06,8.21C6.73,8.09 6.37,8.03 6,8.03A3,3 0 0,0 3,11.03A3,3 0 0,0 6,14.03M12,14.15C12.18,14.39 12.37,14.66 12.56,14.94C13,15.56 14,17.03 14,18C14,19.11 13.1,20 12,20A2,2 0 0,1 10,18C10,17.03 11,15.56 11.44,14.94C11.63,14.66 11.82,14.4 12,14.15M12,11.03L11.5,11.59C11.5,11.59 10.65,12.55 9.79,13.81C8.93,15.06 8,16.56 8,18A4,4 0 0,0 12,22A4,4 0 0,0 16,18C16,16.56 15.07,15.06 14.21,13.81C13.35,12.55 12.5,11.59 12.5,11.59";
    pub const POURING: &str = "M9,12C9.53,12.14 9.85,12.69 9.71,13.22L8.41,18.05C8.27,18.59 7.72,18.9 7.19,18.76C6.65,18.62 6.34,18.07 6.5,17.54L7.78,12.71C7.92,12.17 8.47,11.86 9,12M13,12C13.53,12.14 13.85,12.69 13.71,13.22L11.64,20.95C11.5,21.5 10.95,21.8 10.41,21.66C9.88,21.5 9.56,20.97 9.7,20.43L11.78,12.71C11.92,12.17 12.47,11.86 13,12M17,12C17.53,12.14 17.85,12.69 17.71,13.22L16.41,18.05C16.27,18.59 15.72,18.9 15.19,18.76C14.65,18.62 14.34,18.07 14.5,17.54L15.78,12.71C15.92,12.17 16.47,11.86 17,12M17,10V9A5,5 0 0,0 12,4C9.5,4 7.45,5.82 7.06,8.19C6.73,8.07 6.37,8 6,8A3,3 0 0,0 3,11C3,12.11 3.6,13.08 4.5,13.6V13.59C5,13.87 5.14,14.5 4.87,14.96C4.59,15.43 4,15.6 3.5,15.32V15.33C2,14.47 1,12.85 1,11A5,5 0 0,1 6,6C7,3.65 9.3,2 12,2C15.43,2 18.24,4.66 18.5,8.03L19,8A4,4 0 0,1 23,12C23,13.5 22.2,14.77 21,15.46V15.46C20.5,15.73 19.91,15.57 19.63,15.09C19.36,14.61 19.5,14 20,13.72V13.73C20.6,13.39 21,12.74 21,12A2,2 0 0,0 19,10H17Z";
    pub const SNOWY: &str = "M6,14A1,1 0 0,1 7,15A1,1 0 0,1 6,16A5,5 0 0,1 1,11A5,5 0 0,1 6,6C7,3.65 9.3,2 12,2C15.43,2 18.24,4.66 18.5,8.03L19,8A4,4 0 0,1 23,12A4,4 0 0,1 19,16H18A1,1 0 0,1 17,15A1,1 0 0,1 18,14H19A2,2 0 0,0 21,12A2,2 0 0,0 19,10H17V9A5,5 0 0,0 12,4C9.5,4 7.45,5.82 7.06,8.19C6.73,8.07 6.37,8 6,8A3,3 0 0,0 3,11A3,3 0 0,0 6,14M7.88,18.07L10.07,17.5L8.46,15.88C8.07,15.5 8.07,14.86 8.46,14.46C8.85,14.07 9.5,14.07 9.88,14.46L11.5,16.07L12.07,13.88C12.21,13.34 12.76,13.03 13.29,13.17C13.83,13.31 14.14,13.86 14,14.4L13.41,16.59L15.6,16C16.14,15.86 16.69,16.17 16.83,16.71C16.97,17.24 16.66,17.79 16.12,17.93L13.93,18.5L15.54,20.12C15.93,20.5 15.93,21.15 15.54,21.54C15.15,21.93 14.5,21.93 14.12,21.54L12.5,19.93L11.93,22.12C11.79,22.66 11.24,22.97 10.71,22.83C10.17,22.69 9.86,22.14 10,21.6L10.59,19.41L8.4,20C7.86,20.14 7.31,19.83 7.17,19.29C7.03,18.76 7.34,18.21 7.88,18.07Z";
    pub const SNOWY_RAINY: &str = "M18.5,18.67C18.5,19.96 17.5,21 16.25,21C15,21 14,19.96 14,18.67C14,17.12 16.25,14.5 16.25,14.5C16.25,14.5 18.5,17.12 18.5,18.67M4,17.36C3.86,16.82 4.18,16.25 4.73,16.11L7,15.5L5.33,13.86C4.93,13.46 4.93,12.81 5.33,12.4C5.73,12 6.4,12 6.79,12.4L8.45,14.05L9.04,11.8C9.18,11.24 9.75,10.92 10.29,11.07C10.85,11.21 11.17,11.78 11,12.33L10.42,14.58L12.67,14C13.22,13.83 13.79,14.15 13.93,14.71C14.08,15.25 13.76,15.82 13.2,15.96L10.95,16.55L12.6,18.21C13,18.6 13,19.27 12.6,19.67C12.2,20.07 11.54,20.07 11.15,19.67L9.5,18L8.89,20.27C8.75,20.83 8.18,21.14 7.64,21C7.08,20.86 6.77,20.29 6.91,19.74L7.5,17.5L5.26,18.09C4.71,18.23 4.14,17.92 4,17.36M1,11A5,5 0 0,1 6,6C7,3.65 9.3,2 12,2C15.43,2 18.24,4.66 18.5,8.03L19,8A4,4 0 0,1 23,12A4,4 0 0,1 19,16A1,1 0 0,1 18,15A1,1 0 0,1 19,14A2,2 0 0,0 21,12A2,2 0 0,0 19,10H17V9A5,5 0 0,0 12,4C9.5,4 7.45,5.82 7.06,8.19C6.73,8.07 6.37,8 6,8A3,3 0 0,0 3,11C3,11.85 3.35,12.61 3.91,13.16C4.27,13.55 4.26,14.16 3.88,14.54C3.5,14.93 2.85,14.93 2.47,14.54C1.56,13.63 1,12.38 1,11Z";
    pub const FOG: &str = "M3,15H13A1,1 0 0,1 14,16A1,1 0 0,1 13,17H3A1,1 0 0,1 2,16A1,1 0 0,1 3,15M16,15H21A1,1 0 0,1 22,16A1,1 0 0,1 21,17H16A1,1 0 0,1 15,16A1,1 0 0,1 16,15M1,12A5,5 0 0,1 6,7C7,4.65 9.3,3 12,3C15.43,3 18.24,5.66 18.5,9.03L19,9C21.19,9 22.97,10.76 23,13H21A2,2 0 0,0 19,11H17V10A5,5 0 0,0 12,5C9.5,5 7.45,6.82 7.06,9.19C6.73,9.07 6.37,9 6,9A3,3 0 0,0 3,12C3,12.35 3.06,12.69 3.17,13H1.1L1,12M3,19H5A1,1 0 0,1 6,20A1,1 0 0,1 5,21H3A1,1 0 0,1 2,20A1,1 0 0,1 3,19M8,19H21A1,1 0 0,1 22,20A1,1 0 0,1 21,21H8A1,1 0 0,1 7,20A1,1 0 0,1 8,19Z";
    pub const HAIL: &str = "M6,14A1,1 0 0,1 7,15A1,1 0 0,1 6,16A5,5 0 0,1 1,11A5,5 0 0,1 6,6C7,3.65 9.3,2 12,2C15.43,2 18.24,4.66 18.5,8.03L19,8A4,4 0 0,1 23,12A4,4 0 0,1 19,16H18A1,1 0 0,1 17,15A1,1 0 0,1 18,14H19A2,2 0 0,0 21,12A2,2 0 0,0 19,10H17V9A5,5 0 0,0 12,4C9.5,4 7.45,5.82 7.06,8.19C6.73,8.07 6.37,8 6,8A3,3 0 0,0 3,11A3,3 0 0,0 6,14M10,18A2,2 0 0,1 12,20A2,2 0 0,1 10,22A2,2 0 0,1 8,20A2,2 0 0,1 10,18M14.5,16A1.5,1.5 0 0,1 16,17.5A1.5,1.5 0 0,1 14.5,19A1.5,1.5 0 0,1 13,17.5A1.5,1.5 0 0,1 14.5,16M10.5,12A1.5,1.5 0 0,1 12,13.5A1.5,1.5 0 0,1 10.5,15A1.5,1.5 0 0,1 9,13.5A1.5,1.5 0 0,1 10.5,12Z";
    pub const LIGHTNING: &str = "M6,16A5,5 0 0,1 1,11A5,5 0 0,1 6,6C7,3.65 9.3,2 12,2C15.43,2 18.24,4.66 18.5,8.03L19,8A4,4 0 0,1 23,12A4,4 0 0,1 19,16H18A1,1 0 0,1 17,15A1,1 0 0,1 18,14H19A2,2 0 0,0 21,12A2,2 0 0,0 19,10H17V9A5,5 0 0,0 12,4C9.5,4 7.45,5.82 7.06,8.19C6.73,8.07 6.37,8 6,8A3,3 0 0,0 3,11A3,3 0 0,0 6,14H7A1,1 0 0,1 8,15A1,1 0 0,1 7,16H6M12,11H15L13,15H15L11.25,22L12,17H9.5L12,11Z";
    pub const LIGHTNING_RAINY: &str = "M4.5,13.59C5,13.87 5.14,14.5 4.87,14.96C4.59,15.44 4,15.6 3.5,15.33V15.33C2,14.47 1,12.85 1,11A5,5 0 0,1 6,6C7,3.65 9.3,2 12,2C15.43,2 18.24,4.66 18.5,8.03L19,8A4,4 0 0,1 23,12A4,4 0 0,1 19,16A1,1 0 0,1 18,15A1,1 0 0,1 19,14A2,2 0 0,0 21,12A2,2 0 0,0 19,10H17V9A5,5 0 0,0 12,4C9.5,4 7.45,5.82 7.06,8.19C6.73,8.07 6.37,8 6,8A3,3 0 0,0 3,11C3,12.11 3.6,13.08 4.5,13.6V13.59M9.5,11H12.5L10.5,15H12.5L8.75,22L9.5,17H7L9.5,11M17.5,18.67C17.5,19.96 16.5,21 15.25,21C14,21 13,19.96 13,18.67C13,17.12 15.25,14.5 15.25,14.5C15.25,14.5 17.5,17.12 17.5,18.67Z";
    pub const WINDY: &str = "M4,10A1,1 0 0,1 3,9A1,1 0 0,1 4,8H12A2,2 0 0,0 14,6A2,2 0 0,0 12,4C11.45,4 10.95,4.22 10.59,4.59C10.2,5 9.56,5 9.17,4.59C8.78,4.2 8.78,3.56 9.17,3.17C9.9,2.45 10.9,2 12,2A4,4 0 0,1 16,6A4,4 0 0,1 12,10H4M19,12A1,1 0 0,0 20,11A1,1 0 0,0 19,10C18.72,10 18.47,10.11 18.29,10.29C17.9,10.68 17.27,10.68 16.88,10.29C16.5,9.9 16.5,9.27 16.88,8.88C17.42,8.34 18.17,8 19,8A3,3 0 0,1 22,11A3,3 0 0,1 19,14H5A1,1 0 0,1 4,13A1,1 0 0,1 5,12H19M18,18H4A1,1 0 0,1 3,17A1,1 0 0,1 4,16H18A3,3 0 0,1 21,19A3,3 0 0,1 18,22C17.17,22 16.42,21.66 15.88,21.12C15.5,20.73 15.5,20.1 15.88,19.71C16.27,19.32 16.9,19.32 17.29,19.71C17.47,19.89 17.72,20 18,20A1,1 0 0,0 19,19A1,1 0 0,0 18,18Z";
    pub const EXCEPTIONAL: &str = "M11,15H13V17H11V15M11,7H13V13H11V7M12,2C6.47,2 2,6.5 2,12A10,10 0 0,0 12,22A10,10 0 0,0 22,12A10,10 0 0,0 12,2M12,20A8,8 0 0,1 4,12A8,8 0 0,1 12,4A8,8 0 0,1 20,12A8,8 0 0,1 12,20Z";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<Value> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/ha")
            .join(name);
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{name}: {e}: {l}")))
            .collect()
    }

    fn feed(m: &mut WeatherModel, msgs: &[Value]) -> Vec<Handled> {
        msgs.iter().map(|v| m.handle(v)).collect()
    }

    #[test]
    fn state_diff_and_forecast() {
        let mut m = WeatherModel::new("weather.home", 1, 2);
        let out = feed(&mut m, &fixture("weather-available.jsonl"));
        assert!(out.iter().any(|h| h.changed));
        let w = m.view("2026-11-04").unwrap();
        // The diff turned cloudy 13.7 into rainy 12.4 and removed `wind_bearing`.
        assert_eq!(w.condition, "rainy");
        assert_eq!(w.temperature, 12.4);
        assert_eq!(w.unit, "°C");
        let t = w.today.unwrap();
        assert_eq!(
            (t.date.as_str(), t.high, t.low),
            ("2026-11-04", Some(15.2), Some(7.9))
        );
        assert_eq!(w.next_days.len(), 2);
        assert_eq!(w.next_days[0].condition, "sunny");
        // The next day: the past entries are stale and skipped.
        let w = m.view("2026-11-05").unwrap();
        assert!(w.today.as_ref().is_some_and(|d| d.date == "2026-11-05"));
        assert_eq!(w.next_days.len(), 1);
        // A forecast that starts tomorrow: no "today", every day is a next day.
        let w = m.view("2026-11-02").unwrap();
        assert!(w.today.is_none());
        assert_eq!(w.next_days.len(), 4);
    }

    #[test]
    fn unavailable_entity_hides_the_weather_and_comes_back() {
        let mut m = WeatherModel::new("weather.home", 1, 2);
        let out = feed(&mut m, &fixture("weather-unavailable.jsonl"));
        assert!(m.entity_known());
        assert_eq!(m.state(), "unavailable");
        assert!(m.view("2026-11-04").is_none());
        // The forecast subscription was refused while unavailable…
        assert!(
            out.iter()
                .any(|h| h.error.as_ref().is_some_and(|(id, _)| *id == 2))
        );
        // …and is asked again when the entity comes back.
        let back: Value = serde_json::from_str(
            r#"{"id":1,"type":"event","event":{"c":{"weather.home":{"+":{"s":"sunny","a":{"temperature":9.5,"temperature_unit":"°C"}}}}}}"#,
        )
        .unwrap();
        let h = m.handle(&back);
        assert!(h.changed && h.resubscribe_forecast);
        let w = m.view("2026-11-04").unwrap();
        assert_eq!((w.condition.as_str(), w.temperature), ("sunny", 9.5));
        assert!(w.today.is_none());
        // Unavailable again: hidden again.
        let gone: Value = serde_json::from_str(
            r#"{"id":1,"type":"event","event":{"c":{"weather.home":{"+":{"s":"unavailable","a":{}}}}}}"#,
        )
        .unwrap();
        assert!(m.handle(&gone).changed);
        assert!(m.view("2026-11-04").is_none());
    }

    #[test]
    fn missing_entity_and_removal() {
        let mut m = WeatherModel::new("weather.home", 1, 2);
        let empty: Value =
            serde_json::from_str(r#"{"id":1,"type":"event","event":{"a":{}}}"#).unwrap();
        m.handle(&empty);
        assert!(!m.entity_known());
        assert!(m.view("2026-11-04").is_none());
        feed(&mut m, &fixture("weather-available.jsonl"));
        assert!(m.view("2026-11-04").is_some());
        let removed: Value =
            serde_json::from_str(r#"{"id":1,"type":"event","event":{"r":["weather.home"]}}"#)
                .unwrap();
        assert!(m.handle(&removed).changed);
        assert!(m.view("2026-11-04").is_none());
    }

    #[test]
    fn other_entities_and_ids_are_ignored() {
        let mut m = WeatherModel::new("weather.home", 1, 2);
        let other: Value = serde_json::from_str(
            r#"{"id":1,"type":"event","event":{"a":{"weather.other":{"s":"sunny","a":{"temperature":30}}}}}"#,
        )
        .unwrap();
        assert!(!m.handle(&other).changed);
        let pong: Value = serde_json::from_str(r#"{"id":7,"type":"pong"}"#).unwrap();
        assert_eq!(m.handle(&pong), Handled::default());
        assert!(m.view("2026-11-04").is_none());
    }

    #[test]
    fn null_forecast_and_no_temperature() {
        let mut m = WeatherModel::new("weather.home", 1, 2);
        let ev: Value = serde_json::from_str(
            r#"{"id":1,"type":"event","event":{"a":{"weather.home":{"s":"fog","a":{}}}}}"#,
        )
        .unwrap();
        m.handle(&ev);
        // A condition but no temperature: not real data.
        assert!(m.view("2026-11-04").is_none());
        let fc: Value = serde_json::from_str(
            r#"{"id":2,"type":"event","event":{"type":"daily","forecast":null}}"#,
        )
        .unwrap();
        assert!(!m.handle(&fc).changed);
    }

    #[test]
    fn every_condition_has_its_icon() {
        let conditions = [
            ("sunny", icons::SUNNY),
            ("clear-night", icons::NIGHT),
            ("partlycloudy", icons::PARTLY_CLOUDY),
            ("cloudy", icons::CLOUDY),
            ("rainy", icons::RAINY),
            ("pouring", icons::POURING),
            ("snowy", icons::SNOWY),
            ("fog", icons::FOG),
            ("windy", icons::WINDY),
            ("windy-variant", icons::WINDY),
            ("lightning", icons::LIGHTNING),
            ("lightning-rainy", icons::LIGHTNING_RAINY),
            ("hail", icons::HAIL),
            ("snowy-rainy", icons::SNOWY_RAINY),
            ("exceptional", icons::EXCEPTIONAL),
            ("something-new", icons::EXCEPTIONAL),
        ];
        let mut seen = std::collections::HashSet::new();
        for (c, icon) in conditions {
            assert_eq!(condition_icon(c), icon, "{c}");
            assert!(icon.starts_with('M') && icon.len() > 40, "{c}");
            seen.insert(icon);
        }
        // Fourteen pictures: windy-variant shares windy's, unknown ones the exceptional one.
        assert_eq!(seen.len(), 14);
    }
}
