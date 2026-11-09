// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the brain's house commands and the "do not disturb" reflex put on the screen (v0.2):
//! the "done" card after a command went through ("Kitchen light: off", with Undo when the brain
//! can undo it), the Yes/No card while a command waits for an answer, and do not disturb. Pure:
//! the screen feeds it the times and draws what it says (screen.rs).

use std::time::{Duration, Instant};

/// The "done" card goes away by itself after this.
pub const DONE_CARD: Duration = Duration::from_secs(10);
/// With an Undo, a big pop-up in the middle comes first for this long (a wrong command is put
/// right at once, with a big target), then the card stays for the rest of [`DONE_CARD`].
pub const UNDO_POPUP: Duration = Duration::from_secs(5);

/// A house command went through (BrainMsg::Changed).
#[derive(Debug, Clone, PartialEq)]
pub struct DoneCard {
    pub id: i64,
    /// "Kitchen light: off".
    pub line: String,
    pub undo: bool,
    until: Instant,
    since: Instant,
    /// Until when the Undo pop-up is up (None: not, or closed).
    popup_until: Option<Instant>,
}

/// A house command waits for a yes (BrainMsg::Confirm).
#[derive(Debug, Clone, PartialEq)]
pub struct AskCard {
    pub id: i64,
    pub text: String,
    /// Unix seconds: the brain gives up then (and says so with `confirm_done`).
    pub expires_at: i64,
    /// When it came (unix seconds), for the countdown's full length.
    since: f64,
}

/// The two cards; at most one of each (a newer one replaces the older).
#[derive(Debug, Clone, Default)]
pub struct HouseCards {
    pub done: Option<DoneCard>,
    pub ask: Option<AskCard>,
}

impl HouseCards {
    /// A command went through: its card for [`DONE_CARD`].
    pub fn changed(&mut self, id: i64, line: String, undo: bool, now: Instant) {
        self.done = Some(DoneCard {
            id,
            line,
            undo,
            until: now + DONE_CARD,
            since: now,
            popup_until: undo.then_some(now + UNDO_POPUP),
        });
    }

    /// Whether the Undo pop-up is up.
    pub fn popup(&self) -> bool {
        self.done.as_ref().is_some_and(|d| d.popup_until.is_some())
    }

    /// The pop-up's time left, 1 (just done) to 0: its countdown's length.
    pub fn popup_left(&self, now: Instant) -> f32 {
        let Some(until) = self
            .done
            .as_ref()
            .and_then(|d| d.popup_until.map(|u| (d.since, u)))
        else {
            return 0.0;
        };
        let total = (until.1 - until.0).as_secs_f32().max(0.001);
        (until.1.saturating_duration_since(now).as_secs_f32() / total).clamp(0.0, 1.0)
    }

    /// The pop-up's OK (or a tap around it): the pop-up closes, the card stays.
    pub fn close_popup(&mut self) -> bool {
        match self.done.as_mut() {
            Some(d) if d.popup_until.is_some() => {
                d.popup_until = None;
                true
            }
            _ => false,
        }
    }

    /// A question: shown until `expires_at` or its `confirm_done`. False (nothing shown) when
    /// it has expired already (a late event).
    pub fn confirm(&mut self, id: i64, text: String, expires_at: i64, now_unix: f64) -> bool {
        if (expires_at as f64) <= now_unix {
            return false;
        }
        self.ask = Some(AskCard {
            id,
            text,
            expires_at,
            since: now_unix,
        });
        true
    }

    /// The question `id` is over (answered here, by voice, or expired at the brain): true if
    /// it was the one on screen.
    pub fn confirm_done(&mut self, id: i64) -> bool {
        if self.ask.as_ref().is_some_and(|a| a.id == id) {
            self.ask = None;
            return true;
        }
        false
    }

    /// Yes or No touched: the question's id, the card closed (the brain's `confirm_done`
    /// follows and finds nothing to close).
    pub fn answer(&mut self) -> Option<i64> {
        self.ask.take().map(|a| a.id)
    }

    /// "Undo" touched: the command's id, the card closed.
    pub fn undo(&mut self) -> Option<i64> {
        match self.done.take() {
            Some(d) if d.undo => Some(d.id),
            other => {
                self.done = other;
                None
            }
        }
    }

    /// Cards whose time is over go: true if one went.
    pub fn tick(&mut self, now: Instant, now_unix: f64) -> bool {
        let mut gone = false;
        if self.done.as_ref().is_some_and(|d| now >= d.until) {
            self.done = None;
            gone = true;
        }
        if let Some(d) = self.done.as_mut() {
            if d.popup_until.is_some_and(|u| now >= u) {
                d.popup_until = None;
                gone = true;
            }
        }
        if self
            .ask
            .as_ref()
            .is_some_and(|a| now_unix >= a.expires_at as f64)
        {
            self.ask = None;
            gone = true;
        }
        gone
    }

    /// The question's time left, 1 (just asked) to 0 (expired): the countdown's length.
    pub fn ask_left(&self, now_unix: f64) -> f32 {
        let Some(a) = &self.ask else { return 0.0 };
        let total = (a.expires_at as f64 - a.since).max(1.0);
        ((a.expires_at as f64 - now_unix) / total).clamp(0.0, 1.0) as f32
    }
}

/// The pop-up's two lines from the card's line ([`done_line`]): "Luce cucina: spento" →
/// ("Luce cucina", "spento"); a line without a state is all name.
pub fn split_line(line: &str) -> (&str, &str) {
    line.rsplit_once(": ").unwrap_or((line, ""))
}

/// The card's line: the entity's name and its state now, in the user's words ("Luce cucina:
/// spenta" comes from the locale's `house_state_off`). `word` gives a locale word or None when
/// the locale has no such key. Without a state (Home Assistant had not updated it yet) the
/// service says what it should be; an unknown one is just "done".
pub fn done_line(
    name: &str,
    state: Option<&str>,
    service: &str,
    word: &dyn Fn(&str) -> Option<String>,
) -> String {
    let by_service = || {
        let s = match service {
            "turn_on" => "on",
            "turn_off" => "off",
            "open_cover" | "open_valve" => "open",
            "close_cover" | "close_valve" => "closed",
            _ => return None,
        };
        word(&format!("house_state_{s}"))
    };
    let what = match state {
        Some(s) if !s.is_empty() => word(&format!("house_state_{s}"))
            .or_else(by_service)
            .unwrap_or_else(|| s.to_string()),
        _ => by_service()
            .or_else(|| word("house_done"))
            .unwrap_or_default(),
    };
    if what.is_empty() {
        name.to_string()
    } else if name.is_empty() {
        what
    } else {
        format!("{name}: {what}")
    }
}

/// Do not disturb (UiCommand::DoNotDisturb): until a unix time, or off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dnd {
    until: Option<u64>,
}

impl Dnd {
    pub fn set(&mut self, until: Option<u64>) {
        self.until = until;
    }

    pub fn until(&self) -> Option<u64> {
        self.until
    }

    pub fn active(&self, now_unix: u64) -> bool {
        self.until.is_some_and(|u| now_unix < u)
    }

    /// Its time ran out: off now (true once, to redraw).
    pub fn expire(&mut self, now_unix: u64) -> bool {
        if self.until.is_some_and(|u| now_unix >= u) {
            self.until = None;
            return true;
        }
        false
    }
}

/// The next time the local clock reads `hour`:00 after `now` (unix seconds; `gmtoff` = the
/// local offset from UTC in seconds): the control center's do not disturb lasts until the
/// morning (the night's end hour), as a phone's "until tomorrow morning".
pub fn next_hour(now: i64, gmtoff: i64, hour: u32) -> i64 {
    let local = now + gmtoff;
    let day = local.div_euclid(86_400) * 86_400;
    let mut at = day + hour as i64 * 3600;
    if at <= local {
        at += 86_400;
    }
    at - gmtoff
}

/// What the user can say, by group (Altro → "What to say"): the locale's keys of each group's
/// title and examples, and its icon in companion.slint.
pub const WHAT_GROUPS: [(&str, &str); 7] = [
    ("time", "timer"),
    ("alarms", "alarm"),
    ("reminders", "calendar-clock"),
    ("lists", "cart"),
    ("house", "view-dashboard"),
    ("music", "music-note"),
    ("screen", "screen"),
];

/// The page's rows: (icon, title, examples), `{name}` replaced by the character's.
pub fn what_rows(word: &dyn Fn(&str) -> String, name: &str) -> Vec<(String, String, String)> {
    // Each sentence in the language's quotes ("«{s}»").
    let quote = word("what_quote");
    WHAT_GROUPS
        .iter()
        .map(|(key, icon)| {
            (
                icon.to_string(),
                word(&format!("what_{key}")).replace("{name}", name),
                word(&format!("what_{key}_say"))
                    .replace("{name}", name)
                    .split('|')
                    .map(|s| quote.replace("{s}", s.trim()))
                    .collect::<Vec<_>>()
                    .join("   "),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pop_up_splits_name_and_state() {
        assert_eq!(split_line("Luce cucina: spento"), ("Luce cucina", "spento"));
        assert_eq!(split_line("Fatto"), ("Fatto", ""));
    }

    #[test]
    fn an_undo_pops_up_first_then_the_card_stays() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        c.changed(7, "Luce studio: spenta".into(), true, t0);
        assert!(c.popup());
        assert_eq!(c.popup_left(t0), 1.0);
        assert!((c.popup_left(t0 + UNDO_POPUP / 2) - 0.5).abs() < 0.01);
        // After 5 s the pop-up goes, the card stays until 10 s.
        assert!(c.tick(t0 + UNDO_POPUP, 0.0));
        assert!(!c.popup() && c.done.is_some());
        assert!(!c.tick(t0 + UNDO_POPUP + Duration::from_secs(1), 0.0));
        assert!(c.tick(t0 + DONE_CARD, 0.0));
        assert!(c.done.is_none());
        // OK closes the pop-up only; without an Undo there is no pop-up.
        c.changed(8, "x".into(), true, t0);
        assert!(c.close_popup() && !c.popup() && c.done.is_some());
        assert!(!c.close_popup());
        c.changed(9, "y".into(), false, t0);
        assert!(!c.popup());
        assert_eq!(c.popup_left(t0), 0.0);
    }

    fn it(k: &str) -> Option<String> {
        match k {
            "house_state_off" => Some("spenta".into()),
            "house_state_on" => Some("accesa".into()),
            "house_state_closed" => Some("chiusa".into()),
            "house_done" => Some("fatto".into()),
            _ => None,
        }
    }

    #[test]
    fn done_card_goes_after_its_time() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        // (No Undo here: no pop-up going away at 5 s.)
        c.changed(41, "Kitchen light: off".into(), false, t0);
        assert!(!c.tick(t0 + Duration::from_secs(9), 0.0));
        assert!(c.done.is_some());
        assert!(c.tick(t0 + DONE_CARD, 0.0));
        assert!(c.done.is_none());
        // A newer one replaces it, and starts its own time.
        c.changed(41, "a".into(), true, t0);
        c.changed(42, "b".into(), false, t0 + Duration::from_secs(5));
        assert!(!c.tick(t0 + Duration::from_secs(14), 0.0));
        assert_eq!(c.done.as_ref().map(|d| d.id), Some(42));
    }

    #[test]
    fn undo_only_when_offered() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        c.changed(7, "x".into(), false, t0);
        assert_eq!(c.undo(), None);
        assert!(c.done.is_some(), "a card without Undo stays");
        c.changed(8, "y".into(), true, t0);
        assert_eq!(c.undo(), Some(8));
        assert!(c.done.is_none());
        assert_eq!(c.undo(), None);
    }

    #[test]
    fn ask_expires_and_counts_down() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        assert!(c.confirm(42, "Close the blind?".into(), 1030, 1000.0));
        assert_eq!(c.ask_left(1000.0), 1.0);
        assert!((c.ask_left(1015.0) - 0.5).abs() < 1e-6);
        assert!(!c.tick(t0, 1029.9));
        assert!(c.tick(t0, 1030.0));
        assert!(c.ask.is_none());
        assert_eq!(c.ask_left(1031.0), 0.0);
        // Already over when it arrives: nothing.
        assert!(!c.confirm(43, "late".into(), 999, 1000.0));
        assert!(c.ask.is_none());
    }

    #[test]
    fn confirm_done_closes_only_its_own() {
        let mut c = HouseCards::default();
        c.confirm(42, "a".into(), 2000, 1000.0);
        assert!(!c.confirm_done(41), "another question");
        assert!(c.ask.is_some());
        // A newer question replaced 42: 42's end leaves it.
        c.confirm(43, "b".into(), 2000, 1000.0);
        assert!(!c.confirm_done(42));
        assert!(c.confirm_done(43));
        assert!(c.ask.is_none());
        assert!(!c.confirm_done(43));
    }

    #[test]
    fn answer_takes_the_question() {
        let mut c = HouseCards::default();
        assert_eq!(c.answer(), None);
        c.confirm(42, "a".into(), 2000, 1000.0);
        assert_eq!(c.answer(), Some(42));
        assert!(c.ask.is_none());
    }

    #[test]
    fn the_done_line() {
        assert_eq!(
            done_line("Luce cucina", Some("off"), "turn_off", &it),
            "Luce cucina: spenta"
        );
        // No state yet: the service tells.
        assert_eq!(
            done_line("Luce cucina", None, "turn_on", &it),
            "Luce cucina: accesa"
        );
        assert_eq!(
            done_line("Tapparella", None, "close_cover", &it),
            "Tapparella: chiusa"
        );
        // A state the locale has no word for: as Home Assistant says it.
        assert_eq!(
            done_line("Termostato", Some("heat"), "set_hvac_mode", &it),
            "Termostato: heat"
        );
        assert_eq!(
            done_line("Scena", None, "turn_on_scene", &it),
            "Scena: fatto"
        );
        assert_eq!(done_line("", Some("off"), "turn_off", &it), "spenta");
    }

    #[test]
    fn dnd_until_and_off() {
        let mut d = Dnd::default();
        assert!(!d.active(100));
        d.set(Some(200));
        assert!(d.active(100) && d.active(199));
        assert!(!d.expire(199));
        assert!(!d.active(200));
        assert!(d.expire(200));
        assert_eq!(d.until(), None);
        assert!(!d.expire(300), "once");
        d.set(Some(500));
        d.set(None);
        assert!(!d.active(100));
    }

    #[test]
    fn next_morning() {
        // 2026-11-03 14:25 UTC, in UTC: 07:00 the next day.
        let now = 1_793_715_900;
        let day = 1_793_664_000; // 2026-11-03 00:00 UTC
        assert_eq!(next_hour(now, 0, 7), day + 86_400 + 7 * 3600);
        // Before 7 in the morning: today's.
        assert_eq!(next_hour(day + 3600, 0, 7), day + 7 * 3600);
        // Exactly 7: tomorrow's.
        assert_eq!(next_hour(day + 7 * 3600, 0, 7), day + 86_400 + 7 * 3600);
        // In Rome (UTC+1): 07:00 local = 06:00 UTC.
        assert_eq!(next_hour(now, 3600, 7), day + 86_400 + 6 * 3600);
    }

    #[test]
    fn what_rows_have_the_name() {
        let w = |k: &str| match k {
            "what_time" => "Tempo".to_string(),
            "what_time_say" => "Che ore sono? | Timer di 5 minuti".to_string(),
            "what_screen_say" => "{name}, spegni lo schermo".to_string(),
            "what_quote" => "«{s}»".to_string(),
            k => k.to_string(),
        };
        let r = what_rows(&w, "Alfred");
        assert_eq!(r.len(), WHAT_GROUPS.len());
        assert_eq!(
            r[0],
            (
                "timer".to_string(),
                "Tempo".to_string(),
                "«Che ore sono?»   «Timer di 5 minuti»".to_string()
            )
        );
        assert_eq!(r[6].2, "«Alfred, spegni lo schermo»");
    }
}
