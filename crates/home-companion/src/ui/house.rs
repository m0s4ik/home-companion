// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the brain's house commands and the "do not disturb" reflex put on the screen (v0.2):
//! the "done" card after a command went through ("Kitchen light: off", with Undo when the brain
//! can undo it), the Yes/No card while a command waits for an answer, and do not disturb. Pure:
//! the screen feeds it the times and draws what it says (screen.rs).

use std::time::{Duration, Instant};

/// The "done" card goes away by itself after this.
pub const DONE_CARD: Duration = Duration::from_secs(20);
/// Commands this close to the previous one join its card ("spegni le luci dello studio" runs
/// one command per light): one pop-up, one Undo for all of them.
pub const GROUP: Duration = Duration::from_secs(4);
/// A group's pop-up lists at most this many lines of names.
pub const POPUP_NAMES: usize = 3;
/// At most this many commands in one card (the brain keeps as many Undos).
pub const GROUP_MAX: usize = 16;
/// With an Undo, a big pop-up in the middle comes first for this long (a wrong command is put
/// right at once, with a big target), then the card stays for the rest of [`DONE_CARD`].
pub const UNDO_POPUP: Duration = Duration::from_secs(5);

/// A house command went through (BrainMsg::Changed).
#[derive(Debug, Clone, PartialEq)]
pub struct DoneCard {
    /// The commands in the card, oldest first (one, or a group).
    pub ids: Vec<i64>,
    /// Their lines, "Kitchen light: off" ([`done_line`]).
    pub lines: Vec<String>,
    pub undo: bool,
    /// When the last one came (a group grows while they keep coming).
    last: Instant,
    until: Instant,
    since: Instant,
    /// Until when the Undo pop-up is up (None: not, or closed).
    popup_until: Option<Instant>,
    /// It came while a question was being carried out (after its yes): its pop-up is that
    /// question's window going on ("Doing it…" → "Closed"), not a new one.
    pub from_ask: bool,
    /// Cancel was touched while it was being carried out: undone as soon as it is here.
    undo_now: bool,
}

/// After a tap on Yes or No, the card waits this long for the brain's `confirm_done` before it
/// closes by itself (the brain is gone or did not hear it).
pub const TAPPED_WAIT_S: f64 = 3.0;

/// A house command waits for a yes (BrainMsg::Confirm). The brain's events are its only truth:
/// it opens with `confirm`, works with `confirm_pending`, closes with `confirm_done`; the
/// safety nets (the brain died) close it at `expires_at` or [`TAPPED_WAIT_S`] after a tap.
#[derive(Debug, Clone, PartialEq)]
pub struct AskCard {
    pub id: i64,
    pub text: String,
    /// The full name of each device it is about.
    pub names: Vec<String>,
    /// Unix seconds: the brain gives up then (and says so with `confirm_done`).
    pub expires_at: i64,
    /// When it came (unix seconds), for the countdown's full length.
    since: f64,
    /// Yes or No touched then (unix seconds): further taps are ignored.
    tapped: Option<f64>,
    /// A spoken yes was received: the commands run then (unix ms) unless a no comes first.
    pending_until_ms: Option<i64>,
    /// Yes was touched: the commands run at once.
    tapped_yes: bool,
    /// Cancel was touched once it was too late for a no: what it does is undone when done.
    undo_wanted: bool,
}

impl AskCard {
    /// The line under the question: the devices' names when there are several (one is in the
    /// question already).
    pub fn names_line(&self) -> String {
        if self.names.len() > 1 {
            self.names.join(", ")
        } else {
            String::new()
        }
    }

    /// A yes came (spoken, waiting for its seconds, or touched): the window says "Doing it…"
    /// with Cancel only, until the brain closes the question and its "done" takes the window.
    pub fn working(&self) -> bool {
        self.tapped_yes || self.pending_until_ms.is_some()
    }

    /// The window's title while it works: the devices' names, one per line ([`POPUP_NAMES`]
    /// at most, then `others`, "e altri {n}"), as the "done" that follows names them.
    pub fn working_title(&self, others: &str) -> String {
        if self.names.is_empty() {
            return self.text.clone();
        }
        let mut names = self.names.clone();
        if names.len() > POPUP_NAMES {
            let rest = names.len() - (POPUP_NAMES - 1);
            names.truncate(POPUP_NAMES - 1);
            names.push(others.replace("{n}", &rest.to_string()));
        }
        names.join("\n")
    }

    /// Cancel was touched too late for a no: the window waits for the "done" to undo it.
    pub fn undo_wanted(&self) -> bool {
        self.undo_wanted
    }
}

/// What Cancel in the question's window does ([`HouseCards::cancel`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancel {
    /// The spoken yes is still in its seconds: a no to this question (nothing ran).
    No(i64),
    /// The commands have run (or are running): undo these as soon as their "done" comes.
    Later,
    /// The done is here: undo it now (the same as its Undo).
    Undo,
    /// Nothing to cancel.
    Nothing,
}

/// How a question left the screen (for the core: UiEvent::QuestionClosed).
#[derive(Debug, Clone, PartialEq)]
pub struct Closed {
    pub id: i64,
    /// Answered (the brain's `confirm_done`), or None.
    pub yes: Option<bool>,
    /// As the brain names it, or "screen" (the card's own safety net), "brain" (a new
    /// subscription: the brain replays what is still open).
    pub via: String,
}

/// The two cards; at most one of each (a newer one replaces the older).
#[derive(Debug, Clone, Default)]
pub struct HouseCards {
    pub done: Option<DoneCard>,
    pub ask: Option<AskCard>,
    /// The last question that left the screen, until the screen tells the core.
    closed: Option<Closed>,
    /// A question just closed after its yes (when, Cancel touched): a "done" that comes a
    /// moment after its question closed still goes on in its window.
    last_yes: Option<(Instant, bool)>,
}

/// How long after its question closed a "done" still belongs to it.
const AFTER_YES: Duration = Duration::from_secs(3);

impl HouseCards {
    /// A command went through: its card for [`DONE_CARD`], or into the card of the one just
    /// before (within [`GROUP`], both with or both without Undo): then its time starts again.
    /// True when it joined a group.
    pub fn changed(&mut self, id: i64, line: String, undo: bool, now: Instant) -> bool {
        // Carried out after a yes: the question's window goes on with it.
        let (from_ask, undo_now) = match self.ask.as_ref().filter(|a| a.working()) {
            Some(a) => (true, a.undo_wanted),
            None => self
                .last_yes
                .filter(|(at, _)| now.saturating_duration_since(*at) <= AFTER_YES)
                .map_or((false, false), |(_, undo)| (true, undo)),
        };
        if let Some(d) = self.done.as_mut() {
            if d.undo == undo
                && d.ids.len() < GROUP_MAX
                && now.saturating_duration_since(d.last) <= GROUP
            {
                d.ids.push(id);
                d.lines.push(line);
                d.last = now;
                d.until = now + DONE_CARD;
                d.from_ask |= from_ask;
                d.undo_now |= undo_now;
                if undo {
                    d.since = now;
                    d.popup_until = Some(now + UNDO_POPUP);
                }
                return true;
            }
        }
        self.done = Some(DoneCard {
            ids: vec![id],
            lines: vec![line],
            undo,
            last: now,
            until: now + DONE_CARD,
            since: now,
            popup_until: undo.then_some(now + UNDO_POPUP),
            from_ask,
            undo_now,
        });
        false
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

    /// The pop-up goes on from a question's window (same size and place, no new window).
    pub fn popup_from_ask(&self) -> bool {
        self.popup() && self.done.as_ref().is_some_and(|d| d.from_ask)
    }

    /// Cancel touched in the question's window, at `now_ms` (unix ms). Before the yes there is
    /// no Cancel (No is there); within a spoken yes's seconds it is a no; after, an undo.
    pub fn cancel(&mut self, now_ms: i64) -> Cancel {
        let Some(a) = self.ask.as_mut() else {
            return if self.popup_from_ask() {
                Cancel::Undo
            } else {
                Cancel::Nothing
            };
        };
        if !a.working() || a.undo_wanted {
            return Cancel::Nothing;
        }
        match a.pending_until_ms {
            Some(u) if !a.tapped_yes && a.tapped.is_none() && now_ms < u => {
                // Should the no come too late (the brain ran it already), its done is undone.
                a.tapped = Some(now_ms as f64 / 1000.0);
                a.undo_wanted = true;
                Cancel::No(a.id)
            }
            _ => {
                a.undo_wanted = true;
                Cancel::Later
            }
        }
    }

    /// A done that came with Cancel already touched: true once, the screen undoes it.
    pub fn take_undo_now(&mut self) -> bool {
        match self.done.as_mut() {
            Some(d) if d.undo_now && d.undo => {
                d.undo_now = false;
                true
            }
            _ => false,
        }
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
    /// it has expired already (a late event). A newer question replaces the one on screen; the
    /// same one again (the brain's replay) keeps its tap and its blink.
    pub fn confirm(
        &mut self,
        id: i64,
        text: String,
        names: Vec<String>,
        expires_at: i64,
        now_unix: f64,
    ) -> bool {
        if (expires_at as f64) <= now_unix {
            return false;
        }
        if let Some(old) = self.ask.as_ref().filter(|a| a.id != id) {
            self.closed = Some(Closed {
                id: old.id,
                yes: None,
                via: "replaced".into(),
            });
        }
        let keep = self.ask.take().filter(|a| a.id == id);
        self.ask = Some(AskCard {
            id,
            text,
            names,
            expires_at,
            since: keep.as_ref().map_or(now_unix, |a| a.since),
            tapped: keep.as_ref().and_then(|a| a.tapped),
            pending_until_ms: keep.as_ref().and_then(|a| a.pending_until_ms),
            tapped_yes: keep.as_ref().is_some_and(|a| a.tapped_yes),
            undo_wanted: keep.is_some_and(|a| a.undo_wanted),
        });
        true
    }

    /// A spoken yes to `id` was received (`confirm_pending`): the window works ("Doing it…",
    /// Cancel = no) until `until_ms`. False when that question is not on screen.
    pub fn pending(&mut self, id: i64, until_ms: i64) -> bool {
        match self.ask.as_mut().filter(|a| a.id == id) {
            Some(a) => {
                a.pending_until_ms = Some(until_ms);
                true
            }
            None => false,
        }
    }

    /// The question `id` is over (`confirm_done`): the card closes. After a yes, the devices
    /// that did not do it (`failed`, already worded: "Tapparella: non riuscito") stay as a card
    /// without Undo. True if it was the one on screen.
    pub fn confirm_done(
        &mut self,
        id: i64,
        yes: Option<bool>,
        via: &str,
        failed: Vec<String>,
        now: Instant,
    ) -> bool {
        if yes == Some(true) {
            for line in failed {
                self.changed(id, line, false, now);
            }
        }
        if self.ask.as_ref().is_some_and(|a| a.id == id) {
            if let Some(a) = self.ask.take().filter(|a| yes == Some(true) && a.working()) {
                self.last_yes = Some((now, a.undo_wanted));
            }
            self.closed = Some(Closed {
                id,
                yes,
                via: via.to_string(),
            });
            return true;
        }
        false
    }

    /// Yes or No touched: the question's id, once (further taps are ignored). The card stays
    /// until the brain says the question is over (after a yes: "Doing it…").
    pub fn answer(&mut self, now_unix: f64, yes: bool) -> Option<i64> {
        let a = self.ask.as_mut()?;
        if a.tapped.is_some() || a.working() {
            return None;
        }
        a.tapped = Some(now_unix);
        a.tapped_yes = yes;
        Some(a.id)
    }

    /// A new subscription to the brain: the question on screen is forgotten (the brain replays
    /// it if it is still open).
    pub fn clear_question(&mut self) {
        if let Some(a) = self.ask.take() {
            self.closed = Some(Closed {
                id: a.id,
                yes: None,
                via: "brain".into(),
            });
        }
    }

    /// The last question that left the screen, once.
    pub fn take_closed(&mut self) -> Option<Closed> {
        self.closed.take()
    }

    /// "Undo" touched: the commands' ids, the card closed.
    pub fn undo(&mut self) -> Option<Vec<i64>> {
        match self.done.take() {
            Some(d) if d.undo => Some(d.ids),
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
        if let Some(id) = self
            .ask
            .as_ref()
            .filter(|a| {
                now_unix >= a.expires_at as f64
                    || a.tapped.is_some_and(|t| now_unix >= t + TAPPED_WAIT_S)
            })
            .map(|a| a.id)
        {
            self.closed = Some(Closed {
                id,
                yes: None,
                via: "screen".into(),
            });
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

impl DoneCard {
    /// What the card and the pop-up say: (card line, pop-up name, pop-up state). One command:
    /// its line, its name and state. A group: "<first> and N more: <state>" on the card
    /// (`more` is the locale's "{name} e altri {n}"), on the pop-up the names one per line, at
    /// most [`POPUP_NAMES`] then "and N more" (`others`, "e altri {n}"), the state when they all
    /// agree.
    pub fn texts(&self, more: &str, others: &str) -> (String, String, String) {
        let parts: Vec<(&str, &str)> = self.lines.iter().map(|l| split_line(l)).collect();
        if parts.len() == 1 {
            let (n, s) = parts[0];
            return (self.lines[0].clone(), n.to_string(), s.to_string());
        }
        let state = if parts.iter().all(|p| p.1 == parts[0].1) {
            parts[0].1
        } else {
            ""
        };
        let first = more
            .replace("{name}", parts[0].0)
            .replace("{n}", &(parts.len() - 1).to_string());
        let card = if state.is_empty() {
            first
        } else {
            format!("{first}: {state}")
        };
        let mut names: Vec<String> = parts.iter().map(|p| p.0.to_string()).collect();
        if names.len() > POPUP_NAMES {
            let rest = names.len() - (POPUP_NAMES - 1);
            names.truncate(POPUP_NAMES - 1);
            names.push(others.replace("{n}", &rest.to_string()));
        }
        (card, names.join("\n"), state.to_string())
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
        // After 5 s the pop-up goes, the card stays until 20 s.
        assert!(c.tick(t0 + UNDO_POPUP, 0.0));
        assert!(!c.popup() && c.done.is_some());
        assert!(!c.tick(t0 + UNDO_POPUP + Duration::from_secs(1), 0.0));
        assert!(c.tick(t0 + DONE_CARD, 0.0));
        assert!(c.done.is_none());
        // OK closes the pop-up only; without an Undo there is no pop-up.
        let t0 = t0 + DONE_CARD;
        c.changed(8, "x".into(), true, t0);
        assert!(c.close_popup() && !c.popup() && c.done.is_some());
        assert!(!c.close_popup());
        c.changed(9, "y".into(), false, t0 + GROUP * 2);
        assert!(!c.popup());
        assert_eq!(c.popup_left(t0), 0.0);
    }

    #[test]
    fn commands_close_together_are_one_card_with_one_undo() {
        let t0 = Instant::now();
        let s = Duration::from_millis(70);
        let mut c = HouseCards::default();
        assert!(!c.changed(1, "Studio SX: spento".into(), true, t0));
        assert!(c.changed(2, "Studio centro: spento".into(), true, t0 + s));
        assert!(c.changed(3, "Libreria: spento".into(), true, t0 + s * 2));
        let d = c.done.clone().unwrap();
        let more = "{name} e altri {n}";
        assert_eq!(
            d.texts(more, "e altri {n}"),
            (
                "Studio SX e altri 2: spento".to_string(),
                "Studio SX\nStudio centro\nLibreria".to_string(),
                "spento".to_string()
            )
        );
        // States that differ: no state; four names: two, then "and 2 more".
        c.changed(4, "Tenda: aperto".into(), true, t0 + s * 3);
        let t = c.done.as_ref().unwrap().texts(more, "e altri {n}");
        assert_eq!(t.2, "");
        assert_eq!(t.1, "Studio SX\nStudio centro\ne altri 2");
        assert_eq!(c.undo(), Some(vec![1, 2, 3, 4]));
        // Far apart, or one with Undo and one without: two cards (the newer replaces).
        c.changed(5, "a: spento".into(), true, t0);
        assert!(!c.changed(6, "b: spento".into(), true, t0 + GROUP * 2));
        assert!(!c.changed(7, "c: acceso".into(), false, t0 + GROUP * 2));
        assert_eq!(c.done.as_ref().unwrap().ids, vec![7]);
        assert_eq!(
            c.done.as_ref().unwrap().texts("x", "y"),
            ("c: acceso".into(), "c".into(), "acceso".into())
        );
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
        assert!(!c.tick(t0 + Duration::from_secs(19), 0.0));
        assert!(c.done.is_some());
        assert!(c.tick(t0 + DONE_CARD, 0.0));
        assert!(c.done.is_none());
        // A newer one replaces it, and starts its own time.
        c.changed(41, "a".into(), true, t0);
        c.changed(42, "b".into(), false, t0 + Duration::from_secs(5));
        assert!(!c.tick(t0 + Duration::from_secs(24), 0.0));
        assert_eq!(c.done.as_ref().map(|d| d.ids.clone()), Some(vec![42]));
    }

    #[test]
    fn undo_only_when_offered() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        c.changed(7, "x".into(), false, t0);
        assert_eq!(c.undo(), None);
        assert!(c.done.is_some(), "a card without Undo stays");
        c.changed(8, "y".into(), true, t0);
        assert_eq!(c.undo(), Some(vec![8]));
        assert!(c.done.is_none());
        assert_eq!(c.undo(), None);
    }

    fn ask(c: &mut HouseCards, id: i64) -> bool {
        c.confirm(
            id,
            "Chiudo?".into(),
            vec!["Tapparella".into()],
            2000,
            1000.0,
        )
    }

    #[test]
    fn ask_expires_and_counts_down() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        assert!(c.confirm(42, "Close the blind?".into(), vec![], 1030, 1000.0));
        assert_eq!(c.ask_left(1000.0), 1.0);
        assert!((c.ask_left(1015.0) - 0.5).abs() < 1e-6);
        assert!(!c.tick(t0, 1029.9));
        // The safety net: closed at expires_at (the brain died), the core told so.
        assert!(c.tick(t0, 1030.0));
        assert!(c.ask.is_none());
        assert_eq!(
            c.take_closed(),
            Some(Closed {
                id: 42,
                yes: None,
                via: "screen".into()
            })
        );
        assert_eq!(c.take_closed(), None, "once");
        assert_eq!(c.ask_left(1031.0), 0.0);
        // Already over when it arrives: nothing.
        assert!(!c.confirm(43, "late".into(), vec![], 999, 1000.0));
        assert!(c.ask.is_none());
    }

    #[test]
    fn confirm_done_closes_only_its_own() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        ask(&mut c, 42);
        assert!(
            !c.confirm_done(41, None, "timeout", vec![], t0),
            "another question"
        );
        assert!(c.ask.is_some());
        // A newer question replaced 42: 42's end leaves it.
        ask(&mut c, 43);
        assert_eq!(
            c.take_closed().map(|x| (x.id, x.via)),
            Some((42, "replaced".into()))
        );
        assert!(!c.confirm_done(42, None, "replaced", vec![], t0));
        assert!(c.confirm_done(43, Some(false), "voice_reflex", vec![], t0));
        assert!(c.ask.is_none());
        assert_eq!(
            c.take_closed(),
            Some(Closed {
                id: 43,
                yes: Some(false),
                via: "voice_reflex".into()
            })
        );
        assert!(!c.confirm_done(43, Some(false), "voice_reflex", vec![], t0));
    }

    /// C24: a tap marks the card (later taps ignored) and it stays until the brain's
    /// `confirm_done`; without one it closes 3 s after the tap.
    #[test]
    fn a_tap_marks_the_card_until_the_brain_closes_it() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        assert_eq!(c.answer(1000.0, true), None, "nothing to answer");
        ask(&mut c, 42);
        assert_eq!(c.answer(1001.0, true), Some(42));
        assert_eq!(c.answer(1001.5, false), None, "one tap only");
        assert!(c.ask.is_some(), "still on screen after the tap");
        assert!(
            c.ask.as_ref().unwrap().working(),
            "a touched yes works at once"
        );
        assert!(!c.tick(t0, 1003.9));
        assert!(c.confirm_done(42, Some(true), "screen", vec![], t0));
        assert!(c.ask.is_none());
        // No confirm_done: 3 s after the tap it goes by itself.
        ask(&mut c, 44);
        let _ = c.take_closed();
        assert_eq!(c.answer(1010.0, false), Some(44));
        assert!(!c.ask.as_ref().unwrap().working(), "a no does not work");
        assert!(!c.tick(t0, 1012.9));
        assert!(c.tick(t0, 1013.0));
        assert_eq!(c.take_closed().map(|x| (x.id, x.yes)), Some((44, None)));
    }

    /// A spoken yes makes the window work ("Doing it…", Cancel); the brain's replay keeps it;
    /// a tap on the old Yes/No no longer answers.
    #[test]
    fn a_spoken_yes_works_until_the_brain_closes_it() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        assert!(!c.pending(42, 5000), "no question on screen");
        ask(&mut c, 42);
        assert!(!c.ask.as_ref().unwrap().working());
        assert!(!c.pending(41, 5000), "another question");
        assert!(c.pending(42, 5000));
        assert!(c.ask.as_ref().unwrap().working());
        assert_eq!(c.answer(1001.0, false), None, "Yes/No are gone");
        ask(&mut c, 42);
        assert!(c.ask.as_ref().unwrap().working(), "the replay keeps it");
        assert!(c.confirm_done(42, Some(true), "voice_reflex", vec![], t0));
        assert!(c.ask.is_none());
    }

    /// One window from the yes on: Cancel within the spoken yes's seconds is a no; later it
    /// undoes what is done as soon as it is here; the done goes on in the question's window.
    #[test]
    fn one_window_from_the_yes_to_the_done() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        assert_eq!(c.cancel(1000), Cancel::Nothing, "nothing on screen");
        ask(&mut c, 42);
        assert_eq!(c.cancel(1000), Cancel::Nothing, "no Cancel before the yes");
        c.pending(42, 5000);
        assert_eq!(c.cancel(4999), Cancel::No(42), "within the seconds: a no");
        assert_eq!(c.cancel(4999), Cancel::Nothing, "once");

        // The seconds are over: Cancel waits for the done, which is undone at once.
        let mut c = HouseCards::default();
        ask(&mut c, 43);
        c.pending(43, 5000);
        assert_eq!(c.cancel(5000), Cancel::Later);
        assert_eq!(c.cancel(5001), Cancel::Nothing, "already asked");
        assert!(!c.changed(7, "Tapparella: Ho chiuso".into(), true, t0));
        assert!(c.done.as_ref().unwrap().from_ask);
        assert!(c.take_undo_now());
        assert!(!c.take_undo_now(), "once");

        // A touched yes, the done, then Cancel: the done's own undo.
        let mut c = HouseCards::default();
        ask(&mut c, 44);
        assert_eq!(c.answer(1000.0, true), Some(44));
        c.changed(8, "Tapparella: Ho chiuso".into(), true, t0);
        assert!(c.confirm_done(44, Some(true), "screen", vec![], t0));
        assert!(c.popup_from_ask());
        assert!(!c.take_undo_now());
        assert_eq!(c.cancel(1001), Cancel::Undo);

        // The done a moment after its question closed still goes on in its window.
        let mut c = HouseCards::default();
        ask(&mut c, 45);
        c.pending(45, 5000);
        assert_eq!(c.cancel(6000), Cancel::Later);
        assert!(c.confirm_done(45, Some(true), "voice_reflex", vec![], t0));
        c.changed(
            9,
            "Tapparella: Ho chiuso".into(),
            true,
            t0 + Duration::from_secs(1),
        );
        assert!(c.popup_from_ask());
        assert!(c.take_undo_now());

        // A light with no question: its own pop-up, as before.
        let mut c = HouseCards::default();
        c.changed(10, "Luce: spenta".into(), true, t0);
        assert!(c.popup() && !c.popup_from_ask());
        assert_eq!(c.cancel(1000), Cancel::Nothing);
    }

    /// C24: a new subscription forgets the question; the brain's replay brings it back.
    #[test]
    fn a_new_subscription_clears_and_the_replay_restores() {
        let mut c = HouseCards::default();
        ask(&mut c, 42);
        c.clear_question();
        assert!(c.ask.is_none());
        assert_eq!(
            c.take_closed().map(|x| (x.id, x.via)),
            Some((42, "brain".into()))
        );
        assert!(ask(&mut c, 42));
        assert_eq!(c.ask.as_ref().map(|a| a.id), Some(42));
        c.clear_question();
        c.clear_question();
        assert_eq!(c.take_closed().map(|x| x.id), Some(42));
        assert_eq!(c.take_closed(), None);
    }

    /// C24: several names under the question; after a yes the ones that failed stay as a card
    /// without Undo.
    #[test]
    fn names_and_failed_lines() {
        let t0 = Instant::now();
        let mut c = HouseCards::default();
        c.confirm(
            42,
            "Chiudo 2 tapparelle?".into(),
            vec!["Tapparella sinistra".into(), "Tapparella destra".into()],
            2000,
            1000.0,
        );
        assert_eq!(
            c.ask.as_ref().unwrap().names_line(),
            "Tapparella sinistra, Tapparella destra"
        );
        ask(&mut c, 43);
        assert_eq!(
            c.ask.as_ref().unwrap().names_line(),
            "",
            "one name: in the question"
        );
        assert!(c.confirm_done(
            43,
            Some(true),
            "screen",
            vec!["Tapparella: non riuscito".into()],
            t0
        ));
        let d = c.done.clone().expect("a card for what failed");
        assert_eq!(d.lines, vec!["Tapparella: non riuscito".to_string()]);
        assert!(!d.undo && !c.popup());
        // A no: no card.
        let mut c = HouseCards::default();
        ask(&mut c, 44);
        c.confirm_done(44, Some(false), "screen", vec!["x".into()], t0);
        assert!(c.done.is_none());
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
