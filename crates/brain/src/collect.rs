// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the memory may keep: the rules in code, not only in the model's instructions.
//!
//! A fact proposed by the model (at the end of a conversation, or with the `remember` tool on
//! its own initiative) goes through [`gate`]: a closed set of kinds, one short sentence, no
//! years (a record of a day, not a standing fact), a quote of the user's own words it rests on
//! (never the assistant's answer), not a chronicle of commands and lists. A person's name heard
//! once stays a *candidate* (not told to the agent) until the user confirms it, says it again
//! on another day, or it is a person registered in Home Assistant. Conversations made only of
//! commands never reach the model ([`worth_extracting`]). Pure functions, tested here.

use crate::memory::Exchange;

/// The kinds of fact; anything else from the model is refused, older kinds read as `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Person,
    Preference,
    Routine,
    House,
    Other,
}

impl Kind {
    /// One of the closed set, or None.
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s.trim().to_lowercase().as_str() {
            "person" => Kind::Person,
            "preference" => Kind::Preference,
            "routine" => Kind::Routine,
            "house" => Kind::House,
            "other" => Kind::Other,
            _ => return None,
        })
    }

    /// A stored kind (older ones: `fact`, `event`, `commitment` → `Other`).
    pub fn of(s: &str) -> Kind {
        Kind::parse(s).unwrap_or(Kind::Other)
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Person => "person",
            Kind::Preference => "preference",
            Kind::Routine => "routine",
            Kind::House => "house",
            Kind::Other => "other",
        }
    }
}

/// A fact the model proposes.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposed<'a> {
    pub text: &'a str,
    pub kind: &'a str,
    /// The user's words it rests on; None = the user's sentence itself is the evidence (the
    /// `remember` tool, called while the user speaks).
    pub quote: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Told to the agent from the next conversation.
    Active,
    /// Kept, shown on the screen under "To confirm", not told to the agent.
    Candidate,
    /// Not kept: why (logged as a count, never with the text).
    Reject(&'static str),
}

/// Words of a request to remember something (lower case; a part of the sentence is enough,
/// as for the other writing tools). "ricordami" is a reminder, not here.
pub const ASK_REMEMBER: &[&str] = &[
    "ricordati",
    "ricorda che",
    "ricordati che",
    "tieni a mente",
    "memorizza",
    "remember that",
    "remember ",
    "keep in mind",
    "onthoud",
    "vergeet niet dat",
];

/// … to forget something or to say it is wrong.
pub const ASK_FORGET: &[&str] = &[
    "dimentic",
    "scorda",
    "non è vero",
    "sbagliat",
    "non mi chiamo",
    "forget",
    "wrong",
    "not true",
    "vergeet",
    "klopt niet",
];

/// The tools that act (the house, timers, lists, reminders, the screen): a conversation made
/// only of them has nothing to remember.
const ACTION_TOOLS: &[&str] = &[
    "home_control",
    "home_entities",
    "timer",
    "timer_cancel",
    "alarm",
    "schedule",
    "list_add",
    "list_read",
    "list_done",
    "list_remove",
    "cancel_job",
    "list_jobs",
    "do_not_disturb",
    "show_view",
    "announce",
    "read_briefing",
];

/// Words that carry no meaning for comparing facts (with `memory::STOP`).
const COMMON: &[&str] = &[
    "mia", "mio", "miei", "mie", "suo", "sua", "suoi", "sue", "nostra", "nostro", "questa",
    "questo", "anche", "sempre", "utente", "user", "his", "her", "our", "their", "this", "that",
    "has", "have", "with", "always", "mijn", "zijn", "haar", "onze", "het", "een", "van", "die",
    "dat", "heeft", "altijd",
];

/// Lower case, letters and digits kept (accents too), every other run → one space.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim_end().to_string()
}

/// A year in the text (a token `19dd` or `20dd`): a record of a day, not a standing fact.
/// "compie gli anni il 12 marzo" has none; a count like "2026 libri" is a false positive,
/// accepted (the safe side).
pub fn has_year(text: &str) -> bool {
    normalize(text).split(' ').any(|w| {
        w.len() == 4
            && w.chars().all(|c| c.is_ascii_digit())
            && (w.starts_with("19") || w.starts_with("20"))
    })
}

/// The quote is the user's: at least two words, found whole (case and punctuation aside) in
/// one of the user's sentences.
pub fn quote_in(quote: &str, user_text: &[String]) -> bool {
    let q = normalize(quote);
    if q.split(' ').filter(|w| !w.is_empty()).count() < 2 {
        return false;
    }
    let q = format!(" {q} ");
    user_text
        .iter()
        .any(|u| format!(" {} ", normalize(u)).contains(&q))
}

/// A chronicle of what was done (the shopping list's content, a blind moved, a timer set):
/// not a fact. Deliberately narrow; the instructions and the quote rule do the general work.
pub fn chronicle_words(text: &str) -> bool {
    let t = format!(" {} ", normalize(text));
    // What is on the shopping list today is never a lasting fact.
    if [
        " lista della spesa ",
        " shopping list ",
        " boodschappenlijst ",
    ]
    .iter()
    .any(|w| t.contains(w))
    {
        return true;
    }
    let things = [
        "tapparell",
        "blind",
        "rolluik",
        "timer",
        "promemoria",
        "reminder",
        "herinnering",
    ];
    let past = [
        " ha aggiunto ",
        " ha chiesto ",
        " ha confermato ",
        " ha spostato ",
        " added ",
        " asked ",
        " confirmed ",
        " moved ",
        "toegevoegd",
        "gevraagd",
        "bevestigd",
    ];
    things.iter().any(|w| t.contains(w)) && past.iter().any(|w| t.contains(w))
}

/// The words of a fact that tell it apart (three letters or more, no common words).
fn tokens(s: &str) -> std::collections::BTreeSet<String> {
    normalize(s)
        .split(' ')
        .filter(|w| w.chars().count() >= 3)
        .filter(|w| !crate::memory::STOP.contains(w) && !COMMON.contains(w))
        .map(str::to_string)
        .collect()
}

/// Two facts say the same thing: most words in common (Jaccard ≥ 0.7), or all the words of the
/// shorter one (three at least) in the other.
pub fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (tokens(a), tokens(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let common = a.intersection(&b).count();
    let union = a.union(&b).count();
    let (small, large) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    common as f64 / union as f64 >= 0.7 || (small.len() >= 3 && small.is_subset(large))
}

/// How many meaningful words of `query` are in `text`, by their stems (the first four letters,
/// as speech and inflection change the ends: "chiamo" finds "chiama").
pub fn overlap(query: &str, text: &str) -> usize {
    let stem = |w: &str| w.chars().take(4).collect::<String>();
    let t: Vec<String> = tokens(text).iter().map(|w| stem(w)).collect();
    tokens(query)
        .iter()
        .filter(|w| t.contains(&stem(w)))
        .count()
}

/// Whether `text` names one of `names` (whole words, case aside; a name's first word counts).
fn names_one(text: &str, names: &[String]) -> bool {
    let t = format!(" {} ", normalize(text));
    names.iter().any(|n| {
        let n = normalize(n);
        let first = n.split(' ').next().unwrap_or("");
        [n.as_str(), first]
            .iter()
            .any(|w| w.chars().count() >= 2 && t.contains(&format!(" {w} ")))
    })
}

/// The names in facts about people already known (their capitalized words, common words aside).
pub fn known_names<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut v: Vec<String> = texts
        .into_iter()
        .flat_map(|t| t.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.chars().count() >= 3 && w.chars().next().is_some_and(char::is_uppercase))
        .map(str::to_lowercase)
        .filter(|w| !crate::memory::STOP.contains(&w.as_str()) && !COMMON.contains(&w.as_str()))
        .filter(|w| !["the", "il", "lo", "la", "le", "gli", "de", "het"].contains(&w.as_str()))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// What happens to one proposed fact. `user_text`: the user's sentences of the conversation;
/// `residents`: the people registered in Home Assistant; `known_names`: names in the facts
/// about people already known ([`known_names`]).
pub fn gate(
    op: &Proposed,
    user_text: &[String],
    residents: &[String],
    known: &[String],
) -> Verdict {
    let Some(kind) = Kind::parse(op.kind) else {
        return Verdict::Reject("kind");
    };
    let text = op.text.trim();
    if text.is_empty() || text.chars().count() > 200 || normalize(text).split(' ').count() < 3 {
        return Verdict::Reject("shape");
    }
    if has_year(text) {
        return Verdict::Reject("dated");
    }
    if let Some(q) = op.quote
        && !quote_in(q, user_text)
    {
        return Verdict::Reject("no evidence");
    }
    if chronicle_words(text) {
        return Verdict::Reject("chronicle");
    }
    if kind == Kind::Person && !names_one(text, residents) && !names_one(text, known) {
        return Verdict::Candidate;
    }
    Verdict::Active
}

/// At most `max` new facts from one conversation: the most important ones (ties: the first
/// proposed). `items`: (importance, verdict) in the order proposed; the others become
/// `Reject("too many")`.
pub fn cap(items: &mut [(i64, Verdict)], max: usize) {
    let mut order: Vec<usize> = (0..items.len())
        .filter(|&i| !matches!(items[i].1, Verdict::Reject(_)))
        .collect();
    order.sort_by_key(|&i| (-items[i].0, i));
    for &i in order.iter().skip(max) {
        items[i].1 = Verdict::Reject("too many");
    }
}

/// Whether `text` asks to remember (one of [`ASK_REMEMBER`]).
pub fn asks_to_remember(text: &str) -> bool {
    let t = text.to_lowercase();
    ASK_REMEMBER.iter().any(|w| t.contains(w))
}

/// A conversation worth the model's time at its end: one exchange asks to remember something,
/// or used no tool that acts and has a sentence of four words or more (a chat, where something
/// lasting may have been said). Commands only ("close the blind"): no.
pub fn worth_extracting(ex: &[Exchange]) -> bool {
    ex.iter().any(|e| {
        if asks_to_remember(&e.user) {
            return true;
        }
        let acted = e
            .tools
            .split(',')
            .map(str::trim)
            .any(|t| ACTION_TOOLS.contains(&t));
        !acted
            && normalize(&e.user)
                .split(' ')
                .filter(|w| !w.is_empty())
                .count()
                >= 4
    })
}

/// The summary of a conversation not worth the model: the user's sentences, at most 200 chars.
pub fn plain_summary(ex: &[Exchange]) -> String {
    let s = ex
        .iter()
        .map(|e| e.user.trim())
        .filter(|u| !u.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(user: &str, tools: &str) -> Exchange {
        Exchange {
            user: user.into(),
            answer: "ok".into(),
            tools: tools.into(),
        }
    }

    fn user(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn g1_years() {
        assert!(has_year("il 13 novembre 2026 ha chiuso"));
        assert!(has_year("Il 2026-11-08 l'utente ha segnalato"));
        assert!(!has_year("compie gli anni il 12 marzo"));
        assert!(!has_year("abita al civico 12345"));
        // Documented false positive: a count that looks like a year.
        assert!(has_year("ha 2026 libri"));
    }

    #[test]
    fn g2_quotes() {
        let u = user(&["Mi chiamo Mario, piacere!", "chiudi la tapparella"]);
        assert!(quote_in("mi chiamo Mario", &u));
        assert!(quote_in("MI CHIAMO, mario!", &u));
        // The assistant's words are not the user's.
        assert!(!quote_in("ho risolto l'errore", &u));
        // One word is not evidence.
        assert!(!quote_in("Mario", &u));
        // Whole words only.
        assert!(!quote_in("hiamo Mar", &u));
    }

    #[test]
    fn g3_gate_on_the_device_snapshot() {
        // The eight facts of the device (14/11), proposed again as `add`; the name replaced.
        let u = user(&[
            "accendi la lampada della cucina",
            "aggiungi i rotoloni alla spesa",
            "chiudi la tapparella del soggiorno",
            "mi chiamo Mario",
            "ricordami di controllare il forno",
        ]);
        let g = |text: &str, kind: &str, quote: &str| {
            gate(
                &Proposed {
                    text,
                    kind,
                    quote: Some(quote),
                },
                &u,
                &[],
                &[],
            )
        };
        assert_eq!(
            g(
                "Il 2026-11-08 l'utente ha segnalato un problema di permessi con la lampada della cucina.",
                "house",
                "la lampada della cucina"
            ),
            Verdict::Reject("dated")
        );
        // Built on the assistant's answer: no quote of the user.
        assert_eq!(
            g(
                "È stato risolto l'errore di sincronizzazione delle luci della cucina.",
                "house",
                "risolto l'errore di sincronizzazione"
            ),
            Verdict::Reject("no evidence")
        );
        assert_eq!(
            g(
                "Il 9 novembre 2026 l'utente ha confermato la chiusura della tapparella sinistra.",
                "house",
                "chiudi la tapparella del soggiorno"
            ),
            Verdict::Reject("dated")
        );
        assert_eq!(
            g(
                "La lista della spesa dell'utente ora include latte, pane e rotoloni.",
                "house",
                "aggiungi i rotoloni alla spesa"
            ),
            Verdict::Reject("chronicle")
        );
        assert_eq!(
            g(
                "L'utente ha aggiunto rotoloni alla lista della spesa il 13 novembre 2026.",
                "preference",
                "aggiungi i rotoloni alla spesa"
            ),
            Verdict::Reject("dated")
        );
        assert_eq!(
            g(
                "L'utente vuole essere ricordato di controllare il forno.",
                "commitment",
                "controllare il forno"
            ),
            Verdict::Reject("kind")
        );
        assert_eq!(
            g(
                "L'utente ha spostato la tapparella al 10%.",
                "house",
                "chiudi la tapparella del soggiorno"
            ),
            Verdict::Reject("chronicle")
        );
        // A name heard once: a candidate; a resident's: active.
        let p = Proposed {
            text: "L'utente si chiama Mario R.",
            kind: "person",
            quote: Some("mi chiamo mario"),
        };
        assert_eq!(gate(&p, &u, &[], &[]), Verdict::Candidate);
        assert_eq!(gate(&p, &u, &user(&["Mario"]), &[]), Verdict::Active);
        assert_eq!(gate(&p, &u, &user(&["Mario Rossi"]), &[]), Verdict::Active);
        assert_eq!(gate(&p, &u, &[], &user(&["mario"])), Verdict::Active);
        let u2 = user(&["sai, Marco non beve caffè dopo le cinque"]);
        assert_eq!(
            gate(
                &Proposed {
                    text: "Marco non beve caffè dopo le cinque",
                    kind: "preference",
                    quote: Some("Marco non beve caffè dopo le cinque"),
                },
                &u2,
                &[],
                &[],
            ),
            Verdict::Active
        );
        assert_eq!(
            g("Due parole", "house", "chiudi la tapparella"),
            Verdict::Reject("shape")
        );
    }

    #[test]
    fn g4_two_new_facts_per_conversation() {
        let mut v = vec![
            (3, Verdict::Active),
            (9, Verdict::Candidate),
            (5, Verdict::Reject("dated")),
            (7, Verdict::Active),
            (6, Verdict::Active),
        ];
        cap(&mut v, 2);
        assert_eq!(
            v.iter().map(|x| x.1).collect::<Vec<_>>(),
            vec![
                Verdict::Reject("too many"),
                Verdict::Candidate,
                Verdict::Reject("dated"),
                Verdict::Active,
                Verdict::Reject("too many"),
            ]
        );
    }

    #[test]
    fn d1_similar_facts() {
        assert!(similar(
            "Il gatto di casa si chiama Neve",
            "Il gatto si chiama Neve"
        ));
        assert!(similar(
            "Marco beve il caffè senza zucchero",
            "Marco beve sempre il caffè senza zucchero la mattina"
        ));
        assert!(!similar(
            "Il gatto si chiama Neve",
            "La caldaia è in cantina"
        ));
        assert!(similar(
            "The cat is called Snow",
            "The family cat is called Snow"
        ));
        assert!(similar("De kat heet Sneeuw", "Onze kat heet Sneeuw"));
        assert!(!similar("", "Il gatto si chiama Neve"));
    }

    #[test]
    fn w1_which_conversations_reach_the_model() {
        assert!(!worth_extracting(&[ex(
            "chiudi la tapparella del soggiorno",
            "home_control"
        )]));
        assert!(worth_extracting(&[
            ex("chiudi la tapparella", "home_control"),
            ex("ricordati che il gatto si chiama Neve", "home_control"),
        ]));
        assert!(worth_extracting(&[ex(
            "sai che domenica viene mia sorella",
            ""
        )]));
        assert!(!worth_extracting(&[ex("grazie", "")]));
        assert!(!worth_extracting(&[ex(
            "metti un timer di dieci minuti",
            "timer,home_entities"
        )]));
        assert_eq!(
            plain_summary(&[ex("chiudi", "x"), ex("apri", "y")]),
            "chiudi · apri"
        );
    }

    #[test]
    fn overlap_by_stems() {
        assert_eq!(overlap("mi chiamo Mario", "L'utente si chiama Mario R."), 2);
        assert_eq!(overlap("mi chiamo Mario", "Il gatto si chiama Neve"), 1);
    }

    #[test]
    fn kinds_and_names() {
        assert_eq!(Kind::parse("event"), None);
        assert_eq!(Kind::of("commitment"), Kind::Other);
        assert_eq!(Kind::of("Person"), Kind::Person);
        assert_eq!(
            known_names(["Mia moglie si chiama Giulia", "Il cane di Luca"]),
            vec!["giulia".to_string(), "luca".to_string()]
        );
    }
}
