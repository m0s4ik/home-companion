// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The house tools' side of Home Assistant: its WebSocket API (the body's own minimal client,
//! `crates/home-companion/src/ha/ws.rs`, compiled in here as well), one short connection per call.
//! The house tools read the entities exposed to Assist and call services on them; the memory
//! jobs think through `ai_task.generate_data`.

#[path = "../../home-companion/src/ha/ws.rs"]
#[rustfmt::skip] // formatted with the body, not here
#[allow(dead_code)]
mod ws;

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::config::{HaCfg, read_secret};
use ws::{Message, WebSocket};

pub struct Ha {
    cfg: HaCfg,
    /// `[home_assistant] url`, or the body's (`ha.token` with `url`: the QR link's address, so
    /// brain.toml needs none).
    url: Mutex<String>,
    /// The body's access token (`ha.token` on the control socket) and when it expires
    /// (seconds since the epoch): the QR link's tokens, so the brain needs no token of its own.
    pushed: Mutex<Option<(String, u64)>>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `http://host:port[/base]` → (host, port, websocket path).
pub fn endpoint(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("http://")
        .ok_or_else(|| format!("{url}: expected http://host:port (no TLS yet)"))?;
    let (auth, base) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (host, port) = match auth.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("{url}: bad port"))?),
        None => (auth, 80),
    };
    if host.is_empty() {
        return Err(format!("{url}: no host"));
    }
    Ok((host.to_string(), port, format!("{base}/api/websocket")))
}

impl Ha {
    pub fn new(cfg: HaCfg) -> Ha {
        Ha {
            url: Mutex::new(cfg.url.clone()),
            cfg,
            pushed: Mutex::new(None),
        }
    }

    pub fn url(&self) -> String {
        self.url.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Home Assistant's address is known (brain.toml or the body).
    pub fn configured(&self) -> bool {
        !self.url().is_empty()
    }

    /// The body's address for Home Assistant; brain.toml's wins when it has one.
    pub fn set_url(&self, url: &str) {
        if self.cfg.url.is_empty() && !url.is_empty() {
            *self.url.lock().unwrap_or_else(|e| e.into_inner()) = url.to_string();
        }
    }

    /// The body hands over its access token (valid until `expires_at`).
    pub fn set_token(&self, token: String, expires_at: u64) {
        *self.pushed.lock().unwrap_or_else(|e| e.into_inner()) = Some((token, expires_at));
    }

    /// The body's token while it is valid for another 30 s, else the token file's.
    fn token(&self) -> Result<String, String> {
        let pushed = self.pushed.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match pushed {
            Some((t, exp)) if exp > now_secs() + 30 => Ok(t),
            _ => read_secret(&self.cfg.token_file).ok_or_else(|| {
                format!(
                    "no Home Assistant token (none from the body, none in {})",
                    self.cfg.token_file
                )
            }),
        }
    }

    /// A token from the body or the token file is there.
    pub fn has_token(&self) -> bool {
        self.configured() && self.token().is_ok()
    }

    /// One command on a fresh, authenticated connection; the `result` of the reply.
    pub fn call(&self, cmd: Value, timeout: Duration) -> Result<Value, String> {
        self.call_many(vec![cmd], timeout)?
            .pop()
            .unwrap_or_else(|| Err("no answer".into()))
    }

    /// Several commands on one authenticated connection, answered in order: one result each
    /// (a failed command does not stop the others; a lost connection fails the whole call).
    pub fn call_many(
        &self,
        cmds: Vec<Value>,
        timeout: Duration,
    ) -> Result<Vec<Result<Value, String>>, String> {
        let token = self.token()?;
        let (host, port, path) = endpoint(&self.url())?;
        let deadline = Instant::now() + timeout;
        let mut ws = WebSocket::connect(&host, port, &path, timeout.min(Duration::from_secs(5)))
            .map_err(|e| format!("Home Assistant: {e}"))?;
        let n = cmds.len();
        let mut out: Vec<Option<Result<Value, String>>> = vec![None; n];
        let mut cmds = Some(cmds);
        loop {
            if out.iter().all(Option::is_some) {
                ws.close();
                return Ok(out.into_iter().flatten().collect());
            }
            if Instant::now() >= deadline {
                ws.close();
                return Err("Home Assistant did not answer in time".into());
            }
            let msg = match ws.read().map_err(|e| format!("Home Assistant: {e}"))? {
                Some(Message::Text(t)) => t,
                Some(Message::Close) => return Err("Home Assistant closed the connection".into()),
                None => continue,
            };
            let v: Value = serde_json::from_str(&msg).unwrap_or(Value::Null);
            match v.get("type").and_then(Value::as_str) {
                Some("auth_required") => {
                    let auth = json!({"type": "auth", "access_token": token}).to_string();
                    ws.send_text(&auth).map_err(|e| e.to_string())?;
                }
                Some("auth_invalid") => return Err("Home Assistant refused the token".into()),
                Some("auth_ok") => {
                    for (i, mut cmd) in cmds.take().into_iter().flatten().enumerate() {
                        cmd["id"] = json!(i + 1);
                        ws.send_text(&cmd.to_string()).map_err(|e| e.to_string())?;
                    }
                }
                Some("result") => {
                    let Some(i) = v
                        .get("id")
                        .and_then(Value::as_u64)
                        .filter(|i| (1..=n as u64).contains(i))
                    else {
                        continue;
                    };
                    out[i as usize - 1] = Some(
                        if v.get("success").and_then(Value::as_bool) == Some(true) {
                            Ok(v.get("result").cloned().unwrap_or(Value::Null))
                        } else {
                            let why = v
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .unwrap_or("failed");
                            Err(format!("Home Assistant: {why}"))
                        },
                    );
                }
                _ => {}
            }
        }
    }

    /// Text from Home Assistant's AI task (`ai_task.generate_data`): `entity` or, when empty,
    /// the AI task entity chosen in Home Assistant's settings. The language model is Home
    /// Assistant's, with its key: the brain has none.
    pub fn generate(&self, instructions: &str, entity: &str, timeout: Duration) -> Result<String, String> {
        let entity = if entity.is_empty() {
            self.ai_task_entity(timeout)?
        } else {
            entity.to_string()
        };
        let data = json!({"task_name": "home-companion-brain", "instructions": instructions, "entity_id": entity});
        let r = self.call(
            json!({"type": "call_service", "domain": "ai_task", "service": "generate_data",
                   "service_data": data, "return_response": true}),
            timeout,
        )?;
        let d = r.pointer("/response/data").cloned().unwrap_or(Value::Null);
        Ok(match d {
            Value::String(s) => s,
            Value::Null => return Err("Home Assistant's AI task gave no answer".into()),
            other => other.to_string(),
        })
    }

    /// The AI task entity to use: the one chosen in Home Assistant's settings, else the first
    /// `ai_task.*` there is (Home Assistant asks for one when none is chosen).
    pub fn ai_task_entity(&self, timeout: Duration) -> Result<String, String> {
        let mut r = self
            .call_many(
                vec![json!({"type": "ai_task/preferences/get"}), json!({"type": "get_states"})],
                timeout,
            )?
            .into_iter();
        let prefs = r.next().and_then(Result::ok).unwrap_or(Value::Null);
        if let Some(e) = prefs.get("gen_data_entity_id").and_then(Value::as_str) {
            return Ok(e.to_string());
        }
        let states = r.next().and_then(Result::ok).unwrap_or(Value::Null);
        states
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.get("entity_id").and_then(Value::as_str))
            .find(|id| id.starts_with("ai_task."))
            .map(str::to_string)
            .ok_or_else(|| "Home Assistant has no AI task (add an AI integration such as Google Gemini)".into())
    }

    /// The entities exposed to Assist (Home Assistant's own choice, Settings → Voice
    /// assistants → Expose), with name, area, state and the attributes a command needs.
    pub fn exposed(&self, timeout: Duration) -> Result<Vec<Entity>, String> {
        let mut r = self.call_many(
            vec![
                json!({"type": "homeassistant/expose_entity/list"}),
                json!({"type": "get_states"}),
                json!({"type": "config/entity_registry/list_for_display"}),
                json!({"type": "config/device_registry/list"}),
                json!({"type": "config/area_registry/list"}),
            ],
            timeout,
        )?
        .into_iter();
        let mut next = || r.next().unwrap_or_else(|| Err("no answer".into()));
        let exposed = next()?;
        let states = next()?;
        let registry = next().unwrap_or(Value::Null);
        let devices = next().unwrap_or(Value::Null);
        let areas = next().unwrap_or(Value::Null);
        Ok(entities_from(&exposed, &states, &registry, &devices, &areas))
    }

    /// A service of the entity's own domain on one entity (`light.turn_on`, `cover.open_cover`…).
    pub fn control(&self, entity_id: &str, service: &str, data: &Value, timeout: Duration) -> Result<(), String> {
        let domain = entity_id.split('.').next().unwrap_or("");
        let mut sd = match data {
            Value::Object(_) => data.clone(),
            _ => json!({}),
        };
        sd["entity_id"] = json!(entity_id);
        self.call(
            json!({"type": "call_service", "domain": domain, "service": service, "service_data": sd}),
            timeout,
        )
        .map(|_| ())
    }
}

/// An entity exposed to Assist, as the house tools show it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub entity_id: String,
    /// The entity's own name ("Temperatura"), or its device's when it has none (the device's main
    /// feature, or an entity named the old way). Home Assistant 2026.11 composes names from entity,
    /// device and area and plans to drop `friendly_name`, the old "device + entity" string.
    pub name: String,
    /// The device the entity belongs to, when its name is not already the device's ("" otherwise).
    pub device: String,
    pub area: String,
    pub state: String,
    pub device_class: String,
    /// A few attributes worth knowing (brightness, position, temperature, unit…).
    pub attributes: Value,
}

impl Entity {
    /// Name and device together, for matching words like "garage" or "cancello" in either.
    pub fn full_name(&self) -> String {
        if self.device.is_empty() { self.name.clone() } else { format!("{} {}", self.device, self.name) }
    }

    pub fn domain(&self) -> &str {
        self.entity_id.split('.').next().unwrap_or("")
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({"entity_id": self.entity_id, "name": self.name, "state": self.state});
        if !self.device.is_empty() {
            v["device"] = json!(self.device);
        }
        if !self.area.is_empty() {
            v["area"] = json!(self.area);
        }
        if !self.device_class.is_empty() {
            v["device_class"] = json!(self.device_class);
        }
        if self.attributes.as_object().is_some_and(|m| !m.is_empty()) {
            v["attributes"] = self.attributes.clone();
        }
        v
    }
}

const KEPT_ATTRIBUTES: &[&str] = &[
    "unit_of_measurement", "brightness", "color_temp_kelvin", "current_position",
    "current_temperature", "temperature", "target_temp_low", "target_temp_high", "hvac_modes",
    "preset_mode", "percentage", "volume_level", "media_title", "media_artist", "source",
];

/// Exposed entities from the five registry answers (pure, for tests).
pub fn entities_from(exposed: &Value, states: &Value, registry: &Value, devices: &Value, areas: &Value) -> Vec<Entity> {
    let is_exposed = |id: &str| {
        exposed
            .pointer("/exposed_entities")
            .and_then(|m| m.get(id))
            .and_then(|e| e.get("conversation"))
            .and_then(Value::as_bool)
            == Some(true)
    };
    let area_name = |id: &str| -> String {
        areas
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a.get("area_id").and_then(Value::as_str) == Some(id))
            .and_then(|a| a.get("name").and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    };
    let device = |id: &str| -> Option<&Value> {
        devices.as_array().into_iter().flatten().find(|d| d.get("id").and_then(Value::as_str) == Some(id))
    };
    let device_area = |id: &str| -> Option<String> {
        device(id).and_then(|d| d.get("area_id").and_then(Value::as_str)).map(str::to_string)
    };
    let device_name = |id: &str| -> Option<String> {
        let d = device(id)?;
        ["name_by_user", "name"]
            .iter()
            .find_map(|k| d.get(*k).and_then(Value::as_str).filter(|n| !n.trim().is_empty()))
            .map(|n| n.trim().to_string())
    };
    // list_for_display: {"entities": [{"ei": id, "ai": area, "di": device, …}]}
    let reg = |id: &str| -> Option<&Value> {
        registry
            .pointer("/entities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|e| e.get("ei").and_then(Value::as_str) == Some(id))
    };
    let mut out: Vec<Entity> = states
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s.get("entity_id")?.as_str()?;
            if !is_exposed(id) {
                return None;
            }
            let attrs = s.get("attributes").cloned().unwrap_or(Value::Null);
            // list_for_display: "en" = the entity's own name, "di" = its device.
            let own = reg(id).and_then(|r| r.get("en").and_then(Value::as_str)).filter(|n| !n.trim().is_empty());
            let dev = reg(id).and_then(|r| r.get("di").and_then(Value::as_str)).and_then(device_name);
            let friendly = attrs.get("friendly_name").and_then(Value::as_str).filter(|n| !n.trim().is_empty());
            let (name, device) = match (own, dev) {
                (Some(n), Some(d)) if n.trim() != d => (n.trim().to_string(), d),
                (Some(n), _) => (n.trim().to_string(), String::new()),
                (None, Some(d)) => (d, String::new()),
                (None, None) => (friendly.unwrap_or(id).to_string(), String::new()),
            };
            let area_id = reg(id).and_then(|r| {
                r.get("ai")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| r.get("di").and_then(Value::as_str).and_then(device_area))
            });
            let mut kept = serde_json::Map::new();
            for k in KEPT_ATTRIBUTES {
                if let Some(v) = attrs.get(*k).filter(|v| !v.is_null()) {
                    kept.insert((*k).to_string(), v.clone());
                }
            }
            Some(Entity {
                entity_id: id.to_string(),
                name,
                device,
                area: area_id.as_deref().map(area_name).unwrap_or_default(),
                state: s.get("state").and_then(Value::as_str).unwrap_or("").to_string(),
                device_class: attrs.get("device_class").and_then(Value::as_str).unwrap_or("").to_string(),
                attributes: Value::Object(kept),
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.area, &a.entity_id).cmp(&(&b.area, &b.entity_id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        assert_eq!(
            endpoint("http://ha.example:8123/").unwrap(),
            ("ha.example".into(), 8123, "/api/websocket".into())
        );
        assert!(endpoint("https://x").is_err());
    }

    #[test]
    fn exposed_entities_with_areas() {
        let exposed = json!({"exposed_entities": {
            "light.kitchen": {"conversation": true},
            "cover.living": {"conversation": true},
            "lock.door": {"conversation": false}}});
        let states = json!([
            {"entity_id": "light.kitchen", "state": "on", "attributes": {"friendly_name": "Luce cucina", "brightness": 200}},
            {"entity_id": "cover.living", "state": "open", "attributes": {"friendly_name": "Tapparella", "device_class": "shutter", "current_position": 100}},
            {"entity_id": "lock.door", "state": "locked", "attributes": {}},
            {"entity_id": "sensor.x", "state": "1", "attributes": {}},
            {"entity_id": "sensor.rm4_temperature", "state": "21", "attributes": {"friendly_name": "RM4 Cucina Temperatura"}}]);
        let exposed = {
            let mut e = exposed;
            e["exposed_entities"]["sensor.rm4_temperature"] = json!({"conversation": true});
            e
        };
        let registry = json!({"entities": [{"ei": "light.kitchen", "ai": "kitchen"}, {"ei": "cover.living", "di": "d1"},
            {"ei": "sensor.rm4_temperature", "en": "Temperatura", "di": "d2"}]});
        let devices = json!([{"id": "d1", "area_id": "living", "name": "Shelly 2PM", "name_by_user": "Tapparella sala"},
            {"id": "d2", "area_id": "kitchen", "name": "RM4 Cucina"}]);
        let areas = json!([{"area_id": "kitchen", "name": "Cucina"}, {"area_id": "living", "name": "Soggiorno"}]);
        let e = entities_from(&exposed, &states, &registry, &devices, &areas);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].entity_id, "light.kitchen");
        assert_eq!(e[0].area, "Cucina");
        assert_eq!(e[0].attributes["brightness"], 200);
        // No registry name, no device: the old friendly_name.
        assert_eq!((e[0].name.as_str(), e[0].device.as_str()), ("Luce cucina", ""));
        // The entity's own name, with its device beside it.
        assert_eq!(e[1].entity_id, "sensor.rm4_temperature");
        assert_eq!((e[1].name.as_str(), e[1].device.as_str()), ("Temperatura", "RM4 Cucina"));
        assert_eq!(e[1].full_name(), "RM4 Cucina Temperatura");
        // No own name: the device's, the user's name for it first.
        assert_eq!(e[2].area, "Soggiorno");
        assert_eq!((e[2].name.as_str(), e[2].device.as_str()), ("Tapparella sala", ""));
        assert_eq!(e[2].device_class, "shutter");
        assert_eq!(e[2].domain(), "cover");
    }

    #[test]
    fn pushed_token_until_it_expires() {
        let ha = Ha::new(HaCfg {
            url: "http://ha.example:8123".into(),
            token_file: "/nonexistent/ha-token".into(),
        });
        assert!(!ha.has_token());
        ha.set_token("A1".into(), now_secs() + 600);
        assert_eq!(ha.token().unwrap(), "A1");
        ha.set_token("A0".into(), now_secs() + 10);
        assert!(ha.token().is_err());
    }
}
