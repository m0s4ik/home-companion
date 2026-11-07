// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Linking to Home Assistant with a QR code. While Settings → Home
//! Assistant is on screen, [`Link::start`] listens on `[home_assistant] link_port` and the
//! screen shows a QR code with Home Assistant's own authorisation page:
//!
//! ```text
//! <ha>/auth/authorize?response_type=code&client_id=http://<body>:<port>/
//!                    &redirect_uri=http://<body>:<port>/auth_callback&state=<random>
//! ```
//!
//! The phone opens it (the user is usually logged in already) and confirms; Home Assistant sends
//! the phone back to `/auth_callback?code=..&state=..` here; the body exchanges the one-time code
//! at `/auth/token` for an access and a refresh token ([`super::auth`]), stores them in
//! `<state_dir>/ha-auth.json` and the screen says "linked". Home Assistant accepts an `http`
//! client id on a private address and a redirect on the same host and port without fetching
//! anything (homeassistant/components/auth/indieauth.py: `_parse_client_id`, `is_local`;
//! `verify_redirect_uri`, same scheme and netloc). `/` still answers with the IndieAuth `<link>`
//! and the character's name, for clients that look.
//!
//! The listener exists only while the page is shown and closes after the link: nothing listens
//! on the port otherwise (the firewall must allow it from the LAN: packaging/README.md). One
//! connection at a time, a 3 s read timeout and an 8 KiB request cap.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use super::auth::{self, TokenError};

const MAX_REQUEST: usize = 8 << 10;

/// What a link needs.
#[derive(Debug, Clone)]
pub struct LinkSetup {
    /// `[home_assistant] url`.
    pub hass_url: String,
    /// `[home_assistant] link_port`.
    pub port: u16,
    /// Where `ha-auth.json` goes.
    pub state_dir: PathBuf,
    /// The pages the phone shows: the character's name and the words, already translated.
    pub name: String,
    pub done_text: String,
    pub failed_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// Tokens stored: linked.
    Linked,
    /// This attempt failed (the page stays: the user can scan again).
    Failed(String),
}

/// A link in progress: the listener's thread, until dropped.
pub struct Link {
    /// What the QR code holds.
    pub authorize_url: String,
    events: Receiver<LinkEvent>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The body's address toward Home Assistant (the interface its packets leave from). No packet is
/// sent: connecting a UDP socket only picks the route.
pub fn local_address(host: &str, port: u16) -> io::Result<IpAddr> {
    let target = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    let any: SocketAddr = if target.is_ipv4() {
        "0.0.0.0:0".parse().expect("literal")
    } else {
        "[::]:0".parse().expect("literal")
    };
    let s = UdpSocket::bind(any)?;
    s.connect(target)?;
    Ok(s.local_addr()?.ip())
}

/// How long a QR code stays good after its page closes: a phone still on Home Assistant's login
/// page (or reloading it after a network problem) comes back with the state it was given, and
/// the page is often closed and opened again meanwhile. The code Home Assistant hands out is
/// single use whatever the state; the state only ties the callback to a code this body showed.
const STATE_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// The state of the last QR code and when it was made (see [`STATE_LIFETIME`]).
static RECENT_STATE: std::sync::Mutex<Option<(String, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// The last QR code's state while it is young, a new one otherwise.
fn link_state() -> io::Result<String> {
    let mut recent = RECENT_STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((s, made)) = recent.as_ref()
        && made.elapsed() < STATE_LIFETIME
    {
        return Ok(s.clone());
    }
    let s = random_state()?;
    *recent = Some((s.clone(), std::time::Instant::now()));
    Ok(s)
}

/// Linked: the next QR code gets a new state.
fn forget_state() {
    *RECENT_STATE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 16 random bytes as hex, for `state`.
fn random_state() -> io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// `http://<addr>:<port>/` (IPv6 in brackets).
fn client_id(addr: IpAddr, port: u16) -> String {
    match addr {
        IpAddr::V4(a) => format!("http://{a}:{port}/"),
        IpAddr::V6(a) => format!("http://[{a}]:{port}/"),
    }
}

/// Home Assistant's authorisation page for this link.
pub fn authorize_url(hass_origin: &str, client_id: &str, state: &str) -> String {
    format!(
        "{hass_origin}/auth/authorize?response_type=code&client_id={}&redirect_uri={}&state={}",
        auth::encode(client_id),
        auth::encode(&format!("{client_id}auth_callback")),
        auth::encode(state)
    )
}

impl Link {
    /// Listens on the link port and makes the QR code's address. Fails without Home Assistant's
    /// address, a route to it, or the port.
    pub fn start(setup: LinkSetup) -> Result<Link, String> {
        let origin =
            crate::ui::webview::origin(&setup.hass_url).ok_or("home_assistant.url is not set")?;
        let ep = super::endpoint(&setup.hass_url)?;
        let addr = local_address(&ep.host, ep.port)
            .map_err(|e| format!("no route to {}: {e}", ep.host))?;
        let bind: SocketAddr = match addr {
            IpAddr::V4(_) => ([0, 0, 0, 0], setup.port).into(),
            IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, setup.port).into(),
        };
        let listener =
            TcpListener::bind(bind).map_err(|e| format!("link port {}: {e}", setup.port))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("link port: {e}"))?;
        let state = link_state().map_err(|e| format!("/dev/urandom: {e}"))?;
        let client = client_id(addr, setup.port);
        let url = authorize_url(&origin, &client, &state);
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let server = Server {
            setup,
            client_id: client.clone(),
            state,
            tx,
        };
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ha-link".into())
            .spawn(move || server.run(listener, &s))
            .map_err(|e| format!("link thread: {e}"))?;
        log::info!(event = "ha_link"; "Home Assistant link: listening as {client}");
        Ok(Link {
            authorize_url: url,
            events: rx,
            stop,
            thread: Some(thread),
        })
    }

    /// What happened since the last call (never blocks).
    pub fn poll(&self) -> Option<LinkEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Link {
    /// Called on the UI thread when the page closes: never waits for an exchange in progress
    /// (up to 10 s); the thread sees `stop` within 100 ms of its current request and closes
    /// the port.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take()
            && t.is_finished()
        {
            let _ = t.join();
        }
        log::info!(event = "ha_link"; "Home Assistant link: closed");
    }
}

struct Server {
    setup: LinkSetup,
    client_id: String,
    state: String,
    tx: Sender<LinkEvent>,
}

/// One HTTP answer.
struct Page {
    status: &'static str,
    body: String,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn html(title: &str, text: &str, head: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width,initial-scale=1\">{head}<title>{t}</title><style>body{{font:\
         20px/1.4 system-ui,sans-serif;background:#16130f;color:#f3ead8;margin:0;display:flex;\
         min-height:100vh;align-items:center;justify-content:center;text-align:center;padding:24px}}\
         </style></head><body><p>{x}</p></body></html>",
        t = escape(title),
        x = escape(text)
    )
}

impl Server {
    fn run(self, listener: TcpListener, stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((s, peer)) => {
                    if self.handle(s, peer) {
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    log::warn!(event = "ha_link"; "Home Assistant link: accept: {e}");
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    /// Answers one request; true once linked (the listener closes).
    fn handle(&self, mut s: TcpStream, peer: SocketAddr) -> bool {
        let _ = s.set_nonblocking(false);
        let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
        let target = match read_target(&mut s) {
            Ok(t) => t,
            Err(e) => {
                log::debug!("Home Assistant link: {peer}: {e}");
                return false;
            }
        };
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let mut linked = false;
        let page = match path {
            "/" => Page {
                status: "200 OK",
                body: html(
                    &self.setup.name,
                    &self.setup.name,
                    &format!(
                        "<link rel=\"redirect_uri\" href=\"{}auth_callback\">",
                        escape(&self.client_id)
                    ),
                )
                .replace("<p>", "<div class=\"h-app\"><p class=\"p-name\">")
                .replace("</p>", "</p></div>"),
            },
            "/auth_callback" => {
                let q = auth::query_pairs(query);
                let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
                let outcome = match (get("state"), get("code"), get("error")) {
                    (_, _, Some(err)) => Err(Some(format!("Home Assistant: {err}"))),
                    (Some(st), Some(code), None) if st == self.state && !code.is_empty() => {
                        self.finish(code).map_err(Some)
                    }
                    // An older code (a phone tab left from an earlier attempt, a code from before
                    // a restart): the phone is told, the screen is not (the right one may be
                    // coming a second later).
                    _ => Err(None),
                };
                match outcome {
                    Ok(()) => {
                        linked = true;
                        forget_state();
                        let _ = self.tx.send(LinkEvent::Linked);
                        Page {
                            status: "200 OK",
                            body: html(&self.setup.name, &self.setup.done_text, ""),
                        }
                    }
                    Err(why) => {
                        match why {
                            Some(why) => {
                                log::warn!(event = "ha_link"; "Home Assistant link failed: {why}");
                                let _ = self.tx.send(LinkEvent::Failed(why));
                            }
                            None => log::info!(
                                event = "ha_link";
                                "Home Assistant link: a callback with an older code (state): \
                                 refused, the screen unchanged"
                            ),
                        }
                        Page {
                            status: "400 Bad Request",
                            body: html(&self.setup.name, &self.setup.failed_text, ""),
                        }
                    }
                }
            }
            _ => Page {
                status: "404 Not Found",
                body: html("404", "404", ""),
            },
        };
        let _ = write!(
            s,
            "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{}",
            page.status,
            page.body.len(),
            page.body
        );
        let _ = s.flush();
        linked
    }

    /// The code for tokens, stored.
    fn finish(&self, code: &str) -> Result<(), String> {
        let t =
            auth::exchange_code(&self.setup.hass_url, &self.client_id, code).map_err(
                |e| match e {
                    TokenError::Refused(w)
                    | TokenError::Unreachable(w)
                    | TokenError::Missing(w) => w,
                },
            )?;
        let path = self.setup.state_dir.join(auth::AUTH_FILE);
        auth::save(&path, &t).map_err(|e| format!("{}: {e}", path.display()))?;
        log::info!(event = "ha_link"; "Home Assistant: linked, tokens in {}", path.display());
        Ok(())
    }
}

/// The request target of a GET ("/auth_callback?code=.."), after reading the whole head.
fn read_target(s: &mut TcpStream) -> io::Result<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REQUEST {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too large",
            ));
        }
    }
    let line = buf.split(|&b| b == b'\r').next().unwrap_or_default();
    let line = std::str::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))?;
    let mut parts = line.split(' ');
    match (parts.next(), parts.next()) {
        (Some("GET" | "HEAD"), Some(t)) if t.starts_with('/') => Ok(t.to_string()),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "not a GET")),
    }
}

/// The QR code of `text` as rows of dark (true) and light modules, with the 4-module quiet zone.
pub fn qr_modules(text: &str) -> Option<(usize, Vec<bool>)> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Low).ok()?;
    let n = qr.size() as usize;
    let side = n + 8;
    let mut m = vec![false; side * side];
    for y in 0..n {
        for x in 0..n {
            m[(y + 4) * side + x + 4] = qr.get_module(x as i32, y as i32);
        }
    }
    Some((side, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let c = client_id("192.0.2.5".parse().unwrap(), 8124);
        assert_eq!(c, "http://192.0.2.5:8124/");
        assert_eq!(
            client_id("fd00::5".parse().unwrap(), 8124),
            "http://[fd00::5]:8124/"
        );
        assert_eq!(
            authorize_url("http://ha.example:8123", &c, "ab12"),
            "http://ha.example:8123/auth/authorize?response_type=code\
             &client_id=http%3A%2F%2F192.0.2.5%3A8124%2F\
             &redirect_uri=http%3A%2F%2F192.0.2.5%3A8124%2Fauth_callback&state=ab12"
        );
        assert_eq!(random_state().unwrap().len(), 32);
    }

    #[test]
    fn qr() {
        let (side, m) = qr_modules("http://ha.example:8123/auth/authorize?x=1").unwrap();
        assert_eq!(m.len(), side * side);
        // Quiet zone light, finder pattern dark in the corner.
        assert!(!m[0] && !m[3 * side + 3]);
        assert!(m[4 * side + 4]);
    }

    fn get(port: u16, target: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    /// The whole link against a fake Home Assistant on loopback.
    #[test]
    fn link_end_to_end() {
        let d = std::env::temp_dir().join(format!("home-companion-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let ha = TcpListener::bind("127.0.0.1:0").unwrap();
        let ha_url = format!("http://127.0.0.1:{}", ha.local_addr().unwrap().port());
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let link = Link::start(LinkSetup {
            hass_url: ha_url.clone(),
            port,
            state_dir: d.clone(),
            name: "Alfred".into(),
            done_text: "Alfred è collegato".into(),
            failed_text: "Non è andata".into(),
        })
        .unwrap();
        assert!(
            link.authorize_url
                .starts_with(&format!("{ha_url}/auth/authorize?"))
        );
        let q = link.authorize_url.split_once('?').unwrap().1;
        let pairs = auth::query_pairs(q);
        let val = |k: &str| pairs.iter().find(|(n, _)| n == k).unwrap().1.clone();
        let state = val("state");
        assert_eq!(val("client_id"), format!("http://127.0.0.1:{port}/"));
        assert_eq!(
            val("redirect_uri"),
            format!("http://127.0.0.1:{port}/auth_callback")
        );

        let home = get(port, "/");
        assert!(home.starts_with("HTTP/1.1 200"));
        assert!(home.contains(&format!(
            "rel=\"redirect_uri\" href=\"http://127.0.0.1:{port}/auth_callback\""
        )));
        assert!(get(port, "/other").starts_with("HTTP/1.1 404"));
        // A wrong state (an older code): refused on the phone, nothing on the screen; the page
        // stays.
        let bad = get(port, "/auth_callback?code=c1&state=nope");
        assert!(bad.starts_with("HTTP/1.1 400"));
        assert_eq!(link.poll(), None);
        // Home Assistant's own refusal is shown.
        let no = get(port, "/auth_callback?error=access_denied&state=nope");
        assert!(no.starts_with("HTTP/1.1 400"));
        assert!(matches!(link.poll(), Some(LinkEvent::Failed(_))));

        let h = auth::tests::serve_once(
            ha,
            200,
            r#"{"access_token":"A1","expires_in":1800,"refresh_token":"R1","token_type":"Bearer"}"#,
        );
        let ok = get(port, &format!("/auth_callback?code=c%2F1&state={state}"));
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("Alfred è collegato"));
        let req = h.join().unwrap();
        assert!(req.contains("grant_type=authorization_code&code=c%2F1&client_id="));
        assert_eq!(link.poll(), Some(LinkEvent::Linked));
        let t = auth::load(&d.join(auth::AUTH_FILE)).unwrap().unwrap();
        assert_eq!(t.refresh_token, "R1");
        assert_eq!(t.client_id, format!("http://127.0.0.1:{port}/"));
        drop(link);
        // Closed after the link (the thread ends right after its answer).
        let closed = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            TcpStream::connect(("127.0.0.1", port)).is_err()
        });
        assert!(closed);
        let _ = std::fs::remove_dir_all(&d);
    }
}
