// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The events the brain sends to the body on a `subscribe` connection (control.rs), one JSON
//! object per line. This file is the contract: the brain builds every event here, the body
//! parses them in crates/home-companion/src/brain_link.rs, and both test against the same
//! examples in `contract/events.jsonl`.
//!
//! | event | fields | what the body does |
//! |---|---|---|
//! | `notify` | `id`, `kind`, `how`, `late`, `text`, `due_at` | a reminder card (jobs.rs builds it) |
//! | `timer` | `seconds`, `name` (optional) | a local timer |
//! | `timer_cancel` | `all` | stops the last timer, or all |
//! | `dnd` | `on`, `minutes` (optional) | do not disturb on for `minutes`, or until the body's default (an hour by day, the morning at night); off when `on` is false |
//! | `alarm` | `hour`, `minute`, `days` | a local alarm (days: bit 0 = Monday; 0 = once) |
//! | `changed` | `id`, `entity_id`, `name`, `service`, `state` (or null), `undo` | a card "done" for a house command, with "Undo" when `undo` |
//! | `confirm` | `id`, `text`, `expires_at` (unix s) | a Yes/No card; the answer goes back as `confirm.answer` |
//! | `confirm_done` | `id`, `yes` (true/false; null = expired) | the Yes/No card closes (answered by voice, on screen or too late) |
//! | `show_view` | `path` (a Home Assistant path) or `camera` (a `camera.*` entity) | opens Home Assistant's view there |
//! | `upcoming` | `items`: `start`, `end` (unix s), `all_day`, `title`, `calendar` | the calendar events for the "next" card, soonest first; sent on subscribe and when they change |

use serde_json::{Value, json};

/// A timer of `seconds`, with what it is for ("pasta") when the user said it.
pub fn timer(seconds: i64, name: Option<&str>) -> Value {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => json!({"event": "timer", "seconds": seconds, "name": n}),
        None => json!({"event": "timer", "seconds": seconds}),
    }
}

pub fn timer_cancel(all: bool) -> Value {
    json!({"event": "timer_cancel", "all": all})
}

/// Do not disturb on (for `minutes`, or the body's default when None) or off.
pub fn dnd(on: bool, minutes: Option<i64>) -> Value {
    match (on, minutes) {
        (true, Some(m)) => json!({"event": "dnd", "on": true, "minutes": m}),
        _ => json!({"event": "dnd", "on": on}),
    }
}

pub fn alarm(hour: u8, minute: u8, days: u8) -> Value {
    json!({"event": "alarm", "hour": hour, "minute": minute, "days": days})
}

/// A house command went through: `state` is the entity's state afterwards when known.
pub fn changed(
    id: i64,
    entity_id: &str,
    name: &str,
    service: &str,
    state: Option<&str>,
    undo: bool,
) -> Value {
    json!({"event": "changed", "id": id, "entity_id": entity_id, "name": name,
           "service": service, "state": state, "undo": undo})
}

/// A house command waits for a yes: the question as the screen shows it.
pub fn confirm(id: i64, text: &str, expires_at: i64) -> Value {
    json!({"event": "confirm", "id": id, "text": text, "expires_at": expires_at})
}

/// The question `id` is over: `Some(answer)` or `None` when nobody answered in time.
pub fn confirm_done(id: i64, yes: Option<bool>) -> Value {
    json!({"event": "confirm_done", "id": id, "yes": yes})
}

/// What to open in Home Assistant's view.
pub enum View<'a> {
    /// A dashboard path ("/lovelace/energy").
    Path(&'a str),
    /// A `camera.*` entity.
    Camera(&'a str),
}

pub fn show_view(view: View) -> Value {
    match view {
        View::Path(p) => json!({"event": "show_view", "path": p}),
        View::Camera(c) => json!({"event": "show_view", "camera": c}),
    }
}

/// One calendar event for the "next" card.
pub struct Upcoming<'a> {
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub title: &'a str,
    pub calendar: &'a str,
}

pub fn upcoming(items: &[Upcoming]) -> Value {
    let items: Vec<Value> = items
        .iter()
        .map(|u| {
            json!({"start": u.start, "end": u.end, "all_day": u.all_day,
                   "title": u.title, "calendar": u.calendar})
        })
        .collect();
    json!({"event": "upcoming", "items": items})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The builders produce exactly the contract's examples (the body parses the same lines).
    #[test]
    fn builders_match_the_contract() {
        let built = [
            changed(
                41,
                "light.kitchen",
                "Kitchen light",
                "turn_off",
                Some("off"),
                true,
            ),
            confirm(42, "Living room blind: close it?", 1_794_150_000),
            confirm_done(42, Some(true)),
            confirm_done(43, None),
            show_view(View::Path("/lovelace/energy")),
            show_view(View::Camera("camera.front_door")),
            upcoming(&[Upcoming {
                start: 1_794_150_000,
                end: 1_794_153_600,
                all_day: false,
                title: "Dentist",
                calendar: "calendar.family",
            }]),
            dnd(true, Some(60)),
            dnd(true, None),
            dnd(false, Some(5)),
        ];
        let lines: Vec<Value> = include_str!("../contract/events.jsonl")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(built.len(), lines.len());
        for (b, l) in built.iter().zip(&lines) {
            assert_eq!(b, l);
        }
    }
}
