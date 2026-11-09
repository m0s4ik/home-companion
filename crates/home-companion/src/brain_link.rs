// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's side of home-companion-brain's control socket (crates/brain/src/control.rs): one JSON line
//! per request and answer. Two threads, never the UI thread: one sends the requests the UI
//! queues (a fresh connection each, 5 s at most), one keeps a `subscribe` connection open for
//! the reminder cards (`notify` events), reconnecting every 10 s while the brain is away. The UI
//! reads what came back with [`BrainLink::try_recv`] from its own loop.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use serde_json::{Value, json};

pub const SOCKET: &str = "/run/home-companion-brain/brain.sock";

/// What the brain said.
#[derive(Debug, Clone, PartialEq)]
pub enum BrainMsg {
    /// The answer to a request (`cmd` as sent); `None` = the brain did not answer.
    Reply { cmd: String, answer: Option<Value> },
    /// A `notify` event (a reminder, the morning summary, missed reminders).
    Notify(Value),
    /// A `timer` event: start a local timer of this many seconds (asked by voice through the brain).
    /// Seconds and what it is for ("Pasta"), when said.
    Timer(u64, Option<String>),
    /// A `timer_cancel` event: stop the timer set last (false) or every timer (true).
    TimerCancel(bool),
    /// An `alarm` event: a local alarm (hour, minute, days as in alarms.rs; 0 = once).
    Alarm(u8, u8, u8),
    /// A house command went through (`changed`): a "done" card, with "Undo" when `undo`.
    Changed(Changed),
    /// A house command waits for a yes (`confirm`): a Yes/No card until `expires_at` (unix s).
    Confirm {
        id: i64,
        text: String,
        expires_at: i64,
    },
    /// The question `id` is over (`confirm_done`): answered (`Some`) or expired (`None`).
    ConfirmDone { id: i64, yes: Option<bool> },
    /// Open Home Assistant's view there (`show_view`).
    ShowView(View),
    /// The calendar events for the "next" card, soonest first (`upcoming`).
    Upcoming(Vec<Upcoming>),
    /// The subscription is up (true) or lost (false).
    Present(bool),
}

/// A house command the brain carried out (BrainMsg::Changed).
#[derive(Debug, Clone, PartialEq)]
pub struct Changed {
    pub id: i64,
    pub entity_id: String,
    pub name: String,
    pub service: String,
    /// The entity's state afterwards, when Home Assistant had it already.
    pub state: Option<String>,
    pub undo: bool,
}

/// Where to open Home Assistant's view (BrainMsg::ShowView).
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// A dashboard path ("/lovelace/energy").
    Path(String),
    /// A `camera.*` entity.
    Camera(String),
}

/// One calendar event (BrainMsg::Upcoming).
#[derive(Debug, Clone, PartialEq)]
pub struct Upcoming {
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub title: String,
    pub calendar: String,
}

/// The screen's Yes or No on the brain's question `id` (send it with [`BrainLink::send`]).
pub fn confirm_answer(id: i64, yes: bool) -> Value {
    json!({"cmd": "confirm.answer", "id": id, "yes": yes})
}

/// The screen's "Undo" on the house command `id`.
pub fn undo(id: i64) -> Value {
    json!({"cmd": "control.undo", "id": id})
}

/// One event line of the subscription (crates/brain/src/events.rs is the other side; both
/// test against crates/brain/contract/events.jsonl). None: unknown or malformed, skipped.
pub fn parse_event(v: Value) -> Option<BrainMsg> {
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    let int_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64);
    Some(match v.get("event").and_then(Value::as_str)? {
        "notify" => BrainMsg::Notify(v),
        "timer" => match v.get("seconds").and_then(Value::as_u64) {
            Some(s) if s > 0 => {
                BrainMsg::Timer(s, timer_name(v.get("name").and_then(Value::as_str)))
            }
            _ => return None,
        },
        "timer_cancel" => {
            BrainMsg::TimerCancel(v.get("all").and_then(Value::as_bool).unwrap_or(false))
        }
        "alarm" => {
            let n = |k: &str| v.get(k).and_then(Value::as_u64).map(|x| x as u8);
            match (n("hour"), n("minute")) {
                (Some(h), Some(m)) if h < 24 && m < 60 => {
                    BrainMsg::Alarm(h, m, n("days").unwrap_or(0))
                }
                _ => return None,
            }
        }
        "changed" => BrainMsg::Changed(Changed {
            id: int_of(&v, "id")?,
            entity_id: str_of(&v, "entity_id")?,
            name: str_of(&v, "name").unwrap_or_default(),
            service: str_of(&v, "service").unwrap_or_default(),
            state: str_of(&v, "state"),
            undo: v.get("undo").and_then(Value::as_bool).unwrap_or(false),
        }),
        "confirm" => BrainMsg::Confirm {
            id: int_of(&v, "id")?,
            text: str_of(&v, "text")?,
            expires_at: int_of(&v, "expires_at").unwrap_or(0),
        },
        "confirm_done" => BrainMsg::ConfirmDone {
            id: int_of(&v, "id")?,
            yes: v.get("yes").and_then(Value::as_bool),
        },
        "show_view" => match (str_of(&v, "path"), str_of(&v, "camera")) {
            (Some(p), _) if p.starts_with('/') => BrainMsg::ShowView(View::Path(p)),
            (_, Some(c)) if c.starts_with("camera.") => BrainMsg::ShowView(View::Camera(c)),
            _ => return None,
        },
        "upcoming" => BrainMsg::Upcoming(
            v.get("items")?
                .as_array()?
                .iter()
                .filter_map(|i| {
                    Some(Upcoming {
                        start: int_of(i, "start")?,
                        end: int_of(i, "end").unwrap_or(0),
                        all_day: i.get("all_day").and_then(Value::as_bool).unwrap_or(false),
                        title: str_of(i, "title")?,
                        calendar: str_of(i, "calendar").unwrap_or_default(),
                    })
                })
                .collect(),
        ),
        _ => return None,
    })
}

pub struct BrainLink {
    requests: Sender<Value>,
    incoming: Receiver<BrainMsg>,
}

/// The socket: `HOME_COMPANION_BRAIN_SOCKET` (tests, the simulator) or [`SOCKET`].
pub fn socket_path() -> PathBuf {
    std::env::var_os("HOME_COMPANION_BRAIN_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(SOCKET))
}

/// One request on a fresh connection.
pub fn request(path: &std::path::Path, req: &Value) -> Option<Value> {
    let mut s = UnixStream::connect(path).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
    writeln!(s, "{req}").ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

impl BrainLink {
    pub fn start() -> BrainLink {
        Self::start_at(socket_path())
    }

    pub fn start_at(path: PathBuf) -> BrainLink {
        let (req_tx, req_rx) = channel::<Value>();
        let (tx, rx) = channel::<BrainMsg>();
        let (p, t) = (path.clone(), tx.clone());
        std::thread::Builder::new()
            .name("brain-req".into())
            .spawn(move || {
                for r in req_rx {
                    let cmd = r
                        .get("cmd")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let answer = request(&p, &r);
                    if t.send(BrainMsg::Reply { cmd, answer }).is_err() {
                        break;
                    }
                }
            })
            .ok();
        std::thread::Builder::new()
            .name("brain-sub".into())
            .spawn(move || subscribe_loop(&path, &tx))
            .ok();
        BrainLink {
            requests: req_tx,
            incoming: rx,
        }
    }

    pub fn send(&self, req: Value) {
        let _ = self.requests.send(req);
    }

    pub fn ask(&self, cmd: &str) {
        self.send(json!({"cmd": cmd}));
    }

    pub fn try_recv(&self) -> Option<BrainMsg> {
        self.incoming.try_recv().ok()
    }
}

/// What the user said (`heard`, the satellite's transcript) and Home Assistant's answer
/// (`said`), for the brain: a spoken "sì" confirms a house command, the exchanges become
/// memories. A short thread each, never blocking the caller; lost when the brain is away.
pub fn tell(cmd: &'static str, text: &str) {
    if text.trim().is_empty() || std::env::var_os("HOME_COMPANION_NO_BRAIN").is_some() {
        return;
    }
    let req = json!({"cmd": cmd, "text": text});
    let path = socket_path();
    if !path.exists() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("brain-tell".into())
        .spawn(move || {
            let _ = request(&path, &req);
        });
}

/// How often the body hands its Home Assistant token to the brain (access tokens last 30 min;
/// the body refreshes them a minute before they expire): every minute, so a restarted brain
/// has it again soon (a local message; the refresh itself only when due).
const TOKEN_EVERY: Duration = Duration::from_secs(60);

/// The body's Home Assistant token for the brain (`ha.token`), now and every few minutes, on
/// its own thread: the brain uses the QR link's tokens and needs none of its own. A long-lived
/// token (`token_file`) is passed on as valid for 15 min, so it is handed over again in time.
/// With the token go Home Assistant's address and the satellite the setup found
/// (`<state_dir>/ha-setup.json`), so brain.toml needs neither, and the language in use (the
/// one chosen on the screen, read again each time from `settings`; else `language`).
pub fn feed_ha_token(
    path: PathBuf,
    auth: std::sync::Arc<crate::ha::auth::Auth>,
    url: String,
    setup_state: PathBuf,
    settings: crate::settings::Settings,
    language: String,
) {
    let _ = std::thread::Builder::new()
        .name("brain-token".into())
        .spawn(move || {
            let mut told = None;
            loop {
                let token = match auth.linked_tokens() {
                    Ok(Some(t)) => Ok((t.access_token, t.expires_at)),
                    Ok(None) => auth.access_token().map(|t| (t, now_secs() + 900)),
                    Err(e) => Err(e),
                };
                let sent = match token {
                    Ok((token, expires_at)) => request(&path, &{
                        let st = crate::ha::setup::State::load(&setup_state);
                        let lang = settings.language().unwrap_or_else(|| language.clone());
                        ha_token_request(
                            &token,
                            expires_at,
                            &url,
                            &st.satellite,
                            &st.ha_addresses,
                            &lang,
                            &settings.lists(),
                            &settings.calendars(),
                        )
                    })
                    .is_some_and(|a| a.get("ok") == Some(&Value::Bool(true))),
                    Err(e) => {
                        log::debug!("brain token: {e}");
                        false
                    }
                };
                if told != Some(sent) {
                    if sent {
                        log::info!("brain: Home Assistant token handed over");
                    }
                    told = Some(sent);
                }
                std::thread::sleep(if sent {
                    TOKEN_EVERY
                } else {
                    Duration::from_secs(10)
                });
            }
        });
}

/// The `ha.token` request: the token with Home Assistant's address, the satellite, the allowed
/// addresses, the language and the lists and calendars the user chose on the screen (empty =
/// all of them).
#[allow(clippy::too_many_arguments)]
pub fn ha_token_request(
    token: &str,
    expires_at: u64,
    url: &str,
    satellite: &str,
    allow: &[String],
    language: &str,
    lists: &[String],
    calendars: &[String],
) -> Value {
    json!({"cmd": "ha.token", "token": token, "expires_at": expires_at, "url": url,
           "satellite_entity": satellite, "allow": allow, "language": language,
           "lists": lists, "calendars": calendars})
}

/// What the brain says about its tools (Settings → Home Assistant): None = it does not answer.
pub fn brain_info() -> Option<crate::ha::setup::BrainInfo> {
    let st = request(&socket_path(), &json!({"cmd": "status"}))?;
    (st.get("ok") == Some(&Value::Bool(true))).then(|| crate::ha::setup::BrainInfo {
        tools_listed_at: st
            .get("tools_listed_at")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        tools_hash: st
            .get("tools_hash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn subscribe_loop(path: &std::path::Path, tx: &Sender<BrainMsg>) {
    let mut present = None;
    loop {
        let up = (|| -> std::io::Result<()> {
            let mut s = UnixStream::connect(path)?;
            writeln!(s, "{}", json!({"cmd": "subscribe"}))?;
            let mut r = BufReader::new(s);
            let mut line = String::new();
            r.read_line(&mut line)?;
            let ok: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
            if ok.get("ok") != Some(&Value::Bool(true)) {
                return Err(std::io::Error::other("subscribe refused"));
            }
            if present != Some(true) {
                present = Some(true);
                let _ = tx.send(BrainMsg::Present(true));
            }
            loop {
                line.clear();
                if r.read_line(&mut line)? == 0 {
                    return Ok(());
                }
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                let Some(msg) = parse_event(v) else {
                    continue;
                };
                if tx.send(msg).is_err() {
                    return Ok(());
                }
            }
        })();
        if let Err(e) = up {
            log::debug!("brain link: {e}");
        }
        if present != Some(false) {
            present = Some(false);
            if tx.send(BrainMsg::Present(false)).is_err() {
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// A timer's name as the screen shows the reflexes' ones: trimmed, first letter capital
/// ("pasta" → "Pasta"); None when empty.
pub fn timer_name(n: Option<&str>) -> Option<String> {
    let n = n?.trim();
    let mut c = n.chars();
    let first = c.next()?;
    Some(first.to_uppercase().chain(c).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_timer_from_the_brain_keeps_its_name() {
        use super::{BrainMsg, parse_event};
        use serde_json::json;
        let t = |v| match parse_event(v) {
            Some(BrainMsg::Timer(s, n)) => (s, n),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            t(json!({"event": "timer", "seconds": 60, "name": "pasta"})),
            (60, Some("Pasta".into()))
        );
        assert_eq!(
            t(json!({"event": "timer", "seconds": 60, "name": " tè "})),
            (60, Some("Tè".into()))
        );
        assert_eq!(t(json!({"event": "timer", "seconds": 30})), (30, None));
        assert_eq!(
            t(json!({"event": "timer", "seconds": 30, "name": ""})),
            (30, None)
        );
    }

    #[test]
    fn ha_token_request_carries_the_choices() {
        let r = super::ha_token_request(
            "t",
            1794150000,
            "http://ha.example:8123",
            "assist_satellite.x",
            &["192.0.2.10".to_string()],
            "it",
            &["todo.shopping".to_string(), "todo.bring_home".to_string()],
            &[],
        );
        // The same shape as the brain's contract (crates/brain/contract).
        let line = include_str!("../../brain/contract/requests.jsonl")
            .lines()
            .find(|l| l.contains("ha.token"))
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap());
        if let Some(c) = line {
            assert_eq!(r, c);
        }
        assert_eq!(r["calendars"], serde_json::json!([]));
        assert_eq!(r["lists"][1], "todo.bring_home");
    }
    use super::*;
    use std::os::unix::net::UnixListener;

    fn contract(file: &str) -> Vec<Value> {
        file.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// The brain's examples (crates/brain/contract) parse into what the screen expects.
    #[test]
    fn contract_events_parse() {
        let got: Vec<BrainMsg> = contract(include_str!("../../brain/contract/events.jsonl"))
            .into_iter()
            .map(|v| parse_event(v.clone()).unwrap_or_else(|| panic!("{v}")))
            .collect();
        assert_eq!(
            got,
            vec![
                BrainMsg::Changed(Changed {
                    id: 41,
                    entity_id: "light.kitchen".into(),
                    name: "Kitchen light".into(),
                    service: "turn_off".into(),
                    state: Some("off".into()),
                    undo: true,
                }),
                BrainMsg::Confirm {
                    id: 42,
                    text: "Living room blind: close it?".into(),
                    expires_at: 1_794_150_000,
                },
                BrainMsg::ConfirmDone {
                    id: 42,
                    yes: Some(true)
                },
                BrainMsg::ConfirmDone { id: 43, yes: None },
                BrainMsg::ShowView(View::Path("/lovelace/energy".into())),
                BrainMsg::ShowView(View::Camera("camera.front_door".into())),
                BrainMsg::Upcoming(vec![Upcoming {
                    start: 1_794_150_000,
                    end: 1_794_153_600,
                    all_day: false,
                    title: "Dentist".into(),
                    calendar: "calendar.family".into(),
                }]),
            ]
        );
        assert_eq!(
            parse_event(json!({"event": "show_view", "path": "lovelace"})),
            None
        );
        assert_eq!(
            parse_event(json!({"event": "show_view", "camera": "light.x"})),
            None
        );
        assert_eq!(parse_event(json!({"event": "new_in_v9"})), None);
    }

    #[test]
    fn contract_requests_match() {
        let req = contract(include_str!("../../brain/contract/requests.jsonl"));
        assert_eq!(confirm_answer(42, true), req[0]);
        assert_eq!(undo(41), req[1]);
    }

    #[test]
    fn requests_and_events() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-brain-link-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("brain.sock");
        let _ = std::fs::remove_file(&path);
        let l = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let mut c = c.unwrap();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(c.try_clone().unwrap());
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let v: Value = serde_json::from_str(&line).unwrap();
                    match v["cmd"].as_str().unwrap() {
                        "subscribe" => {
                            writeln!(c, "{}", json!({"ok": true})).unwrap();
                            writeln!(c, "{}", json!({"event": "notify", "id": 7, "text": "pane"}))
                                .unwrap();
                            std::thread::sleep(Duration::from_secs(2));
                        }
                        _ => writeln!(c, "{}", json!({"ok": true, "facts": 3})).unwrap(),
                    }
                });
            }
        });
        let link = BrainLink::start_at(path);
        link.ask("status");
        let mut got = Vec::new();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let done = |got: &[BrainMsg]| {
            got.contains(&BrainMsg::Present(true))
                && got.iter().any(|m| matches!(m, BrainMsg::Notify(_)))
                && got.iter().any(|m| matches!(m, BrainMsg::Reply { .. }))
        };
        while !done(&got) && std::time::Instant::now() < until {
            match link.try_recv() {
                Some(m) => got.push(m),
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(got.contains(&BrainMsg::Present(true)), "{got:?}");
        assert!(
            got.iter()
                .any(|m| matches!(m, BrainMsg::Notify(v) if v["id"] == 7))
        );
        assert!(got.iter().any(|m| matches!(m, BrainMsg::Reply { cmd, answer: Some(a) } if cmd == "status" && a["facts"] == 3)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
