// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The shopping lists on the screen (any `todo.*` entity of Home Assistant: its own shopping
//! list, Bring!, …). The connection that subscribes to the items (`todo/item/subscribe`, so a
//! change from the phone shows at once) and ticks an item (`todo.update_item`, status
//! `completed`) is [`super::hub`]'s; this file reads what comes back.

use serde_json::Value;

/// An open item: (uid, what to buy).
pub type Item = (String, String);

/// The open items of a `todo/item/subscribe` event (in Home Assistant's order).
pub fn open_items(event: &Value) -> Option<Vec<Item>> {
    let items = event.pointer("/event/items")?.as_array()?;
    Some(
        items
            .iter()
            .filter(|i| i.get("status").and_then(Value::as_str) != Some("completed"))
            .filter_map(|i| {
                Some((
                    i.get("uid")?.as_str()?.to_string(),
                    i.get("summary")?.as_str()?.trim().to_string(),
                ))
            })
            .filter(|(_, s)| !s.is_empty())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn open_items_skip_completed_and_empty() {
        let ev = json!({"id": 1, "type": "event", "event": {"items": [
            {"uid": "a", "summary": "Pane", "status": "needs_action"},
            {"uid": "b", "summary": "Latte", "status": "completed"},
            {"uid": "c", "summary": "  ", "status": "needs_action"},
            {"uid": "d", "summary": "2 kg farina", "status": "needs_action"}]}});
        assert_eq!(
            open_items(&ev).unwrap(),
            vec![
                ("a".into(), "Pane".into()),
                ("d".into(), "2 kg farina".into())
            ]
        );
        assert!(open_items(&json!({"type": "result"})).is_none());
    }
}
