// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The home's "next" card: the next calendar event merged with the next reminder, whichever
//! comes first. Pure: times are unix seconds, days are whatever `day_of` says (local days).

use crate::brain_link::Upcoming;

/// All-day events count as this long after their day's start when ordering them against
/// reminders (a reminder at 07:00 comes before "today", one at 15:00 after it).
const ALL_DAY_ORDER_S: i64 = 8 * 3600;

/// One thing still to come.
#[derive(Debug, Clone, PartialEq)]
pub struct NextItem {
    /// Start (all-day events: the day's start).
    pub at: i64,
    pub all_day: bool,
    pub text: String,
    /// A calendar event (else a reminder).
    pub event: bool,
    /// The calendar's entity (events only).
    pub calendar: String,
}

fn still_to_come(e: &Upcoming, now: i64) -> bool {
    match (e.all_day, e.end) {
        (true, 0) => e.start + 86400 > now,
        (_, end) if end > 0 => end > now,
        _ => e.start >= now - 60,
    }
}

/// Reminders (due, text) and calendar events, soonest first; what is over is left out.
pub fn merge(reminders: &[(i64, String)], events: &[Upcoming], now: i64) -> Vec<NextItem> {
    let mut v: Vec<(i64, NextItem)> = reminders
        .iter()
        .filter(|(at, _)| *at >= now - 60)
        .map(|(at, t)| {
            (
                *at,
                NextItem {
                    at: *at,
                    all_day: false,
                    text: t.clone(),
                    event: false,
                    calendar: String::new(),
                },
            )
        })
        .collect();
    v.extend(events.iter().filter(|e| still_to_come(e, now)).map(|e| {
        let key = if e.all_day {
            e.start + ALL_DAY_ORDER_S
        } else {
            e.start
        };
        (
            key,
            NextItem {
                at: e.start,
                all_day: e.all_day,
                text: e.title.clone(),
                event: true,
                calendar: e.calendar.clone(),
            },
        )
    }));
    v.sort_by_key(|(k, _)| *k);
    v.into_iter().map(|(_, i)| i).collect()
}

/// The events of today and tomorrow, soonest first, with 0 or 1 (days from today). An event
/// that has begun (all-day, or still running) is today's.
pub fn today_tomorrow<'a>(
    events: &'a [Upcoming],
    now: i64,
    day_of: &dyn Fn(i64) -> i64,
) -> Vec<(i64, &'a Upcoming)> {
    let today = day_of(now);
    let mut v: Vec<(i64, &Upcoming)> = events
        .iter()
        .filter(|e| still_to_come(e, now))
        .map(|e| ((day_of(e.start) - today).max(0), e))
        .filter(|(d, _)| *d <= 1)
        .collect();
    v.sort_by_key(|(d, e)| (*d, !e.all_day, e.start));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(start: i64, end: i64, all_day: bool, title: &str) -> Upcoming {
        Upcoming {
            start,
            end,
            all_day,
            title: title.into(),
            calendar: "calendar.family".into(),
        }
    }

    const NOW: i64 = 1_000_000;

    #[test]
    fn the_sooner_of_reminder_and_event_comes_first() {
        let r = vec![(NOW + 3600, "Call mum".to_string())];
        let e = vec![
            ev(NOW + 600, NOW + 1200, false, "Dentist"),
            ev(NOW + 7200, NOW + 7800, false, "Dinner"),
        ];
        let m = merge(&r, &e, NOW);
        let t: Vec<&str> = m.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(t, ["Dentist", "Call mum", "Dinner"]);
        assert!(m[0].event && !m[1].event);
    }

    #[test]
    fn past_things_are_dropped_and_running_events_stay() {
        let r = vec![(NOW - 3600, "Old".to_string())];
        let e = vec![
            ev(NOW - 7200, NOW - 3600, false, "Over"),
            ev(NOW - 600, NOW + 600, false, "Running"),
        ];
        let m = merge(&r, &e, NOW);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].text, "Running");
    }

    #[test]
    fn all_day_events_sit_in_the_morning() {
        let day = NOW - NOW % 86400;
        let e = vec![ev(day, day + 86400, true, "Holiday")];
        let early = vec![(day + 6 * 3600, "Early".to_string())];
        let late = vec![(day + 15 * 3600, "Late".to_string())];
        let now = day + 5 * 3600;
        assert_eq!(merge(&early, &e, now)[0].text, "Early");
        assert_eq!(merge(&late, &e, now)[0].text, "Holiday");
        // Yesterday's all-day event is gone.
        assert!(merge(&[], &[ev(day - 86400, day, true, "Gone")], now).is_empty());
        // An all-day event without an end lasts its day.
        assert_eq!(merge(&[], &[ev(day, 0, true, "Open")], now).len(), 1);
    }

    #[test]
    fn today_and_tomorrow_only() {
        let day = |t: i64| t.div_euclid(86400);
        let base = 20 * 86400;
        let now = base + 3600;
        let e = vec![
            ev(base + 5 * 3600, base + 6 * 3600, false, "Later today"),
            ev(base + 86400 + 100, base + 86400 + 200, false, "Tomorrow"),
            ev(base + 2 * 86400, base + 2 * 86400 + 60, false, "Too far"),
            ev(base, base + 86400, true, "All day"),
        ];
        let v = today_tomorrow(&e, now, &day);
        let t: Vec<(i64, &str)> = v.iter().map(|(d, e)| (*d, e.title.as_str())).collect();
        assert_eq!(t, [(0, "All day"), (0, "Later today"), (1, "Tomorrow")]);
    }
}
