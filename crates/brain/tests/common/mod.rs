// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the brain's integration tests share: a fake Home Assistant (its WebSocket API, with
//! entities that can be un-exposed, services that fail and a switch that takes it down), the
//! body's side of the control socket, and readers of the brain's events that never sleep.

#![allow(dead_code)]

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use brain::agent::Brain;
use brain::config::Config;
use serde_json::{Value, json};

pub type Todo = Arc<Mutex<Vec<(String, String, String)>>>;
/// Exposed entities: (entity_id, name, device_class); changeable while a test runs.
pub type Exposed = Arc<Mutex<Vec<(String, String, String)>>>;

pub fn exposed(v: &[(&str, &str, &str)]) -> Exposed {
    Arc::new(Mutex::new(
        v.iter()
            .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
            .collect(),
    ))
}

/// What the fake Home Assistant knows and answers.
#[derive(Default, Clone)]
pub struct Fake {
    /// `ai_task.generate_data` answers, in order; the instructions it got are kept.
    pub ai: Arc<Mutex<Vec<String>>>,
    pub asked: Arc<Mutex<Vec<String>>>,
    pub exposed: Exposed,
    pub todo: Todo,
    /// Every command received.
    pub calls: Arc<Mutex<Vec<Value>>>,
    /// Other entities in `get_states` (not exposed): (entity_id, friendly name).
    pub states: Vec<(&'static str, &'static str)>,
    /// The `calendar.get_events` answer (`{"calendar.x": {"events": […]}}`).
    pub calendar: Arc<Mutex<Value>>,
    /// Areas of exposed entities: (entity_id, area name).
    pub areas: Vec<(&'static str, &'static str)>,
    /// Groups among the exposed entities: (entity_id, members).
    pub groups: Vec<(&'static str, Vec<&'static str>)>,
    /// Entities whose `call_service` fails (`success: false`).
    pub failing: Arc<Mutex<Vec<String>>>,
    /// Home Assistant is down: every connection is dropped at once.
    pub down: Arc<AtomicBool>,
}

impl Fake {
    /// Takes an entity out of the ones exposed to the voice assistant.
    pub fn unexpose(&self, entity_id: &str) {
        self.exposed.lock().unwrap().retain(|e| e.0 != entity_id);
    }

    pub fn fail(&self, entity_id: &str) {
        self.failing.lock().unwrap().push(entity_id.to_string());
    }

    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }
}

/// "2026-11-05T17:00:00Z" for a time in seconds (UTC).
pub fn rfc3339(ts: i64) -> String {
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
    } else if p.len() < 65536 {
        f.push(126);
        f.extend_from_slice(&(p.len() as u16).to_be_bytes());
    } else {
        f.push(127);
        f.extend_from_slice(&(p.len() as u64).to_be_bytes());
    }
    f.extend_from_slice(&p);
    let _ = s.write_all(&f);
}

fn answer(f: &Fake, cmd: &Value) -> Result<Value, String> {
    f.calls.lock().unwrap().push(cmd.clone());
    if cmd["type"] == "call_service"
        && let Some(id) = cmd["service_data"]["entity_id"].as_str()
        && f.failing.lock().unwrap().iter().any(|x| x == id)
    {
        return Err(format!("{id} is unavailable"));
    }
    let mut t = f.todo.lock().unwrap();
    let exposed = f.exposed.lock().unwrap().clone();
    Ok(
        match (
            cmd["type"].as_str(),
            cmd["domain"].as_str(),
            cmd["service"].as_str(),
        ) {
            (Some("ai_task/preferences/get"), _, _) => {
                json!({"gen_data_entity_id": "ai_task.test"})
            }
            (Some("call_service"), Some("ai_task"), Some("generate_data")) => {
                f.asked.lock().unwrap().push(
                    cmd["service_data"]["instructions"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                );
                let mut q = f.ai.lock().unwrap();
                let data = if q.is_empty() {
                    String::new()
                } else {
                    q.remove(0)
                };
                json!({"response": {"data": data, "conversation_id": "c"}})
            }
            (Some("homeassistant/expose_entity/list"), _, _) => {
                let m: serde_json::Map<String, Value> = exposed
                    .iter()
                    .map(|(id, _, _)| (id.to_string(), json!({"conversation": true})))
                    .collect();
                json!({"exposed_entities": m})
            }
            (Some("get_states"), _, _) => json!(
                exposed
                    .iter()
                    .map(|(id, name, class)| {
                        let mut a = json!({"friendly_name": name, "device_class": class});
                        if let Some((_, m)) = f.groups.iter().find(|(g, _)| g == id) {
                            a["entity_id"] = json!(m);
                        }
                        json!({"entity_id": id, "state": "open", "attributes": a})
                    })
                    .chain(f.states.iter().map(|(id, name)| {
                        json!({"entity_id": id, "state": "idle",
                    "attributes": {"friendly_name": name}})
                    }))
                    .collect::<Vec<_>>()
            ),
            (Some("call_service"), Some("calendar"), Some("get_events")) => {
                json!({"context": {}, "response": f.calendar.lock().unwrap().clone()})
            }
            (Some("config/entity_registry/list_for_display"), _, _) => json!({"entities":
            f.areas.iter().map(|(id, a)| json!({"ei": id, "ai": a.to_lowercase().replace(' ', "_")})).collect::<Vec<_>>()}),
            (Some("config/area_registry/list"), _, _) => {
                json!(
                    f.areas
                        .iter()
                        .map(|(_, a)| json!({"area_id": a.to_lowercase().replace(' ', "_"), "name": a}))
                        .collect::<Vec<_>>()
                )
            }
            (Some("config/device_registry/list"), _, _) => json!([]),
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
        },
    )
}

/// A fake Home Assistant: any number of commands on each connection, answered in order.
pub fn fake_ha(f: Fake) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut s) = c else { break };
            if f.down.load(Ordering::SeqCst) {
                drop(s);
                continue;
            }
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
                    let reply = match answer(&f, &cmd) {
                        Ok(result) => {
                            json!({"id": cmd["id"], "type": "result", "success": true, "result": result})
                        }
                        Err(e) => json!({"id": cmd["id"], "type": "result", "success": false,
                            "error": {"code": "home_assistant_error", "message": e}}),
                    };
                    ws_send(&mut s, &reply);
                }
            });
        }
    });
    url
}

pub fn setup(name: &str, ha_url: &str) -> (Config, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "home-companion-brain-test-{name}-{}",
        std::process::id()
    ));
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

pub fn tool(b: &Brain, name: &str, args: Value) -> (Value, bool) {
    b.mcp_call(name, &args)
}

/// The services Home Assistant was asked for: "domain.service entity_id".
pub fn services(f: &Fake) -> Vec<String> {
    f.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c["type"] == "call_service")
        .map(|c| {
            format!(
                "{}.{} {}",
                c["domain"].as_str().unwrap(),
                c["service"].as_str().unwrap(),
                c["service_data"]["entity_id"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// The body's side of the control socket: a `subscribe` connection reading the brain's events.
pub struct Screen {
    r: std::io::BufReader<UnixStream>,
}

impl Screen {
    pub fn connect(b: &Arc<Brain>, dir: &std::path::Path) -> Screen {
        let hub = Arc::new(brain::control::Hub::default());
        b.set_hub(hub.clone());
        let path = dir.join("c.sock").to_string_lossy().to_string();
        brain::control::start(b.clone(), hub.clone(), &path).unwrap();
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(b"{\"cmd\":\"subscribe\"}\n").unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut r = std::io::BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("\"ok\":true"), "{line}");
        // The connection joins the hub after the replay: wait for it.
        for _ in 0..100 {
            if hub.subscribers() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(hub.subscribers(), 1);
        Screen { r }
    }

    /// The next event named `name` (others are skipped); panics after 5 s.
    pub fn event(&mut self, name: &str) -> Value {
        loop {
            let mut line = String::new();
            assert!(
                self.r.read_line(&mut line).unwrap() > 0,
                "closed while waiting for {name}"
            );
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            if v["event"] == name {
                return v;
            }
        }
    }
}

/// A subscriber added straight to a hub: what the brain broadcast is read at once, without
/// waiting (the brain writes its events before the call that caused them returns).
pub struct Events {
    s: UnixStream,
    pending: Vec<u8>,
}

impl Events {
    pub fn join(hub: &brain::control::Hub) -> Events {
        let (mine, theirs) = UnixStream::pair().unwrap();
        hub.add_subscriber(theirs);
        mine.set_nonblocking(true).unwrap();
        Events {
            s: mine,
            pending: Vec::new(),
        }
    }

    /// Every event received since the last call (none: empty).
    pub fn drain(&mut self) -> Vec<Value> {
        let mut b = [0u8; 4096];
        loop {
            match self.s.read(&mut b) {
                Ok(0) => break,
                Ok(n) => self.pending.extend_from_slice(&b[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("events: {e}"),
            }
        }
        let mut out = Vec::new();
        while let Some(i) = self.pending.iter().position(|c| *c == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=i).collect();
            out.push(serde_json::from_slice(&line).unwrap());
        }
        out
    }
}
