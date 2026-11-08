// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The calendars: Home Assistant's `calendar.get_events` answer → the `upcoming` event of the
//! "next" card (events.rs): the next events from now to +36 h, soonest first, at most 6.

use serde_json::Value;

use crate::clock;

/// How far ahead the card looks, and how many events it takes.
pub const HORIZON_HOURS: i64 = 36;
pub const MAX_EVENTS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub title: String,
    pub calendar: String,
}

/// A calendar time: "2026-11-08T10:00:00+01:00" (a moment) or "2026-11-08" (a whole day, from
/// local midnight). Returns (unix seconds, all_day).
fn when(s: &str) -> Option<(i64, bool)> {
    if s.len() == 10 {
        let utc_midnight = clock::parse_rfc3339(&format!("{s}T00:00:00Z"))?;
        return Some((utc_midnight - clock::local(utc_midnight).offset, true));
    }
    clock::parse_rfc3339(s).map(|t| (t, false))
}

/// The events of a `calendar.get_events` answer (`{"response": {"calendar.x": {"events": […]}}}`,
/// or the inner map) still to come or running at `now`, within the horizon, soonest first.
pub fn upcoming_from(result: &Value, now: i64) -> Vec<Event> {
    let by_calendar = result.get("response").unwrap_or(result);
    let mut out = Vec::new();
    for (calendar, v) in by_calendar.as_object().into_iter().flatten() {
        for e in v.get("events").and_then(Value::as_array).into_iter().flatten() {
            let Some((start, all_day)) = e.get("start").and_then(Value::as_str).and_then(when) else {
                continue;
            };
            let end = e
                .get("end")
                .and_then(Value::as_str)
                .and_then(when)
                .map_or(start, |(t, _)| t);
            let title = crate::safety::sanitize(
                e.get("summary").and_then(Value::as_str).unwrap_or("").trim(),
                80,
            );
            if title.is_empty() || end <= now || start >= now + HORIZON_HOURS * 3600 {
                continue;
            }
            out.push(Event { start, end, all_day, title, calendar: calendar.clone() });
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.title.cmp(&b.title)));
    out.truncate(MAX_EVENTS);
    out
}

pub fn to_event(items: &[Event]) -> Value {
    let u: Vec<crate::events::Upcoming> = items
        .iter()
        .map(|e| crate::events::Upcoming {
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            title: &e.title,
            calendar: &e.calendar,
        })
        .collect();
    crate::events::upcoming(&u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upcoming_sorted_filtered_and_capped() {
        let now = 1_794_150_000; // 2026-11-08 ~15:00 UTC
        let r = json!({"response": {
            "calendar.family": {"events": [
                {"start": "2026-11-09T09:00:00+00:00", "end": "2026-11-09T10:00:00+00:00", "summary": "Dentist"},
                {"start": "2026-11-08T14:00:00+00:00", "end": "2026-11-08T14:30:00+00:00", "summary": "Over already"},
                {"start": "2026-11-12T09:00:00+00:00", "end": "2026-11-12T10:00:00+00:00", "summary": "Too far"},
                {"start": "2026-11-09T09:00:00+00:00", "end": "2026-11-09T10:00:00+00:00"}
            ]},
            "calendar.birthdays": {"events": [
                {"start": "2026-11-09", "end": "2026-11-10", "summary": "Birthday"}
            ]}
        }});
        let v = upcoming_from(&r, now);
        let titles: Vec<&str> = v.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles.len(), 2, "{v:?}");
        assert!(titles.contains(&"Dentist") && titles.contains(&"Birthday"));
        let b = v.iter().find(|e| e.title == "Birthday").unwrap();
        assert!(b.all_day && b.end - b.start == 86400 && b.calendar == "calendar.birthdays");
        assert!(v[0].start <= v[1].start);
        // An event running now is kept; many events are capped at six.
        let many = json!({"calendar.x": {"events": (0..9).map(|i| json!({
            "start": format!("2026-11-08T{:02}:00:00+00:00", 15 + i % 8),
            "end": "2026-11-09T20:00:00+00:00", "summary": format!("e{i}")})).collect::<Vec<_>>()}});
        assert_eq!(upcoming_from(&many, now).len(), MAX_EVENTS);
    }
}
