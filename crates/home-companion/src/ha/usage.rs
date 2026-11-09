// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The precise use of the AI, when the agent lives under the "OpenAI with Token & Cost
//! Tracking" integration (domain `openai_usage`): its sensors (`<entry_id>_cost_today`,
//! `_cost_month`, `_requests_today`, `_tokens_today`; US dollars, daily ones reset at local
//! midnight) are found in the entity registry by platform, entry and unique id, read with
//! `/api/states/<entity>`, and the last days come from the history of the daily cost
//! (`/api/history/period`, each day's maximum). Without those sensors: `None`, the screen keeps
//! the body's estimate (crate::usage).
//!
//! Cheap: the registry is read again only when the entry changes or a sensor goes missing (and
//! at most every [`LOOKUP_EVERY`] seconds when there is none), the history every
//! [`HISTORY_EVERY`] seconds; in between four small states.

use serde_json::Value;

use super::setup::Api;
use crate::usage::{Precise, daily_max, local_day, local_midnight, parse_time};

/// The integration's domain (the registry's `platform` of its sensors).
pub const DOMAIN: &str = "openai_usage";
/// Where to get it (shown under an estimate).
pub const WHERE: &str = "github.com/m0s4ik/ha-openai-usage";
/// Without its sensors, the registry is looked at again after this many seconds.
const LOOKUP_EVERY: i64 = 600;
/// The history of the daily cost is read again after this many seconds (or on a new day).
const HISTORY_EVERY: i64 = 1800;

/// The integration's sensors of one config entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sensors {
    pub cost_today: String,
    pub cost_month: String,
    pub requests_today: Option<String>,
    pub tokens_today: Option<String>,
}

/// The sensors of `entry` in the entity registry (`config/entity_registry/list`): both cost
/// sensors enabled, or none.
pub fn find_sensors(registry: &[Value], entry: &str) -> Option<Sensors> {
    if entry.is_empty() {
        return None;
    }
    let find = |key: &str| {
        let uid = format!("{entry}_{key}");
        registry
            .iter()
            .find(|e| {
                e.get("platform").and_then(Value::as_str) == Some(DOMAIN)
                    && e.get("config_entry_id").and_then(Value::as_str) == Some(entry)
                    && e.get("unique_id").and_then(Value::as_str) == Some(uid.as_str())
                    && e.get("disabled_by").is_none_or(Value::is_null)
            })
            .and_then(|e| e.get("entity_id").and_then(Value::as_str))
            .map(String::from)
    };
    Some(Sensors {
        cost_today: find("cost_today")?,
        cost_month: find("cost_month")?,
        requests_today: find("requests_today"),
        tokens_today: find("tokens_today"),
    })
}

/// A history answer (`[[{state, last_changed}, …]]`) → (seconds since the epoch, value), the
/// states that are not numbers left out.
pub fn history_points(v: &Value) -> Vec<(i64, f64)> {
    v.as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_array)
        .map(|states| {
            states
                .iter()
                .filter_map(|s| {
                    let value = s.get("state")?.as_str()?.parse::<f64>().ok()?;
                    let t = s
                        .get("last_changed")
                        .or_else(|| s.get("last_updated"))
                        .and_then(Value::as_str)
                        .and_then(parse_time)?;
                    Some((t, value))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What the tracker remembers between two refreshes.
#[derive(Debug, Default)]
pub struct Tracker {
    entry: String,
    sensors: Option<Sensors>,
    looked_at: Option<i64>,
    /// The days from the history, when and on which day it was read.
    days: Vec<(u32, f64)>,
    history_at: Option<i64>,
    history_day: String,
}

/// `/api/history/period/<start>`'s times: UTC, "Z".
fn iso(t: i64) -> String {
    crate::logger::rfc3339(std::time::Duration::from_secs(t.max(0) as u64))
}

/// A sensor's state as a number (None: unknown, unavailable or missing).
fn number(api: &mut dyn Api, entity: &str) -> Result<Option<f64>, String> {
    let (_, v) = api.rest("GET", &format!("/api/states/{entity}"), None)?;
    Ok(v.get("state")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|x| x.is_finite()))
}

impl Tracker {
    /// The integration's numbers for the AI entry `entry` at `now` (seconds since the epoch):
    /// `Ok(None)` when it is not that integration (or its cost sensors are off).
    pub fn refresh(
        &mut self,
        api: &mut dyn Api,
        entry: &str,
        now: i64,
    ) -> Result<Option<Precise>, String> {
        if entry != self.entry {
            *self = Tracker {
                entry: entry.to_string(),
                ..Default::default()
            };
        }
        if entry.is_empty() {
            return Ok(None);
        }
        if self.sensors.is_none() {
            if self.looked_at.is_some_and(|t| now - t < LOOKUP_EVERY) {
                return Ok(None);
            }
            self.looked_at = Some(now);
            let reg = super::setup::entity_registry(api)?;
            self.sensors = find_sensors(&reg, entry);
            let Some(s) = &self.sensors else {
                return Ok(None);
            };
            log::info!(event = "usage"; "precise use from {} ({})", DOMAIN, s.cost_today);
        }
        let s = self.sensors.clone().unwrap_or_default();
        let (today, month) = match (number(api, &s.cost_today), number(api, &s.cost_month)) {
            (Ok(t), Ok(m)) => (t.unwrap_or(0.0), m.unwrap_or(0.0)),
            (Err(e), _) | (_, Err(e)) => {
                // Gone (removed, renamed): the registry again next time.
                self.sensors = None;
                self.looked_at = None;
                return Err(e);
            }
        };
        let count = |api: &mut dyn Api, e: &Option<String>| {
            e.as_deref()
                .and_then(|e| number(api, e).ok().flatten())
                .map(|x| x.max(0.0) as u64)
        };
        let requests = count(api, &s.requests_today);
        let tokens = count(api, &s.tokens_today);
        let day = local_day(now);
        if self.history_day != day || self.history_at.is_none_or(|t| now - t >= HISTORY_EVERY) {
            // From a minute past the first day's midnight: the state at the start is then the
            // counter after its reset, not the day before's total.
            let start = local_midnight(now - (crate::usage::VIEW_DAYS as i64 - 1) * 86_400) + 60;
            let path = format!(
                "/api/history/period/{}?filter_entity_id={}&end_time={}&minimal_response&no_attributes",
                iso(start),
                s.cost_today,
                iso(now + 60)
            );
            match api.rest("GET", &path, None) {
                Ok((_, v)) => {
                    self.days = daily_max(&history_points(&v), now);
                    self.history_at = Some(now);
                    self.history_day = day;
                }
                Err(e) => log::info!(event = "usage"; "history: {e}"),
            }
        }
        let mut days = if self.days.is_empty() {
            daily_max(&[], now)
        } else {
            self.days.clone()
        };
        // Today's bar is the counter now.
        if let Some(last) = days.last_mut() {
            last.1 = last.1.max(today);
        }
        Ok(Some(Precise {
            today_cost: today,
            month_cost: month,
            requests,
            tokens,
            days,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    const ENTRY: &str = "01OPENAI";

    fn sensor(key: &str, entity: &str) -> Value {
        json!({"platform": DOMAIN, "config_entry_id": ENTRY, "unique_id": format!("{ENTRY}_{key}"),
               "entity_id": entity, "disabled_by": null})
    }

    fn registry() -> Vec<Value> {
        vec![
            json!({"platform": "openai_conversation", "config_entry_id": "other", "unique_id": "x",
                   "entity_id": "conversation.chatgpt"}),
            sensor("cost_today", "sensor.openai_cost_today"),
            sensor("cost_month", "sensor.openai_cost_this_month"),
            sensor("requests_today", "sensor.openai_requests_today"),
            json!({"platform": DOMAIN, "config_entry_id": ENTRY, "unique_id": format!("{ENTRY}_tokens_today"),
                   "entity_id": "sensor.openai_tokens_today", "disabled_by": "user"}),
        ]
    }

    #[test]
    fn the_sensors_are_found_by_entry_and_key() {
        let s = find_sensors(&registry(), ENTRY).unwrap();
        assert_eq!(s.cost_today, "sensor.openai_cost_today");
        assert_eq!(s.cost_month, "sensor.openai_cost_this_month");
        assert_eq!(
            s.requests_today.as_deref(),
            Some("sensor.openai_requests_today")
        );
        assert_eq!(s.tokens_today, None, "disabled");
        assert_eq!(find_sensors(&registry(), "other"), None);
        assert_eq!(find_sensors(&registry(), ""), None);
    }

    #[test]
    fn a_minimal_history_gives_points() {
        let v = json!([[
            {"entity_id": "sensor.openai_cost_today", "state": "0.12", "last_changed": "2026-11-08T23:01:00+00:00"},
            {"state": "unavailable", "last_changed": "2026-11-09T01:00:00+00:00"},
            {"state": "0.2", "last_changed": "2026-11-09T02:00:00.5+00:00"}
        ]]);
        let p = history_points(&v);
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].1, 0.2);
        assert_eq!(p[1].0, parse_time("2026-11-09T02:00:00Z").unwrap());
        assert!(history_points(&json!([])).is_empty());
    }

    /// A Home Assistant with the registry, states and history above; counts the calls.
    struct Fake {
        registry: Vec<Value>,
        states: HashMap<String, String>,
        history: Value,
        calls: Vec<String>,
    }

    impl Api for Fake {
        fn ws(&mut self, msg: Value) -> Result<Value, String> {
            self.calls
                .push(msg["type"].as_str().unwrap_or("").to_string());
            Ok(Value::Array(self.registry.clone()))
        }
        fn rest(&mut self, _: &str, path: &str, _: Option<Value>) -> Result<(u16, Value), String> {
            self.calls
                .push(path.split('?').next().unwrap_or("").to_string());
            if path.starts_with("/api/history/period/") {
                assert!(path.contains("filter_entity_id=sensor.openai_cost_today"));
                assert!(path.contains("minimal_response") && path.contains("no_attributes"));
                assert!(path.contains("end_time="));
                return Ok((200, self.history.clone()));
            }
            let e = path.trim_start_matches("/api/states/");
            match self.states.get(e) {
                Some(s) => Ok((200, json!({"entity_id": e, "state": s}))),
                None => Err(format!("GET {path}: HTTP 404 Entity not found.")),
            }
        }
    }

    #[test]
    fn precise_numbers_from_the_sensors_and_the_history() {
        let now = 1_794_200_000;
        let yesterday_noon = local_midnight(now) - 12 * 3600;
        let mut ha = Fake {
            registry: registry(),
            states: [
                ("sensor.openai_cost_today", "0.018"),
                ("sensor.openai_cost_this_month", "1.5"),
                ("sensor.openai_requests_today", "7"),
            ]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
            history: json!([[{"state": "0.4", "last_changed": iso(yesterday_noon)},
                             {"state": "0.01", "last_changed": iso(now - 60)}]]),
            calls: vec![],
        };
        let mut t = Tracker::default();
        let p = t.refresh(&mut ha, ENTRY, now).unwrap().unwrap();
        assert_eq!(p.today_cost, 0.018);
        assert_eq!(p.month_cost, 1.5);
        assert_eq!(p.requests, Some(7));
        assert_eq!(p.tokens, None);
        assert_eq!(p.days.len(), crate::usage::VIEW_DAYS);
        assert_eq!(p.days.last().unwrap().1, 0.018, "today = the counter now");
        assert_eq!(p.days[p.days.len() - 2].1, 0.4);
        // A minute later: no registry, no history, only the states.
        ha.calls.clear();
        t.refresh(&mut ha, ENTRY, now + 60).unwrap().unwrap();
        assert_eq!(ha.calls.len(), 3, "{:?}", ha.calls);
        assert!(
            !ha.calls
                .iter()
                .any(|c| c.contains("history") || c.contains("registry"))
        );
        // The sensor goes: an error, then the registry again.
        ha.states.remove("sensor.openai_cost_today");
        assert!(t.refresh(&mut ha, ENTRY, now + 120).is_err());
        ha.registry.clear();
        ha.calls.clear();
        assert_eq!(t.refresh(&mut ha, ENTRY, now + 180).unwrap(), None);
        assert_eq!(ha.calls, ["config/entity_registry/list"]);
        // Not the integration: the registry is not read again at every refresh.
        ha.calls.clear();
        assert_eq!(t.refresh(&mut ha, ENTRY, now + 240).unwrap(), None);
        assert!(ha.calls.is_empty());
        assert_eq!(t.refresh(&mut ha, "", now + 300).unwrap(), None);
    }
}
