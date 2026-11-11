// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The Yes/No question of the house commands, row by row: scripted sequences of what Home
//! Assistant's model calls (there is no model in the brain: the script is the model), what the
//! body sends (turns, answers on the screen, by voice, by the stop word) and how time passes (a
//! hand-driven clock: no row sleeps). Every bug found in the field becomes one more row.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use brain::agent::Brain;
use brain::clock::ManualClock;
use brain::config::Config;
use brain::control::Hub;
use common::*;
use serde_json::{Value, json};

/// One row's world: the brain on a manual clock, the fake house, the body's event stream.
struct S {
    b: Arc<Brain>,
    f: Fake,
    clock: Arc<ManualClock>,
    hub: Arc<Hub>,
    ev: Events,
    seen: usize,
    cfg: Config,
    dir: PathBuf,
}

/// The fixture house (generic names only).
fn house() -> Fake {
    Fake {
        exposed: exposed(&[
            ("light.kitchen", "Kitchen light", ""),
            ("light.study_left", "Study left", ""),
            ("light.study_right", "Study right", ""),
            ("light.study", "Study", ""),
            ("cover.living_left", "Living room left blind", "shutter"),
            ("cover.living_right", "Living room right blind", "shutter"),
            ("cover.study", "Study blind", "shutter"),
            ("valve.garden", "Garden water", "water"),
            ("lock.front", "Front door", ""),
            ("cover.garage", "Garage door", "garage"),
        ]),
        areas: vec![
            ("light.kitchen", "Kitchen"),
            ("light.study_left", "Study"),
            ("light.study_right", "Study"),
            ("light.study", "Study"),
            ("cover.living_left", "Living room"),
            ("cover.living_right", "Living room"),
            ("cover.study", "Study"),
        ],
        groups: vec![("light.study", vec!["light.study_left", "light.study_right"])],
        states: vec![("todo.shopping", "Shopping")],
        ..Fake::default()
    }
}

impl S {
    fn new(name: &str) -> S {
        S::with(name, |_| {})
    }

    fn with(name: &str, tweak: impl FnOnce(&mut Config)) -> S {
        let f = house();
        let (mut cfg, dir) = setup(&format!("scenario-{name}"), &fake_ha(f.clone()));
        cfg.lists.shopping = "todo.shopping".into();
        tweak(&mut cfg);
        let clock = Arc::new(ManualClock::new(1_794_150_000_000));
        let b = Arc::new(Brain::with_clock(cfg.clone(), clock.clone()).unwrap());
        let hub = Arc::new(Hub::default());
        b.set_hub(hub.clone());
        let ev = Events::join(&hub);
        S {
            b,
            f,
            clock,
            hub,
            ev,
            seen: 0,
            cfg,
            dir,
        }
    }

    fn now(&self) -> i64 {
        use brain::clock::Clock;
        self.clock.now_ms()
    }

    /// The model calls a tool: its result (an error as `{"error": …}`).
    fn call(&self, name: &str, args: Value) -> Value {
        self.b.mcp_call(name, &args).0
    }

    fn control(&self, req: Value) -> Value {
        brain::control::answer(&self.b, &req).0
    }

    /// Home Assistant's transcript of the body's conversation `id`.
    fn turn(&self, id: u64, text: &str, answers: Option<i64>) -> Value {
        self.control(json!({"cmd": "turn", "id": id, "text": text, "answers": answers}))
    }

    fn turn_end(&self, id: u64, said: &str) -> Value {
        self.control(json!({"cmd": "turn.end", "id": id, "said": said}))
    }

    /// The body answers question `id` (the request, then the brain's clock work).
    fn answer(&self, id: i64, yes: bool, via: &str) -> Value {
        let r = self.control(json!({"cmd": "confirm.answer", "id": id, "yes": yes, "via": via}));
        self.b.poll_questions();
        r
    }

    fn advance(&self, ms: i64) {
        self.clock.advance(ms);
        self.b.poll_questions();
    }

    /// A body that (re)connects: what the brain replays to it.
    fn subscribe(&self) -> Vec<Value> {
        let (_, sub) = brain::control::answer(&self.b, &json!({"cmd": "subscribe"}));
        assert!(sub);
        self.b.replay()
    }

    /// The services called since the last look: "domain.service entity_id [data]".
    fn services(&mut self) -> Vec<String> {
        let calls = self.f.calls.lock().unwrap().clone();
        let out = calls[self.seen..]
            .iter()
            .filter(|c| c["type"] == "call_service")
            .map(|c| {
                let (d, s) = (
                    c["domain"].as_str().unwrap(),
                    c["service"].as_str().unwrap(),
                );
                if d == "scene" && s == "create" {
                    return "scene.create".to_string();
                }
                let mut data = c["service_data"].clone();
                let id = data["entity_id"].as_str().unwrap_or("").to_string();
                data.as_object_mut().unwrap().remove("entity_id");
                if data.as_object().unwrap().is_empty() {
                    format!("{d}.{s} {id}")
                } else {
                    format!("{d}.{s} {id} {data}")
                }
            })
            .collect();
        self.seen = calls.len();
        out
    }

    /// Exactly these services and these events (by name) since the last look; the events back.
    fn expect(&mut self, services: &[&str], events: &[&str]) -> Vec<Value> {
        let got = self.services();
        assert_eq!(got, services, "services");
        let ev = self.ev.drain();
        let names: Vec<&str> = ev.iter().map(|e| e["event"].as_str().unwrap()).collect();
        assert_eq!(names, events, "events: {ev:?}");
        ev
    }

    /// The first question of a call's result.
    fn asked(r: &Value) -> i64 {
        r["asked"]["id"]
            .as_i64()
            .unwrap_or_else(|| panic!("nothing asked: {r}"))
    }
}

impl Drop for S {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn close(entity: &str) -> Value {
    json!({"entity_id": entity, "service": "close_cover"})
}

fn assert_shape(r: &Value) {
    for k in ["done", "refused", "failed"] {
        assert!(r[k].is_array(), "{k} in {r}");
    }
    assert!(r.get("asked").is_some(), "asked in {r}");
}

const SAY_NOTHING: &str =
    "nothing: the question is on the screen and the device listens for the answer";

#[test]
fn b01_close_one_cover_asks() {
    let mut s = S::new("b01");
    let r = s.call(
        "home_control",
        json!({"entity_id": "cover.living_left", "service": "close"}),
    );
    assert_shape(&r);
    assert_eq!(r["say"], SAY_NOTHING, "{r}");
    let q = S::asked(&r);
    let ev = s.expect(&[], &["confirm"]);
    assert_eq!(ev[0]["id"], q);
    assert_eq!(ev[0]["names"], json!(["Living room left blind"]));
    assert_eq!(ev[0]["expires_at"], s.now() / 1000 + 30);
}

#[test]
fn b02_position_zero_keeps_its_service() {
    let mut s = S::new("b02");
    let r = s.call(
        "home_control",
        json!({"entity_ids": ["cover.living_left"], "service": "set_cover_position", "data": {"position": 0}}),
    );
    let q = S::asked(&r);
    let text = r["asked"]["question"].as_str().unwrap();
    assert!(
        text.starts_with("Chiudo") || text.starts_with("Close"),
        "{text}"
    );
    s.expect(&[], &["confirm"]);
    s.answer(q, true, "screen");
    s.expect(
        &[r#"cover.set_cover_position cover.living_left {"position":0}"#],
        &["changed", "confirm_done"],
    );
}

#[test]
fn b03_a_room_with_one_cover_asks() {
    let mut s = S::new("b03");
    let r = s.call(
        "home_control",
        json!({"area": "Study", "domain": "cover", "service": "close_cover"}),
    );
    assert_shape(&r);
    S::asked(&r);
    assert_eq!(r["done"], json!([]));
    s.expect(&[], &["confirm"]);
}

#[test]
fn b04_a_room_with_two_covers_one_question() {
    let mut s = S::new("b04");
    let r = s.call(
        "home_control",
        json!({"area": "Living room", "domain": "cover", "service": "close_cover"}),
    );
    let q = S::asked(&r);
    let ev = s.expect(&[], &["confirm"]);
    assert_eq!(ev[0]["names"].as_array().unwrap().len(), 2);
    s.answer(q, true, "screen");
    let ev = s.expect(
        &[
            "cover.close_cover cover.living_left",
            "cover.close_cover cover.living_right",
        ],
        &["changed", "changed", "confirm_done"],
    );
    assert_eq!(ev[0]["undo"], false);
    assert_eq!(ev[1]["undo"], false);
    assert_eq!(
        ev[2],
        json!({"event": "confirm_done", "id": q, "yes": true, "via": "screen", "failed": []})
    );
}

#[test]
fn b05_a_no_then_the_same_call_in_the_same_turn() {
    let mut s = S::new("b05");
    s.turn(7, "chiudi la tapparella", None);
    let r = s.call("home_control", close("cover.study"));
    let q = S::asked(&r);
    s.expect(&[], &["confirm"]);
    s.answer(q, false, "screen");
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(
        (ev[0]["yes"].clone(), ev[0]["via"].clone()),
        (json!(false), json!("screen"))
    );
    let mut again = close("cover.study");
    again["confirm_id"] = json!(q);
    let r = s.call("home_control", again);
    assert_eq!(r["state"], "answered_no", "{r}");
    assert!(r["asked"].is_null());
    s.expect(&[], &[]);
}

fn after_a_no_a_new_turn_asks_again(name: &str, later_ms: i64) {
    let mut s = S::new(name);
    s.turn(7, "chiudi la tapparella", None);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.answer(q, false, "screen");
    s.expect(&[], &["confirm", "confirm_done"]);
    s.advance(later_ms);
    s.turn(8, "chiudi la tapparella", None);
    let mut again = close("cover.study");
    again["confirm_id"] = json!(q);
    let r = s.call("home_control", again);
    let q2 = S::asked(&r);
    assert_ne!(q2, q);
    let ev = s.expect(&[], &["confirm"]);
    assert_eq!(ev[0]["id"], q2);
}

#[test]
fn b06_after_a_no_a_new_turn_asks_again() {
    after_a_no_a_new_turn_asks_again("b06", 10_000);
}

#[test]
fn b07_after_a_no_minutes_later_asks_again() {
    after_a_no_a_new_turn_asks_again("b07", 300_000);
}

#[test]
fn b08_a_screen_yes_executes_without_pending() {
    let mut s = S::new("b08");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    let r = s.answer(q, true, "screen");
    assert_eq!(r["ok"], true, "{r}");
    let ev = s.expect(
        &["cover.close_cover cover.study"],
        &["changed", "confirm_done"],
    );
    assert_eq!(ev[1]["via"], "screen");
}

#[test]
fn b09_a_voice_yes_waits_a_second() {
    let mut s = S::new("b09");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    let r = s.answer(q, true, "voice_reflex");
    assert_eq!(r["state"], "confirming", "{r}");
    let ev = s.expect(&[], &["confirm_pending"]);
    assert_eq!(ev[0]["until_ms"], s.now() + 1000);
    s.advance(999);
    s.expect(&[], &[]);
    s.advance(1);
    let ev = s.expect(
        &["cover.close_cover cover.study"],
        &["changed", "confirm_done"],
    );
    assert_eq!(
        ev[1],
        json!({"event": "confirm_done", "id": q, "yes": true, "via": "voice_reflex", "failed": []})
    );
}

fn voice_yes_then(name: &str, yes: bool, via: &str) -> S {
    let mut s = S::new(name);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.answer(q, true, "voice_reflex");
    s.expect(&[], &["confirm", "confirm_pending"]);
    s.advance(500);
    s.answer(q, yes, via);
    s
}

#[test]
fn b10_a_screen_no_inside_the_second_wins() {
    let mut s = voice_yes_then("b10", false, "screen");
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(
        (ev[0]["yes"].clone(), ev[0]["via"].clone()),
        (json!(false), json!("screen"))
    );
    s.advance(2000);
    s.expect(&[], &[]);
}

#[test]
fn b11_the_stop_word_inside_the_second_is_a_no() {
    let mut s = voice_yes_then("b11", false, "stop");
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(
        (ev[0]["yes"].clone(), ev[0]["via"].clone()),
        (json!(false), json!("stop"))
    );
    s.advance(2000);
    s.expect(&[], &[]);
}

#[test]
fn b12_a_screen_yes_inside_the_second_executes_once() {
    let mut s = voice_yes_then("b12", true, "screen");
    let ev = s.expect(
        &["cover.close_cover cover.study"],
        &["changed", "confirm_done"],
    );
    assert_eq!(ev[1]["via"], "screen");
    s.advance(2000);
    s.expect(&[], &[]);
}

#[test]
fn b13_a_yes_heard_by_home_assistant() {
    let mut s = S::new("b13");
    s.turn(7, "chiudi la tapparella dello studio", None);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    s.turn(8, "Sì.", Some(q));
    s.expect(&[], &["confirm_pending"]);
    s.advance(1000);
    let ev = s.expect(
        &["cover.close_cover cover.study"],
        &["changed", "confirm_done"],
    );
    assert_eq!(ev[1]["via"], "voice_ha");
}

#[test]
fn b14_a_no_heard_by_home_assistant() {
    let mut s = S::new("b14");
    s.turn(7, "chiudi la tapparella dello studio", None);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    s.turn(8, "No, grazie.", Some(q));
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(
        ev[0],
        json!({"event": "confirm_done", "id": q, "yes": false, "via": "voice_ha", "failed": []})
    );
}

#[test]
fn b15_a_yes_in_a_turn_that_answers_nothing() {
    let mut s = S::new("b15");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    s.turn(8, "sì", None);
    s.advance(5000);
    s.expect(&[], &[]);
    // Still open: it expires.
    s.advance(25_001);
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(
        (ev[0]["id"].as_i64(), ev[0]["via"].as_str()),
        (Some(q), Some("timeout"))
    );
}

#[test]
fn b16_sentences_that_are_not_an_answer() {
    let mut s = S::new("b16");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    for (i, t) in [
        "Si può sapere che ore sono?",
        "sì però aspetta",
        "no anzi sì",
    ]
    .iter()
    .enumerate()
    {
        s.turn(8 + i as u64, t, Some(q));
        s.advance(1000);
        s.expect(&[], &[]);
    }
    s.advance(30_001);
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(ev[0]["via"], "timeout");
}

#[test]
fn b17_the_model_calls_again_after_the_yes() {
    let mut s = S::new("b17");
    s.turn(7, "chiudi la tapparella dello studio", None);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.turn(8, "sì", Some(q));
    s.expect(&[], &["confirm", "confirm_pending"]);
    let r = s.call("home_control", close("cover.study"));
    assert_eq!(r["state"], "confirming", "{r}");
    assert!(r["asked"].is_null());
    s.expect(&[], &[]);
    s.advance(1000);
    s.expect(
        &["cover.close_cover cover.study"],
        &["changed", "confirm_done"],
    );
    let r = s.call("home_control", close("cover.study"));
    assert_eq!(r["state"], "done", "{r}");
    let mut wrong = close("cover.study");
    wrong["confirm_id"] = json!(q + 1000);
    let r = s.call("home_control", wrong);
    assert_eq!(r["state"], "done", "{r}");
    s.advance(2000);
    s.expect(&[], &[]);
}

#[test]
fn b18_double_answers() {
    let mut s = S::new("b18");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.answer(q, true, "screen");
    let r = s.answer(q, true, "screen");
    assert_eq!(
        (r["ok"].clone(), r["state"].clone()),
        (json!(true), json!("yes"))
    );
    s.expect(
        &["cover.close_cover cover.study"],
        &["confirm", "changed", "confirm_done"],
    );
    let q = S::asked(&s.call("home_control", close("cover.living_left")));
    s.answer(q, true, "screen");
    let r = s.answer(q, false, "screen");
    assert_eq!(
        (r["ok"].clone(), r["state"].clone()),
        (json!(true), json!("yes"))
    );
    s.expect(
        &["cover.close_cover cover.living_left"],
        &["confirm", "changed", "confirm_done"],
    );
}

#[test]
fn b19a_lights_at_once_the_lock_never() {
    let mut s = S::new("b19a");
    let r = s.call(
        "home_control",
        json!({"entity_ids": ["light.kitchen", "light.study_left", "lock.front"], "service": "turn_off"}),
    );
    assert_shape(&r);
    assert_eq!(r["done"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["refused"], json!(["Front door"]));
    assert!(r["note"].as_str().unwrap().contains("never"), "{r}");
    assert!(r["asked"].is_null());
    let ev = s.expect(
        &[
            "scene.create",
            "light.turn_off light.kitchen",
            "scene.create",
            "light.turn_off light.study_left",
        ],
        &["changed", "changed"],
    );
    assert!(ev.iter().all(|e| e["undo"] == true));
}

#[test]
fn b19b_a_blind_asks_the_garage_never() {
    let mut s = S::new("b19b");
    let r = s.call(
        "home_control",
        json!({"entity_ids": ["cover.study", "cover.garage"], "service": "close_cover"}),
    );
    assert_shape(&r);
    S::asked(&r);
    assert_eq!(r["refused"], json!(["Garage door"]));
    let ev = s.expect(&[], &["confirm"]);
    assert_eq!(ev[0]["names"], json!(["Study blind"]));
}

#[test]
fn b20_groups_made_up_ids_and_areas() {
    let mut s = S::new("b20");
    let r = s.call(
        "home_control",
        json!({"entity_ids": ["light.study", "light.study_left", "light.study_right"], "service": "turn_on"}),
    );
    assert_eq!(r["done"].as_array().unwrap().len(), 2, "{r}");
    s.expect(
        &[
            "scene.create",
            "light.turn_on light.study_left",
            "scene.create",
            "light.turn_on light.study_right",
        ],
        &["changed", "changed"],
    );
    let r = s.call(
        "home_control",
        json!({"entity_id": "light.kitchen_light", "service": "turn_on"}),
    );
    assert_eq!(r["done"], json!(["Kitchen light"]), "{r}");
    s.expect(
        &["scene.create", "light.turn_on light.kitchen"],
        &["changed"],
    );
    let r = s.call(
        "home_control",
        json!({"area": "Attic", "service": "turn_off"}),
    );
    assert!(r["error"].as_str().unwrap().contains("Kitchen"), "{r}");
    s.expect(&[], &[]);
}

#[test]
fn b21_services_and_data_outside_the_table() {
    let mut s = S::new("b21");
    for service in ["toggle", "set_cover_tilt_position"] {
        let r = s.call(
            "home_control",
            json!({"entity_id": "cover.study", "service": service}),
        );
        let e = r["error"].as_str().unwrap_or_else(|| panic!("{r}"));
        assert!(e.contains("close_cover") && e.contains("open_cover"), "{e}");
    }
    let r = s.call(
        "home_control",
        json!({"entity_id": "cover.study", "service": "close_cover", "data": {"brightness_pct": 10}}),
    );
    assert!(r["error"].as_str().unwrap().contains("position"), "{r}");
    let r = s.call(
        "home_control",
        json!({"entity_id": "cover.study", "service": "set_cover_position", "data": {"position": 150}}),
    );
    assert!(r["error"].is_string(), "{r}");
    s.expect(&[], &[]);
    let r = s.call(
        "home_control",
        json!({"entity_id": "cover.study", "service": "close_cover", "data": {"entity_id": "cover.garage"}}),
    );
    let q = S::asked(&r);
    s.answer(q, true, "screen");
    s.expect(
        &["cover.close_cover cover.study"],
        &["confirm", "changed", "confirm_done"],
    );
}

fn changed_between_question_and_yes(name: &str, change: impl FnOnce(&S)) {
    let mut s = S::new(name);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    change(&s);
    s.answer(q, true, "screen");
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(
        ev[0],
        json!({"event": "confirm_done", "id": q, "yes": true, "via": "screen", "failed": ["Study blind"]})
    );
}

#[test]
fn b22a_the_gate_becomes_never() {
    changed_between_question_and_yes("b22a", |s| {
        s.b.set_gate("cover.study", Some("never")).unwrap()
    });
}

#[test]
fn b22b_no_longer_exposed() {
    changed_between_question_and_yes("b22b", |s| s.f.unexpose("cover.study"));
}

#[test]
fn b22c_home_assistant_down() {
    changed_between_question_and_yes("b22c", |s| s.f.set_down(true));
}

#[test]
fn b23_one_of_two_fails() {
    let mut s = S::new("b23");
    s.f.fail("cover.living_right");
    let q = S::asked(&s.call(
        "home_control",
        json!({"area": "Living room", "domain": "cover", "service": "close_cover"}),
    ));
    s.answer(q, true, "screen");
    let ev = s.expect(
        &[
            "cover.close_cover cover.living_left",
            "cover.close_cover cover.living_right",
        ],
        &["confirm", "changed", "confirm_done"],
    );
    assert_eq!(ev[2]["failed"], json!(["Living room right blind"]));
    assert_eq!(ev[2]["yes"], true);
}

#[test]
fn b24_a_second_question_while_one_is_open() {
    let mut s = S::new("b24");
    s.turn(7, "chiudi la tapparella", None);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    let first = s.expect(&[], &["confirm"])[0].clone();
    s.advance(5000);
    let r = s.call("home_control", close("cover.study"));
    assert_eq!(r["state"], "waiting", "{r}");
    assert_eq!(r["say"], SAY_NOTHING);
    s.expect(&[], &[]);
    let replay = s.subscribe();
    let c = replay.iter().find(|e| e["event"] == "confirm").unwrap();
    assert_eq!(
        (c["id"].clone(), c["expires_at"].clone()),
        (json!(q), first["expires_at"].clone())
    );
    let r = s.call("home_control", close("cover.living_left"));
    let q2 = S::asked(&r);
    let ev = s.expect(&[], &["confirm_done", "confirm"]);
    assert_eq!(
        ev[0],
        json!({"event": "confirm_done", "id": q, "yes": null, "via": "replaced", "failed": []})
    );
    assert_eq!(ev[1]["id"], q2);
}

#[test]
fn b25_too_late() {
    let mut s = S::new("b25");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    s.advance(30_001);
    let ev = s.expect(&[], &["confirm_done"]);
    assert_eq!(
        ev[0],
        json!({"event": "confirm_done", "id": q, "yes": null, "via": "timeout", "failed": []})
    );
    let r = s.answer(q, true, "screen");
    assert_eq!(
        (r["ok"].clone(), r["state"].clone()),
        (json!(true), json!("expired"))
    );
    s.turn(9, "sì", Some(q));
    s.advance(2000);
    s.expect(&[], &[]);
}

#[test]
fn b26_a_body_that_reconnects_sees_the_question() {
    let mut s = S::new("b26");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    let first = s.expect(&[], &["confirm"])[0].clone();
    let names = |v: &[Value]| -> Vec<String> {
        v.iter()
            .filter(|e| e["event"].as_str().unwrap().starts_with("confirm"))
            .map(|e| e["event"].as_str().unwrap().to_string())
            .collect()
    };
    let replay = s.subscribe();
    assert_eq!(names(&replay), ["confirm"]);
    assert_eq!(
        replay.iter().find(|e| e["event"] == "confirm").unwrap(),
        &first
    );
    s.answer(q, true, "voice_reflex");
    let replay = s.subscribe();
    assert_eq!(names(&replay), ["confirm", "confirm_pending"]);
    s.advance(1000);
    let replay = s.subscribe();
    assert!(names(&replay).is_empty(), "{replay:?}");
}

#[test]
fn b27_no_undo_after_a_yes() {
    let mut s = S::new("b27");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.answer(q, true, "screen");
    let ev = s.expect(
        &["cover.close_cover cover.study"],
        &["confirm", "changed", "confirm_done"],
    );
    assert_eq!(ev[1]["undo"], false);
    let r = s.control(json!({"cmd": "control.undo", "id": ev[1]["id"]}));
    assert_eq!(r["ok"], false, "{r}");
    s.expect(&[], &[]);
}

#[test]
fn b28_the_answering_turn_after_a_no() {
    let mut s = S::new("b28");
    s.turn(7, "chiudi la tapparella", None);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.turn(8, "no", Some(q));
    s.expect(&[], &["confirm", "confirm_done"]);
    let r = s.call("home_control", close("cover.study"));
    assert_eq!(r["state"], "answered_no", "{r}");
    s.expect(&[], &[]);
}

#[test]
fn b29_legacy_heard_and_said() {
    let mut s = S::with("b29", |c| c.memory.session_gap_minutes = 0);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    s.control(json!({"cmd": "heard", "text": "sì"}));
    s.advance(2000);
    s.expect(&[], &[]);
    let _ = q;
    // heard + said: an exchange the memory job reads.
    s.f.ai.lock().unwrap().push(
        "{\"summary\": \"s\", \"ops\": [{\"op\": \"add\", \"text\": \"La caldaia è in cantina\"}]}"
            .into(),
    );
    s.control(json!({"cmd": "heard", "text": "la caldaia è in cantina"}));
    s.control(json!({"cmd": "said", "text": "Me lo ricordo."}));
    s.b.close_sessions();
    assert!(
        s.f.asked.lock().unwrap()[0].contains("la caldaia è in cantina"),
        "{:?}",
        s.f.asked
    );
    // The writing tools only when the turn asked for them.
    s.turn(20, "Quanti gradi ci sono a Roma?", None);
    let r = s.call("list_add", json!({"items": ["Richiedi la temperatura"]}));
    assert!(r["error"].as_str().unwrap().contains("did not ask"), "{r}");
    s.turn(21, "aggiungi il latte alla spesa", None);
    let r = s.call("list_add", json!({"items": ["latte"]}));
    assert!(r.get("error").is_none(), "{r}");
}

#[test]
fn b30_a_restarted_brain_knows_no_old_question() {
    let mut s = S::new("b30");
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.expect(&[], &["confirm"]);
    let b2 = Brain::with_clock(s.cfg.clone(), s.clock.clone()).unwrap();
    let (r, _) = brain::control::answer(
        &b2,
        &json!({"cmd": "confirm.answer", "id": q, "yes": true, "via": "screen"}),
    );
    assert_eq!(r["error"], "no such question", "{r}");
    assert!(b2.replay().iter().all(|e| e["event"] != "confirm"));
}

#[test]
fn b31_a_new_call_after_the_turn_ended() {
    let mut s = S::new("b31");
    s.turn(7, "chiudi la tapparella", None);
    let q = S::asked(&s.call("home_control", close("cover.study")));
    s.turn_end(7, "");
    let q2 = S::asked(&s.call("home_control", close("cover.study")));
    assert_ne!(q, q2);
    let ev = s.expect(&[], &["confirm", "confirm_done", "confirm"]);
    assert_eq!(
        (ev[1]["id"].as_i64(), ev[1]["via"].as_str()),
        (Some(q), Some("replaced"))
    );
    assert_eq!(ev[2]["id"], q2);
}

#[test]
fn b32_a_light_at_once_with_undo() {
    let mut s = S::new("b32");
    let r = s.call(
        "home_control",
        json!({"entity_id": "light.kitchen", "service": "turn_off"}),
    );
    assert_shape(&r);
    assert_eq!(r["done"], json!(["Kitchen light"]), "{r}");
    let ev = s.expect(
        &["scene.create", "light.turn_off light.kitchen"],
        &["changed"],
    );
    assert_eq!(ev[0]["undo"], true);
    let r = s.control(json!({"cmd": "control.undo", "id": ev[0]["id"]}));
    assert_eq!(r["ok"], true, "{r}");
    let got = s.services();
    assert_eq!(got.len(), 1);
    assert!(got[0].starts_with("scene.turn_on scene."), "{got:?}");
    let _ = &s.hub;
}
