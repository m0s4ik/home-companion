// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The shopping list on the screen (`[lists] shopping`, any `todo.*` entity of Home Assistant:
//! its own shopping list, Bring!, …): one thread keeps a WebSocket connection, subscribes to the
//! entity's items (`todo/item/subscribe`, so a change from the phone shows at once) and ticks an
//! item when the screen asks (`todo.update_item`, status `completed`). It reconnects with a
//! growing pause and never touches the UI thread: the UI gets the open items over a channel.

use std::io;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::auth;
use super::ws::{Message, WebSocket};
use super::{Backoff, Endpoint, endpoint};

/// An open item: (uid, what to buy).
pub type Item = (String, String);

/// What the screen asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum ShopCmd {
    /// Bought: the item is completed in Home Assistant.
    Done(String),
}

/// The open items of a `todo/item/subscribe` event (in Home Assistant's order).
pub fn open_items(event: &Value) -> Option<Vec<Item>> {
    let items = event.pointer("/event/items")?.as_array()?;
    Some(
        items
            .iter()
            .filter(|i| i.get("status").and_then(Value::as_str) != Some("completed"))
            .filter_map(|i| {
                Some((
                    i.get("uid")?.as_str()?.to_string(),
                    i.get("summary")?.as_str()?.trim().to_string(),
                ))
            })
            .filter(|(_, s)| !s.is_empty())
            .collect(),
    )
}

pub struct ShopConfig {
    pub url: String,
    pub auth: Arc<auth::Auth>,
    pub entity: String,
}

/// The thread's ends: the open items (each time the list changes), the screen's ticks.
pub type ShopLink = (Receiver<Vec<Item>>, Sender<ShopCmd>, JoinHandle<()>);

/// Starts the thread.
pub fn spawn(cfg: ShopConfig, stop: fn() -> bool) -> io::Result<ShopLink> {
    let (tx, rx) = channel();
    let (cmd_tx, cmd_rx) = channel();
    let h = std::thread::Builder::new()
        .name("ha-shopping".into())
        .spawn(move || run(cfg, tx, cmd_rx, stop))?;
    Ok((rx, cmd_tx, h))
}

fn pause(d: Duration, stop: fn() -> bool) -> bool {
    let until = Instant::now() + d;
    while Instant::now() < until {
        if stop() {
            return false;
        }
        std::thread::sleep((until - Instant::now()).min(Duration::from_millis(250)));
    }
    !stop()
}

fn run(cfg: ShopConfig, tx: Sender<Vec<Item>>, cmds: Receiver<ShopCmd>, stop: fn() -> bool) {
    let ep = match endpoint(&cfg.url) {
        Ok(e) => e,
        Err(e) => {
            log::warn!(event = "ha_shopping"; "shopping list off: home_assistant.url = {e}");
            return;
        }
    };
    log::info!(event = "ha_shopping"; "shopping list: {}", cfg.entity);
    let mut backoff = Backoff::new(Duration::from_secs(2), Duration::from_secs(300));
    loop {
        if stop() {
            return;
        }
        let token = match cfg.auth.access_token() {
            Ok(t) => t,
            Err(e) => {
                log::debug!("shopping list: {e}");
                if !pause(Duration::from_secs(60), stop) {
                    return;
                }
                continue;
            }
        };
        let started = Instant::now();
        match session(&cfg, &ep, &token, &tx, &cmds, stop) {
            Ok(()) => return,
            Err(why) => {
                if started.elapsed() >= Duration::from_secs(60) {
                    backoff.reset();
                }
                log::info!(event = "ha_shopping"; "shopping list: {why}, retrying");
            }
        }
        if !pause(backoff.next_delay(), stop) {
            return;
        }
    }
}

/// One connection; Ok = stop (or the UI is gone), Err = lost.
fn session(
    cfg: &ShopConfig,
    ep: &Endpoint,
    token: &str,
    tx: &Sender<Vec<Item>>,
    cmds: &Receiver<ShopCmd>,
    stop: fn() -> bool,
) -> Result<(), String> {
    let lost = |e: io::Error| format!("{}:{}: {e}", ep.host, ep.port);
    let mut ws: WebSocket<TcpStream> =
        WebSocket::connect(&ep.host, ep.port, &ep.path, Duration::from_secs(10)).map_err(lost)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let msg = match ws.read().map_err(lost)? {
            Some(Message::Text(t)) => t,
            Some(Message::Close) => return Err("closed during authentication".into()),
            None if stop() => return Ok(()),
            None if Instant::now() < deadline => continue,
            None => return Err("no answer to the authentication".into()),
        };
        let v: Value = serde_json::from_str(&msg).unwrap_or(Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("auth_required") => {
                let auth = json!({"type": "auth", "access_token": token}).to_string();
                ws.send_text(&auth).map_err(lost)?;
            }
            Some("auth_ok") => break,
            Some("auth_invalid") => return Err("token refused".into()),
            _ => {}
        }
    }
    let mut next_id = 2u64;
    let sub = json!({"id": 1, "type": "todo/item/subscribe", "entity_id": cfg.entity});
    ws.send_text(&sub.to_string()).map_err(lost)?;
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    loop {
        if stop() {
            ws.close();
            return Ok(());
        }
        loop {
            match cmds.try_recv() {
                Ok(ShopCmd::Done(uid)) => {
                    let id = next_id;
                    next_id += 1;
                    let c = json!({"id": id, "type": "call_service", "domain": "todo",
                        "service": "update_item", "service_data": {"item": uid, "status": "completed"},
                        "target": {"entity_id": cfg.entity}});
                    ws.send_text(&c.to_string()).map_err(lost)?;
                    log::info!(event = "ha_shopping"; "item ticked on the screen");
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    ws.close();
                    return Ok(());
                }
            }
        }
        match ws.read().map_err(lost)? {
            Some(Message::Text(t)) => {
                last_rx = Instant::now();
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                if v.get("id").and_then(Value::as_u64) == Some(1) {
                    if v.get("type").and_then(Value::as_str) == Some("result")
                        && v.get("success").and_then(Value::as_bool) == Some(false)
                    {
                        let why = v
                            .pointer("/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("refused");
                        return Err(format!("{}: {why}", cfg.entity));
                    }
                    if let Some(items) = open_items(&v)
                        && tx.send(items).is_err()
                    {
                        ws.close();
                        return Ok(());
                    }
                }
            }
            Some(Message::Close) => return Err("Home Assistant closed the connection".into()),
            None => {}
        }
        if last_rx.elapsed() >= Duration::from_secs(90) {
            return Err("no message from Home Assistant".into());
        }
        if last_ping.elapsed() >= Duration::from_secs(30) {
            last_ping = Instant::now();
            let id = next_id;
            next_id += 1;
            ws.send_text(&json!({"id": id, "type": "ping"}).to_string())
                .map_err(lost)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_items_skip_completed_and_empty() {
        let ev = json!({"id": 1, "type": "event", "event": {"items": [
            {"uid": "a", "summary": "Pane", "status": "needs_action"},
            {"uid": "b", "summary": "Latte", "status": "completed"},
            {"uid": "c", "summary": "  ", "status": "needs_action"},
            {"uid": "d", "summary": "2 kg farina", "status": "needs_action"}]}});
        assert_eq!(
            open_items(&ev).unwrap(),
            vec![
                ("a".into(), "Pane".into()),
                ("d".into(), "2 kg farina".into())
            ]
        );
        assert!(open_items(&json!({"type": "result"})).is_none());
    }

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn read_frame(s: &mut TcpStream, buf: &mut Vec<u8>) -> Value {
        loop {
            if let Some(fr) = super::super::ws::decode_frame(buf).unwrap() {
                buf.drain(..fr.used);
                if fr.opcode == 1 {
                    return serde_json::from_slice(&fr.payload).unwrap();
                }
                continue;
            }
            let mut c = [0u8; 4096];
            let n = s.read(&mut c).unwrap();
            assert!(n > 0, "client closed");
            buf.extend_from_slice(&c[..n]);
        }
    }

    fn send(s: &mut TcpStream, v: &Value) {
        let text = v.to_string();
        let p = text.as_bytes();
        let mut f = vec![0x81u8];
        if p.len() < 126 {
            f.push(p.len() as u8);
        } else {
            f.push(126);
            f.extend_from_slice(&(p.len() as u16).to_be_bytes());
        }
        f.extend_from_slice(p);
        s.write_all(&f).unwrap();
    }

    fn no_stop() -> bool {
        false
    }

    #[test]
    fn subscribes_shows_items_and_ticks_one() {
        let dir = std::env::temp_dir().join(format!("home-companion-todo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("ha-token"), "tok\n").unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let auth = Arc::new(auth::Auth::new(&url, &dir.join("ha-token"), &dir));
        let cfg = ShopConfig {
            url,
            auth,
            entity: "todo.spesa".into(),
        };
        let (rx, tx, _) = spawn(cfg, no_stop).unwrap();
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = Vec::new();
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut c = [0u8; 1024];
            let n = s.read(&mut c).unwrap();
            buf.extend_from_slice(&c[..n]);
        }
        s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            .unwrap();
        let mut buf = Vec::new();
        send(&mut s, &json!({"type": "auth_required"}));
        assert_eq!(read_frame(&mut s, &mut buf)["access_token"], "tok");
        send(&mut s, &json!({"type": "auth_ok"}));
        let sub = read_frame(&mut s, &mut buf);
        assert_eq!(sub["type"], "todo/item/subscribe");
        assert_eq!(sub["entity_id"], "todo.spesa");
        send(&mut s, &json!({"id": 1, "type": "result", "success": true}));
        let items = |done_a: bool| {
            json!({"id": 1, "type": "event", "event": {"items": [
            {"uid": "a", "summary": "Pane", "status": if done_a { "completed" } else { "needs_action" }},
            {"uid": "b", "summary": "Latte", "status": "needs_action"}]}})
        };
        send(&mut s, &items(false));
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got.len(), 2);
        // A tick on the screen: todo.update_item, then the phone's (and our) change comes back.
        tx.send(ShopCmd::Done("a".into())).unwrap();
        let call = read_frame(&mut s, &mut buf);
        assert_eq!(call["type"], "call_service");
        assert_eq!(call["service"], "update_item");
        assert_eq!(
            call["service_data"],
            json!({"item": "a", "status": "completed"})
        );
        assert_eq!(call["target"]["entity_id"], "todo.spesa");
        send(&mut s, &items(true));
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, vec![("b".to_string(), "Latte".to_string())]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
