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
//!   pose: working        # optional: a pose of the character (downloaded, or its own by state) while the card shows
//!   quiet: true          # optional: no sound and no waking the screen (an update of a card already shown)
//! ```
//!
//! `home_companion_card_dismiss` with `id` takes a card away.
//!
//! `home_companion_screen` with `state: "off"` puts the screen to sleep, `state: "on"` wakes it
//! (e.g. from a presence sensor); `device` works as for cards.
//!
//! The body fires one event too: `home_companion_conversation`, when a conversation starts and ends
//! (see [`conversation_data`]).

use serde_json::Value;

/// The event types the body subscribes to.
pub const CARD_EVENT: &str = "home_companion_card";
pub const DISMISS_EVENT: &str = "home_companion_card_dismiss";
/// The event type the body fires when the user taps OK on one of Home Assistant's cards.
pub const CARD_OK_EVENT: &str = "home_companion_card_ok";
pub const SCREEN_EVENT: &str = "home_companion_screen";
/// The event type the body fires when a conversation starts and ends.
pub const CONVERSATION_EVENT: &str = "home_companion_conversation";

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
    /// A pose of the character shown instead of its home pose while the card shows: a
    /// downloaded one (Settings → Poses) or one of its own by state name (`working`); only
    /// lowercase letters, digits, `-` and `_`.
    pub pose: Option<String>,
    /// No sound and no waking the screen: for a card sent again and again with fresh values (a
    /// temperature every minute) under the same `id`. The card shows and replaces as any other.
    pub quiet: bool,
}

/// What an event asks.
#[derive(Debug, Clone, PartialEq)]
pub enum CardEvent {
    Show(HaCard),
    Dismiss(String),
}

impl CardEvent {
    /// Whether it calls for attention when it changes what is shown: a card, unless `quiet`.
    pub fn chimes(&self) -> bool {
        matches!(self, CardEvent::Show(c) if !c.quiet)
    }
}

/// The card longest and widest the screen takes: longer texts are cut.
const MAX_TEXT: usize = 280;
const MAX_TITLE: usize = 60;
/// A pose's name: maybe a file name, so short and plain.
const MAX_POSE: usize = 40;

/// A pose's name, safe as a file name: lowercase letters, digits, `-` and `_` only.
fn pose_name(s: &str) -> Option<String> {
    (s.len() <= MAX_POSE
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
    .then(|| s.to_string())
}

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
            pose: s("pose").and_then(pose_name),
            quiet: data.get("quiet").and_then(Value::as_bool).unwrap_or(false),
        })),
        DISMISS_EVENT => Some(CardEvent::Dismiss(s("id")?.to_string())),
        _ => None,
    }
}

/// A `home_companion_screen` event for this device: `Some(true)` wake, `Some(false)` sleep.
/// None: another event, another device, or a `state` that is not "on" or "off".
pub fn parse_screen(msg: &Value, device: &str) -> Option<bool> {
    let ev = msg.get("event")?;
    if ev.get("event_type")?.as_str()? != SCREEN_EVENT {
        return None;
    }
    let data = ev.get("data")?;
    let s = |k: &str| data.get(k).and_then(Value::as_str).map(str::trim);
    if let Some(d) = s("device").filter(|d| !d.is_empty())
        && !d.eq_ignore_ascii_case(device)
    {
        return None;
    }
    match s("state")?.to_ascii_lowercase().as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// The data of `home_companion_card_ok` (the hub adds `device`): the `id` the card was sent with.
pub fn card_ok_data(id: &str) -> Value {
    serde_json::json!({ "id": id })
}

/// Who dealt with a conversation, from its outcome: "reflex" (the body itself), "home_assistant"
/// (speech went there or an answer came back) or "none" (stopped or nothing said).
pub fn handled_by(outcome: &str, audio_seconds_sent: f64, answer: &str) -> &'static str {
    if outcome == "reflex" {
        "reflex"
    } else if audio_seconds_sent > 0.0 || !answer.is_empty() {
        "home_assistant"
    } else {
        "none"
    }
}

/// The data of `home_companion_conversation` (the hub adds `device`): `state` is "started" or
/// "ended", `trigger` what started it ("wake", "talk" = a touch, "follow_up"), `handled_by` only
/// on "ended".
pub fn conversation_data(state: &str, trigger: &str, handled_by: Option<&str>) -> Value {
    let mut d = serde_json::json!({"state": state, "trigger": trigger});
    if let Some(h) = handled_by {
        d["handled_by"] = h.into();
    }
    d
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
                pose: None,
                quiet: false,
            }))
        );
        assert!(parse(&m, "Alfred").unwrap().chimes());
        // An update: the same card, quiet.
        let m = event(
            CARD_EVENT,
            json!({"id": "kamado", "text": "T1 182 °C", "quiet": true}),
        );
        let quiet = parse(&m, "Alfred").unwrap();
        assert!(matches!(&quiet, CardEvent::Show(c) if c.quiet));
        assert!(!quiet.chimes());
        // Only a true boolean is quiet.
        let m = event(CARD_EVENT, json!({"text": "x", "quiet": "yes"}));
        assert!(parse(&m, "Alfred").unwrap().chimes());
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
    fn screen_on_off_for_this_device() {
        let m = |d: Value| event(SCREEN_EVENT, d);
        assert_eq!(
            parse_screen(&m(json!({"state": "off"})), "Alfred"),
            Some(false)
        );
        assert_eq!(
            parse_screen(&m(json!({"state": " ON "})), "Alfred"),
            Some(true)
        );
        assert_eq!(
            parse_screen(&m(json!({"state": "on", "device": "alfred"})), "Alfred"),
            Some(true)
        );
        assert_eq!(
            parse_screen(&m(json!({"state": "on", "device": "Kitchen"})), "Alfred"),
            None
        );
        assert_eq!(parse_screen(&m(json!({"state": "dim"})), "Alfred"), None);
        assert_eq!(parse_screen(&m(json!({})), "Alfred"), None);
        assert_eq!(
            parse_screen(&event(CARD_EVENT, json!({"state": "on"})), "Alfred"),
            None
        );
        // A screen event is not a card.
        assert_eq!(
            parse(&m(json!({"state": "on", "text": "x"})), "Alfred"),
            None
        );
    }

    #[test]
    fn card_ok_event_data() {
        assert_eq!(card_ok_data("washing"), json!({"id": "washing"}));
    }

    #[test]
    fn conversation_event_data() {
        assert_eq!(
            conversation_data("started", "wake", None),
            json!({"state": "started", "trigger": "wake"})
        );
        assert_eq!(
            conversation_data("ended", "talk", Some("reflex")),
            json!({"state": "ended", "trigger": "talk", "handled_by": "reflex"})
        );
    }

    #[test]
    fn who_handled_the_conversation() {
        assert_eq!(handled_by("reflex", 0.0, ""), "reflex");
        assert_eq!(handled_by("played", 2.4, "Done."), "home_assistant");
        // Stopped after the speech went to Home Assistant, or with its answer in.
        assert_eq!(handled_by("stopped", 1.0, ""), "home_assistant");
        assert_eq!(handled_by("stopped", 0.0, "Ok"), "home_assistant");
        assert_eq!(handled_by("no speech", 0.0, ""), "none");
        assert_eq!(handled_by("stopped", 0.0, ""), "none");
    }

    #[test]
    fn a_pose_is_a_plain_file_name() {
        let pose = |p: &str| {
            let m = event(CARD_EVENT, json!({"text": "hi", "pose": p}));
            let Some(CardEvent::Show(c)) = parse(&m, "Alfred") else {
                panic!()
            };
            c.pose
        };
        assert_eq!(pose(" laundry "), Some("laundry".into()));
        assert_eq!(pose("kamado_2"), Some("kamado_2".into()));
        assert_eq!(pose("bbq-grill"), Some("bbq-grill".into()));
        for bad in [
            "",
            "../poses/idle",
            "Laundry",
            "laundry.png",
            "a b",
            &"x".repeat(41),
        ] {
            assert_eq!(pose(bad), None, "{bad:?}");
        }
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
