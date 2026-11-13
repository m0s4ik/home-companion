// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! home-companion-brain end to end against a fake Home Assistant (WebSocket API): the tools Home
//! Assistant's AI calls over MCP, the memory jobs through `ai_task`, the shopping list.

mod common;

use std::sync::{Arc, Mutex};

use brain::agent::Brain;
use common::*;
use serde_json::{Value, json};

#[test]
fn remember_and_recall_through_the_tools() {
    let (cfg, dir) = setup("remember", "");
    let b = Brain::new(cfg).unwrap();
    let (r, err) = tool(
        &b,
        "remember",
        json!({"text": "Andrea ha il dentista il 12 novembre"}),
    );
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
    assert_eq!(
        b.facts("caldaia")[0].text,
        "La caldaia di casa è in cantina"
    );
    let asked = f.asked.lock().unwrap();
    assert!(
        asked[0].contains("la caldaia è in cantina, ricordatelo"),
        "{asked:?}"
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
    assert!(
        asked[0].contains("[4] (") && asked[0].contains("dentista"),
        "{asked:?}"
    );
    // The AI task chosen in Home Assistant was used.
    assert!(
        f.calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["service_data"]["entity_id"] == "ai_task.test")
    );
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
    let _ = answer(
        &b,
        &json!({"cmd": "ha.token", "token": "t", "expires_at": 1, "language": "en"}),
    );
    assert_eq!(b.language(), "en");
    let _ = answer(
        &b,
        &json!({"cmd": "ha.token", "token": "t", "expires_at": 1, "language": "../x"}),
    );
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
    let (_, err) = tool(
        &b,
        "schedule",
        json!({"when": rfc3339(now - 7200), "text": "ieri"}),
    );
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
    let url = fake_ha(Fake {
        todo: todo.clone(),
        calls: calls.clone(),
        ..Fake::default()
    });
    let dir =
        std::env::temp_dir().join(format!("home-companion-brain-lists-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("tok"), "t\n").unwrap();
    let ha = brain::ha::Ha::new(brain::config::HaCfg {
        url,
        token_file: dir.join("tok").to_string_lossy().into(),
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

// ---- v0.2: several lists, calendars, house commands on the screen, views, announcements

#[test]
fn list_tools_work_on_several_lists_by_name() {
    let f = Fake {
        states: vec![
            ("todo.casa", "Home to-do"),
            ("todo.bring", "Spesa"),
            ("todo.garden", "Garden"),
        ],
        ..Fake::default()
    };
    let (mut cfg, dir) = setup("lists2", &fake_ha(f.clone()));
    cfg.lists.shopping = "todo.bring".into();
    let b = Brain::new(cfg).unwrap();
    b.set_chosen(Some(vec!["todo.casa".into(), "todo.bring".into()]), None);
    let target = |f: &Fake, service: &str| -> String {
        f.calls
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|c| c["service"] == service)
            .map(|c| c["target"]["entity_id"].as_str().unwrap().to_string())
            .unwrap()
    };
    let (r, err) = tool(&b, "list_add", json!({"items": ["pane"]}));
    assert!(!err, "{r}");
    assert_eq!(target(&f, "add_item"), "todo.bring");
    let (r, err) = tool(
        &b,
        "list_add",
        json!({"items": ["tidy shed"], "list": "home to do"}),
    );
    assert!(!err && r.to_string().contains("Home to-do"), "{r}");
    assert_eq!(target(&f, "add_item"), "todo.casa");
    // Not among the chosen lists: refused, with the names that are.
    let (r, err) = tool(&b, "list_read", json!({"list": "garden"}));
    assert!(err && r.to_string().contains("Spesa"), "{r}");
    // Everything chosen again (empty = all): the garden list is there.
    b.set_chosen(Some(vec![]), None);
    let (r, err) = tool(&b, "list_read", json!({"list": "todo.garden"}));
    assert!(!err, "{r}");
    // Without a configured shopping list: the one named like it.
    let (mut cfg, dir2) = setup("lists3", &fake_ha(f.clone()));
    cfg.lists.shopping.clear();
    let b = Brain::new(cfg).unwrap();
    let (_, err) = tool(&b, "list_add", json!({"items": ["uova"]}));
    assert!(!err);
    assert_eq!(target(&f, "add_item"), "todo.bring");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}

#[test]
fn upcoming_events_on_subscribe_and_with_the_chosen_calendars() {
    let now = brain::memory::now();
    let at = |t: i64| rfc3339(t);
    let f = Fake {
        states: vec![("calendar.family", "Family"), ("calendar.work", "Work")],
        ..Fake::default()
    };
    *f.calendar.lock().unwrap() = json!({
        "calendar.family": {"events": [
            {"start": at(now + 7200), "end": at(now + 10800), "summary": "Dentist"}]},
        "calendar.work": {"events": [
            {"start": at(now + 3600), "end": at(now + 5400), "summary": "Stand-up"}]}
    });
    let (cfg, dir) = setup("upcoming", &fake_ha(f.clone()));
    let b = Arc::new(Brain::new(cfg).unwrap());
    // Nothing chosen = every calendar of Home Assistant; soonest first.
    let mut screen = Screen::connect(&b, &dir);
    let u = screen.event("upcoming");
    let titles: Vec<&str> = u["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Stand-up", "Dentist"], "{u}");
    assert_eq!(u["items"][0]["calendar"], "calendar.work");
    assert_eq!(u["items"][0]["all_day"], false);
    let ask = f
        .calls
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|c| c["service"] == "get_events")
        .cloned()
        .unwrap();
    assert_eq!(
        ask["target"]["entity_id"],
        json!(["calendar.family", "calendar.work"])
    );
    // Nothing changed: no new event; a new choice reads again and sends the new list.
    assert!(b.refresh_upcoming(now + 30).is_none());
    b.set_chosen(None, Some(vec!["calendar.family".into()]));
    // The fake answers both calendars anyway: same events, nothing to send, but it did read again.
    assert!(b.refresh_upcoming(now + 60).is_none());
    let ask = f
        .calls
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|c| c["service"] == "get_events")
        .cloned()
        .unwrap();
    assert_eq!(ask["target"]["entity_id"], json!(["calendar.family"]));
    // Ten minutes later the calendars are read again; same events: nothing to send.
    assert!(b.refresh_upcoming(now + 60 + 599).is_none());
    *f.calendar.lock().unwrap() = json!({"calendar.family": {"events": []}});
    let ev = b
        .refresh_upcoming(now + 60 + 601)
        .expect("the events changed");
    assert_eq!(ev["items"], json!([]));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_whole_room_in_one_call() {
    let f = Fake {
        exposed: exposed(&[
            ("light.studio", "Studio", ""),
            ("light.studio_sx", "Studio SX", ""),
            ("light.libreria", "Libreria", ""),
            ("light.cucina", "Luce cucina", ""),
            ("cover.studio", "Tapparella studio", "shutter"),
        ]),
        areas: vec![
            ("light.studio", "Studio"),
            ("light.studio_sx", "Studio"),
            ("light.libreria", "Studio"),
            ("light.cucina", "Cucina"),
            ("cover.studio", "Studio"),
        ],
        // light.studio is the room's group (a Hue room): done through its lights, not twice.
        groups: vec![("light.studio", vec!["light.studio_sx", "light.libreria"])],
        ..Fake::default()
    };
    let (cfg, dir) = setup("room", &fake_ha(f.clone()));
    let b = Arc::new(Brain::new(cfg).unwrap());
    let mut screen = Screen::connect(&b, &dir);
    let (r, err) = tool(
        &b,
        "home_control",
        json!({"area": "studio", "service": "turn_off"}),
    );
    assert!(!err, "{r}");
    assert_eq!(r["done"].as_array().unwrap().len(), 2, "{r}");
    for _ in 0..2 {
        assert_eq!(screen.event("changed")["undo"], true);
    }
    let lights: Vec<String> = services(&f)
        .into_iter()
        .filter(|s| s.starts_with("light."))
        .collect();
    assert_eq!(
        lights,
        [
            "light.turn_off light.libreria",
            "light.turn_off light.studio_sx"
        ]
    );
    // The room's only shutter: its own gate, a question on the screen (never done at once).
    let (r, err) = tool(
        &b,
        "home_control",
        json!({"area": "Studio", "domain": "cover", "service": "close_cover"}),
    );
    assert!(!err && r["asked"].is_object(), "{r}");
    assert!(services(&f).iter().all(|s| !s.starts_with("cover.")));
    // A list; an unknown area says which areas there are.
    let (r, err) = tool(
        &b,
        "home_control",
        json!({"entity_ids": ["light.cucina", "light.libreria"], "service": "turn_on"}),
    );
    assert!(!err && r["done"].as_array().unwrap().len() == 2, "{r}");
    let (r, err) = tool(
        &b,
        "home_control",
        json!({"area": "Garage", "service": "turn_off"}),
    );
    assert!(err && r.to_string().contains("Cucina"), "{r}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_house_command_is_reported_and_can_be_undone() {
    let f = Fake {
        exposed: exposed(&[
            ("light.cucina", "Luce cucina", ""),
            ("cover.soggiorno", "Tapparella soggiorno", "shutter"),
            ("climate.casa", "Termostato", ""),
        ]),
        ..Fake::default()
    };
    let (cfg, dir) = setup("undo", &fake_ha(f.clone()));
    let b = Arc::new(Brain::new(cfg).unwrap());
    let mut screen = Screen::connect(&b, &dir);
    let (r, err) = tool(
        &b,
        "home_control",
        json!({"entity_id": "light.cucina", "service": "turn_off"}),
    );
    assert!(!err, "{r}");
    let c = screen.event("changed");
    assert_eq!(
        (
            c["entity_id"].as_str(),
            c["service"].as_str(),
            c["state"].as_str(),
            c["undo"].as_bool()
        ),
        (
            Some("light.cucina"),
            Some("turn_off"),
            Some("off"),
            Some(true)
        ),
        "{c}"
    );
    b.undo_control(c["id"].as_i64().unwrap()).unwrap();
    let c2 = screen.event("changed");
    assert_eq!(
        (
            c2["service"].as_str(),
            c2["state"].as_str(),
            c2["undo"].as_bool()
        ),
        (Some("turn_on"), Some("on"), Some(false)),
        "{c2}"
    );
    // A snapshot before the command, the snapshot's scene for the undo (brightness, colour…).
    let snap = f
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|c| c["service"] == "create")
        .cloned()
        .unwrap();
    assert_eq!(
        (
            snap["domain"].as_str(),
            snap["service_data"]["snapshot_entities"].clone()
        ),
        (Some("scene"), json!(["light.cucina"])),
        "{snap}"
    );
    assert!(snap["service_data"].get("entity_id").is_none(), "{snap}");
    let scene = format!(
        "scene.{}",
        snap["service_data"]["scene_id"].as_str().unwrap()
    );
    assert_eq!(
        services(&f),
        [
            "scene.create ".to_string(),
            "light.turn_off light.cucina".to_string(),
            format!("scene.turn_on {scene}"),
        ]
    );
    // Once only; an unknown id is an error.
    assert!(b.undo_control(c["id"].as_i64().unwrap()).is_err());
    assert!(b.undo_control(12345).is_err());
    // Toggle undoes itself; a command with no clear inverse has no Undo.
    let (_, err) = tool(
        &b,
        "home_control",
        json!({"entity_id": "light.cucina", "service": "toggle"}),
    );
    assert!(!err);
    assert_eq!(screen.event("changed")["undo"], true);
    let (_, err) = tool(
        &b,
        "home_control",
        json!({"entity_id": "light.cucina", "service": "turn_on", "data": {"brightness_pct": 30}}),
    );
    assert!(!err);
    let c = screen.event("changed");
    assert_eq!(c["undo"], true);
    // A service outside the table is refused; one with no inverse is undone by its snapshot.
    let (_, err) = tool(
        &b,
        "home_control",
        json!({"entity_id": "light.cucina", "service": "set_foo"}),
    );
    assert!(err);
    let (r, err) = tool(
        &b,
        "home_control",
        json!({"entity_id": "climate.casa", "service": "set_temperature", "data": {"temperature": 20}}),
    );
    assert!(!err, "{r}");
    assert_eq!(screen.event("changed")["undo"], true);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_list_of_one_blind_asks_its_yes() {
    let f = Fake {
        exposed: exposed(&[("cover.soggiorno", "Tapparella soggiorno", "shutter")]),
        ..Fake::default()
    };
    let (cfg, dir) = setup("list-one", &fake_ha(f.clone()));
    let b = Arc::new(Brain::new(cfg).unwrap());
    let _screen = Screen::connect(&b, &dir);
    let (r, err) = tool(
        &b,
        "home_control",
        json!({"entity_ids": ["cover.soggiorno"], "service": "set_cover_position", "data": {"position": 0}}),
    );
    assert!(!err && r["asked"].is_object(), "{r}");
    assert!(services(&f).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn show_view_opens_paths_and_exposed_cameras_only() {
    let f = Fake {
        exposed: exposed(&[
            ("camera.front", "Front door", ""),
            ("light.cucina", "Luce cucina", ""),
        ]),
        states: vec![("camera.secret", "Bedroom camera")],
        ..Fake::default()
    };
    let (cfg, dir) = setup("view", &fake_ha(f.clone()));
    let b = Arc::new(Brain::new(cfg).unwrap());
    // No screen connected: an error, like the timer.
    let (r, err) = tool(&b, "show_view", json!({"path": "/lovelace/energy"}));
    assert!(err && r.to_string().contains("not connected"), "{r}");
    let mut screen = Screen::connect(&b, &dir);
    let (_, err) = tool(&b, "show_view", json!({"path": "/lovelace/energy"}));
    assert!(!err);
    assert_eq!(screen.event("show_view")["path"], "/lovelace/energy");
    let (_, err) = tool(&b, "show_view", json!({"camera": "camera.front"}));
    assert!(!err);
    assert_eq!(screen.event("show_view")["camera"], "camera.front");
    // Not exposed, not a camera, bad paths, both or neither.
    for args in [
        json!({"camera": "camera.secret"}),
        json!({"camera": "light.cucina"}),
        json!({"path": "http://evil.example/"}),
        json!({"path": "//evil.example/x"}),
        json!({"path": "/a b"}),
        json!({"path": "/../config"}),
        json!({"path": "/x", "camera": "camera.front"}),
        json!({}),
    ] {
        let (r, err) = tool(&b, "show_view", args.clone());
        assert!(err, "{args}: {r}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn announce_goes_to_every_satellite_or_the_named_ones() {
    let f = Fake {
        states: vec![
            ("assist_satellite.kitchen", "Kitchen"),
            ("assist_satellite.hall", "Hall"),
            ("todo.x", "X"),
        ],
        ..Fake::default()
    };
    let (cfg, dir) = setup("announce", &fake_ha(f.clone()));
    let b = Brain::new(cfg).unwrap();
    let last = |f: &Fake| {
        f.calls
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|c| c["service"] == "announce")
            .cloned()
            .unwrap()
    };
    let (r, err) = tool(&b, "announce", json!({"message": "La cena è pronta"}));
    assert!(!err, "{r}");
    let c = last(&f);
    assert_eq!(c["domain"], "assist_satellite");
    assert_eq!(c["service_data"]["message"], "La cena è pronta");
    assert_eq!(
        c["target"]["entity_id"],
        json!(["assist_satellite.kitchen", "assist_satellite.hall"])
    );
    let (_, err) = tool(
        &b,
        "announce",
        json!({"message": "Ciao", "satellites": ["assist_satellite.hall"]}),
    );
    assert!(!err);
    assert_eq!(
        last(&f)["target"]["entity_id"],
        json!(["assist_satellite.hall"])
    );
    // Unknown satellite, empty message: refused; long text is cut.
    let (_, err) = tool(
        &b,
        "announce",
        json!({"message": "x", "satellites": ["light.cucina"]}),
    );
    assert!(err);
    let (_, err) = tool(&b, "announce", json!({"message": "  "}));
    assert!(err);
    let (_, err) = tool(&b, "announce", json!({"message": "x".repeat(500)}));
    assert!(!err);
    assert!(
        last(&f)["service_data"]["message"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 201
    );
    let _ = std::fs::remove_dir_all(dir);
}
