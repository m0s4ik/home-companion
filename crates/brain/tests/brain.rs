// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! home-companion-brain end to end against a fake Home Assistant (WebSocket API): the tools Home
//! Assistant's AI calls over MCP, the memory jobs through `ai_task`, the shopping list.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use brain::agent::Brain;
use brain::config::Config;
use serde_json::{Value, json};

type Todo = Arc<Mutex<Vec<(String, String, String)>>>;

/// What the fake Home Assistant knows and answers.
#[derive(Default, Clone)]
struct Fake {
    /// `ai_task.generate_data` answers, in order; the instructions it got are kept.
    ai: Arc<Mutex<Vec<String>>>,
    asked: Arc<Mutex<Vec<String>>>,
    /// Exposed entities: (entity_id, name, device_class).
    exposed: Vec<(&'static str, &'static str, &'static str)>,
    todo: Todo,
    /// Every command received.
    calls: Arc<Mutex<Vec<Value>>>,
}

/// "2026-11-05T17:00:00Z" for a time in seconds (UTC).
fn rfc3339(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

fn ws_read(s: &mut TcpStream) -> Option<Value> {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).ok()?;
    let mut len = (h[1] & 0x7f) as usize;
    if len == 126 {
        let mut b = [0u8; 2];
        s.read_exact(&mut b).ok()?;
        len = u16::from_be_bytes(b) as usize;
    } else if len == 127 {
        let mut b = [0u8; 8];
        s.read_exact(&mut b).ok()?;
        len = u64::from_be_bytes(b) as usize;
    }
    let mut mask = [0u8; 4];
    if h[1] & 0x80 != 0 {
        s.read_exact(&mut mask).ok()?;
    }
    let mut p = vec![0u8; len];
    s.read_exact(&mut p).ok()?;
    for (i, b) in p.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    if h[0] & 0x0f != 1 {
        return None;
    }
    serde_json::from_slice(&p).ok()
}

fn ws_send(s: &mut TcpStream, v: &Value) {
    let p = v.to_string().into_bytes();
    let mut f = vec![0x81u8];
    if p.len() < 126 {
        f.push(p.len() as u8);
    } else {
        f.push(126);
        f.extend_from_slice(&(p.len() as u16).to_be_bytes());
    }
    f.extend_from_slice(&p);
    let _ = s.write_all(&f);
}

fn answer(f: &Fake, cmd: &Value) -> Value {
    f.calls.lock().unwrap().push(cmd.clone());
    let mut t = f.todo.lock().unwrap();
    match (cmd["type"].as_str(), cmd["domain"].as_str(), cmd["service"].as_str()) {
        (Some("ai_task/preferences/get"), _, _) => json!({"gen_data_entity_id": "ai_task.test"}),
        (Some("call_service"), Some("ai_task"), Some("generate_data")) => {
            f.asked.lock().unwrap().push(
                cmd["service_data"]["instructions"].as_str().unwrap_or("").to_string(),
            );
            let mut q = f.ai.lock().unwrap();
            let data = if q.is_empty() { String::new() } else { q.remove(0) };
            json!({"response": {"data": data, "conversation_id": "c"}})
        }
        (Some("homeassistant/expose_entity/list"), _, _) => {
            let m: serde_json::Map<String, Value> = f
                .exposed
                .iter()
                .map(|(id, _, _)| (id.to_string(), json!({"conversation": true})))
                .collect();
            json!({"exposed_entities": m})
        }
        (Some("get_states"), _, _) => json!(
            f.exposed
                .iter()
                .map(|(id, name, class)| json!({"entity_id": id, "state": "open",
                    "attributes": {"friendly_name": name, "device_class": class}}))
                .collect::<Vec<_>>()
        ),
        (Some("config/entity_registry/list_for_display"), _, _) => json!({"entities": []}),
        (Some("config/device_registry/list"), _, _) | (Some("config/area_registry/list"), _, _) => {
            json!([])
        }
        (Some("todo/item/list"), _, _) => json!({"items": t.iter().map(|(u, s, st)|
            json!({"uid": u, "summary": s, "status": st})).collect::<Vec<_>>()}),
        (Some("call_service"), _, Some("add_item")) => {
            let n = t.len() + 1;
            t.push((
                format!("u{n}"),
                cmd["service_data"]["item"].as_str().unwrap().into(),
                "needs_action".into(),
            ));
            json!(null)
        }
        (Some("call_service"), _, Some("update_item")) => {
            let uid = cmd["service_data"]["item"].as_str().unwrap();
            for i in t.iter_mut().filter(|i| i.0 == uid) {
                i.2 = cmd["service_data"]["status"].as_str().unwrap().into();
            }
            json!(null)
        }
        (Some("call_service"), _, Some("remove_item")) => {
            let uids: Vec<String> = cmd["service_data"]["item"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            t.retain(|i| !uids.contains(&i.0));
            json!(null)
        }
        _ => json!(null),
    }
}

/// A fake Home Assistant: any number of commands on each connection, answered in order.
fn fake_ha(f: Fake) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut s) = c else { break };
            let f = f.clone();
            std::thread::spawn(move || {
                let mut req = Vec::new();
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let mut b = [0u8; 512];
                    let n = s.read(&mut b).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&b[..n]);
                }
                let _ = s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n");
                ws_send(&mut s, &json!({"type": "auth_required"}));
                if ws_read(&mut s).is_none() {
                    return;
                }
                ws_send(&mut s, &json!({"type": "auth_ok"}));
                while let Some(cmd) = ws_read(&mut s) {
                    let result = answer(&f, &cmd);
                    ws_send(
                        &mut s,
                        &json!({"id": cmd["id"], "type": "result", "success": true, "result": result}),
                    );
                }
            });
        }
    });
    url
}

fn setup(name: &str, ha_url: &str) -> (Config, PathBuf) {
    let dir = std::env::temp_dir().join(format!("home-companion-brain-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tok = dir.join("tok");
    std::fs::write(&tok, "t\n").unwrap();
    let mut c = Config {
        character_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/../../characters/alfred").into(),
        ..Config::default()
    };
    c.home_assistant.url = ha_url.into();
    c.home_assistant.token_file = tok.to_string_lossy().into();
    c.memory.dir = dir.join("mem").to_string_lossy().into();
    (c, dir)
}

fn tool(b: &Brain, name: &str, args: Value) -> (Value, bool) {
    b.mcp_call(name, &args)
}

#[test]
fn remember_and_recall_through_the_tools() {
    let (cfg, dir) = setup("remember", "");
    let b = Brain::new(cfg).unwrap();
    let (r, err) = tool(&b, "remember", json!({"text": "Andrea ha il dentista il 12 novembre"}));
    assert!(!err, "{r}");
    assert_eq!(b.facts("dentista").len(), 1);
    let (r, _) = tool(&b, "recall", json!({"query": "dentista"}));
    assert!(r.to_string().contains("12 novembre"), "{r}");
    // No Home Assistant: no house tools, no AI for the memory jobs.
    let names: Vec<&str> = b.mcp_tools().iter().map(|t| t.name).collect();
    assert!(!names.contains(&"home_control"));
    assert_eq!(b.status()["key"], false);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn end_of_conversation_extraction_through_ai_task() {
    let f = Fake::default();
    f.ai.lock().unwrap().push(
        "```json\n{\"summary\": \"Andrea ha detto dove è la caldaia.\", \"ops\": [\
          {\"op\": \"add\", \"text\": \"La caldaia di casa è in cantina\", \"kind\": \"house\", \"importance\": 6}]}\n```"
            .into(),
    );
    let (mut cfg, dir) = setup("extract", &fake_ha(f.clone()));
    cfg.memory.session_gap_minutes = 0;
    let b = Brain::new(cfg).unwrap();
    // The satellite heard the user, Home Assistant answered: an exchange for the memory.
    b.heard("la caldaia è in cantina, ricordatelo");
    b.said("Va bene, me lo ricordo.");
    b.close_sessions();
    assert_eq!(b.facts("caldaia")[0].text, "La caldaia di casa è in cantina");
    let asked = f.asked.lock().unwrap();
    assert!(asked[0].contains("la caldaia è in cantina, ricordatelo"), "{asked:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn house_gates_need_a_spoken_yes_and_never_open_doors() {
    let f = Fake {
        exposed: vec![
            ("cover.soggiorno", "Tapparella soggiorno", "shutter"),
            ("cover.garage", "Garage", "garage"),
            ("light.cucina", "Luce cucina", ""),
        ],
        ..Fake::default()
    };
    let (cfg, dir) = setup("gate", &fake_ha(f.clone()));
    let b = Brain::new(cfg).unwrap();
    let services = |f: &Fake| -> Vec<String> {
        f.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "call_service")
            .map(|c| format!("{}.{} {}", c["domain"].as_str().unwrap(), c["service"].as_str().unwrap(),
                c["service_data"]["entity_id"].as_str().unwrap()))
            .collect()
    };
    // A light: at once.
    let (r, err) = tool(&b, "home_control", json!({"entity_id": "light.cucina", "service": "turn_on"}));
    assert!(!err, "{r}");
    // A garage door: never.
    let (r, err) = tool(&b, "home_control", json!({"entity_id": "cover.garage", "service": "open_cover"}));
    assert!(err && r.to_string().contains("never"), "{r}");
    // A shutter: asks, and runs only after the user's own "sì".
    let args = json!({"entity_id": "cover.soggiorno", "service": "close_cover"});
    let (r, err) = tool(&b, "home_control", args.clone());
    assert!(!err && r["needs_confirmation"] == true, "{r}");
    let id = r["confirm_id"].clone();
    let mut confirmed = args.clone();
    confirmed["confirm_id"] = id;
    std::thread::sleep(std::time::Duration::from_millis(5));
    b.heard("no, lascia stare");
    let (r, err) = tool(&b, "home_control", confirmed.clone());
    assert!(err && r.to_string().contains("did not confirm"), "{r}");
    let (r, _) = tool(&b, "home_control", args);
    let mut again = json!({"entity_id": "cover.soggiorno", "service": "close_cover"});
    again["confirm_id"] = r["confirm_id"].clone();
    std::thread::sleep(std::time::Duration::from_millis(5));
    b.heard("sì");
    let (r, err) = tool(&b, "home_control", again);
    assert!(!err && r["done"] == true, "{r}");
    assert_eq!(
        services(&f),
        ["light.turn_on light.cucina", "cover.close_cover cover.soggiorno"]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nightly_tidy_up_merges_updates_and_retires() {
    let f = Fake::default();
    f.ai.lock().unwrap().push(
        "{\"merge\": [{\"ids\": [1, 2], \"text\": \"Andrea beve il caffè senza zucchero\"}], \
          \"update\": [{\"id\": 3, \"text\": \"La caldaia di casa è in cantina, a sinistra\"}], \
          \"invalidate\": [4, 99], \"profile\": \"- Si chiama Andrea\", \"notes\": null}"
            .into(),
    );
    let (cfg, dir) = setup("tidy", &fake_ha(f.clone()));
    let b = Brain::new(cfg).unwrap();
    for t in [
        "Andrea prende il caffè amaro",
        "Andrea non mette zucchero nel caffè",
        "La caldaia è in cantina",
        "Andrea ha il dentista il 3 novembre 2026",
        "Il gatto si chiama Neve",
    ] {
        b.add_fact(t);
    }
    let r = b.consolidate();
    assert!(r.ok, "{r:?}");
    assert_eq!((r.merged, r.updated, r.invalidated, r.blocks), (2, 1, 1, 1));
    let list = b.memory_list();
    let texts: Vec<&str> = list["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts.contains(&"Andrea beve il caffè senza zucchero"));
    assert!(!texts.iter().any(|t| t.contains("dentista")));
    assert_eq!(list["profile"], "- Si chiama Andrea\n");
    let asked = f.asked.lock().unwrap();
    assert!(asked[0].contains("[4] (") && asked[0].contains("dentista"), "{asked:?}");
    // The AI task chosen in Home Assistant was used.
    assert!(f.calls.lock().unwrap().iter().any(|c| c["service_data"]["entity_id"] == "ai_task.test"));
    assert_eq!(b.status()["tidy"]["merged"], 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn tidy_up_without_home_assistant_does_nothing() {
    let (cfg, dir) = setup("tidy-noai", "");
    let b = Brain::new(cfg).unwrap();
    b.add_fact("Il gatto si chiama Neve");
    let r = b.consolidate();
    assert!(!r.ok && r.skipped == "no AI", "{r:?}");
    assert_eq!(b.memory_list()["facts"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn control_socket_commands() {
    use brain::control::answer;
    let (cfg, dir) = setup("control", "");
    let b = Brain::new(cfg).unwrap();
    b.add_fact("Il gatto si chiama Neve");
    let (s, sub) = answer(&b, &json!({"cmd": "status"}));
    assert!(!sub);
    assert_eq!(
        (s["ok"].clone(), s["key"].clone(), s["facts"].clone()),
        (json!(true), json!(false), json!(1))
    );
    let (l, _) = answer(&b, &json!({"cmd": "memory.list"}));
    let id = l["facts"][0]["id"].as_i64().unwrap();
    assert!(l["facts"][0]["created"].as_i64().unwrap() > 0);
    let (d, _) = answer(&b, &json!({"cmd": "memory.delete", "id": id}));
    assert_eq!(d["deleted"], 1);
    let (f, _) = answer(&b, &json!({"cmd": "memory.forget_all"}));
    assert_eq!(f["ok"], false);
    let (f, _) = answer(&b, &json!({"cmd": "memory.forget_all", "confirm": true}));
    assert_eq!(f["ok"], true);
    let (u, _) = answer(&b, &json!({"cmd": "shell"}));
    assert_eq!(u["ok"], false);
    let (_, sub) = answer(&b, &json!({"cmd": "subscribe"}));
    assert!(sub);
    // The language chosen on the body's screen comes with the token and wins over brain.toml's.
    assert_eq!(b.language(), "it");
    let _ = answer(&b, &json!({"cmd": "ha.token", "token": "t", "expires_at": 1, "language": "en"}));
    assert_eq!(b.language(), "en");
    let _ = answer(&b, &json!({"cmd": "ha.token", "token": "t", "expires_at": 1, "language": "../x"}));
    assert_eq!(b.language(), "en");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_reminder_by_voice_is_delivered_as_a_card() {
    let now = brain::memory::now();
    let when = now + 3600;
    let (cfg, dir) = setup("jobs", "");
    let b = Brain::new(cfg).unwrap();
    let (r, err) = tool(
        &b,
        "schedule",
        json!({"when": rfc3339(when), "text": "chiamare la mamma", "how": "card_then_voice"}),
    );
    assert!(!err, "{r}");
    // A time in the past is refused.
    let (_, err) = tool(&b, "schedule", json!({"when": rfc3339(now - 7200), "text": "ieri"}));
    assert!(err);
    let jobs = b.jobs_list();
    assert_eq!(jobs.as_array().unwrap().len(), 1, "{jobs}");
    assert_eq!(jobs[0]["due_at"], when);
    // Not yet due: nothing; due: one card; no satellite configured: never spoken.
    assert!(b.tick(when - 120).iter().all(|e| e["kind"] != "reminder"));
    let ev = b.tick(when + 10);
    let cards: Vec<&Value> = ev.iter().filter(|e| e["kind"] == "reminder").collect();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0]["text"], "chiamare la mamma");
    assert_eq!(cards[0]["late"], false);
    // A body that reconnects sees it again until it is touched.
    assert_eq!(b.replay().len(), 1);
    let id = cards[0]["id"].as_i64().unwrap();
    assert_eq!(b.ack_job(id).unwrap(), 1);
    assert!(b.replay().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

// ---- the shopping list against the fake Home Assistant

#[test]
fn shopping_list_add_read_done_remove() {
    let todo: Todo = Arc::new(Mutex::new(vec![
        ("u1".into(), "Pane".into(), "needs_action".into()),
        ("u2".into(), "Latte".into(), "completed".into()),
    ]));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let url = fake_ha(Fake { todo: todo.clone(), calls: calls.clone(), ..Fake::default() });
    let dir = std::env::temp_dir().join(format!("home-companion-brain-lists-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("tok"), "t\n").unwrap();
    let ha = brain::ha::Ha::new(brain::config::HaCfg {
        url,
        token_file: dir.join("tok").to_string_lossy().into(),
        ..Default::default()
    });
    let l = brain::lists::Lists {
        ha: &ha,
        entity: "todo.spesa",
        timeout: std::time::Duration::from_secs(5),
    };
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // "pane" is there already, "latte" was ticked: back on the list, "2 kg farina" is new.
    let r = l
        .add(&s(&["pane", "Latte", "2 kg farina", "2 kg Farina"]))
        .unwrap();
    assert_eq!(r["added"], json!(["2 kg farina"]));
    assert_eq!(r["already_on_the_list"], json!(["Pane"]));
    assert_eq!(r["put_back"], json!(["Latte"]));
    let r = l.read(10).unwrap();
    assert_eq!(r["items"], json!(["Pane", "Latte", "2 kg farina"]));
    // "ho preso il pane e la farina"
    let r = l
        .done_or_remove(&s(&["il pane", "farina", "uova"]), false)
        .unwrap();
    assert_eq!(r["ticked"], json!(["Pane", "2 kg farina"]));
    assert_eq!(r["not_on_the_list"], json!(["uova"]));
    let r = l.done_or_remove(&s(&["latte"]), true).unwrap();
    assert_eq!(r["removed"], json!(["Latte"]));
    let t = todo.lock().unwrap();
    assert_eq!(t.len(), 2);
    assert!(t.iter().all(|i| i.2 == "completed"));
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c["target"].is_null() || c["target"]["entity_id"] == "todo.spesa")
    );
    let _ = std::fs::remove_dir_all(dir);
}
