// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `home-companion-panel`: the browser panel's pages on the home network (plain HTTP, port 8125
//! by default, from systemd's socket). This process keeps nothing and knows no secret: every
//! request becomes one line on the body's Unix socket (`panel.sock`, a closed list of requests,
//! crates/home-companion/src/panel/protocol.rs), and the body decides.
//!
//! What it guards on its side, before the body sees anything:
//! - only private addresses (the firewall says so too), at most 20 requests a second from one
//!   address and 8 connections at once, a 16 KiB head and a 64 KiB body, 5 s to send them;
//! - `Host` must be a private address or this device's own name (DNS rebinding: a page of
//!   another site resolving to this address is answered 421);
//! - a write is a POST with `Origin` equal to `http://<Host>` and `Content-Type:
//!   application/json` (CSRF: a page elsewhere cannot send that without a CORS preflight, and
//!   no CORS header is ever sent);
//! - the session's token lives only in a cookie `HttpOnly; SameSite=Strict`: the pages' script
//!   never sees it, and the body's answers carrying it are stripped before they leave;
//! - every answer has a strict Content-Security-Policy (no inline script), `nosniff`,
//!   `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use httpd::{Limits, Request, Response};
use serde_json::{Value, json};

pub const LIMITS: Limits = Limits::new(16 << 10, 64 << 10);
/// A whole request (head and body) within this.
pub const REQUEST_WITHIN: Duration = Duration::from_secs(5);
pub const MAX_CONNECTIONS: usize = 8;
pub const PER_SECOND: u32 = 20;
pub const COOKIE: &str = "hc_panel";
/// The cookie lasts as long as a session can (the body ends it sooner when unused).
const COOKIE_AGE: u64 = 30 * 86400;

pub const INDEX: &str = include_str!("../web/index.html");
pub const CSS: &str = include_str!("../web/panel.css");
pub const JS: &str = include_str!("../web/panel.js");

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Asks the body: one JSON line out, one back (None: no answer).
pub trait Body {
    fn call(&self, request: Value) -> Option<Value>;
}

/// The body's socket.
pub struct SocketBody {
    pub path: PathBuf,
}

impl Body for SocketBody {
    fn call(&self, request: Value) -> Option<Value> {
        let s = UnixStream::connect(&self.path).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(6))).ok()?;
        s.set_write_timeout(Some(Duration::from_secs(2))).ok()?;
        let mut w = s.try_clone().ok()?;
        writeln!(w, "{request}").ok()?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    }
}

/// Requests per address within the current second.
#[derive(Default)]
pub struct RateLimit {
    seen: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl RateLimit {
    /// True if `ip` may make one more request now.
    pub fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let mut m = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 1024 {
            m.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(1));
        }
        let e = m.entry(ip).or_insert((now, 0));
        if now.duration_since(e.0) >= Duration::from_secs(1) {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= PER_SECOND
    }
}

/// What the server needs to know of itself.
pub struct Panel<B: Body> {
    pub body: B,
    /// This device's host name (`<name>.local` and `<name>` are good `Host`s).
    pub hostname: String,
    pub rate: RateLimit,
}

/// The host part of a `Host` header ("192.0.2.5:8125" → "192.0.2.5", "[fd00::1]:8125" →
/// "fd00::1"), lowercase.
fn host_name(host: &str) -> String {
    let h = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':')
            .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
            .map_or(host, |(h, _)| h)
    };
    h.to_ascii_lowercase()
}

impl<B: Body> Panel<B> {
    /// `Host` names this device: a private address, `localhost`, or its own name.
    pub fn host_ok(&self, host: Option<&str>) -> bool {
        let Some(host) = host else { return false };
        let h = host_name(host);
        if let Ok(ip) = h.parse::<IpAddr>() {
            return httpd::is_private(ip);
        }
        let me = self.hostname.to_ascii_lowercase();
        h == "localhost" || (!me.is_empty() && (h == me || h == format!("{me}.local")))
    }
}

/// "Chrome", "macOS" from a User-Agent: only to recognise the device on the screen.
pub fn agent(ua: &str) -> (&'static str, &'static str) {
    let browser = if ua.contains("Edg/") || ua.contains("EdgA/") || ua.contains("EdgiOS/") {
        "Edge"
    } else if ua.contains("OPR/") {
        "Opera"
    } else if ua.contains("Firefox/") || ua.contains("FxiOS/") {
        "Firefox"
    } else if ua.contains("SamsungBrowser/") {
        "Samsung Internet"
    } else if ua.contains("Chrome/") || ua.contains("CriOS/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "Browser"
    };
    let os = if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") {
        "iPad"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("CrOS") {
        "ChromeOS"
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "?"
    };
    (browser, os)
}

/// The browser's languages, best first ("it-IT,it;q=0.9,en;q=0.8" → it-IT, it, en).
pub fn languages(accept: &str) -> Vec<String> {
    let mut v: Vec<(f32, usize, String)> = accept
        .split(',')
        .enumerate()
        .filter_map(|(i, part)| {
            let mut it = part.split(';');
            let tag = it.next()?.trim();
            if tag.is_empty() || tag == "*" || tag.len() > 16 {
                return None;
            }
            let q = it
                .find_map(|p| p.trim().strip_prefix("q=").and_then(|q| q.parse().ok()))
                .unwrap_or(1.0f32);
            Some((q, i, tag.to_string()))
        })
        .take(8)
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, _, t)| t).collect()
}

/// The session's token from the `Cookie` header (64 hex digits, else none).
pub fn token(req: &Request) -> String {
    req.headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, v)| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(n, _)| *n == COOKIE)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_default()
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The headers every answer has.
fn secure(r: Response) -> Response {
    r.header("Content-Security-Policy", CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("X-Frame-Options", "DENY")
        .header("Referrer-Policy", "no-referrer")
        .header("Cache-Control", "no-store")
        .header("Cross-Origin-Opener-Policy", "same-origin")
        .header("Cross-Origin-Resource-Policy", "same-origin")
}

fn json_response(status: u16, v: &Value) -> Response {
    Response::new(status).body("application/json", v.to_string())
}

fn error(status: u16, code: &str) -> Response {
    json_response(status, &json!({"ok": false, "error": code}))
}

fn set_cookie(r: Response, token: &str) -> Response {
    r.header(
        "Set-Cookie",
        &format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_AGE}"),
    )
}

fn clear_cookie(r: Response) -> Response {
    r.header(
        "Set-Cookie",
        &format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    )
}

/// The body's answer as the browser gets it: its status by the error, never the token.
fn relay(answer: Option<Value>) -> Response {
    let Some(mut a) = answer else {
        return error(502, "no_body");
    };
    if let Some(o) = a.as_object_mut() {
        o.remove("token");
    }
    let status = if a["ok"] == true {
        200
    } else {
        match a["error"].as_str().unwrap_or("") {
            "no_session" => 401,
            "off" => 503,
            "invalid" | "unknown_key" | "unknown_op" | "bad_request" => 400,
            _ => 409,
        }
    };
    json_response(status, &a)
}

impl<B: Body> Panel<B> {
    /// One request → its answer. `peer`: the browser's address.
    pub fn route(&self, req: &Request, peer: IpAddr, now: Instant) -> Response {
        secure(self.route_inner(req, peer, now))
    }

    fn route_inner(&self, req: &Request, peer: IpAddr, now: Instant) -> Response {
        if !httpd::is_private(peer) {
            return error(403, "not_local");
        }
        if !self.rate.allow(peer, now) {
            return error(429, "slow_down").header("Retry-After", "1");
        }
        let host = req.header("host");
        if !self.host_ok(host) {
            return error(421, "host");
        }
        let host = host.unwrap_or_default();
        let get = matches!(req.method.as_str(), "GET" | "HEAD");
        let post = req.method == "POST";
        if !get && !post {
            return error(405, "method").header("Allow", "GET, HEAD, POST");
        }
        let mut body_json = Value::Null;
        if post {
            // Same origin only: a page of another site cannot send these two.
            if req.header("origin") != Some(&format!("http://{host}")) {
                return error(403, "origin");
            }
            if req
                .header("sec-fetch-site")
                .is_some_and(|s| s != "same-origin")
            {
                return error(403, "origin");
            }
            let ct = req.header("content-type").unwrap_or("");
            if ct.split(';').next().map(str::trim) != Some("application/json") {
                return error(415, "content_type");
            }
            body_json = match serde_json::from_slice::<Value>(&req.body) {
                Ok(v) if v.is_object() => v,
                _ => return error(400, "bad_request"),
            };
        }
        let ua = req.header("user-agent").unwrap_or("");
        let (browser, os) = agent(ua);
        let lang = languages(req.header("accept-language").unwrap_or(""));
        let ask = |op: &str, extra: Value| -> Option<Value> {
            let mut r = json!({"op": op, "token": token(req), "peer": peer.to_string(),
                               "agent": {"browser": browser, "os": os}, "lang": lang});
            if let Value::Object(m) = extra {
                for (k, v) in m {
                    r[k] = v;
                }
            }
            self.body.call(r)
        };
        // Only the fields each request has, never what else the browser put in.
        let field = |k: &str| body_json.get(k).cloned().unwrap_or(Value::Null);
        let path = req.path();
        match (post, path) {
            (false, "/") => Response::new(200).body("text/html; charset=utf-8", INDEX),
            (false, "/panel.css") => Response::new(200).body("text/css; charset=utf-8", CSS),
            (false, "/panel.js") => Response::new(200).body("text/javascript; charset=utf-8", JS),
            (false, p) if p.starts_with("/q/") => {
                // The QR code's ticket: let in at once, then the page.
                let ticket = &p[3..];
                // Only a GET spends the ticket (a HEAD, a link preview's probe, does not).
                if req.method != "GET" {
                    return Response::new(200).body("text/html; charset=utf-8", INDEX);
                }
                if !is_hex(ticket, 64) {
                    return Response::new(303).header("Location", "/?qr=bad_ticket");
                }
                match ask("pair.qr", json!({"ticket": ticket})) {
                    Some(a) if a["ok"] == true => {
                        let t = a["token"].as_str().unwrap_or("");
                        set_cookie(Response::new(303).header("Location", "/"), t)
                    }
                    Some(a) => {
                        let code = a["error"].as_str().unwrap_or("error");
                        let code: String = code
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        Response::new(303).header("Location", &format!("/?qr={code}"))
                    }
                    None => error(502, "no_body"),
                }
            }
            (false, "/api/hello") => relay(ask("hello", json!({}))),
            (false, "/api/strings") => relay(ask("strings", json!({}))),
            (false, "/api/schema") => relay(ask("schema", json!({}))),
            (false, "/api/state") => relay(ask("state", json!({}))),
            (false, "/api/shopping") => relay(ask("shopping.list", json!({}))),
            (false, "/api/sessions") => relay(ask("sessions.list", json!({}))),
            (false, p) if p.starts_with("/api/pair/") => {
                let id = &p["/api/pair/".len()..];
                if !is_hex(id, 32) {
                    return error(400, "bad_request");
                }
                let a = ask("pair.status", json!({"id": id}));
                let t = a
                    .as_ref()
                    .and_then(|a| a["token"].as_str())
                    .map(String::from);
                let r = relay(a);
                match t {
                    Some(t) if is_hex(&t, 64) => set_cookie(r, &t),
                    _ => r,
                }
            }
            (true, "/api/pair") => relay(ask("pair.request", json!({}))),
            (true, "/api/set") => relay(ask(
                "set",
                json!({"key": field("key"), "value": field("value")}),
            )),
            (true, "/api/shopping/add") => relay(ask(
                "shopping.add",
                json!({"list": field("list"), "text": field("text")}),
            )),
            (true, "/api/shopping/done") => relay(ask(
                "shopping.done",
                json!({"list": field("list"), "uid": field("uid")}),
            )),
            (true, "/api/shopping/remove") => relay(ask(
                "shopping.remove",
                json!({"list": field("list"), "uid": field("uid")}),
            )),
            (true, "/api/sessions/revoke") => {
                relay(ask("sessions.revoke", json!({"id": field("id")})))
            }
            (true, "/api/logout") => clear_cookie(relay(ask("session.end", json!({})))),
            (true, _) => error(404, "not_found"),
            (false, _) => error(404, "not_found"),
        }
    }

    /// Serves one connection: one request, one answer.
    pub fn serve(&self, mut s: TcpStream, peer: IpAddr) {
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
        let r = match httpd::read_request(&mut httpd::Timed::new(&s, REQUEST_WITHIN), LIMITS) {
            Ok(req) => {
                let head = req.method == "HEAD";
                (self.route(&req, peer, Instant::now()), head)
            }
            Err(e) => match e.status() {
                Some(st) => (secure(error(st, "bad_request")), false),
                None => return,
            },
        };
        let _ = r.0.write_to(&mut s, r.1);
    }
}

/// Accepts until nobody asked anything for `idle` (systemd starts it again at the next
/// connection), one thread per connection, at most [`MAX_CONNECTIONS`].
pub fn run<B: Body + Send + Sync + 'static>(
    panel: std::sync::Arc<Panel<B>>,
    listener: TcpListener,
    idle: Duration,
) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let open = std::sync::Arc::new(AtomicUsize::new(0));
    let mut last = Instant::now();
    loop {
        match listener.accept() {
            Ok((s, addr)) => {
                last = Instant::now();
                let peer = match addr.ip() {
                    IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
                    ip => ip,
                };
                if !httpd::is_private(peer) || open.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    continue;
                }
                let _ = s.set_nonblocking(false);
                open.fetch_add(1, Ordering::Relaxed);
                let (p, o) = (panel.clone(), open.clone());
                let spawned =
                    std::thread::Builder::new()
                        .name("panel-http".into())
                        .spawn(move || {
                            p.serve(s, peer);
                            o.fetch_sub(1, Ordering::Relaxed);
                        });
                if spawned.is_err() {
                    open.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if last.elapsed() >= idle && open.load(Ordering::Relaxed) == 0 {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// This device's host name.
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_default()
}

/// The listening socket systemd passed (`LISTEN_FDS`, fd 3), if it is ours.
pub fn systemd_listener() -> Option<TcpListener> {
    use std::os::fd::FromRawFd;
    let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    let fds: u32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    if pid != std::process::id() || fds < 1 {
        return None;
    }
    // SAFETY: systemd hands fd 3 to this process as a listening socket (sd_listen_fds(3)).
    Some(unsafe { TcpListener::from_raw_fd(3) })
}

pub fn default_socket() -> &'static Path {
    Path::new("/run/home-companion/panel.sock")
}

#[cfg(test)]
mod tests;
