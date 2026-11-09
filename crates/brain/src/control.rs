// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's control socket (`control_socket`, a Unix socket; mode 0660, the directory's group
//! `home-companion-brain`, which the body's user `home-companion` is in). One JSON object per line each way, one answer
//! per request:
//!
//! | request | answer |
//! |---|---|
//! | `{"cmd":"status"}` | `{"ok":true,"key":…,"provider":…,"model":…,"facts":N,"jobs":N,"tidy":{…}\|null}` |
//! | `{"cmd":"memory.list"}` | `{"ok":true,"profile":"…","notes":"…","facts":[{id,text,kind,importance,created,updated}]}` |
//! | `{"cmd":"memory.delete","id":N}` | `{"ok":true,"deleted":0\|1}` |
//! | `{"cmd":"memory.forget_all","confirm":true}` | `{"ok":true}` (without `confirm`: an error) |
//! | `{"cmd":"jobs.list"}` | `{"ok":true,"jobs":[{id,due_at,repeat,text,how,status,source,late}]}` |
//! | `{"cmd":"jobs.cancel","id":N}` / `{"cmd":"job.ack","id":N}` | `{"ok":true,"changed":0\|1}` |
//! | `{"cmd":"job.snooze","id":N,"minutes":M}` | `{"ok":true,"id":N\|null}`: acknowledged, the same words again in M minutes (default 10) |
//! | `{"cmd":"subscribe"}` | `{"ok":true}`, then the reminders delivered and not yet touched, then every new event (events.rs) on this connection |
//! | `{"cmd":"ha.token","token":…,"expires_at":…,"url":…,"satellite_entity":…,"allow":[…],"language":…,"lists":[…],"calendars":[…]}` | `{"ok":true}`: the body's Home Assistant link; `lists`/`calendars` are the `todo.*`/`calendar.*` entities chosen on the screen (absent = unchanged, empty = all) |
//! | `{"cmd":"heard","text":…}` / `{"cmd":"said","text":…}` | `{"ok":true}`: the user's transcript and Home Assistant's answer |
//! | `{"cmd":"confirm.answer","id":N,"yes":B}` | `{"ok":true}`: Yes or No on the screen for the `confirm` event `id` |
//! | `{"cmd":"control.undo","id":N}` | `{"ok":true}`: the screen's "Undo" on the `changed` event `id` |
//!
//! The examples both sides test against: `contract/requests.jsonl` and `contract/events.jsonl`.
//!
//! Errors: `{"ok":false,"error":"…"}`. Nothing else is exposed (no TCP, no other commands).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::Brain;

/// The connections that asked for events.
#[derive(Default)]
pub struct Hub {
    subscribers: Mutex<Vec<UnixStream>>,
}

impl Hub {
    /// Sends each event to every subscriber; a subscriber that cannot take it is dropped.
    pub fn broadcast(&self, events: &[Value]) {
        if events.is_empty() {
            return;
        }
        let mut subs = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain_mut(|s| events.iter().all(|e| writeln!(s, "{e}").is_ok()));
    }

    pub fn subscribers(&self) -> usize {
        self.subscribers.lock().map_or(0, |s| s.len())
    }
}

/// One request → its answer (and whether the connection now subscribes).
pub fn answer(brain: &Brain, req: &Value) -> (Value, bool) {
    let id = req.get("id").and_then(Value::as_i64).unwrap_or(0);
    let ok = |mut v: Value| {
        v["ok"] = json!(true);
        v
    };
    let err = |e: String| json!({"ok": false, "error": e});
    let r = match req.get("cmd").and_then(Value::as_str).unwrap_or("") {
        "status" => ok(brain.status()),
        "memory.list" => ok(brain.memory_list()),
        "memory.delete" => brain
            .delete_fact(id)
            .map_or_else(err, |n| json!({"ok": true, "deleted": n})),
        "memory.forget_all" => {
            if req.get("confirm").and_then(Value::as_bool) == Some(true) {
                brain
                    .forget_everything()
                    .map_or_else(err, |_| json!({"ok": true}))
            } else {
                err("confirm: true is required".into())
            }
        }
        "jobs.list" => json!({"ok": true, "jobs": brain.jobs_list()}),
        "jobs.cancel" => brain
            .cancel_job(id)
            .map_or_else(err, |n| json!({"ok": true, "changed": n})),
        "job.ack" => brain
            .ack_job(id)
            .map_or_else(err, |n| json!({"ok": true, "changed": n})),
        "job.snooze" => brain
            .snooze_job(id, req.get("minutes").and_then(Value::as_i64).unwrap_or(10))
            .map_or_else(err, |n| json!({"ok": true, "id": n})),
        "ha.token" => match (
            req.get("token").and_then(Value::as_str),
            req.get("expires_at").and_then(Value::as_u64),
        ) {
            (Some(t), Some(exp)) if !t.is_empty() => {
                if let Some(a) = req.get("allow").and_then(Value::as_array) {
                    let a: Vec<String> = a
                        .iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    brain.set_learned_allow(&a);
                }
                if let Some(l) = req.get("language").and_then(Value::as_str) {
                    brain.set_language(l);
                }
                let entities = |k: &str| {
                    req.get(k).and_then(Value::as_array).map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect::<Vec<_>>()
                    })
                };
                brain.set_chosen(entities("lists"), entities("calendars"));
                brain
                    .set_ha_token(
                        t.to_string(),
                        exp,
                        req.get("url").and_then(Value::as_str),
                        req.get("satellite_entity").and_then(Value::as_str),
                    )
                    .map_or_else(err, |_| json!({"ok": true}))
            }
            _ => err("token and expires_at are required".into()),
        },
        "heard" => {
            brain.heard(req.get("text").and_then(Value::as_str).unwrap_or(""));
            json!({"ok": true})
        }
        "said" => {
            brain.said(req.get("text").and_then(Value::as_str).unwrap_or(""));
            json!({"ok": true})
        }
        "confirm.answer" => match req.get("yes").and_then(Value::as_bool) {
            Some(yes) => brain
                .answer_confirm(id, yes)
                .map_or_else(err, |_| json!({"ok": true})),
            None => err("yes: true or false is required".into()),
        },
        "control.undo" => brain
            .undo_control(id)
            .map_or_else(err, |_| json!({"ok": true})),
        "subscribe" => return (json!({"ok": true}), true),
        other => err(format!("unknown command {other:?}")),
    };
    (r, false)
}

fn serve(brain: &Brain, hub: &Hub, stream: UnixStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut w = stream.try_clone()?;
    let mut r = BufReader::new(stream.try_clone()?.take(64 * 1024));
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let req: Value = match serde_json::from_str(line.trim()) {
            Ok(v) => v,
            Err(e) => {
                writeln!(
                    w,
                    "{}",
                    json!({"ok": false, "error": format!("bad JSON: {e}")})
                )?;
                continue;
            }
        };
        let (a, subscribe) = answer(brain, &req);
        writeln!(w, "{a}")?;
        if subscribe {
            for e in brain.replay() {
                writeln!(w, "{e}")?;
            }
            // From now on the connection only receives; the body keeps it open.
            stream.set_read_timeout(None)?;
            hub.subscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(w);
            return Ok(());
        }
    }
}

/// Binds the socket (replacing a stale one) and serves it on its own threads.
pub fn start(brain: Arc<Brain>, hub: Arc<Hub>, path: &str) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    log::info!("control socket {path}");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            let (b, h) = (brain.clone(), hub.clone());
            std::thread::spawn(move || {
                if let Err(e) = serve(&b, &h, stream) {
                    log::debug!("control connection: {e}");
                }
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every request of the contract is known (an unfinished one may still answer an error,
    /// never "unknown command"), and `ha.token` with lists and calendars is accepted.
    #[test]
    fn contract_requests_are_known() {
        let dir = std::env::temp_dir().join(format!(
            "home-companion-brain-control-{}",
            std::process::id()
        ));
        let mut cfg = crate::config::Config::default();
        cfg.memory.dir = dir.display().to_string();
        let brain = Brain::new(cfg).unwrap();
        for line in include_str!("../contract/requests.jsonl").lines() {
            if line.trim().is_empty() {
                continue;
            }
            let req: Value = serde_json::from_str(line).unwrap();
            let (a, subscribe) = answer(&brain, &req);
            assert!(!subscribe);
            let error = a["error"].as_str().unwrap_or("");
            assert!(!error.starts_with("unknown command"), "{line}: {a}");
            if req["cmd"] == "ha.token" {
                assert_eq!(a["ok"], true, "{a}");
            }
        }
        let (a, _) = answer(&brain, &json!({"cmd": "confirm.answer", "id": 1}));
        assert_eq!(a["ok"], false);
        let _ = std::fs::remove_dir_all(dir);
    }
}
