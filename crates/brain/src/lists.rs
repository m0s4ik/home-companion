// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The shopping list by voice (`[lists] shopping`, any `todo.*` entity of Home Assistant): the
//! `list_add`, `list_read`, `list_done` and `list_remove` tools, over Home Assistant's WebSocket
//! (`todo/item/list`, `todo.add_item`, `todo.update_item`, `todo.remove_item`). Generic over
//! `todo` entities (Home Assistant's own shopping list, Bring!, …). Matching is forgiving: case,
//! accents, punctuation and a final vowel (pane/pani, mela/mele) do not count.

use std::time::Duration;

use serde_json::{Value, json};

use crate::ha::Ha;

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub uid: String,
    pub summary: String,
    pub done: bool,
}

/// Lower case, no accents, letters and digits only, single spaces.
pub fn normalize(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        let c = match c {
            'à' | 'á' | 'â' | 'ä' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        };
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Each word without its final vowel when it is longer than three letters.
fn stem(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            if w.chars().count() > 3 && w.ends_with(['a', 'e', 'i', 'o']) {
                &w[..w.len() - 1]
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether what was said names this item: the same words (forgiving), or one is a whole-word
/// part of the other ("pane" ↔ "pane integrale").
pub fn matches(said: &str, item: &str) -> bool {
    let (a, b) = (stem(&normalize(said)), stem(&normalize(item)));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let words = |s: &str| format!(" {s} ");
    a == b || words(&b).contains(&words(&a)) || words(&a).contains(&words(&b))
}

/// The best open item for what was said: an exact (forgiving) match first, else the only
/// partial one; None when nothing or more than one partial match.
pub fn find<'a>(items: &'a [Item], said: &str, done: bool) -> Option<&'a Item> {
    let pool: Vec<&Item> = items.iter().filter(|i| i.done == done).collect();
    let n = stem(&normalize(said));
    if let Some(i) = pool.iter().find(|i| stem(&normalize(&i.summary)) == n) {
        return Some(i);
    }
    let partial: Vec<&&Item> = pool.iter().filter(|i| matches(said, &i.summary)).collect();
    (partial.len() == 1).then(|| *partial[0])
}

pub fn parse_items(result: &Value) -> Vec<Item> {
    result
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|i| {
            Some(Item {
                uid: i.get("uid")?.as_str()?.to_string(),
                summary: i.get("summary")?.as_str()?.trim().to_string(),
                done: i.get("status").and_then(Value::as_str) == Some("completed"),
            })
        })
        .collect()
}

/// Which `todo.*` list a tool call is about. `known` = every `todo.*` entity of Home Assistant
/// as (entity id, friendly name); `chosen` = the ones chosen on the body's screen (empty = all);
/// `shopping` = `[lists] shopping`; `wanted` = the model's `list` argument (name or entity id,
/// may be empty). Without `wanted`: the shopping list, else a list named like shopping/spesa,
/// else the first chosen one. Returns (entity id, name).
pub fn pick(
    known: &[(String, String)],
    chosen: &[String],
    shopping: &str,
    wanted: &str,
) -> Result<(String, String), String> {
    let name_of = |id: &str| {
        known
            .iter()
            .find(|(k, _)| k == id)
            .map_or_else(|| id.to_string(), |(_, n)| n.clone())
    };
    let mut pool: Vec<(String, String)> = if chosen.is_empty() {
        known.to_vec()
    } else {
        chosen.iter().map(|id| (id.clone(), name_of(id))).collect()
    };
    if !shopping.is_empty() && !pool.iter().any(|(id, _)| id == shopping) {
        pool.push((shopping.to_string(), name_of(shopping)));
    }
    let wanted = wanted.trim();
    if wanted.is_empty() {
        if !shopping.is_empty() {
            return Ok((shopping.to_string(), name_of(shopping)));
        }
        let shop = pool.iter().find(|(id, n)| {
            let t = normalize(&format!("{id} {n}"));
            t.contains("shopping") || t.contains("spesa") || t.contains("grocer")
        });
        return shop
            .or(pool.first())
            .cloned()
            .ok_or_else(|| "no list is available: Home Assistant has no todo list".to_string());
    }
    let w = normalize(wanted);
    let names = || pool.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ");
    if let Some(p) = pool.iter().find(|(id, n)| {
        id.eq_ignore_ascii_case(wanted) || *id == format!("todo.{wanted}") || normalize(n) == w
    }) {
        return Ok(p.clone());
    }
    let partial: Vec<&(String, String)> = pool.iter().filter(|(_, n)| matches(wanted, n)).collect();
    match partial.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("no list called '{wanted}'; the lists are: {}", names())),
        _ => Err(format!("'{wanted}' fits several lists, which one? {}", names())),
    }
}

/// The items of the list as `Value` strings for the model (sanitized by the caller).
pub struct Lists<'a> {
    pub ha: &'a Ha,
    pub entity: &'a str,
    pub timeout: Duration,
}

impl Lists<'_> {
    pub fn items(&self) -> Result<Vec<Item>, String> {
        let r = self.ha.call(
            json!({"type": "todo/item/list", "entity_id": self.entity}),
            self.timeout,
        )?;
        Ok(parse_items(&r))
    }

    fn service(&self, service: &str, data: Value) -> Result<(), String> {
        self.ha
            .call(
                json!({"type": "call_service", "domain": "todo", "service": service,
                    "service_data": data, "target": {"entity_id": self.entity}}),
                self.timeout,
            )
            .map(|_| ())
    }

    /// Adds what is not on the list yet (open); a ticked one comes back as open.
    pub fn add(&self, said: &[String]) -> Result<Value, String> {
        let items = self.items()?;
        let (mut added, mut already, mut reopened) = (Vec::new(), Vec::new(), Vec::new());
        for s in said.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Some(i) = find(&items, s, false)
                .filter(|i| stem(&normalize(&i.summary)) == stem(&normalize(s)))
            {
                already.push(i.summary.clone());
            } else if let Some(i) = find(&items, s, true)
                .filter(|i| stem(&normalize(&i.summary)) == stem(&normalize(s)))
            {
                self.service(
                    "update_item",
                    json!({"item": i.uid, "status": "needs_action"}),
                )?;
                reopened.push(i.summary.clone());
            } else if added
                .iter()
                .any(|a: &String| stem(&normalize(a)) == stem(&normalize(s)))
            {
                continue;
            } else {
                self.service("add_item", json!({"item": s}))?;
                added.push(s.to_string());
            }
        }
        Ok(json!({"added": added, "already_on_the_list": already, "put_back": reopened}))
    }

    /// The open items: at most `max`, then how many more.
    pub fn read(&self, max: usize) -> Result<Value, String> {
        let open: Vec<String> = self
            .items()?
            .into_iter()
            .filter(|i| !i.done)
            .map(|i| i.summary)
            .collect();
        let more = open.len().saturating_sub(max);
        Ok(
            json!({"items": open.iter().take(max).collect::<Vec<_>>(), "and_more": more,
            "note": "say at most this many; if and_more > 0 add 'e altre N cose'"}),
        )
    }

    /// Ticks (bought) or removes what was said; what did not match is listed.
    pub fn done_or_remove(&self, said: &[String], remove: bool) -> Result<Value, String> {
        let items = self.items()?;
        let (mut ok, mut unknown) = (Vec::new(), Vec::new());
        let mut uids = Vec::new();
        for s in said {
            let hit =
                find(&items, s, false).or_else(|| remove.then(|| find(&items, s, true)).flatten());
            match hit {
                Some(i) if !uids.contains(&i.uid) => {
                    uids.push(i.uid.clone());
                    ok.push(i.summary.clone());
                }
                Some(_) => {}
                None => unknown.push(s.clone()),
            }
        }
        if remove {
            if !uids.is_empty() {
                self.service("remove_item", json!({"item": uids}))?;
            }
        } else {
            for u in &uids {
                self.service("update_item", json!({"item": u, "status": "completed"}))?;
            }
        }
        let key = if remove { "removed" } else { "ticked" };
        Ok(json!({key: ok, "not_on_the_list": unknown}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(uid: &str, s: &str, done: bool) -> Item {
        Item {
            uid: uid.into(),
            summary: s.into(),
            done,
        }
    }

    #[test]
    fn forgiving_matches() {
        assert_eq!(normalize("  Caffè, LATTE! "), "caffe latte");
        assert!(matches("pane", "Pane"));
        assert!(matches("il pane", "pane") || matches("pane", "pane integrale"));
        assert!(matches("mele", "Mela"));
        assert!(matches("pane", "Pane integrale"));
        assert!(!matches("pane", "panettone"));
        assert!(!matches("", "pane"));
    }

    #[test]
    fn find_prefers_exact_and_refuses_ambiguity() {
        let items = vec![
            item("1", "Latte", false),
            item("2", "Latte di soia", false),
            item("3", "Pane", true),
            item("4", "2 kg farina", false),
        ];
        assert_eq!(find(&items, "latte", false).unwrap().uid, "1");
        assert_eq!(find(&items, "soia", false).unwrap().uid, "2");
        assert_eq!(find(&items, "farina", false).unwrap().uid, "4");
        assert!(find(&items, "pane", false).is_none());
        assert_eq!(find(&items, "pane", true).unwrap().uid, "3");
        let two = vec![
            item("1", "Mele rosse", false),
            item("2", "Mele verdi", false),
        ];
        assert!(find(&two, "mele", false).is_none(), "ambiguous");
    }

    #[test]
    fn items_from_home_assistant() {
        let r = json!({"items": [{"uid": "a", "summary": "Pane ", "status": "needs_action"},
            {"uid": "b", "summary": "Latte", "status": "completed"}, {"summary": "no uid"}]});
        assert_eq!(
            parse_items(&r),
            vec![item("a", "Pane", false), item("b", "Latte", true)]
        );
    }

    #[test]
    fn pick_a_list_by_name_or_default() {
        let k = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
        };
        let known = k(&[
            ("todo.casa", "Home to-do"),
            ("todo.bring", "Spesa"),
            ("todo.garden", "Garden jobs"),
        ]);
        let chosen = vec!["todo.casa".to_string(), "todo.bring".to_string()];
        let id = |r: Result<(String, String), String>| r.map(|p| p.0);
        // Default: the configured list, else the one named like shopping, else the first chosen.
        assert_eq!(id(pick(&known, &chosen, "todo.casa", "")).unwrap(), "todo.casa");
        assert_eq!(id(pick(&known, &chosen, "", "")).unwrap(), "todo.bring");
        assert_eq!(id(pick(&known, &["todo.casa".into()], "", "")).unwrap(), "todo.casa");
        // By friendly name (forgiving), by entity id, by part of the name.
        assert_eq!(id(pick(&known, &chosen, "", "home to do")).unwrap(), "todo.casa");
        assert_eq!(id(pick(&known, &chosen, "", "todo.bring")).unwrap(), "todo.bring");
        assert_eq!(id(pick(&known, &chosen, "todo.bring", "home")).unwrap(), "todo.casa");
        // Not among the chosen ones: refused with the names; empty chosen = all of them.
        let e = pick(&known, &chosen, "", "garden jobs").unwrap_err();
        assert!(e.contains("Spesa"), "{e}");
        assert_eq!(id(pick(&known, &[], "", "garden jobs")).unwrap(), "todo.garden");
        assert!(pick(&[], &[], "", "").is_err());
    }
}
