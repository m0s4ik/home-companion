// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The home's middle band: at most three cards, the most urgent first:
//! (0) a house command that just went through ("Studio: spento", with Undo while it can be
//! undone; after its big pop-up, for its 20 s), (1) the body's own status (Home Assistant not answering), (1b) the setup in Home Assistant
//! asking for the user ("Complete" when an AI appeared, or something to fix: it opens the
//! Home Assistant page), (2) messages that need an OK (one
//! card, "1 di 3" when more wait), (2b) a screen mirroring the user left for the home (it
//! reopens the mirrored screen), (3) timers, soonest first (at most two; more become one
//! "+N timer" card that opens the timers page), (4) the next alarm within a day (it opens the
//! timers and alarms page), (5) the next reminder of home-companion-brain (it opens
//! Ricordi), (6) the shopping list, (7) what plays, (8) the last long answer for a few minutes
//! (it opens Conversazioni). Nothing alive: no cards, the invitation shows.

use std::time::{Duration, Instant};

use super::CardView;
use crate::ha::events::{CardEvent, HaCard};
use crate::timers::TimerView;

pub const MAX_CARDS: usize = 3;
pub const MAX_TIMER_CARDS: usize = 2;
/// An answer longer than this does not fit Alfred's five lines: it gets a card.
pub const LONG_ANSWER: usize = 160;

/// What may become a card.
#[derive(Debug, Default)]
pub struct Alive<'a> {
    /// (what, its state, Undo) of a house command that just went through.
    pub done: Option<(String, String, bool)>,
    /// (title, text) when the body has a problem to tell.
    pub status: Option<(String, String)>,
    /// (title, text) when the setup in Home Assistant needs the user.
    pub setup: Option<(String, String)>,
    /// Messages waiting for an OK: (title, text).
    pub messages: &'a [(String, String)],
    /// ("Duplicazione schermo", "iPhone · Tocca per tornare") while a phone mirrors its screen
    /// and the page is not on screen.
    pub mirror: Option<(String, String)>,
    /// ("Configurazione completata", "Tocca per aprire Home Assistant") after the first setup,
    /// until the user opens it or closes it.
    pub ready: Option<(String, String)>,
    /// Cards sent by Home Assistant, oldest first: (title, text, kind from [`icon_kind`]).
    pub ha: &'a [(String, String, &'static str)],
    pub timers: &'a [TimerView],
    /// The next alarm within a day: ("Sveglia · domani 07:00", its days).
    pub alarm: Option<(String, String)>,
    /// The next reminder still to come: ("Promemoria · oggi 15:00", its text).
    pub upcoming: Option<(String, String)>,
    /// The shopping list: ("Spesa · 4 da prendere", the first items).
    pub shopping: Option<(String, String)>,
    /// (title, "artist · source", playing).
    pub music: Option<(String, String, bool)>,
    /// (title, the answer) while it is recent.
    pub answer: Option<(String, String)>,
}

/// The words the cards need: "{i} of {n}" for messages, "+{n} timers", "Paused".
pub struct CardWords<'a> {
    pub of: &'a dyn Fn(usize, usize) -> String,
    pub more_timers: &'a dyn Fn(usize) -> String,
    pub paused: &'a str,
}

pub fn pick(a: &Alive, w: &CardWords) -> Vec<CardView> {
    let mut out = Vec::new();
    let card = |kind, title: &str, text: &str| CardView {
        kind,
        title: title.to_string(),
        text: text.to_string(),
        left: String::new(),
        remaining: 0.0,
        paused: false,
    };
    if let Some((t, x, undo)) = &a.done {
        out.push(card(if *undo { "done-undo" } else { "done" }, t, x));
    }
    if let Some((t, x)) = &a.status {
        out.push(card("status", t, x));
    }
    if let Some((t, x)) = &a.setup {
        out.push(card("setup", t, x));
    }
    if let Some((t, x)) = &a.ready {
        out.push(card("ready", t, x));
    }
    if let Some((t, x)) = a.messages.first() {
        let title = if a.messages.len() > 1 {
            format!("{t} · {}", (w.of)(1, a.messages.len()))
        } else {
            t.clone()
        };
        // Today every message is one of home-companion-brain's (a reminder that is due, the morning
        // summary): the reminder's calendar, not a bell.
        out.push(card("reminder", &title, x));
    }
    for (t, x, kind) in a.ha {
        out.push(card(kind, t, x));
    }
    if let Some((t, x)) = &a.mirror {
        out.push(card("mirror", t, x));
    }
    let shown = if a.timers.len() > MAX_TIMER_CARDS {
        MAX_TIMER_CARDS - 1
    } else {
        a.timers.len()
    };
    for t in &a.timers[..shown] {
        out.push(CardView {
            kind: "timer",
            title: t.name.clone(),
            text: if t.paused {
                w.paused.to_string()
            } else {
                String::new()
            },
            left: t.left.clone(),
            remaining: t.fraction,
            paused: t.paused,
        });
    }
    if a.timers.len() > shown {
        let rest = &a.timers[shown..];
        let names: Vec<&str> = rest.iter().map(|t| t.name.as_str()).collect();
        out.push(card(
            "more",
            &(w.more_timers)(rest.len()),
            &names.join(" · "),
        ));
    }
    if let Some((t, x)) = &a.alarm {
        out.push(card("alarm", t, x));
    }
    if let Some((t, x)) = &a.upcoming {
        out.push(card("upcoming", t, x));
    }
    if let Some((t, x)) = &a.shopping {
        out.push(card("shopping", t, x));
    }
    if let Some((t, x, playing)) = &a.music {
        let mut c = card("music", t, x);
        c.paused = !playing;
        out.push(c);
    }
    if let Some((t, x)) = &a.answer {
        out.push(card("answer", t, x));
    }
    out.truncate(MAX_CARDS);
    out
}

/// The card kinds of Home Assistant's cards (the screen draws the icon from the kind). All of
/// them start with `hc-`, except the generic one.
pub const HA_KINDS: [&str; 10] = [
    "message",
    "hc-alert",
    "hc-check",
    "hc-cart",
    "hc-calendar",
    "hc-alarm",
    "hc-timer",
    "hc-music",
    "hc-rain",
    "hc-sun",
];

/// A card kind of a Home Assistant card: a few common `mdi:` names map to icons the screen
/// has; anything else is the generic message.
pub fn icon_kind(icon: Option<&str>) -> &'static str {
    let name = icon.and_then(|i| i.strip_prefix("mdi:")).unwrap_or("");
    let has = |parts: &[&str]| parts.iter().any(|p| name.contains(p));
    if name.starts_with("bell") {
        "message"
    } else if has(&[
        "alert",
        "fire",
        "smoke",
        "water-alert",
        "alarm-light",
        "shield-alert",
    ]) {
        "hc-alert"
    } else if name.starts_with("check") {
        "hc-check"
    } else if has(&["cart", "basket", "shopping"]) {
        "hc-cart"
    } else if name.starts_with("calendar") {
        "hc-calendar"
    } else if name.starts_with("alarm") {
        "hc-alarm"
    } else if name.starts_with("timer") || has(&["clock", "washing-machine", "dishwasher"]) {
        "hc-timer"
    } else if name.starts_with("music") || has(&["speaker", "volume"]) {
        "hc-music"
    } else if name.contains("rain") || name.contains("umbrella") || name.contains("pouring") {
        "hc-rain"
    } else if name.contains("sun") || name.contains("weather-sunny") {
        "hc-sun"
    } else {
        "message"
    }
}

/// At most this many cards from Home Assistant are kept (the oldest go first).
pub const MAX_HA_CARDS: usize = 6;

#[derive(Debug, Clone)]
struct HaEntry {
    card: HaCard,
    expires: Option<Instant>,
}

/// The cards Home Assistant sent and that are still on: a card with the id of one already
/// there replaces it, `seconds` ends a card by itself, a tap on OK (or a dismiss event) ends it.
#[derive(Debug, Default)]
pub struct HaCards {
    items: Vec<HaEntry>,
}

impl HaCards {
    /// Applies an event; true if what is shown changed (and, for a card, that a sound is due).
    pub fn apply(&mut self, ev: CardEvent, now: Instant) -> bool {
        match ev {
            CardEvent::Show(card) => {
                let entry = HaEntry {
                    expires: card.seconds.map(|s| now + Duration::from_secs(s)),
                    card,
                };
                let same = entry.card.id.as_ref().and_then(|id| {
                    self.items
                        .iter()
                        .position(|e| e.card.id.as_ref() == Some(id))
                });
                match same {
                    Some(i) => self.items[i] = entry,
                    None => {
                        self.items.push(entry);
                        if self.items.len() > MAX_HA_CARDS {
                            self.items.remove(0);
                        }
                    }
                }
                true
            }
            CardEvent::Dismiss(id) => {
                let before = self.items.len();
                self.items
                    .retain(|e| e.card.id.as_deref() != Some(id.as_str()));
                self.items.len() != before
            }
        }
    }

    /// Drops the cards whose time is up; true if there were any.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.items.len();
        self.items.retain(|e| e.expires.is_none_or(|t| t > now));
        self.items.len() != before
    }

    /// The OK of the `n`th card (0 = the oldest).
    pub fn dismiss_nth(&mut self, n: usize) -> bool {
        if n < self.items.len() {
            self.items.remove(n);
            true
        } else {
            false
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// (title, text, kind) for [`Alive::ha`]; `default_title` for a card without one.
    pub fn views(&self, default_title: &str) -> Vec<(String, String, &'static str)> {
        self.items
            .iter()
            .map(|e| {
                (
                    e.card
                        .title
                        .clone()
                        .unwrap_or_else(|| default_title.to_string()),
                    e.card.text.clone(),
                    icon_kind(e.card.icon.as_deref()),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_done_house_command_is_the_first_card() {
        let timers = [];
        let msgs = [("Promemoria".to_string(), "Dentista".to_string())];
        let a = Alive {
            done: Some(("Studio".into(), "spento".into(), true)),
            status: Some(("Offline".into(), "x".into())),
            messages: &msgs,
            timers: &timers,
            ..Default::default()
        };
        let w = CardWords {
            of: &|i, n| format!("{i} di {n}"),
            more_timers: &|n| format!("+{n}"),
            paused: "In pausa",
        };
        let c = pick(&a, &w);
        let kinds: Vec<&str> = c.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, ["done-undo", "status", "reminder"]);
        assert_eq!(
            (c[0].title.as_str(), c[0].text.as_str()),
            ("Studio", "spento")
        );
    }

    fn timer(name: &str, left: &str) -> TimerView {
        TimerView {
            id: name.into(),
            name: name.into(),
            left: left.into(),
            fraction: 0.5,
            paused: false,
            due_unix: 0.0,
        }
    }

    fn words() -> (impl Fn(usize, usize) -> String, impl Fn(usize) -> String) {
        (|i, n| format!("{i} di {n}"), |n| format!("+{n} timer"))
    }

    fn kinds(v: &[CardView]) -> Vec<&str> {
        v.iter().map(|c| c.kind).collect()
    }

    #[test]
    fn nothing_alive_no_cards() {
        let (of, more) = words();
        let w = CardWords {
            of: &of,
            more_timers: &more,
            paused: "In pausa",
        };
        assert!(pick(&Alive::default(), &w).is_empty());
    }

    #[test]
    fn most_urgent_first_three_at_most() {
        let (of, more) = words();
        let w = CardWords {
            of: &of,
            more_timers: &more,
            paused: "In pausa",
        };
        let msgs = vec![
            ("Lavatrice".to_string(), "Ha finito".to_string()),
            ("Porta".into(), "Aperta".into()),
        ];
        let timers = vec![timer("Uova", "07:42")];
        let a = Alive {
            done: None,
            status: Some(("Home Assistant non risponde".into(), "Riprovo".into())),
            setup: None,
            messages: &msgs,
            mirror: None,
            ready: None,
            timers: &timers,
            ha: &[],
            alarm: None,
            upcoming: None,
            shopping: None,
            music: Some(("Clair de lune".into(), "Debussy · AirPlay".into(), true)),
            answer: None,
        };
        let c = pick(&a, &w);
        assert_eq!(kinds(&c), ["status", "reminder", "timer"]);
        let a = Alive {
            setup: Some(("Completa".into(), "Tocca".into())),
            messages: &msgs,
            timers: &timers,
            ..Default::default()
        };
        assert_eq!(kinds(&pick(&a, &w)), ["setup", "reminder", "timer"]);
        assert_eq!(c[1].title, "Lavatrice · 1 di 2");
    }

    #[test]
    fn more_than_two_timers_make_one_more_card() {
        let (of, more) = words();
        let w = CardWords {
            of: &of,
            more_timers: &more,
            paused: "In pausa",
        };
        let timers = vec![
            timer("Uova", "01:00"),
            timer("Pasta", "05:00"),
            timer("Pane", "20:00"),
        ];
        let a = Alive {
            timers: &timers,
            ..Default::default()
        };
        let c = pick(&a, &w);
        assert_eq!(kinds(&c), ["timer", "more"]);
        assert_eq!(c[1].title, "+2 timer");
        assert_eq!(c[1].text, "Pasta · Pane");
        let two = &timers[..2];
        let a = Alive {
            timers: two,
            ..Default::default()
        };
        assert_eq!(kinds(&pick(&a, &w)), ["timer", "timer"]);
    }

    #[test]
    fn mirroring_after_messages_before_timers() {
        let (of, more) = words();
        let w = CardWords {
            of: &of,
            more_timers: &more,
            paused: "In pausa",
        };
        let msgs = vec![("Lavatrice".to_string(), "Ha finito".to_string())];
        let timers = vec![timer("Uova", "07:42")];
        let a = Alive {
            messages: &msgs,
            mirror: Some((
                "Duplicazione schermo".into(),
                "iPhone · Tocca per tornare".into(),
            )),
            timers: &timers,
            music: Some(("t".into(), "d".into(), true)),
            ..Default::default()
        };
        assert_eq!(kinds(&pick(&a, &w)), ["reminder", "mirror", "timer"]);
    }

    #[test]
    fn music_and_answer_come_last() {
        let (of, more) = words();
        let w = CardWords {
            of: &of,
            more_timers: &more,
            paused: "In pausa",
        };
        let a = Alive {
            music: Some(("t".into(), "d".into(), false)),
            answer: Some(("Ultima risposta".into(), "…".into())),
            ..Default::default()
        };
        let c = pick(&a, &w);
        assert_eq!(kinds(&c), ["music", "answer"]);
        assert!(c[0].paused);
    }

    #[test]
    fn next_reminder_after_timers_before_shopping() {
        let (of, more) = words();
        let w = CardWords {
            of: &of,
            more_timers: &more,
            paused: "In pausa",
        };
        let timers = [timer("Pasta", "4:00")];
        let a = Alive {
            timers: &timers,
            upcoming: Some(("Promemoria · oggi 15:00".into(), "dentista".into())),
            shopping: Some(("Spesa · 2 da prendere".into(), "pane · latte".into())),
            ..Default::default()
        };
        assert_eq!(kinds(&pick(&a, &w)), ["timer", "upcoming", "shopping"]);
        let b = Alive {
            alarm: Some(("Sveglia · domani 07:00".into(), String::new())),
            upcoming: Some(("Promemoria · oggi 15:00".into(), "dentista".into())),
            ..Default::default()
        };
        assert_eq!(kinds(&pick(&b, &w)), ["alarm", "upcoming"]);
    }

    fn ha(id: Option<&str>, text: &str, seconds: Option<u64>) -> CardEvent {
        CardEvent::Show(HaCard {
            id: id.map(String::from),
            text: text.into(),
            seconds,
            ..Default::default()
        })
    }

    #[test]
    fn ha_cards_replace_by_id_and_expire() {
        let t0 = Instant::now();
        let mut c = HaCards::default();
        assert!(c.apply(ha(Some("w"), "Done", Some(60)), t0));
        assert!(c.apply(ha(None, "No id", None), t0));
        assert!(c.apply(ha(None, "No id either", None), t0));
        assert!(c.apply(ha(Some("w"), "Again", Some(10)), t0));
        let v = c.views("Message");
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].1, "Again");
        assert_eq!(v[0].0, "Message");
        // Ten seconds later the replaced card is gone, the ones without a time stay.
        assert!(!c.expire(t0 + Duration::from_secs(5)));
        assert!(c.expire(t0 + Duration::from_secs(11)));
        assert_eq!(c.views("M").len(), 2);
        assert!(!c.expire(t0 + Duration::from_secs(9999)));
    }

    #[test]
    fn ha_cards_dismiss_and_limit() {
        let t0 = Instant::now();
        let mut c = HaCards::default();
        c.apply(ha(Some("a"), "A", None), t0);
        c.apply(ha(Some("b"), "B", None), t0);
        assert!(c.apply(CardEvent::Dismiss("a".into()), t0));
        assert!(!c.apply(CardEvent::Dismiss("a".into()), t0));
        assert!(c.dismiss_nth(0));
        assert!(!c.dismiss_nth(0));
        assert!(c.is_empty());
        for i in 0..MAX_HA_CARDS + 3 {
            c.apply(ha(None, &i.to_string(), None), t0);
        }
        let v = c.views("M");
        assert_eq!(v.len(), MAX_HA_CARDS);
        assert_eq!(v[0].1, "3");
    }

    #[test]
    fn mdi_names_map_to_known_icons() {
        assert_eq!(icon_kind(Some("mdi:cart-outline")), "hc-cart");
        assert_eq!(icon_kind(Some("mdi:washing-machine")), "hc-timer");
        assert_eq!(icon_kind(Some("mdi:alert-circle")), "hc-alert");
        assert_eq!(icon_kind(Some("mdi:bell-ring")), "message");
        assert_eq!(icon_kind(Some("mdi:weather-rainy")), "hc-rain");
        assert_eq!(icon_kind(Some("mdi:something-new")), "message");
        assert_eq!(icon_kind(None), "message");
        for k in [
            "hc-cart", "hc-timer", "hc-alert", "hc-rain", "hc-sun", "hc-music", "hc-check",
        ] {
            assert!(HA_KINDS.contains(&k));
        }
    }

    #[test]
    fn ha_cards_come_after_messages() {
        let (of, more) = words();
        let w = CardWords {
            of: &of,
            more_timers: &more,
            paused: "P",
        };
        let msgs = vec![("R".to_string(), "x".to_string())];
        let ha = vec![("Laundry".to_string(), "Done".to_string(), "hc-timer")];
        let a = Alive {
            messages: &msgs,
            ha: &ha,
            ..Default::default()
        };
        assert_eq!(kinds(&pick(&a, &w)), ["reminder", "hc-timer"]);
    }
}
