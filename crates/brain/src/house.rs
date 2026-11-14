// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! One entry for house commands: what `home_control`'s arguments mean, as a list of canonical
//! actions ([`plan`]), before any gate or any call to Home Assistant. Pure: the exposed entities
//! come in, actions or the error the model reads come out.
//!
//! The services are an allowlist ([`SERVICES`]): per domain, the canonical services and the data
//! fields each takes. Models' words are mapped to them ("close" on a cover is `close_cover`,
//! "spegni" is `turn_off`); anything else is an error naming what the domain does take. A
//! position stays a position (`set_cover_position` 0 is not rewritten to `close_cover`: inverted
//! covers and valves); only the question's words read it as closing or opening.

use serde_json::{Map, Value, json};

use crate::ha::Entity;

/// One thing to do: a canonical service with its checked data, on one exposed entity.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub entity: Entity,
    pub service: String,
    pub data: Value,
}

impl Action {
    /// What makes two actions the same: entity, service, data (canonical JSON).
    pub fn key(&self) -> (String, String, String) {
        (
            self.entity.entity_id.clone(),
            self.service.clone(),
            self.data.to_string(),
        )
    }
}

/// The identity of a set of actions: sorted, so the order the model used does not matter.
pub fn key_of(actions: &[Action]) -> Vec<(String, String, String)> {
    let mut k: Vec<_> = actions.iter().map(Action::key).collect();
    k.sort();
    k.dedup();
    k
}

const PCT: &[&str] = &["position", "brightness_pct", "percentage", "humidity"];

/// Per domain: (canonical service, the data fields it takes).
/// A service and the data fields it takes.
pub type Service = (&'static str, &'static [&'static str]);

pub const SERVICES: &[(&str, &[Service])] = &[
    (
        "light",
        &[
            (
                "turn_on",
                &[
                    "brightness_pct",
                    "brightness",
                    "color_temp_kelvin",
                    "rgb_color",
                    "color_name",
                    "effect",
                    "transition",
                ],
            ),
            ("turn_off", &["transition"]),
            ("toggle", &[]),
        ],
    ),
    (
        "switch",
        &[("turn_on", &[]), ("turn_off", &[]), ("toggle", &[])],
    ),
    (
        "input_boolean",
        &[("turn_on", &[]), ("turn_off", &[]), ("toggle", &[])],
    ),
    (
        "fan",
        &[
            ("turn_on", &["percentage"]),
            ("turn_off", &[]),
            ("toggle", &[]),
            ("set_percentage", &["percentage"]),
        ],
    ),
    (
        "cover",
        &[
            ("open_cover", &[]),
            ("close_cover", &[]),
            ("stop_cover", &[]),
            ("set_cover_position", &["position"]),
        ],
    ),
    (
        "valve",
        &[
            ("open_valve", &[]),
            ("close_valve", &[]),
            ("stop_valve", &[]),
            ("set_valve_position", &["position"]),
        ],
    ),
    (
        "climate",
        &[
            (
                "set_temperature",
                &[
                    "temperature",
                    "target_temp_high",
                    "target_temp_low",
                    "hvac_mode",
                ],
            ),
            ("set_hvac_mode", &["hvac_mode"]),
            ("set_fan_mode", &["fan_mode"]),
            ("set_preset_mode", &["preset_mode"]),
            ("turn_on", &[]),
            ("turn_off", &[]),
        ],
    ),
    (
        "media_player",
        &[
            ("media_play", &[]),
            ("media_pause", &[]),
            ("media_play_pause", &[]),
            ("media_stop", &[]),
            ("media_next_track", &[]),
            ("media_previous_track", &[]),
            ("volume_up", &[]),
            ("volume_down", &[]),
            ("volume_set", &["volume_level"]),
            ("volume_mute", &["is_volume_muted"]),
            ("turn_on", &[]),
            ("turn_off", &[]),
        ],
    ),
    (
        "humidifier",
        &[
            ("turn_on", &[]),
            ("turn_off", &[]),
            ("set_humidity", &["humidity"]),
        ],
    ),
    ("water_heater", &[("set_temperature", &["temperature"])]),
    (
        "vacuum",
        &[
            ("start", &[]),
            ("pause", &[]),
            ("stop", &[]),
            ("return_to_base", &[]),
        ],
    ),
    (
        "lawn_mower",
        &[("start_mowing", &[]), ("pause", &[]), ("dock", &[])],
    ),
    ("scene", &[("turn_on", &[])]),
    ("script", &[("turn_on", &[])]),
    ("button", &[("press", &[])]),
    ("input_button", &[("press", &[])]),
    ("input_number", &[("set_value", &["value"])]),
    ("number", &[("set_value", &["value"])]),
    ("input_select", &[("select_option", &["option"])]),
    ("select", &[("select_option", &["option"])]),
];

fn row(domain: &str) -> Option<&'static [Service]> {
    SERVICES.iter().find(|(d, _)| *d == domain).map(|(_, r)| *r)
}

/// The canonical service a model's word means on `domain`, and the fields it takes.
pub fn canonical(
    domain: &str,
    verb: &str,
) -> Result<(&'static str, &'static [&'static str]), String> {
    let r = row(domain)
        .ok_or_else(|| format!("{domain} devices cannot be controlled with home_control"))?;
    let verb = verb.trim().to_lowercase();
    let wanted = match (verb.as_str(), domain) {
        ("open" | "apri", "cover" | "valve") => format!("open_{domain}"),
        ("close" | "chiudi", "cover" | "valve") => format!("close_{domain}"),
        ("stop" | "ferma", "cover" | "valve") => format!("stop_{domain}"),
        ("stop" | "ferma", "media_player") => "media_stop".into(),
        ("ferma", _) => "stop".into(),
        ("on" | "accendi", _) => "turn_on".into(),
        ("off" | "spegni", _) => "turn_off".into(),
        ("set_position", _) => format!("set_{domain}_position"),
        (v, _) => v.to_string(),
    };
    r.iter()
        .find(|(s, _)| *s == wanted)
        .map(|(s, f)| (*s, *f))
        .ok_or_else(|| {
            format!(
                "{domain} has no service {verb:?}; valid services: {}",
                r.iter().map(|(s, _)| *s).collect::<Vec<_>>().join(", ")
            )
        })
}

/// The data of `service`: only the allowed fields (an `entity_id` in it is dropped), percentages
/// within 0..=100.
fn check_data(domain: &str, service: &str, fields: &[&str], data: &Value) -> Result<Value, String> {
    let mut out = Map::new();
    for (k, v) in data.as_object().into_iter().flatten() {
        if k == "entity_id" {
            continue;
        }
        if !fields.contains(&k.as_str()) {
            return Err(if fields.is_empty() {
                let others: Vec<String> = row(domain)
                    .into_iter()
                    .flatten()
                    .filter(|(_, f)| !f.is_empty())
                    .map(|(s, f)| format!("{s} ({})", f.join(", ")))
                    .collect();
                format!(
                    "{service} takes no data (not {k}); the {domain} services with data: {}",
                    if others.is_empty() {
                        "none".to_string()
                    } else {
                        others.join("; ")
                    }
                )
            } else {
                format!(
                    "{service} does not take {k}; allowed fields: {}",
                    fields.join(", ")
                )
            });
        }
        if PCT.contains(&k.as_str()) {
            match v.as_f64() {
                Some(n) if (0.0..=100.0).contains(&n) => {}
                _ => return Err(format!("{k}: a number from 0 to 100")),
            }
        }
        out.insert(k.clone(), v.clone());
    }
    Ok(Value::Object(out))
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// `home_control`'s arguments → the actions to gate: `entity_id` and/or `entity_ids` (names
/// instead of ids are resolved), else `area` (+ `domain`, default light). A group whose members
/// are all in the list goes (its lights are done one by one); duplicates go. Every target must
/// take the service, or nothing is done; except the ones `never` says are never controlled by
/// voice (a lock has no services here): they come back as they are, to be named as refused.
pub fn plan(
    exposed: &[Entity],
    args: &Value,
    never: &dyn Fn(&Entity) -> bool,
) -> Result<Vec<Action>, String> {
    let get = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let service = get("service");
    if service.is_empty() || !service.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        return Err("service: a service name like turn_on".into());
    }
    let mut ids = strings(args.get("entity_id"));
    ids.extend(strings(args.get("entity_ids")));
    let mut targets: Vec<Entity> = Vec::new();
    if !ids.is_empty() {
        for id in &ids {
            targets.push(crate::agent::resolve_entity(exposed, id)?.clone());
        }
    } else {
        let area = get("area").to_lowercase();
        if area.is_empty() {
            return Err("entity_id, entity_ids or area: which devices?".into());
        }
        let domain = match get("domain").trim_end_matches('.') {
            "" => "light".to_string(),
            d => d.to_lowercase(),
        };
        targets = exposed
            .iter()
            .filter(|e| e.area.trim().to_lowercase() == area && e.domain() == domain)
            .cloned()
            .collect();
        if targets.is_empty() {
            let mut areas: Vec<&str> = exposed
                .iter()
                .map(|e| e.area.as_str())
                .filter(|a| !a.is_empty())
                .collect();
            areas.sort_unstable();
            areas.dedup();
            return Err(format!(
                "no exposed {domain} in area {area:?}; areas: {}",
                areas.join(", ")
            ));
        }
    }
    let mut seen: Vec<String> = Vec::new();
    targets.retain(|e| {
        let new = !seen.contains(&e.entity_id);
        seen.push(e.entity_id.clone());
        new
    });
    if targets.len() > 1 {
        let all: Vec<String> = targets.iter().map(|e| e.entity_id.clone()).collect();
        targets.retain(|e| e.members.is_empty() || !e.members.iter().all(|m| all.contains(m)));
    }
    let data = args
        .get("data")
        .filter(|d| d.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    targets
        .into_iter()
        .map(|e| {
            if never(&e) {
                return Ok(Action {
                    entity: e,
                    service: service.to_string(),
                    data: json!({}),
                });
            }
            let (service, fields) = canonical(e.domain(), service)?;
            let data = check_data(e.domain(), service, fields, &data)?;
            Ok(Action {
                entity: e,
                service: service.to_string(),
                data,
            })
        })
        .collect()
}

/// The language of the questions and words written here: Italian, English (also every
/// language without its own words) and Dutch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    It,
    En,
    Nl,
}

impl Lang {
    /// From a language code ("it", "nl-NL", "en_US"); English when there are no words for it.
    pub fn of(code: &str) -> Lang {
        match code.split(['-', '_']).next().unwrap_or("") {
            "it" => Lang::It,
            "nl" => Lang::Nl,
            _ => Lang::En,
        }
    }

    /// One of three words: Italian, English, Dutch.
    fn pick<T>(self, it: T, en: T, nl: T) -> T {
        match self {
            Lang::It => it,
            Lang::En => en,
            Lang::Nl => nl,
        }
    }
}

/// The question before a command that needs a yes, in the user's words: "Chiudo Tapparella
/// salone?", "Sposto Tapparella salone al 30%?" (position 0 and 100 read as closing and
/// opening); a service without a verb here keeps its name. Dutch puts the verb last
/// ("Rolluik woonkamer sluiten?").
pub fn confirm_question(name: &str, service: &str, data: &Value, lang: Lang) -> String {
    let pos = data.get("position").and_then(Value::as_i64);
    let verb = match (service, pos) {
        ("set_cover_position" | "set_valve_position", Some(0)) => ("Chiudo", "Close", "sluiten"),
        ("set_cover_position" | "set_valve_position", Some(100)) => ("Apro", "Open", "openen"),
        ("open_cover" | "open_valve", _) => ("Apro", "Open", "openen"),
        ("close_cover" | "close_valve", _) => ("Chiudo", "Close", "sluiten"),
        ("stop_cover" | "stop_valve", _) => ("Fermo", "Stop", "stoppen"),
        ("turn_on", _) => ("Accendo", "Turn on", "aanzetten"),
        ("turn_off", _) => ("Spengo", "Turn off", "uitzetten"),
        ("set_cover_position" | "set_valve_position", _) => {
            let p = pos.map_or(String::new(), |p| format!(" {p}%"));
            return match lang {
                Lang::It => format!("Sposto {name} al{p}?"),
                Lang::En => format!("Move {name} to{p}?"),
                Lang::Nl => format!("{name} naar{p} zetten?"),
            };
        }
        _ => {
            let s = service.replace('_', " ");
            return format!(
                "{name} ({s}): {}?",
                lang.pick("confermi", "confirm", "bevestigen")
            );
        }
    };
    match lang {
        Lang::It => format!("{} {name}?", verb.0),
        Lang::En => format!("{} {name}?", verb.1),
        Lang::Nl => format!("{name} {}?", verb.2),
    }
}

/// What several devices of one kind are called, in the plural.
fn kind_plural(e: &Entity, lang: Lang) -> &'static str {
    let domain = e.entity_id.split('.').next().unwrap_or("");
    let (it, en, nl) = match (domain, e.device_class.as_str()) {
        ("cover", "curtain") => ("tende", "curtains", "gordijnen"),
        ("cover", "awning") => ("tende da sole", "awnings", "zonneschermen"),
        ("cover", _) => ("tapparelle", "blinds", "rolluiken"),
        ("light", _) => ("luci", "lights", "lampen"),
        ("valve", _) => ("valvole", "valves", "kleppen"),
        ("switch", _) => ("prese", "switches", "schakelaars"),
        ("fan", _) => ("ventilatori", "fans", "ventilatoren"),
        _ => ("dispositivi", "devices", "apparaten"),
    };
    lang.pick(it, en, nl)
}

/// The question for several actions: one service on up to three devices names them ("Chiudo
/// A, B e C?"), on more counts them by kind and room ("Chiudo 5 tapparelle in Salone?"); mixed services: "Confermo 3
/// comandi?".
pub fn question_text(actions: &[Action], lang: Lang) -> String {
    let Some(first) = actions.first() else {
        return String::new();
    };
    let same = actions
        .iter()
        .all(|a| a.service == first.service && a.data == first.data);
    if !same {
        let n = actions.len();
        return match lang {
            Lang::It => format!("Confermo {n} comandi?"),
            Lang::En => format!("Confirm {n} commands?"),
            Lang::Nl => format!("{n} opdrachten bevestigen?"),
        };
    }
    let names: Vec<String> = actions.iter().map(|a| a.entity.full_name()).collect();
    let who = match names.as_slice() {
        [one] => one.clone(),
        many if many.len() <= 3 => {
            let (last, rest) = many.split_last().unwrap_or((&many[0], &[]));
            format!("{} {} {last}", rest.join(", "), lang.pick("e", "and", "en"))
        }
        many => {
            // "4 tapparelle in Salone": what they are (all the same kind) and where (all in one
            // room), never just "4 dispositivi".
            let kind = |a: &Action| kind_plural(&a.entity, lang);
            let noun = kind(first);
            let noun = if actions.iter().all(|a| kind(a) == noun) {
                noun
            } else {
                lang.pick("dispositivi", "devices", "apparaten")
            };
            let area = first.entity.area.trim();
            let one_room = !area.is_empty() && actions.iter().all(|a| a.entity.area.trim() == area);
            if one_room {
                format!("{} {noun} in {area}", many.len())
            } else {
                format!("{} {noun}", many.len())
            }
        }
    };
    confirm_question(&who, &first.service, &first.data, lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan2(h: &[Entity], args: &Value) -> Result<Vec<Action>, String> {
        plan(h, args, &|_| false)
    }

    fn ent(id: &str, name: &str, area: &str) -> Entity {
        Entity {
            entity_id: id.into(),
            name: name.into(),
            device: String::new(),
            area: area.into(),
            state: "on".into(),
            device_class: String::new(),
            attributes: json!({}),
            members: Vec::new(),
        }
    }

    fn house() -> Vec<Entity> {
        let mut group = ent("light.study", "Study", "Study");
        group.members = vec!["light.study_left".into(), "light.study_right".into()];
        vec![
            ent("light.kitchen", "Kitchen light", "Kitchen"),
            ent("light.study_left", "Study left", "Study"),
            ent("light.study_right", "Study right", "Study"),
            group,
            ent("cover.living_left", "Living left", "Living room"),
            ent("cover.living_right", "Living right", "Living room"),
            ent("valve.garden", "Garden water", ""),
            ent("media_player.tv", "TV", "Living room"),
        ]
    }

    fn ids(a: &[Action]) -> Vec<(&str, &str)> {
        a.iter()
            .map(|a| (a.entity.entity_id.as_str(), a.service.as_str()))
            .collect()
    }

    #[test]
    fn targets_by_id_list_or_area() {
        let h = house();
        let p = plan2(
            &h,
            &json!({"entity_id": "cover.living_left", "service": "close"}),
        )
        .unwrap();
        assert_eq!(ids(&p), [("cover.living_left", "close_cover")]);
        let p = plan2(
            &h,
            &json!({"area": "living room", "domain": "cover", "service": "chiudi"}),
        )
        .unwrap();
        assert_eq!(p.len(), 2);
        let p = plan2(&h, &json!({"area": "Study", "service": "spegni"})).unwrap();
        assert_eq!(
            ids(&p),
            [
                ("light.study_left", "turn_off"),
                ("light.study_right", "turn_off")
            ]
        );
        // A group alone stays a group; duplicates go.
        let p = plan2(
            &h,
            &json!({"entity_ids": ["light.study", "light.study"], "service": "on"}),
        )
        .unwrap();
        assert_eq!(ids(&p), [("light.study", "turn_on")]);
        // Names instead of ids.
        let p = plan2(
            &h,
            &json!({"entity_id": "light.kitchen_light", "service": "off"}),
        )
        .unwrap();
        assert_eq!(ids(&p), [("light.kitchen", "turn_off")]);
        let e = plan2(&h, &json!({"area": "Attic", "service": "off"})).unwrap_err();
        assert!(e.contains("Kitchen") && e.contains("Study"), "{e}");
        let e = plan2(&h, &json!({"service": "off"})).unwrap_err();
        assert!(e.contains("which devices"), "{e}");
        assert!(plan2(&h, &json!({"entity_id": "light.nope", "service": "off"})).is_err());
    }

    #[test]
    fn the_service_table() {
        let h = house();
        let e = plan2(
            &h,
            &json!({"entity_id": "cover.living_left", "service": "toggle"}),
        )
        .unwrap_err();
        assert!(
            e.contains("open_cover, close_cover, stop_cover, set_cover_position"),
            "{e}"
        );
        assert!(
            plan2(
                &h,
                &json!({"entity_id": "cover.living_left", "service": "set_cover_tilt_position"})
            )
            .is_err()
        );
        let p = plan2(&h,
            &json!({"entity_id": "valve.garden", "service": "set_position", "data": {"position": 0}}),
        )
        .unwrap();
        assert_eq!(ids(&p), [("valve.garden", "set_valve_position")]);
        assert_eq!(p[0].data, json!({"position": 0}));
        let p = plan2(
            &h,
            &json!({"entity_id": "media_player.tv", "service": "stop"}),
        )
        .unwrap();
        assert_eq!(ids(&p), [("media_player.tv", "media_stop")]);
        let e = plan2(&h,
            &json!({"entity_id": "cover.living_left", "service": "close_cover", "data": {"brightness_pct": 10}}),
        )
        .unwrap_err();
        assert!(e.contains("takes no data"), "{e}");
        let e = plan2(
            &h,
            &json!({"entity_id": "light.kitchen", "service": "turn_on", "data": {"position": 10}}),
        )
        .unwrap_err();
        assert!(e.contains("brightness_pct"), "{e}");
        assert!(plan2(&h, &json!({"entity_id": "light.kitchen", "service": "turn_on", "data": {"brightness_pct": 101}})).is_err());
        let p = plan2(&h,
            &json!({"entity_id": "light.kitchen", "service": "turn_on", "data": {"brightness_pct": 30, "entity_id": "light.x"}}),
        )
        .unwrap();
        assert_eq!(p[0].data, json!({"brightness_pct": 30}));
        // Every target must take the service.
        assert!(plan2(&h, &json!({"entity_ids": ["light.kitchen", "cover.living_left"], "service": "turn_off"})).is_err());
        assert!(canonical("lock", "unlock").is_err());
        // A device never controlled by voice comes back as asked, to be refused.
        let mut h = h;
        h.push(ent("lock.front", "Front door", ""));
        let never = |e: &Entity| e.domain() == "lock";
        let p = plan(
            &h,
            &json!({"entity_ids": ["light.kitchen", "lock.front"], "service": "turn_off"}),
            &never,
        )
        .unwrap();
        assert_eq!(
            ids(&p),
            [("light.kitchen", "turn_off"), ("lock.front", "turn_off")]
        );
        assert!(
            plan(
                &h,
                &json!({"entity_id": "lock.front", "service": "unlock"}),
                &|_: &Entity| false
            )
            .is_err()
        );
    }

    #[test]
    fn questions_in_the_user_words() {
        let q = confirm_question;
        assert_eq!(
            q("Tapparella salone", "close_cover", &json!({}), Lang::It),
            "Chiudo Tapparella salone?"
        );
        assert_eq!(
            q("Living blind", "open_cover", &json!({}), Lang::En),
            "Open Living blind?"
        );
        assert_eq!(
            q(
                "Tapparella salone",
                "set_cover_position",
                &json!({"position": 30}),
                Lang::It
            ),
            "Sposto Tapparella salone al 30%?"
        );
        assert_eq!(
            q("X", "set_cover_position", &json!({"position": 0}), Lang::It),
            "Chiudo X?"
        );
        assert_eq!(
            q(
                "X",
                "set_valve_position",
                &json!({"position": 100}),
                Lang::En
            ),
            "Open X?"
        );
        assert_eq!(
            q("X", "set_foo", &json!({}), Lang::It),
            "X (set foo): confermi?"
        );
        let h = house();
        let two = plan2(
            &h,
            &json!({"area": "Living room", "domain": "cover", "service": "close"}),
        )
        .unwrap();
        assert_eq!(
            question_text(&two, Lang::It),
            "Chiudo Living left e Living right?"
        );
        assert_eq!(
            question_text(&two, Lang::En),
            "Close Living left and Living right?"
        );
        let mut five = two.clone();
        for i in 0..3 {
            let mut a = two[0].clone();
            a.entity.entity_id = format!("cover.x{i}");
            five.push(a);
        }
        assert_eq!(
            question_text(&five, Lang::It),
            "Chiudo 5 tapparelle in Living room?"
        );
        assert_eq!(
            question_text(&five, Lang::En),
            "Close 5 blinds in Living room?"
        );
        let mut spread = five.clone();
        spread[4].entity.area = "Study".into();
        assert_eq!(question_text(&spread, Lang::It), "Chiudo 5 tapparelle?");
        let mut kinds = five.clone();
        kinds[4].entity.entity_id = "light.x".into();
        assert_eq!(
            question_text(&kinds, Lang::It),
            "Chiudo 5 dispositivi in Living room?"
        );
        let mut mixed = two.clone();
        mixed[1].service = "open_cover".into();
        assert_eq!(question_text(&mixed, Lang::It), "Confermo 2 comandi?");
        assert_eq!(question_text(&mixed, Lang::En), "Confirm 2 commands?");
        // Dutch: the verb last, as an infinitive.
        assert_eq!(
            q("Rolluik woonkamer", "close_cover", &json!({}), Lang::Nl),
            "Rolluik woonkamer sluiten?"
        );
        assert_eq!(
            q("Keukenlamp", "turn_on", &json!({}), Lang::Nl),
            "Keukenlamp aanzetten?"
        );
        assert_eq!(
            q(
                "X",
                "set_cover_position",
                &json!({"position": 30}),
                Lang::Nl
            ),
            "X naar 30% zetten?"
        );
        assert_eq!(
            q("X", "set_foo", &json!({}), Lang::Nl),
            "X (set foo): bevestigen?"
        );
        assert_eq!(
            question_text(&two, Lang::Nl),
            "Living left en Living right sluiten?"
        );
        assert_eq!(
            question_text(&five, Lang::Nl),
            "5 rolluiken in Living room sluiten?"
        );
        assert_eq!(
            question_text(&kinds, Lang::Nl),
            "5 apparaten in Living room sluiten?"
        );
        assert_eq!(question_text(&mixed, Lang::Nl), "2 opdrachten bevestigen?");
        assert_eq!(Lang::of("nl-NL"), Lang::Nl);
        assert_eq!(Lang::of("it"), Lang::It);
        assert_eq!(Lang::of("de"), Lang::En);
    }

    #[test]
    fn keys_ignore_the_order() {
        let h = house();
        let a = plan2(
            &h,
            &json!({"entity_ids": ["cover.living_left", "cover.living_right"], "service": "close"}),
        )
        .unwrap();
        let b = plan2(&h, &json!({"entity_ids": ["cover.living_right", "cover.living_left"], "service": "close_cover"})).unwrap();
        assert_eq!(key_of(&a), key_of(&b));
        let c = plan2(
            &h,
            &json!({"entity_ids": ["cover.living_right"], "service": "close_cover"}),
        )
        .unwrap();
        assert_ne!(key_of(&a), key_of(&c));
    }
}
