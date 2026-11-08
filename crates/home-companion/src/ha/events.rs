// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Cards sent from Home Assistant: an automation or a script fires an event on Home Assistant's
//! bus (the built-in `event` action, nothing to install), the body is subscribed to it
//! (`subscribe_events`, one per event type) and shows a card on the home screen.
//!
//! ```yaml
//! action: event
//! event: home_companion_card
//! event_data:
//!   id: washing          # optional: a card with the same id replaces the old one
//!   title: Laundry       # optional
//!   text: The washing machine is done
//!   icon: mdi:washing-machine   # optional
//!   seconds: 600         # optional: the card goes away by itself
//!   device: Alfred       # optional: only the device with this name (default: every device)
//! ```
//!
//! `home_companion_card_dismiss` with `id` takes a card away.

use serde_json::Value;

/// The event types the body subscribes to.
pub const CARD_EVENT: &str = "home_companion_card";
pub const DISMISS_EVENT: &str = "home_companion_card_dismiss";

/// A card from Home Assistant.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HaCard {
    pub id: Option<String>,
    pub title: Option<String>,
    pub text: String,
    /// An `mdi:` name; unknown icons fall back to the generic message icon.
    pub icon: Option<String>,
    /// The card goes away by itself after this many seconds (None: until touched).
    pub seconds: Option<u64>,
}

/// What an event asks.
#[derive(Debug, Clone, PartialEq)]
pub enum CardEvent {
    Show(HaCard),
    Dismiss(String),
}

/// The card longest and widest the screen takes: longer texts are cut.
const MAX_TEXT: usize = 280;
const MAX_TITLE: usize = 60;

fn clipped(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", s[..i].trim_end()),
        None => s.to_string(),
    }
}

/// One `event` message of a `subscribe_events` subscription (`{"type":"event","event":{
/// "event_type":…,"data":{…}}}`), for the device called `device` (the character's name).
/// None: not a card event, for another device, or without text.
pub fn parse(msg: &Value, device: &str) -> Option<CardEvent> {
    let ev = msg.get("event")?;
    let data = ev.get("data")?;
    let s = |k: &str| {
        data.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    if let Some(d) = s("device")
        && !d.eq_ignore_ascii_case(device)
    {
        return None;
    }
    match ev.get("event_type")?.as_str()? {
        CARD_EVENT => Some(CardEvent::Show(HaCard {
            id: s("id").map(String::from),
            title: s("title").map(|t| clipped(t, MAX_TITLE)),
            text: clipped(s("text")?, MAX_TEXT),
            icon: s("icon")
                .filter(|i| i.starts_with("mdi:"))
                .map(String::from),
            seconds: data
                .get("seconds")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0),
        })),
        DISMISS_EVENT => Some(CardEvent::Dismiss(s("id")?.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(t: &str, data: Value) -> Value {
        json!({"id": 5, "type": "event", "event": {"event_type": t, "data": data,
            "origin": "LOCAL", "time_fired": "2026-11-08T10:00:00+00:00"}})
    }

    #[test]
    fn cards_and_dismissals() {
        let m = event(
            CARD_EVENT,
            json!({"id": "washing", "title": "Laundry", "text": " Done ",
                   "icon": "mdi:washing-machine", "seconds": 600}),
        );
        assert_eq!(
            parse(&m, "Alfred"),
            Some(CardEvent::Show(HaCard {
                id: Some("washing".into()),
                title: Some("Laundry".into()),
                text: "Done".into(),
                icon: Some("mdi:washing-machine".into()),
                seconds: Some(600),
            }))
        );
        let m = event(DISMISS_EVENT, json!({"id": "washing"}));
        assert_eq!(
            parse(&m, "Alfred"),
            Some(CardEvent::Dismiss("washing".into()))
        );
    }

    #[test]
    fn only_this_device_and_only_with_text() {
        let m = event(CARD_EVENT, json!({"text": "hi", "device": "alfred"}));
        assert!(parse(&m, "Alfred").is_some());
        let m = event(CARD_EVENT, json!({"text": "hi", "device": "Kitchen"}));
        assert_eq!(parse(&m, "Alfred"), None);
        assert_eq!(
            parse(&event(CARD_EVENT, json!({"title": "x"})), "Alfred"),
            None
        );
        assert_eq!(
            parse(&event("state_changed", json!({"text": "x"})), "Alfred"),
            None
        );
        let m = event(
            CARD_EVENT,
            json!({"text": "hi", "icon": "http://x/y.png", "seconds": 0}),
        );
        let Some(CardEvent::Show(c)) = parse(&m, "Alfred") else {
            panic!()
        };
        assert_eq!((c.icon, c.seconds), (None, None));
    }

    #[test]
    fn long_texts_are_cut() {
        let long = "é".repeat(400);
        let m = event(CARD_EVENT, json!({"text": long}));
        let Some(CardEvent::Show(c)) = parse(&m, "Alfred") else {
            panic!()
        };
        assert_eq!(c.text.chars().count(), MAX_TEXT + 1);
        assert!(c.text.ends_with('…'));
    }
}
