// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

use super::*;
use serde_json::json;

fn tile(kind: TileKind, entity: &str, x: u8, y: u8, w: u8, h: u8) -> Tile {
    Tile {
        x,
        y,
        w,
        h,
        kind,
        entity_id: entity.into(),
        name: String::new(),
        icon: String::new(),
        confirm: None,
        service: None,
        service_data: None,
    }
}

fn page(tiles: Vec<Tile>) -> Canvas {
    Canvas {
        id: "p1".into(),
        name: "Living room".into(),
        tiles,
    }
}

fn board(pages: Vec<Canvas>, widgets: Vec<Canvas>) -> Board {
    Board {
        version: VERSION,
        pages,
        widgets,
    }
}

fn entity(id: &str, name: &str, class: &str) -> Entity {
    Entity {
        id: id.into(),
        name: name.into(),
        device_class: class.into(),
        ..Default::default()
    }
}

fn st(state: &str) -> State {
    State {
        state: state.into(),
        ..Default::default()
    }
}

fn states(v: &[(&str, State)]) -> States {
    States {
        by_id: v.iter().map(|(k, s)| (k.to_string(), s.clone())).collect(),
    }
}

fn locale(code: &str) -> locale::Locale {
    locale::load(&locale::repo_locales(), code).unwrap()
}

fn words() -> BoardWords {
    BoardWords::from_locale(&locale("en"))
}

#[test]
fn the_file_reads_as_the_plan_writes_it_and_back() {
    let text = r#"{ "version": 1,
      "pages":   [ { "id": "p1", "name": "Living room", "tiles": [
          {"x": 0, "y": 0, "w": 2, "h": 1, "kind": "button", "entity_id": "light.living_room"},
          {"x": 2, "y": 0, "w": 2, "h": 1, "kind": "level", "entity_id": "cover.living_room",
           "confirm": false, "icon": "blinds"} ] } ],
      "widgets": [ { "id": "custom", "name": "Lights", "tiles": [
          {"x": 0, "y": 0, "w": 1, "h": 1, "kind": "button", "entity_id": "script.good_night",
           "service": "script.turn_off", "service_data": {"x": 1}, "name": "Night"} ] } ] }"#;
    let (b, why) = parse(text);
    assert!(why.is_empty(), "{why:?}");
    assert_eq!(b.pages[0].tiles.len(), 2);
    assert_eq!(b.pages[0].tiles[1].confirm, Some(false));
    assert_eq!(
        b.widgets[0].tiles[0].service.as_deref(),
        Some("script.turn_off")
    );
    let (b2, why) = parse(&to_json(&b));
    assert!(why.is_empty());
    assert_eq!(b, b2);
    // Empty fields are not written.
    let v: Value = serde_json::from_str(&to_json(&b)).unwrap();
    assert!(v["pages"][0]["tiles"][0].get("name").is_none());
    assert!(v["pages"][0]["tiles"][0].get("confirm").is_none());
}

#[test]
fn a_bad_tile_goes_never_the_file() {
    let text = r#"{"version": 2, "pages": [ {"id": "p1", "tiles": [
        {"x": 0, "y": 0, "w": 1, "h": 1, "kind": "dial", "entity_id": "light.a"},
        {"x": -1, "y": 0, "w": 1, "h": 1, "kind": "button", "entity_id": "light.b"},
        {"x": 1, "y": 0, "w": 1, "h": 1, "kind": "button", "entity_id": "light.c"} ]},
        "not a canvas" ]}"#;
    let (b, why) = parse(text);
    assert_eq!(b.pages.len(), 1);
    assert_eq!(b.pages[0].tiles.len(), 1);
    assert_eq!(b.pages[0].tiles[0].entity_id, "light.c");
    assert_eq!(why.len(), 4, "{why:?}");
    let (b, why) = parse("{ broken");
    assert_eq!(b, Board::default());
    assert_eq!(why.len(), 1);
}

#[test]
fn validation_keeps_what_fits_and_says_why_the_rest_goes() {
    use TileKind::*;
    let tiles = vec![
        tile(Button, "light.living_room", 0, 0, 2, 2),
        tile(Button, "switch.kettle", 1, 1, 1, 1), // over the light
        tile(Level, "light.desk", 0, 2, 1, 1),     // 1x1 is not a level's size
        tile(Level, "light.desk", 0, 2, 4, 1),
        tile(Sensor, "light.desk", 2, 0, 1, 1), // a light is not a sensor
        tile(Button, "sensor.outdoor_temperature", 3, 0, 1, 1), // nor a sensor a button
        tile(Sensor, "sensor.outdoor_temperature", 2, 0, 2, 2),
        tile(Button, "lock.front", 0, 3, 1, 1),
        tile(Button, "alarm_control_panel.house", 1, 3, 1, 1),
        tile(Button, "cover.garage", 2, 3, 1, 1), // garage class (catalogue)
        tile(Button, "switch.relay_2", 3, 3, 1, 1), // named "Garage door"
        tile(Button, "scene.movie_night", 2, 3, 2, 1), // 2,3 + 2 = 4: fits
        tile(Button, "fan.ceiling", 3, 3, 2, 1),  // does not fit
    ];
    let catalog = [
        entity("cover.garage", "Garage", "garage"),
        entity("switch.relay_2", "Garage door", ""),
    ];
    let (b, why) = validate(board(vec![page(tiles)], vec![]), &catalog);
    let kept: Vec<(&str, u8, u8)> = b.pages[0]
        .tiles
        .iter()
        .map(|t| (t.entity_id.as_str(), t.x, t.y))
        .collect();
    assert_eq!(
        kept,
        [
            ("light.living_room", 0, 0),
            ("light.desk", 0, 2),
            ("sensor.outdoor_temperature", 2, 0),
            ("scene.movie_night", 2, 3)
        ]
    );
    assert_eq!(why.len(), 9, "{why:#?}");
    assert!(why.iter().any(|w| w.contains("over another tile")));
    assert!(
        why.iter()
            .any(|w| w.contains("cover.garage is never controlled"))
    );
    assert!(
        why.iter()
            .any(|w| w.contains("switch.relay_2 is never controlled"))
    );
    assert!(why.iter().any(|w| w.contains("does not fit")));
}

#[test]
fn a_widget_is_four_by_two() {
    use TileKind::*;
    let w = Canvas {
        id: "custom".into(),
        name: String::new(),
        tiles: vec![
            tile(Sensor, "sensor.a", 0, 0, 2, 2),
            tile(Button, "light.b", 2, 1, 2, 1),
            tile(Button, "light.c", 2, 1, 1, 2), // 1x2 is no size
            tile(Button, "light.d", 3, 0, 4, 1), // out
        ],
    };
    let (b, why) = validate(board(vec![], vec![w]), &[]);
    assert_eq!(b.widgets[0].tiles.len(), 2);
    assert_eq!(why.len(), 2);
}

#[test]
fn limits_of_pages_widgets_tiles_and_entities() {
    use TileKind::*;
    // Seven pages of sixteen 1x1 tiles, each a new entity: six pages kept, 64 entities.
    let pages: Vec<Canvas> = (0..7)
        .map(|p| Canvas {
            id: format!("p{p}"),
            name: String::new(),
            tiles: (0..16)
                .map(|i| tile(Button, &format!("light.l{p}_{i}"), i % 4, i / 4, 1, 1))
                .collect(),
        })
        .collect();
    let widgets: Vec<Canvas> = (0..5)
        .map(|_| Canvas {
            id: "same".into(),
            name: String::new(),
            tiles: vec![tile(Button, "light.l0_0", 0, 0, 1, 1)],
        })
        .collect();
    let (b, why) = validate(board(pages, widgets), &[]);
    assert_eq!(b.pages.len(), MAX_PAGES);
    assert_eq!(b.widgets.len(), MAX_WIDGETS);
    assert_eq!(b.entities().len(), MAX_ENTITIES);
    let kept: usize = b.pages.iter().map(|p| p.tiles.len()).sum();
    assert_eq!(kept, MAX_ENTITIES, "the 65th entity and later go");
    // An entity already followed is no new one: the widgets keep theirs.
    assert!(b.widgets.iter().all(|w| w.tiles.len() == 1));
    // Ids of their own.
    let ids: HashSet<&str> = b.widgets.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids.len(), MAX_WIDGETS);
    assert!(why.iter().any(|w| w.contains("7 pages")));
    assert!(why.iter().any(|w| w.contains("5 widgets")));
    // Seventeen on one page: the last goes.
    let mut many: Vec<Tile> = (0..16)
        .map(|i| tile(Button, &format!("light.x{i}"), i % 4, i / 4, 1, 1))
        .collect();
    many.push(tile(Button, "light.y", 0, 0, 1, 1));
    let (b, why) = validate(board(vec![page(many)], vec![]), &[]);
    assert_eq!(b.pages[0].tiles.len(), 16);
    assert!(why[0].contains("more than 16 tiles"));
}

#[test]
fn advanced_services_icons_and_names_are_checked() {
    use TileKind::*;
    let mut a = tile(Button, "script.good_night", 0, 0, 1, 1);
    a.service = Some("script.turn_off".into());
    a.service_data = Some(json!({"variables": {"x": 1}}));
    let mut lock = tile(Button, "light.b", 1, 0, 1, 1);
    lock.service = Some("lock.unlock".into());
    let mut sneaky = tile(Button, "light.c", 2, 0, 1, 1);
    sneaky.service = Some("homeassistant.turn_on".into());
    sneaky.service_data = Some(json!({"entity_id": ["light.c", "lock.front"]}));
    let mut odd = tile(Button, "light.d", 3, 0, 1, 1);
    odd.service = Some("not a service".into());
    let mut on_sensor = tile(Sensor, "sensor.t", 0, 1, 1, 1);
    on_sensor.service = Some("light.turn_on".into());
    on_sensor.icon = "Thermo meter!".into();
    on_sensor.name = "  A\u{202e} name\n ".into();
    let (b, why) = validate(
        board(vec![page(vec![a, lock, sneaky, odd, on_sensor])], vec![]),
        &[],
    );
    let t = &b.pages[0].tiles;
    assert_eq!(t.len(), 2, "{why:#?}");
    assert_eq!(t[0].service.as_deref(), Some("script.turn_off"));
    assert_eq!(t[1].service, None, "a service only on a button");
    assert_eq!(t[1].icon, "");
    assert_eq!(t[1].name, "A name");
    assert_eq!(why.len(), 5, "{why:#?}");
    assert_eq!(
        b.advanced(),
        [(
            "script.good_night".to_string(),
            "script.turn_off".to_string()
        )]
    );
}

#[test]
fn advanced_data_never_names_a_target() {
    let mut t = tile(TileKind::Button, "script.good_night", 0, 0, 1, 1);
    t.service = Some("script.turn_on".into());
    t.service_data = Some(json!({
        "entity_id": "light.other", "device_id": "d", "area_id": "a", "floor_id": "f",
        "label_id": "l", "target": {"entity_id": "light.other"}, "variables": {"x": 1}
    }));
    let (b, why) = validate(board(vec![page(vec![t.clone()])], vec![]), &[]);
    let kept = &b.pages[0].tiles[0];
    assert_eq!(kept.service_data, Some(json!({"variables": {"x": 1}})));
    assert_eq!(why.len(), 1, "{why:#?}");
    assert!(why[0].contains("names a target"));
    let on = st("on");
    assert_eq!(
        action_for(&t, Some(&on)),
        None,
        "unvalidated: no call at all"
    );
    let call = action_for(kept, Some(&on)).unwrap();
    assert_eq!(call.entity_id, "script.good_night");
    assert!(!data_targets(&call.data));
}

#[test]
fn what_a_tap_does_by_domain() {
    use TileKind::*;
    let on = st("on");
    let call = |kind, id: &str| {
        action_for(&tile(kind, id, 0, 0, 2, 1), Some(&on))
            .map(|c| format!("{}.{}", c.domain, c.service))
    };
    for (id, want) in [
        ("light.a", "homeassistant.toggle"),
        ("switch.a", "homeassistant.toggle"),
        ("fan.a", "homeassistant.toggle"),
        ("input_boolean.a", "homeassistant.toggle"),
        ("automation.a", "homeassistant.toggle"),
        ("scene.a", "scene.turn_on"),
        ("script.a", "script.turn_on"),
        ("button.a", "button.press"),
        ("input_button.a", "input_button.press"),
        ("cover.a", "cover.toggle"),
        ("valve.a", "valve.toggle"),
        ("media_player.a", "media_player.media_play_pause"),
    ] {
        assert_eq!(call(Button, id).as_deref(), Some(want), "{id}");
        let (d, s) = want.split_once('.').unwrap();
        assert!(allowed(d, s), "{want} is in the closed list");
    }
    assert_eq!(call(Sensor, "sensor.a"), None);
    assert_eq!(
        call(Level, "input_number.a"),
        None,
        "a number has no on/off"
    );
    assert_eq!(
        call(Level, "light.a").as_deref(),
        Some("homeassistant.toggle")
    );
    let t = tile(Button, "light.a", 0, 0, 1, 1);
    assert_eq!(action_for(&t, None), None, "not yet known");
    assert_eq!(action_for(&t, Some(&st("unavailable"))), None);
    let c = action_for(&t, Some(&on)).unwrap();
    assert_eq!(c.entity_id, "light.a");
    assert_eq!(c.data, json!({}));
    // A garage door that slipped in: nothing, by the class Home Assistant gives.
    let garage = State {
        device_class: "garage".into(),
        ..st("closed")
    };
    assert_eq!(
        action_for(&tile(Button, "cover.x", 0, 0, 1, 1), Some(&garage)),
        None
    );
    let mut adv = tile(Button, "script.a", 0, 0, 1, 1);
    adv.service = Some("script.turn_off".into());
    adv.service_data = Some(json!({"a": 1}));
    let c = action_for(&adv, Some(&st("off"))).unwrap();
    assert_eq!(
        (c.domain.as_str(), c.service.as_str()),
        ("script", "turn_off")
    );
    assert_eq!(c.data, json!({"a": 1}));
    assert!(!allowed("script", "turn_off"));
    assert!(!allowed("lock", "unlock"));
    match c.into_cmd(7) {
        crate::ha::hub::HubCmd::Call { id, entity_id, .. } => {
            assert_eq!((id, entity_id.as_str()), (7, "script.a"))
        }
        m => panic!("{m:?}"),
    }
}

#[test]
fn levels_by_domain() {
    use TileKind::*;
    let lvl = |id: &str, s: &State, p: u8| {
        set_level(&tile(Level, id, 0, 0, 2, 1), Some(s), p)
            .map(|c| (format!("{}.{}", c.domain, c.service), c.data))
    };
    let on = st("on");
    assert_eq!(
        lvl("light.a", &on, 40),
        Some(("light.turn_on".into(), json!({"brightness_pct": 40})))
    );
    assert_eq!(
        lvl("media_player.a", &st("playing"), 150),
        Some((
            "media_player.volume_set".into(),
            json!({"volume_level": 1.0})
        ))
    );
    assert_eq!(
        lvl("cover.a", &st("open"), 30),
        Some(("cover.set_cover_position".into(), json!({"position": 30})))
    );
    assert_eq!(
        lvl("fan.a", &on, 66),
        Some(("fan.set_percentage".into(), json!({"percentage": 66})))
    );
    let n = State {
        min: Some(5.0),
        max: Some(30.0),
        step: Some(0.5),
        ..st("21.0")
    };
    assert_eq!(
        lvl("input_number.a", &n, 33),
        Some(("input_number.set_value".into(), json!({"value": 13.5})))
    );
    assert_eq!(
        lvl("number.a", &n, 100),
        Some(("number.set_value".into(), json!({"value": 30.0})))
    );
    let bad = State {
        min: Some(5.0),
        max: Some(5.0),
        ..st("5")
    };
    assert_eq!(lvl("number.a", &bad, 50), None);
    assert_eq!(lvl("light.a", &st("unavailable"), 50), None);
    assert_eq!(
        set_level(&tile(Button, "light.a", 0, 0, 2, 1), Some(&on), 50),
        None,
        "only a level tile"
    );
    for (s, _) in [
        lvl("light.a", &on, 1).unwrap(),
        lvl("media_player.a", &on, 1).unwrap(),
        lvl("cover.a", &on, 1).unwrap(),
        lvl("fan.a", &on, 1).unwrap(),
        lvl("number.a", &n, 1).unwrap(),
        lvl("input_number.a", &n, 1).unwrap(),
    ] {
        let (d, v) = s.split_once('.').unwrap();
        assert!(allowed(d, v), "{s}");
    }
    // And back: where the bar stands.
    let light = State {
        brightness: Some(127.5),
        ..st("on")
    };
    assert_eq!(level_of("light", &light), Some(50));
    assert_eq!(level_of("light", &st("off")), Some(0));
    assert_eq!(level_of("input_number", &n), Some(64));
    assert_eq!(level_of("cover", &st("open")), None);
}

#[test]
fn on_states_and_confirmations() {
    for (d, s, on) in [
        ("light", "on", true),
        ("light", "off", false),
        ("script", "on", true),
        ("cover", "opening", true),
        ("cover", "closed", false),
        ("valve", "open", true),
        ("media_player", "playing", true),
        ("media_player", "paused", false),
        ("scene", "2026-11-20T10:00:00+00:00", false),
        ("button", "on", false),
    ] {
        assert_eq!(is_on(d, s), on, "{d} {s}");
    }
    let mut c = tile(TileKind::Button, "cover.living_room", 0, 0, 1, 1);
    assert!(needs_confirm(&c, "shutter"));
    c.confirm = Some(false);
    assert!(!needs_confirm(&c, "shutter"));
    assert!(needs_confirm(
        &tile(TileKind::Button, "valve.water", 0, 0, 1, 1),
        ""
    ));
    let mut l = tile(TileKind::Button, "light.a", 0, 0, 1, 1);
    assert!(!needs_confirm(&l, ""));
    l.confirm = Some(true);
    assert!(needs_confirm(&l, ""));
}

#[test]
fn icons_and_labels_by_domain_and_class() {
    assert_eq!(default_icon("light", ""), "lightbulb");
    assert_eq!(default_icon("cover", "shutter"), "window-shutter");
    assert_eq!(default_icon("cover", "blind"), "blinds");
    assert_eq!(default_icon("sensor", "temperature"), "thermometer");
    assert_eq!(default_icon("binary_sensor", "window"), "window-closed");
    assert_eq!(default_icon("vacuum", ""), "help-circle-outline");
    // Every label is a key of the English words, and Italian has it too.
    let (en, it) = (locale("en"), locale("it"));
    for (d, c) in [
        ("light", ""),
        ("switch", ""),
        ("fan", ""),
        ("cover", "shutter"),
        ("valve", ""),
        ("media_player", ""),
        ("scene", ""),
        ("script", ""),
        ("automation", ""),
        ("button", ""),
        ("sensor", "temperature"),
        ("sensor", "humidity"),
        ("sensor", "power"),
        ("sensor", "energy"),
        ("sensor", "battery"),
        ("sensor", "pressure"),
        ("binary_sensor", "door"),
        ("binary_sensor", "window"),
        ("binary_sensor", "motion"),
        ("number", ""),
    ] {
        let k = default_label(d, c);
        assert_ne!(en.text(k), k, "{k}");
        assert!(!it.missing.contains(&format!("text.{k}")), "{k}");
    }
}

#[test]
fn state_words_in_the_screen_language() {
    let it = BoardWords::from_locale(&locale("it"));
    assert_eq!(it.on, "Acceso");
    assert_eq!(it.decimal, ',');
    // A language without the board's words yet: English, key by key.
    let de = BoardWords::from_locale(&locale("de"));
    assert_eq!(de.on, "On");
    let w = words();
    assert_eq!(state_words("light", "", "on", &w), "On");
    assert_eq!(state_words("cover", "", "closing", &w), "Closing");
    assert_eq!(state_words("binary_sensor", "window", "on", &w), "Open");
    assert_eq!(state_words("binary_sensor", "motion", "off", &w), "Clear");
    assert_eq!(state_words("binary_sensor", "", "on", &w), "On");
    assert_eq!(state_words("media_player", "", "paused", &w), "Paused");
    assert_eq!(state_words("script", "", "on", &w), "Running");
    assert_eq!(
        state_words("scene", "", "2026-11-20T10:00:00+00:00", &w),
        ""
    );
    assert_eq!(state_words("light", "", "unavailable", &w), "Unavailable");
    assert_eq!(state_words("climate", "", "heat_cool", &w), "heat_cool");
}

#[test]
fn the_screen_model_of_a_page() {
    use TileKind::*;
    let mut wide = tile(Sensor, "sensor.outdoor_temperature", 0, 0, 2, 1);
    wide.name = String::new();
    let small = tile(Sensor, "sensor.power", 2, 0, 1, 1);
    let lamp = tile(Button, "light.living_room", 3, 0, 1, 1);
    let mut bar = tile(Level, "light.desk", 0, 1, 4, 1);
    bar.icon = "desk-lamp".into();
    let gone = tile(Button, "switch.away", 0, 2, 1, 1);
    let shutter = tile(Button, "cover.living_room", 1, 2, 2, 1);
    let canvas = page(vec![wide, small, lamp, bar, gone, shutter]);
    let s = states(&[
        (
            "sensor.outdoor_temperature",
            State {
                name: "Outdoor temperature".into(),
                unit: "°C".into(),
                device_class: "temperature".into(),
                ..st("1234.5")
            },
        ),
        (
            "sensor.power",
            State {
                unit: "W".into(),
                ..st("80")
            },
        ),
        (
            "light.living_room",
            State {
                name: "Lamp".into(),
                ..st("on")
            },
        ),
        (
            "light.desk",
            State {
                brightness: Some(255.0),
                ..st("on")
            },
        ),
        ("switch.away", st("unavailable")),
        (
            "cover.living_room",
            State {
                device_class: "shutter".into(),
                ..st("closed")
            },
        ),
    ]);
    let curve = Curve {
        points: vec![1.0, 2.0],
        first: 1.0,
        low: 1.0,
        high: 2.0,
    };
    let curves: HashMap<String, Curve> = [
        ("sensor.outdoor_temperature".to_string(), curve.clone()),
        ("sensor.power".to_string(), curve.clone()),
    ]
    .into();
    let now = Instant::now();
    let it = BoardWords::from_locale(&locale("it"));
    let v = view(&canvas, &s, &curves, &Pending::default(), &it, now);
    assert_eq!(v[0].name, "Outdoor temperature");
    assert_eq!(v[0].value, "1.234,5°C");
    assert_eq!(v[0].icon, "thermometer");
    assert_eq!(v[0].curve, Some(curve), "a wide sensor draws its day");
    assert!(!v[0].tappable);
    assert_eq!(v[1].value, "80 W");
    assert_eq!(v[1].curve, None, "a small one does not");
    assert_eq!(
        v[1].name, "Power",
        "from its id before Home Assistant names it"
    );
    assert_eq!(
        (v[2].state, v[2].status.as_str()),
        (TileState::On, "Acceso")
    );
    assert!(v[2].tappable && !v[2].confirm);
    assert_eq!((v[3].level, v[3].value.as_str()), (Some(100), "100%"));
    assert_eq!(v[3].icon, "desk-lamp");
    assert_eq!(v[4].state, TileState::Unavailable);
    assert!(!v[4].tappable);
    assert_eq!(v[4].status, "Non disponibile");
    assert_eq!(
        (v[5].state, v[5].confirm, v[5].icon.as_str()),
        (TileState::Off, true, "window-shutter")
    );
    // Not yet known at all.
    let v = view(
        &page(vec![tile(Button, "light.new_one", 0, 0, 1, 1)]),
        &States::default(),
        &curves,
        &Pending::default(),
        &words(),
        now,
    );
    assert_eq!(
        (v[0].state, v[0].name.as_str(), v[0].tappable),
        (TileState::Unavailable, "New one", false)
    );
}

#[test]
fn a_tap_waits_for_its_state_at_most_four_seconds_and_a_failure_shows_three() {
    let canvas = page(vec![tile(TileKind::Button, "light.a", 0, 0, 1, 1)]);
    let mut s = states(&[("light.a", st("off"))]);
    let curves = HashMap::new();
    let t0 = Instant::now();
    let mut p = Pending::default();
    p.tapped("light.a", &s, t0);
    let look = |s: &States, p: &Pending, at| view(&canvas, s, &curves, p, &words(), at)[0].state;
    assert_eq!(look(&s, &p, t0), TileState::Pending);
    assert_eq!(p.next_change(), Some(t0 + PENDING_FOR));
    // Home Assistant answers: the new state shows.
    s.by_id.insert("light.a".into(), st("on"));
    assert_eq!(look(&s, &p, t0 + Duration::from_secs(1)), TileState::On);
    p.prune(&s, t0 + Duration::from_secs(1));
    assert_eq!(p.next_change(), None);
    // No answer: pending for four seconds, then as it is.
    p.tapped("light.a", &s, t0);
    assert_eq!(
        look(&s, &p, t0 + Duration::from_millis(3900)),
        TileState::Pending
    );
    assert_eq!(look(&s, &p, t0 + PENDING_FOR), TileState::On);
    // A failure: three seconds, whatever the state does.
    p.failed("light.a", t0);
    s.by_id.insert("light.a".into(), st("off"));
    assert_eq!(look(&s, &p, t0 + Duration::from_secs(2)), TileState::Failed);
    assert_eq!(look(&s, &p, t0 + FAILED_FOR), TileState::Off);
    p.prune(&s, t0 + FAILED_FOR);
    assert!(p.marks.is_empty());
}

#[test]
fn the_one_line_form_is_row_zero() {
    use TileKind::*;
    let canvas = Canvas {
        id: "custom".into(),
        name: String::new(),
        tiles: vec![
            tile(Button, "light.b", 2, 0, 2, 1),
            tile(Sensor, "sensor.t", 0, 0, 2, 1),
            tile(Button, "light.c", 0, 1, 1, 1),
        ],
    };
    let s = states(&[
        (
            "sensor.t",
            State {
                unit: "°C".into(),
                ..st("12.5")
            },
        ),
        ("light.b", st("off")),
        ("light.c", st("on")),
    ]);
    let v = view(
        &canvas,
        &s,
        &HashMap::new(),
        &Pending::default(),
        &words(),
        Instant::now(),
    );
    let l = line(&v);
    let got: Vec<(u8, &str, &str)> = l
        .iter()
        .map(|i| (i.x, i.icon.as_str(), i.text.as_str()))
        .collect();
    assert_eq!(got, [(0, "eye", "12.5°C"), (2, "lightbulb", "Off")]);
}

#[test]
fn what_the_hub_follows() {
    use TileKind::*;
    let mut adv = tile(Button, "script.s", 0, 1, 1, 1);
    adv.service = Some("script.turn_off".into());
    let b = board(
        vec![page(vec![
            tile(Sensor, "sensor.t", 0, 0, 2, 1),
            tile(Sensor, "sensor.u", 2, 0, 1, 1),
            adv,
        ])],
        vec![Canvas {
            id: "w1".into(),
            name: String::new(),
            tiles: vec![tile(Sensor, "sensor.t", 0, 0, 4, 1)],
        }],
    );
    assert_eq!(b.entities(), ["sensor.t", "sensor.u", "script.s"]);
    assert_eq!(b.curve_entities(), ["sensor.t"]);
    assert_eq!(b.advanced().len(), 1);
}

#[test]
fn offered_by_kind_never_what_the_gates_refuse() {
    use TileKind::*;
    assert!(offered(Button, &entity("light.a", "A", "")));
    assert!(offered(Level, &entity("cover.a", "Shutter", "shutter")));
    assert!(!offered(Button, &entity("cover.g", "G", "garage")));
    assert!(!offered(Button, &entity("lock.front", "Front", "")));
    assert!(!offered(Button, &entity("switch.r", "Gate opener", "")));
    assert!(!offered(Level, &entity("switch.a", "A", "")));
    assert!(offered(
        Sensor,
        &entity("binary_sensor.motion", "Motion", "motion")
    ));
}
