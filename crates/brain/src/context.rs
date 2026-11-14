// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the agent always knows: the active facts as a bullet list within a budget. The body puts
//! it in Home Assistant as a state (`sensor.<name>_memory`, attribute `text`), which the agent's
//! instructions read as a template at every conversation. Pure.

use crate::collect::Kind;

/// An active fact as the context sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextFact<'a> {
    pub id: i64,
    pub text: &'a str,
    pub kind: Kind,
    pub importance: i64,
    /// Said by the user ("remember that…") or confirmed on the screen.
    pub explicit: bool,
    pub updated: i64,
}

/// The order of the context (and of the "Ricordi" page): people, preferences, routines, the
/// house, the rest; the user's own first, then the most important, the newest.
pub fn order(facts: &mut [ContextFact]) {
    facts.sort_by_key(key);
}

/// The sort key of [`order`].
pub fn key(f: &ContextFact) -> (Kind, bool, i64, i64, i64) {
    (f.kind, !f.explicit, -f.importance, -f.updated, f.id)
}

/// One line's text: on one line, no template markers, at most 200 characters.
fn clean(text: &str) -> String {
    let mut t = text.replace(['\n', '\r'], " ");
    for m in ["{{", "}}", "{%", "%}", "{#", "#}"] {
        t = t.replace(m, "");
    }
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    t.chars().take(200).collect()
}

/// The bullet list for the agent's instructions (in [`order`]) and how many facts fit in
/// `max_chars`; a line is never cut, the first that does not fit ends the list.
pub fn render(facts: &[ContextFact], max_chars: usize) -> (String, usize) {
    let mut v = facts.to_vec();
    order(&mut v);
    let mut out = String::new();
    let mut len = 0;
    let mut shown = 0;
    for f in &v {
        let line = format!("- {}", clean(f.text));
        let n = line.chars().count() + usize::from(shown > 0);
        if len + n > max_chars {
            break;
        }
        if shown > 0 {
            out.push('\n');
        }
        out.push_str(&line);
        len += n;
        shown += 1;
    }
    (out, shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: i64, text: &str, kind: Kind, importance: i64, explicit: bool) -> ContextFact<'_> {
        ContextFact {
            id,
            text,
            kind,
            importance,
            explicit,
            updated: 100 + id,
        }
    }

    #[test]
    fn c1_order_budget_and_cleaning() {
        let facts = [
            f(1, "La caldaia è in cantina", Kind::House, 9, false),
            f(
                2,
                "Marco non beve caffè dopo le cinque",
                Kind::Preference,
                5,
                false,
            ),
            f(3, "Il gatto si chiama {{ Neve }}", Kind::Person, 5, false),
            f(4, "Marco è il marito di Giulia", Kind::Person, 3, true),
            f(5, "Vecchio fatto", Kind::Other, 10, true),
        ];
        let (t, n) = render(&facts, 1500);
        assert_eq!(n, 5);
        assert_eq!(
            t,
            "- Marco è il marito di Giulia\n- Il gatto si chiama Neve\n\
             - Marco non beve caffè dopo le cinque\n- La caldaia è in cantina\n- Vecchio fatto"
        );
        // The budget never cuts a line.
        let (t, n) = render(&facts, 60);
        assert_eq!(n, 2);
        assert!(t.chars().count() <= 60);
        assert!(t.ends_with("Neve"));
        assert_eq!(render(&[], 1500), (String::new(), 0));
        let long = "x".repeat(300);
        let (t, _) = render(&[f(1, &long, Kind::Other, 5, false)], 1500);
        assert_eq!(t.chars().count(), 202);
    }
}
