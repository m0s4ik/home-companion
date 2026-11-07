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
    Timer(u64),
    /// A `timer_cancel` event: stop the timer set last (false) or every timer (true).
    TimerCancel(bool),
    /// An `alarm` event: a local alarm (hour, minute, days as in alarms.rs; 0 = once).
    Alarm(u8, u8, u8),
    /// The subscription is up (true) or lost (false).
    Present(bool),
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
                        json!({"cmd": "ha.token", "token": token, "expires_at": expires_at,
                                "url": url, "satellite_entity": st.satellite,
                                "allow": st.ha_addresses, "language": lang})
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
                let msg = match v.get("event").and_then(Value::as_str) {
                    Some("notify") => BrainMsg::Notify(v),
                    Some("timer") => match v.get("seconds").and_then(Value::as_u64) {
                        Some(s) if s > 0 => BrainMsg::Timer(s),
                        _ => continue,
                    },
                    Some("timer_cancel") => BrainMsg::TimerCancel(
                        v.get("all").and_then(Value::as_bool).unwrap_or(false),
                    ),
                    Some("alarm") => {
                        let n = |k: &str| v.get(k).and_then(Value::as_u64).map(|x| x as u8);
                        match (n("hour"), n("minute")) {
                            (Some(h), Some(m)) if h < 24 && m < 60 => {
                                BrainMsg::Alarm(h, m, n("days").unwrap_or(0))
                            }
                            _ => continue,
                        }
                    }
                    _ => continue,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

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
