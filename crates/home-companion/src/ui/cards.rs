// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The home's middle band: at most three cards, the most urgent first:
//! (1) the body's own status (Home Assistant not answering), (1b) the setup in Home Assistant
//! asking for the user ("Complete" when an AI appeared, or something to fix: it opens the
//! Home Assistant page), (2) messages that need an OK (one
//! card, "1 di 3" when more wait), (2b) a screen mirroring the user left for the home (it
//! reopens the mirrored screen), (3) timers, soonest first (at most two; more become one
//! "+N timer" card that opens the timers page), (4) the next alarm within a day (it opens the
//! timers and alarms page), (5) the next reminder of home-companion-brain (it opens
//! Ricordi), (6) the shopping list, (7) what plays, (8) the last long answer for a few minutes
//! (it opens Conversazioni). Nothing alive: no cards, the invitation shows.

use super::CardView;
use crate::timers::TimerView;

pub const MAX_CARDS: usize = 3;
pub const MAX_TIMER_CARDS: usize = 2;
/// An answer longer than this does not fit Alfred's five lines: it gets a card.
pub const LONG_ANSWER: usize = 160;

/// What may become a card.
#[derive(Debug, Default)]
pub struct Alive<'a> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            status: Some(("Home Assistant non risponde".into(), "Riprovo".into())),
            setup: None,
            messages: &msgs,
            mirror: None,
            ready: None,
            timers: &timers,
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
}
