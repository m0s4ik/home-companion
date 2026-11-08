// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The shopping lists as the screen shows them (one section per list when there are several),
//! and the page where the user chooses which `todo.*` lists and `calendar.*` calendars to show
//! (empty choice = all of them). Pure: no I/O.

use crate::ha::hub::{Entities, Named};
use crate::ha::todo::Item;

/// A list with its open items.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShopList {
    pub entity: String,
    pub name: String,
    pub items: Vec<Item>,
}

/// The lists after Home Assistant named the active ones: items of the lists that stay are kept.
pub fn with_active(old: &[ShopList], active: &[Named]) -> Vec<ShopList> {
    active
        .iter()
        .map(|(entity, name)| ShopList {
            entity: entity.clone(),
            name: name.clone(),
            items: old
                .iter()
                .find(|l| l.entity == *entity)
                .map(|l| l.items.clone())
                .unwrap_or_default(),
        })
        .collect()
}

/// The page's rows: (uid, text). A section header (several lists) has an empty uid; an item's
/// uid is "<entity>|<uid>" (see [`split_uid`]).
pub fn rows(lists: &[ShopList]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for l in lists {
        if lists.len() > 1 {
            out.push((String::new(), l.name.clone()));
        }
        out.extend(
            l.items
                .iter()
                .map(|(u, t)| (format!("{}|{u}", l.entity), t.clone())),
        );
    }
    out
}

/// A row's uid → (list entity, item uid).
pub fn split_uid(uid: &str) -> Option<(&str, &str)> {
    uid.split_once('|')
        .filter(|(e, u)| !e.is_empty() && !u.is_empty())
}

/// Items over all lists.
pub fn count(lists: &[ShopList]) -> usize {
    lists.iter().map(|l| l.items.len()).sum()
}

/// The first `n` items' texts over all lists (for the home card).
pub fn first_texts(lists: &[ShopList], n: usize) -> Vec<&str> {
    lists
        .iter()
        .flat_map(|l| l.items.iter())
        .take(n)
        .map(|(_, t)| t.as_str())
        .collect()
}

/// The ticked item gone from the lists at once.
pub fn remove(lists: &mut [ShopList], entity: &str, uid: &str) {
    for l in lists.iter_mut().filter(|l| l.entity == entity) {
        l.items.retain(|(u, _)| u != uid);
    }
}

/// What the choosing page shows and keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Choice {
    pub known: Entities,
    pub lists: Vec<String>,
    pub calendars: Vec<String>,
}

/// One row of the page: (entity id, name, on).
pub type ChoiceRow = (String, String, bool);

/// `chosen` with `id` flipped among `all`: empty means all, and the last one cannot be
/// switched off (nothing at all is not a choice).
pub fn toggled(chosen: &[String], all: &[Named], id: &str) -> Vec<String> {
    let mut on: Vec<String> = if chosen.is_empty() {
        all.iter().map(|(i, _)| i.clone()).collect()
    } else {
        chosen.to_vec()
    };
    if let Some(p) = on.iter().position(|i| i == id) {
        if on.len() > 1 {
            on.remove(p);
        }
    } else if all.iter().any(|(i, _)| i == id) {
        on.push(id.to_string());
    }
    if all.iter().all(|(i, _)| on.contains(i)) {
        return Vec::new();
    }
    on
}

fn rows_of(all: &[Named], chosen: &[String]) -> Vec<ChoiceRow> {
    all.iter()
        .map(|(i, n)| {
            (
                i.clone(),
                n.clone(),
                chosen.is_empty() || chosen.contains(i),
            )
        })
        .collect()
}

impl Choice {
    /// Todo lists first, then calendars, each with its switch.
    pub fn rows(&self) -> (Vec<ChoiceRow>, Vec<ChoiceRow>) {
        (
            rows_of(&self.known.todo, &self.lists),
            rows_of(&self.known.calendar, &self.calendars),
        )
    }

    /// A tap on a row (the entity's domain says which choice it is); true if it was one of ours.
    pub fn toggle(&mut self, id: &str) -> bool {
        if id.starts_with("todo.") {
            self.lists = toggled(&self.lists, &self.known.todo, id);
            true
        } else if id.starts_with("calendar.") {
            self.calendars = toggled(&self.calendars, &self.known.calendar, id);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(v: &[(&str, &str)]) -> Vec<Named> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn list(e: &str, n: &str, items: &[(&str, &str)]) -> ShopList {
        ShopList {
            entity: e.into(),
            name: n.into(),
            items: items
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }

    #[test]
    fn one_list_has_no_header_several_have() {
        let a = list("todo.a", "Groceries", &[("1", "Bread")]);
        let b = list("todo.b", "Hardware", &[("2", "Screws"), ("3", "Glue")]);
        assert_eq!(
            rows(std::slice::from_ref(&a)),
            [("todo.a|1".to_string(), "Bread".to_string())]
        );
        let r = rows(&[a.clone(), b.clone()]);
        assert_eq!(r.len(), 5);
        assert_eq!(r[0], (String::new(), "Groceries".to_string()));
        assert_eq!(r[2], (String::new(), "Hardware".to_string()));
        assert_eq!(split_uid(&r[3].0), Some(("todo.b", "2")));
        assert_eq!(split_uid(""), None);
        assert_eq!(count(&[a.clone(), b.clone()]), 3);
        assert_eq!(first_texts(&[a, b], 2), ["Bread", "Screws"]);
    }

    #[test]
    fn active_lists_keep_their_items_and_removal_is_local() {
        let old = vec![
            list("todo.a", "A", &[("1", "Bread")]),
            list("todo.b", "B", &[("2", "Milk")]),
        ];
        let mut now = with_active(&old, &named(&[("todo.b", "Bee"), ("todo.c", "C")]));
        assert_eq!(now[0].items, [("2".to_string(), "Milk".to_string())]);
        assert_eq!(now[0].name, "Bee");
        assert!(now[1].items.is_empty());
        remove(&mut now, "todo.b", "2");
        assert_eq!(count(&now), 0);
    }

    #[test]
    fn toggling_starts_from_all_and_ends_at_all() {
        let all = named(&[("todo.a", "A"), ("todo.b", "B"), ("todo.c", "C")]);
        // Everything is on; switching b off keeps a and c.
        let c = toggled(&[], &all, "todo.b");
        assert_eq!(c, ["todo.a", "todo.c"]);
        // Switching b back on is "all" again: empty.
        assert!(toggled(&c, &all, "todo.b").is_empty());
        // The last one stays on.
        let one = vec!["todo.a".to_string()];
        assert_eq!(toggled(&one, &all, "todo.a"), one);
        // Unknown ids change nothing.
        assert_eq!(toggled(&one, &all, "todo.zzz"), one);
    }

    #[test]
    fn the_page_model_tells_what_is_on() {
        let mut c = Choice {
            known: Entities {
                todo: named(&[("todo.a", "A"), ("todo.b", "B")]),
                calendar: named(&[("calendar.x", "X"), ("calendar.y", "Y")]),
            },
            ..Default::default()
        };
        let (t, k) = c.rows();
        assert!(t.iter().all(|r| r.2) && k.iter().all(|r| r.2));
        assert!(c.toggle("calendar.x"));
        assert!(!c.toggle("light.kitchen"));
        let (t, k) = c.rows();
        assert!(t.iter().all(|r| r.2));
        assert_eq!(k.iter().map(|r| r.2).collect::<Vec<_>>(), [false, true]);
        assert_eq!(c.calendars, ["calendar.y"]);
    }
}
