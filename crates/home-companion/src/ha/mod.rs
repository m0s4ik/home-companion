// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Home Assistant's WebSocket API (`/api/websocket`), for what Wyoming does not carry. Today
//! only the weather of the home screen: one thread keeps a connection, authenticates with a
//! token from [`auth::Auth`] (the QR-code link's, refreshed, or `home_assistant.token_file`),
//! subscribes to the weather entity and its daily forecast, and sends the UI what to show
//! (`None` = no real data: the weather box is hidden). It reconnects with a growing pause and
//! never touches the UI or the audio threads.
//!
//! Built to grow: the connection ([`ws`]), the tokens ([`auth`]), the link ([`link`]) and the
//! message ids live here.

pub mod auth;
pub mod discover;
pub mod link;
pub mod setup;
pub mod todo;
pub mod weather;
pub mod ws;

use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use weather::{Weather, WeatherModel};
use ws::{Message, WebSocket};

/// Where Home Assistant's WebSocket API is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// `home_assistant.url` ("http://homeassistant.example.home:8123", also `ws://`) → endpoint.
/// TLS (`https://`, `wss://`) is not supported yet.
pub fn endpoint(url: &str) -> Result<Endpoint, String> {
    let url = url.trim().trim_end_matches('/');
    let (rest, default_port) = if let Some(r) = url.strip_prefix("http://") {
        (r, 80)
    } else if let Some(r) = url.strip_prefix("ws://") {
        (r, 80)
    } else if url.starts_with("https://") || url.starts_with("wss://") {
        return Err(format!(
            "{url}: TLS is not supported yet (use http:// in the LAN)"
        ));
    } else {
        return Err(format!("{url}: expected http://host:port"));
    };
    let (authority, base) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let base = base.trim_end_matches("/api/websocket");
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || authority.starts_with('[') => (
            h,
            p.parse::<u16>()
                .map_err(|_| format!("{url}: bad port {p:?}"))?,
        ),
        _ => (authority, default_port),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(format!("{url}: no host"));
    }
    Ok(Endpoint {
        host: host.to_string(),
        port,
        path: format!("{base}/api/websocket"),
    })
}

/// The pause before the next connection: doubles from `first` up to `max`; back to `first`
/// after a connection that worked for a while.
#[derive(Debug, Clone)]
pub struct Backoff {
    first: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    pub fn new(first: Duration, max: Duration) -> Backoff {
        Backoff {
            first,
            max,
            next: first,
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        d
    }

    pub fn reset(&mut self) {
        self.next = self.first;
    }
}

/// What the weather thread needs.
#[derive(Debug, Clone)]
pub struct WeatherConfig {
    pub url: String,
    pub auth: Arc<auth::Auth>,
    pub entity: String,
    /// First and longest pause between connection attempts.
    pub backoff: (Duration, Duration),
    /// A connection that lasted this long resets the pause.
    pub stable_after: Duration,
    /// A ping every `ping_every`; no message for `dead_after` = reconnect.
    pub ping_every: Duration,
    pub dead_after: Duration,
}

impl WeatherConfig {
    pub fn new(url: &str, auth: Arc<auth::Auth>, entity: &str) -> WeatherConfig {
        WeatherConfig {
            url: url.to_string(),
            auth,
            entity: entity.to_string(),
            backoff: (Duration::from_secs(2), Duration::from_secs(300)),
            stable_after: Duration::from_secs(60),
            ping_every: Duration::from_secs(30),
            dead_after: Duration::from_secs(75),
        }
    }
}

/// Today's local date, "2026-11-04".
pub fn local_date() -> String {
    // SAFETY: time(NULL) and localtime_r only write into the struct we pass.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        )
    }
}

/// Why a connection ended.
#[derive(Debug)]
enum End {
    /// Stop requested or nobody listens any more.
    Quit,
    /// The token was refused: wait long, the file may be replaced.
    Auth(String),
    /// Network or protocol: reconnect.
    Lost(String),
}

/// Logs a problem only when it differs from the last one (no spam while Home Assistant is away).
#[derive(Default)]
struct Quiet {
    last: Option<String>,
}

impl Quiet {
    fn warn(&mut self, msg: String) {
        if self.last.as_deref() != Some(msg.as_str()) {
            log::warn!(event = "ha_weather"; "weather: {msg}");
            self.last = Some(msg);
        }
    }

    fn clear(&mut self) {
        self.last = None;
    }
}

/// Starts the weather thread. It sends `Some(weather)` when there is real data and `None` when
/// there is none (any more); nothing is sent while nothing changes.
pub fn spawn_weather(
    cfg: WeatherConfig,
    tx: Sender<Option<Weather>>,
    stop: fn() -> bool,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("ha-weather".into())
        .spawn(move || run_weather(cfg, tx, stop))
}

/// Sleeps `d` in short steps; false if stop was requested.
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

fn run_weather(cfg: WeatherConfig, tx: Sender<Option<Weather>>, stop: fn() -> bool) {
    let ep = match endpoint(&cfg.url) {
        Ok(e) => e,
        Err(e) => {
            log::warn!(event = "ha_weather"; "weather off: home_assistant.url = {e}");
            return;
        }
    };
    log::info!(
        event = "ha_weather";
        "weather: {} from {}:{}{}",
        cfg.entity,
        ep.host,
        ep.port,
        ep.path
    );
    let mut backoff = Backoff::new(cfg.backoff.0, cfg.backoff.1);
    let mut quiet = Quiet::default();
    let mut shown: Option<Weather> = None;
    loop {
        if stop() {
            return;
        }
        // Checked again every minute: a link made on the screen is picked up by itself.
        let token = match cfg.auth.access_token() {
            Ok(t) => t,
            Err(e @ (auth::TokenError::Missing(_) | auth::TokenError::Refused(_))) => {
                quiet.warn(format!("{e}: weather hidden"));
                if !pause(Duration::from_secs(60), stop) {
                    return;
                }
                continue;
            }
            Err(auth::TokenError::Unreachable(e)) => {
                let d = backoff.next_delay();
                quiet.warn(format!("token: {e}: weather hidden, retrying"));
                if !pause(d, stop) {
                    return;
                }
                continue;
            }
        };
        let started = Instant::now();
        let end = session(&cfg, &ep, &token, &tx, &mut shown, &mut quiet, stop);
        // Whatever happened, the data is not current any more.
        if shown.take().is_some() && tx.send(None).is_err() {
            return;
        }
        let wait = match end {
            End::Quit => return,
            End::Auth(why) => {
                quiet.warn(format!(
                    "token refused by Home Assistant ({why}): weather hidden"
                ));
                backoff.reset();
                cfg.backoff.1
            }
            End::Lost(why) => {
                if started.elapsed() >= cfg.stable_after {
                    backoff.reset();
                }
                let d = backoff.next_delay();
                quiet.warn(format!("{why}: weather hidden, retrying"));
                log::debug!("weather: next attempt in {d:?}");
                d
            }
        };
        if !pause(wait, stop) {
            return;
        }
    }
}

/// Sends what the screen should show if it changed; false if the UI is gone.
fn publish(
    model: &WeatherModel,
    tx: &Sender<Option<Weather>>,
    shown: &mut Option<Weather>,
) -> bool {
    let now = model.view(&local_date());
    if now != *shown {
        *shown = now.clone();
        return tx.send(now).is_ok();
    }
    true
}

fn session(
    cfg: &WeatherConfig,
    ep: &Endpoint,
    token: &str,
    tx: &Sender<Option<Weather>>,
    shown: &mut Option<Weather>,
    quiet: &mut Quiet,
    stop: fn() -> bool,
) -> End {
    let lost = |e: io::Error| End::Lost(format!("{}:{}: {e}", ep.host, ep.port));
    let mut ws = match WebSocket::connect(&ep.host, ep.port, &ep.path, Duration::from_secs(10)) {
        Ok(w) => w,
        Err(e) => return lost(e),
    };
    // Authentication: auth_required → auth → auth_ok | auth_invalid (10 s at most).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let msg = match ws.read() {
            Ok(Some(Message::Text(t))) => t,
            Ok(Some(Message::Close)) => return End::Lost("closed during authentication".into()),
            Ok(None) if Instant::now() < deadline && !stop() => continue,
            Ok(None) if stop() => return End::Quit,
            Ok(None) => return End::Lost("no answer to the authentication".into()),
            Err(e) => return lost(e),
        };
        let v: Value = serde_json::from_str(&msg).unwrap_or(Value::Null);
        match v.get("type").and_then(Value::as_str) {
            Some("auth_required") => {
                let auth = json!({"type": "auth", "access_token": token}).to_string();
                if let Err(e) = ws.send_text(&auth) {
                    return lost(e);
                }
            }
            Some("auth_ok") => break,
            Some("auth_invalid") => {
                let why = v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid");
                return End::Auth(why.to_string());
            }
            _ => {}
        }
    }
    let mut next_id = 1u64;
    let mut take_id = || {
        let id = next_id;
        next_id += 1;
        id
    };
    let entities_id = take_id();
    let forecast_id = take_id();
    let mut model = WeatherModel::new(&cfg.entity, entities_id, forecast_id);
    let subscribe_forecast = |id: u64| {
        json!({"id": id, "type": "weather/subscribe_forecast", "entity_id": cfg.entity,
               "forecast_type": "daily"})
        .to_string()
    };
    let subs = [
        json!({"id": entities_id, "type": "subscribe_entities", "entity_ids": [cfg.entity]})
            .to_string(),
        subscribe_forecast(forecast_id),
    ];
    for s in subs {
        if let Err(e) = ws.send_text(&s) {
            return lost(e);
        }
    }
    quiet.clear();
    log::info!(event = "ha_weather"; "weather: connected to Home Assistant");
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let mut last_view = Instant::now();
    let mut first_event = true;
    let mut last_state = String::new();
    loop {
        if stop() {
            ws.close();
            return End::Quit;
        }
        match ws.read() {
            Ok(Some(Message::Text(t))) => {
                last_rx = Instant::now();
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                let h = model.handle(&v);
                let is_entities_event = v.get("id").and_then(Value::as_u64) == Some(entities_id)
                    && v.get("type").and_then(Value::as_str) == Some("event");
                if is_entities_event && first_event {
                    first_event = false;
                    if !model.entity_known() {
                        quiet.warn(format!(
                            "{} not found in Home Assistant: weather hidden",
                            cfg.entity
                        ));
                    }
                }
                if model.state() != last_state {
                    last_state = model.state().to_string();
                    if weather::is_unavailable(&last_state) && model.entity_known() {
                        log::info!(event = "ha_weather"; "weather: {} is {last_state:?}: hidden until it has data", cfg.entity);
                    } else if model.entity_known() {
                        log::info!(event = "ha_weather"; "weather: {} is {last_state}", cfg.entity);
                    }
                }
                if let Some((id, why)) = &h.error {
                    let what = if *id == entities_id {
                        "subscribe_entities"
                    } else {
                        "weather/subscribe_forecast"
                    };
                    log::info!(event = "ha_weather"; "weather: {what} refused: {why}");
                }
                if h.resubscribe_forecast {
                    model.forecast_id = take_id();
                    if let Err(e) = ws.send_text(&subscribe_forecast(model.forecast_id)) {
                        return lost(e);
                    }
                }
                if h.changed && !publish(&model, tx, shown) {
                    ws.close();
                    return End::Quit;
                }
            }
            Ok(Some(Message::Close)) => {
                return End::Lost("Home Assistant closed the connection".into());
            }
            Ok(None) => {}
            Err(e) => return lost(e),
        }
        // Midnight: "today" moves on.
        if last_view.elapsed() >= Duration::from_secs(60) {
            last_view = Instant::now();
            if !publish(&model, tx, shown) {
                ws.close();
                return End::Quit;
            }
        }
        if last_rx.elapsed() >= cfg.dead_after {
            return End::Lost("no message from Home Assistant".into());
        }
        if last_ping.elapsed() >= cfg.ping_every {
            last_ping = Instant::now();
            let ping = json!({"id": take_id(), "type": "ping"}).to_string();
            if let Err(e) = ws.send_text(&ping) {
                return lost(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;

    #[test]
    fn endpoints() {
        let e = endpoint("http://homeassistant.example.home:8123").unwrap();
        assert_eq!(
            e,
            Endpoint {
                host: "homeassistant.example.home".into(),
                port: 8123,
                path: "/api/websocket".into()
            }
        );
        assert_eq!(
            endpoint("http://192.0.2.10:8123/").unwrap().host,
            "192.0.2.10"
        );
        assert_eq!(endpoint("ws://h/api/websocket").unwrap().port, 80);
        assert_eq!(
            endpoint("ws://h/api/websocket").unwrap().path,
            "/api/websocket"
        );
        assert_eq!(
            endpoint("http://[2001:db8::1]:8123").unwrap().host,
            "2001:db8::1"
        );
        assert!(endpoint("https://h:8123").is_err());
        assert!(endpoint("h:8123").is_err());
        assert!(endpoint("http://h:port").is_err());
    }

    #[test]
    fn backoff_doubles_up_to_the_cap_and_resets() {
        let mut b = Backoff::new(Duration::from_secs(2), Duration::from_secs(20));
        let got: Vec<u64> = (0..6).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(got, [2, 4, 8, 16, 20, 20]);
        b.reset();
        assert_eq!(b.next_delay().as_secs(), 2);
    }

    // A fake Home Assistant: one scripted connection per accepted socket.

    fn read_frame(s: &mut TcpStream, buf: &mut Vec<u8>) -> String {
        loop {
            if let Some(fr) = ws::decode_frame(buf).unwrap() {
                buf.drain(..fr.used);
                if fr.opcode == 1 {
                    return String::from_utf8(fr.payload).unwrap();
                }
                continue;
            }
            let mut c = [0u8; 4096];
            let n = s.read(&mut c).unwrap();
            assert!(n > 0, "client closed");
            buf.extend_from_slice(&c[..n]);
        }
    }

    fn send(s: &mut TcpStream, text: &str) {
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

    /// Upgrade, authenticate, check the two subscriptions; returns the stream and leftovers.
    fn accept(l: &TcpListener, token: &str) -> (TcpStream, Vec<u8>) {
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = Vec::new();
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut c = [0u8; 1024];
            let n = s.read(&mut c).unwrap();
            buf.extend_from_slice(&c[..n]);
        }
        let req = String::from_utf8_lossy(&buf).to_string();
        assert!(req.starts_with("GET /api/websocket HTTP/1.1"), "{req}");
        s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
            .unwrap();
        let mut buf = Vec::new();
        send(
            &mut s,
            r#"{"type":"auth_required","ha_version":"2026.10.4"}"#,
        );
        let auth: Value = serde_json::from_str(&read_frame(&mut s, &mut buf)).unwrap();
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["access_token"], token);
        send(&mut s, r#"{"type":"auth_ok","ha_version":"2026.10.4"}"#);
        let sub: Value = serde_json::from_str(&read_frame(&mut s, &mut buf)).unwrap();
        assert_eq!(sub["type"], "subscribe_entities");
        assert_eq!(sub["id"], 1);
        assert_eq!(sub["entity_ids"], json!(["weather.home"]));
        let fc: Value = serde_json::from_str(&read_frame(&mut s, &mut buf)).unwrap();
        assert_eq!(fc["type"], "weather/subscribe_forecast");
        assert_eq!(fc["id"], 2);
        assert_eq!(fc["forecast_type"], "daily");
        (s, buf)
    }

    fn no_stop() -> bool {
        false
    }

    #[test]
    fn subscribes_shows_hides_and_reconnects() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("home-companion-ha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token_file = dir.join("ha-token");
        std::fs::write(&token_file, "test-token\n").unwrap();
        let url = format!("http://127.0.0.1:{port}");
        let auth = Arc::new(auth::Auth::new(&url, &token_file, &dir));
        let mut cfg = WeatherConfig::new(&url, auth, "weather.home");
        cfg.backoff = (Duration::from_millis(50), Duration::from_millis(200));
        let (tx, rx) = mpsc::channel();
        let _client = spawn_weather(cfg, tx, no_stop).unwrap();
        let wait = Duration::from_secs(10);

        // First connection: unavailable (nothing sent: nothing was shown), then data.
        let (mut s, _) = accept(&l, "test-token");
        send(
            &mut s,
            r#"{"id":1,"type":"result","success":true,"result":null}"#,
        );
        send(
            &mut s,
            r#"{"id":1,"type":"event","event":{"a":{"weather.home":{"s":"unavailable","a":{"friendly_name":"Home"}}}}}"#,
        );
        send(
            &mut s,
            r#"{"id":2,"type":"result","success":false,"error":{"code":"forecast_not_supported","message":"no daily forecast"}}"#,
        );
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        send(
            &mut s,
            r#"{"id":1,"type":"event","event":{"c":{"weather.home":{"+":{"s":"sunny","a":{"temperature":21.5,"temperature_unit":"°C"}}}}}}"#,
        );
        let w = rx.recv_timeout(wait).unwrap().unwrap();
        assert_eq!((w.condition.as_str(), w.temperature), ("sunny", 21.5));
        // The entity is back: the forecast is asked again, with a new id.
        let mut buf = Vec::new();
        let again: Value = serde_json::from_str(&read_frame(&mut s, &mut buf)).unwrap();
        assert_eq!(again["type"], "weather/subscribe_forecast");
        assert_eq!(again["id"], 3);
        // Unavailable again: hidden.
        send(
            &mut s,
            r#"{"id":1,"type":"event","event":{"c":{"weather.home":{"+":{"s":"unavailable"}}}}}"#,
        );
        assert_eq!(rx.recv_timeout(wait).unwrap(), None);
        send(
            &mut s,
            r#"{"id":1,"type":"event","event":{"c":{"weather.home":{"+":{"s":"cloudy"}}}}}"#,
        );
        assert!(rx.recv_timeout(wait).unwrap().is_some());

        // Home Assistant goes away: hidden, then the client comes back by itself.
        drop(s);
        assert_eq!(rx.recv_timeout(wait).unwrap(), None);
        let (mut s, _) = accept(&l, "test-token");
        send(
            &mut s,
            r#"{"id":1,"type":"event","event":{"a":{"weather.home":{"s":"rainy","a":{"temperature":9}}}}}"#,
        );
        let w = rx.recv_timeout(wait).unwrap().unwrap();
        assert_eq!((w.condition.as_str(), w.unit.as_str()), ("rainy", "°C"));
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refused_token_hides_and_waits() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let dir =
            std::env::temp_dir().join(format!("home-companion-ha-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token_file = dir.join("ha-token");
        std::fs::write(&token_file, "wrong").unwrap();
        let url = format!("http://127.0.0.1:{port}");
        let auth = Arc::new(auth::Auth::new(&url, &token_file, &dir));
        let cfg = WeatherConfig::new(&url, auth, "weather.home");
        let (tx, rx) = mpsc::channel();
        let _client = spawn_weather(cfg, tx, no_stop).unwrap();
        let (mut s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut buf = Vec::new();
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut c = [0u8; 1024];
            let n = s.read(&mut c).unwrap();
            buf.extend_from_slice(&c[..n]);
        }
        s.write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
            .unwrap();
        send(&mut s, r#"{"type":"auth_required"}"#);
        let mut buf = Vec::new();
        let _auth = read_frame(&mut s, &mut buf);
        send(
            &mut s,
            r#"{"type":"auth_invalid","message":"Invalid access token"}"#,
        );
        // Nothing was shown, so nothing is sent; and no quick retry (the long pause).
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        l.set_nonblocking(true).unwrap();
        assert!(l.accept().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
