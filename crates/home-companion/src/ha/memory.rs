// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the agent always knows, published in Home Assistant: the brain composes the stable facts
//! (`memory.context`), the body writes them as the state `sensor.<name>_memory` (attribute
//! `text`, REST `POST /api/states/…` with the linked user's token), and the agent's
//! instructions read it as a template at every conversation. Such a state is lost when Home
//! Assistant restarts: it is written again within a minute. The same state written again is a
//! no-op for Home Assistant (no history row), so no time goes in it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::auth::{self, Auth};

/// How often the brain is asked for the context.
const ASK_EVERY: Duration = Duration::from_secs(20);
/// Written again at least this often (Home Assistant forgets it when it restarts).
const POST_EVERY: Duration = Duration::from_secs(60);
/// After a refusal (the linked user is not an administrator): try again this late.
const REFUSED_WAIT: Duration = Duration::from_secs(600);

/// `sensor.<slug>_memory`: lower-case ASCII letters and digits, anything else `_`, collapsed
/// and trimmed; an empty slug is `companion`.
pub fn entity_id(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_matches('_');
    let slug = if slug.is_empty() { "companion" } else { slug };
    format!("sensor.{slug}_memory")
}

/// The state written: how many facts, the list, a name and an icon. Nothing that changes by
/// itself (two calls with the same facts are equal).
pub fn state_body(text: &str, shown: u64, name: &str) -> Value {
    json!({"state": shown.to_string(), "attributes": {
        "text": text,
        "friendly_name": format!("{name} · memory"),
        "icon": "mdi:head-lightbulb-outline",
    }})
}

/// Whether to write: the facts changed (`rev` ≠ the last written) or the last good write is a
/// minute old (Home Assistant may have restarted); never written: yes.
pub fn should_post(rev: i64, last_rev: Option<i64>, since_last_ok: Option<Duration>) -> bool {
    last_rev != Some(rev) || since_last_ok.is_none_or(|d| d >= POST_EVERY)
}

/// The publishing thread (`memory-publish`).
pub fn spawn(brain_socket: PathBuf, auth: Arc<Auth>, url: String, name: String) {
    let entity = entity_id(&name);
    let _ = std::thread::Builder::new()
        .name("memory-publish".into())
        .spawn(move || {
            let path = format!("/api/states/{entity}");
            let mut last_rev: Option<i64> = None;
            let mut last_ok: Option<Instant> = None;
            let mut told: Option<(bool, u16)> = None;
            let mut refused_at: Option<Instant> = None;
            loop {
                std::thread::sleep(ASK_EVERY);
                if refused_at.is_some_and(|t| t.elapsed() < REFUSED_WAIT) {
                    continue;
                }
                let Some(ctx) = crate::brain_link::request(
                    &brain_socket,
                    &json!({"cmd": "memory.context"}),
                )
                .filter(|a| a.get("ok") == Some(&Value::Bool(true))) else {
                    continue;
                };
                let rev = ctx["rev"].as_i64().unwrap_or(0);
                if !should_post(rev, last_rev, last_ok.map(|t| t.elapsed())) {
                    continue;
                }
                let Ok(token) = auth.access_token() else {
                    continue;
                };
                let body = state_body(
                    ctx["text"].as_str().unwrap_or(""),
                    ctx["shown"].as_u64().unwrap_or(0),
                    &name,
                )
                .to_string();
                let status = auth::http(
                    &url,
                    "POST",
                    &path,
                    Some(&token),
                    Some(("application/json", body.as_bytes())),
                    64 << 10,
                    Duration::from_secs(10),
                )
                .map_or(0, |r| r.status);
                let ok = (200..300).contains(&status);
                if told != Some((ok, status)) {
                    told = Some((ok, status));
                    if ok {
                        log::info!(event = "ha_memory", ok = true, status = status; "memory: {entity} written");
                    } else if matches!(status, 401 | 403) {
                        log::warn!(event = "ha_memory", ok = false, status = status; "memory: Home Assistant refused {entity}: the linked user is not an administrator");
                    } else {
                        log::warn!(event = "ha_memory", ok = false, status = status; "memory: {entity} not written (HTTP {status})");
                    }
                }
                if ok {
                    last_rev = Some(rev);
                    last_ok = Some(Instant::now());
                    refused_at = None;
                } else if matches!(status, 401 | 403) {
                    refused_at = Some(Instant::now());
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p1_entity_ids() {
        assert_eq!(entity_id("Alfred"), "sensor.alfred_memory");
        assert_eq!(entity_id("Señor Bot 2"), "sensor.se_or_bot_2_memory");
        assert_eq!(entity_id(""), "sensor.companion_memory");
        assert_eq!(entity_id("  --  "), "sensor.companion_memory");
    }

    #[test]
    fn p2_the_state_does_not_change_by_itself() {
        let a = state_body("- Il gatto si chiama Neve", 1, "Alfred");
        assert_eq!(a, state_body("- Il gatto si chiama Neve", 1, "Alfred"));
        assert_eq!(a["state"], "1");
        assert_eq!(a["attributes"]["text"], "- Il gatto si chiama Neve");
    }

    #[test]
    fn p4_when_to_write() {
        assert!(should_post(3, None, None));
        assert!(should_post(4, Some(3), Some(Duration::from_secs(5))));
        assert!(!should_post(3, Some(3), Some(Duration::from_secs(5))));
        assert!(should_post(3, Some(3), Some(Duration::from_secs(61))));
    }
}
