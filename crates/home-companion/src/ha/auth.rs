// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Home Assistant's tokens. Two sources, in this order:
//!
//! 1. **Linked** (Settings → Home Assistant, the QR code: [`super::link`]): `<state_dir>/ha-auth.json`
//!    (mode 600) holds an access token (30 minutes) and a refresh token from Home Assistant's
//!    own OAuth flow (`/auth/authorize`, `/auth/token`). [`Auth::access_token`] refreshes the
//!    access token a minute before it expires; a refresh token Home Assistant refuses (revoked in
//!    the user's profile) removes the file: not linked any more.
//! 2. **Long-lived**: the token in `[home_assistant] token_file`, as before the link existed.
//!
//! The HTTP client is as small as the WebSocket one ([`super::ws`]): plain TCP, no TLS, one
//! request per connection.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The linked tokens, in the state directory.
pub const AUTH_FILE: &str = "ha-auth.json";

/// A refresh this long before the access token expires.
const MARGIN: u64 = 60;

/// Larger responses are refused (a token response is a few hundred bytes).
const MAX_RESPONSE: usize = 64 << 10;

const TIMEOUT: Duration = Duration::from_secs(10);

/// What `ha-auth.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Home Assistant's origin when the link was made ("http://ha.example:8123").
    pub hass_url: String,
    /// The body's client id in that link (`http://<address>:<link_port>/`): refreshing needs it.
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds since the epoch.
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// No token configured (not linked, no token file).
    Missing(String),
    /// Home Assistant refused the token or the code (revoked, expired, wrong client).
    Refused(String),
    /// No answer, or not one we understand: try again later.
    Unreachable(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Missing(s) | TokenError::Refused(s) | TokenError::Unreachable(s) => {
                f.write_str(s)
            }
        }
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The linked tokens; `Ok(None)` when there is no file.
pub fn load(path: &Path) -> io::Result<Option<Tokens>> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes the tokens atomically, mode 600. Run as root (`home-companion web-login` from the helper's unit),
/// the file is given to the owner of its directory, so the body can still read it.
pub fn save(path: &Path, t: &Tokens) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".{AUTH_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(
            serde_json::to_string(t)
                .map_err(io::Error::other)?
                .as_bytes(),
        )?;
        f.sync_all()?;
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        let m = std::fs::metadata(dir)?;
        std::os::unix::fs::chown(&tmp, Some(m.uid()), Some(m.gid()))?;
    }
    std::fs::rename(&tmp, path)
}

/// Percent-encoding of a query or form value (everything but RFC 3986's unreserved characters).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The reverse of [`encode`] (and `+` as a space, as forms send it). Bad escapes stay as they are.
pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len()
                && b[i + 1].is_ascii_hexdigit()
                && b[i + 2].is_ascii_hexdigit() =>
            {
                let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(b[i + 1]) << 4 | hex(b[i + 2]));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A query string ("a=1&b=x%20y") as pairs.
pub fn query_pairs(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (decode(k), decode(v)),
            None => (decode(p), String::new()),
        })
        .collect()
}

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// POST of a form to Home Assistant (`url` = `[home_assistant] url`, `path` = "/auth/token").
pub fn post_form(url: &str, path: &str, form: &[(&str, &str)]) -> io::Result<Response> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    http(
        url,
        "POST",
        path,
        None,
        Some(("application/x-www-form-urlencoded", body.as_bytes())),
        MAX_RESPONSE,
        TIMEOUT,
    )
}

/// One HTTP/1.1 request to Home Assistant (one connection, `Connection: close`): `bearer` = an
/// access token, `body` = (content type, bytes); answers up to `max` bytes.
pub fn http(
    url: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<(&str, &[u8])>,
    max: usize,
    timeout: Duration,
) -> io::Result<Response> {
    let (status, body) = http_bytes(url, method, path, bearer, body, max, timeout)?;
    Ok(Response {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// [`http`] with the answer's body as bytes (audio): (status, body).
pub fn http_bytes(
    url: &str,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<(&str, &[u8])>,
    max: usize,
    timeout: Duration,
) -> io::Result<(u16, Vec<u8>)> {
    let ep = super::endpoint(url).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let base = ep.path.trim_end_matches("/api/websocket");
    let host = if ep.host.contains(':') {
        format!("[{}]:{}", ep.host, ep.port)
    } else {
        format!("{}:{}", ep.host, ep.port)
    };
    let addr = (ep.host.as_str(), ep.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    let mut s = TcpStream::connect_timeout(&addr, timeout.min(TIMEOUT))?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    let mut head =
        format!("{method} {base}{path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\n");
    if let Some(t) = bearer {
        head.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    if let Some((ct, b)) = body {
        head.push_str(&format!(
            "Content-Type: {ct}\r\nContent-Length: {}\r\n",
            b.len()
        ));
    } else if method != "GET" {
        head.push_str("Content-Length: 0\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    let mut req = head.into_bytes();
    if let Some((_, b)) = body {
        req.extend_from_slice(b);
    }
    s.write_all(&req)?;
    let mut raw = Vec::new();
    s.take(max as u64 + 1).read_to_end(&mut raw)?;
    if raw.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "response too large",
        ));
    }
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> io::Result<(u16, Vec<u8>)> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("no HTTP header"))?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| bad("header not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| bad("no HTTP status"))?;
    let chunked = lines.any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.trim().eq_ignore_ascii_case("chunked")
        })
    });
    let mut body = raw[end + 4..].to_vec();
    if chunked {
        body = dechunk(&body).ok_or_else(|| bad("bad chunked body"))?;
    }
    Ok((status, body))
}

fn dechunk(mut b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = b.windows(2).position(|w| w == b"\r\n")?;
        let size = std::str::from_utf8(&b[..eol]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        b = &b[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(b.get(..size)?);
        b = b.get(size + 2..)?;
    }
}

/// Home Assistant's answer to `/auth/token` as tokens. `refresh_token` is kept when the answer
/// has none (a refresh answers with a new access token only).
fn tokens_from(
    url: &str,
    client_id: &str,
    refresh_token: &str,
    r: io::Result<Response>,
) -> Result<Tokens, TokenError> {
    let r = r.map_err(|e| TokenError::Unreachable(format!("Home Assistant: {e}")))?;
    let json: serde_json::Value = serde_json::from_str(&r.body).unwrap_or_default();
    match r.status {
        200 => {
            let access = json["access_token"].as_str().unwrap_or_default();
            if access.is_empty() {
                return Err(TokenError::Unreachable(
                    "Home Assistant: no access_token in its answer".into(),
                ));
            }
            Ok(Tokens {
                hass_url: crate::ui::webview::origin(url).unwrap_or_else(|| url.into()),
                client_id: client_id.into(),
                access_token: access.into(),
                refresh_token: json["refresh_token"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(refresh_token)
                    .into(),
                expires_at: now_secs() + json["expires_in"].as_u64().unwrap_or(1800),
            })
        }
        400 | 401 | 403 => Err(TokenError::Refused(format!(
            "Home Assistant refused it ({})",
            json["error"].as_str().unwrap_or("HTTP error")
        ))),
        s => Err(TokenError::Unreachable(format!("Home Assistant: HTTP {s}"))),
    }
}

/// The link's last step: the one-time code for tokens.
pub fn exchange_code(url: &str, client_id: &str, code: &str) -> Result<Tokens, TokenError> {
    let r = post_form(
        url,
        "/auth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", client_id),
        ],
    );
    let t = tokens_from(url, client_id, "", r)?;
    if t.refresh_token.is_empty() {
        return Err(TokenError::Unreachable(
            "Home Assistant: no refresh_token in its answer".into(),
        ));
    }
    Ok(t)
}

/// A new access token for the same refresh token.
pub fn refresh(url: &str, t: &Tokens) -> Result<Tokens, TokenError> {
    let r = post_form(
        url,
        "/auth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &t.client_id),
            ("refresh_token", &t.refresh_token),
        ],
    );
    tokens_from(url, &t.client_id, &t.refresh_token, r)
}

/// Ends the link in Home Assistant too (the session disappears from the user's profile).
pub fn revoke(url: &str, t: &Tokens) -> io::Result<()> {
    post_form(url, "/auth/revoke", &[("token", &t.refresh_token)]).map(|_| ())
}

/// The body's tokens for Home Assistant, shared by the threads that talk to it.
#[derive(Debug)]
pub struct Auth {
    url: String,
    token_file: PathBuf,
    auth_file: PathBuf,
    /// One refresh at a time.
    lock: Mutex<()>,
}

impl Auth {
    /// `url` and `token_file` from `[home_assistant]`; the linked tokens in `state_dir`.
    pub fn new(url: &str, token_file: &Path, state_dir: &Path) -> Auth {
        Auth {
            url: url.to_string(),
            token_file: token_file.to_path_buf(),
            auth_file: state_dir.join(AUTH_FILE),
            lock: Mutex::new(()),
        }
    }

    pub fn auth_file(&self) -> &Path {
        &self.auth_file
    }

    /// Linked by QR code (a token file in the state directory).
    pub fn is_linked(&self) -> bool {
        self.auth_file.is_file()
    }

    /// A token of either kind is there.
    pub fn has_token(&self) -> bool {
        self.is_linked()
            || std::fs::metadata(&self.token_file).is_ok_and(|m| m.is_file() && m.len() > 0)
    }

    /// The linked tokens, refreshed if they expire within a minute; `Ok(None)` when not linked.
    pub fn linked_tokens(&self) -> Result<Option<Tokens>, TokenError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let t = match load(&self.auth_file) {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(None),
            Err(e) => {
                return Err(TokenError::Missing(format!(
                    "{}: {e}",
                    self.auth_file.display()
                )));
            }
        };
        if t.expires_at > now_secs() + MARGIN {
            return Ok(Some(t));
        }
        match refresh(&self.url, &t) {
            Ok(n) => {
                if let Err(e) = save(&self.auth_file, &n) {
                    log::warn!(event = "ha_auth"; "{}: {e}", self.auth_file.display());
                }
                log::debug!("Home Assistant: access token refreshed");
                Ok(Some(n))
            }
            Err(TokenError::Refused(why)) => {
                // Revoked in Home Assistant (or another Home Assistant): the link is over.
                log::warn!(event = "ha_auth"; "Home Assistant: link refused ({why}): not linked any more");
                let _ = std::fs::remove_file(&self.auth_file);
                Err(TokenError::Refused(why))
            }
            Err(e) => Err(e),
        }
    }

    /// A valid access token: the linked one (refreshed when needed), else the token file's.
    pub fn access_token(&self) -> Result<String, TokenError> {
        if let Some(t) = self.linked_tokens()? {
            return Ok(t.access_token);
        }
        match std::fs::read_to_string(&self.token_file) {
            Ok(t) if !t.trim().is_empty() => Ok(t.trim().to_string()),
            Ok(_) => Err(TokenError::Missing(format!(
                "{} is empty",
                self.token_file.display()
            ))),
            Err(e) => Err(TokenError::Missing(format!(
                "not linked, no token ({}: {e})",
                self.token_file.display()
            ))),
        }
    }

    /// Settings → Home Assistant → unlink: revoked in Home Assistant (best effort), file removed.
    pub fn unlink(&self) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(Some(t)) = load(&self.auth_file)
            && let Err(e) = revoke(&self.url, &t)
        {
            log::warn!(event = "ha_auth"; "Home Assistant: revoke: {e}");
        }
        let _ = std::fs::remove_file(&self.auth_file);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-request HTTP server: returns the request it got (head and body).
    pub(crate) fn serve_once(
        l: TcpListener,
        status: u16,
        body: &'static str,
    ) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = s.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len = text[..i]
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                    if req.len() >= i + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            String::from_utf8_lossy(&req).into_owned()
        })
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("home-companion-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn encoding() {
        assert_eq!(encode("http://a:1/x y"), "http%3A%2F%2Fa%3A1%2Fx%20y");
        assert_eq!(decode("http%3A%2F%2Fa%3A1%2Fx+y%20z"), "http://a:1/x y z");
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(
            query_pairs("code=a%2Fb&state=s&x"),
            vec![
                ("code".into(), "a/b".into()),
                ("state".into(), "s".into()),
                ("x".into(), String::new())
            ]
        );
    }

    #[test]
    fn chunked_response() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!((r.0, r.1.as_slice()), (200, &b"abcde"[..]));
    }

    #[test]
    fn code_exchange() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let h = serve_once(
            l,
            200,
            r#"{"access_token":"A1","expires_in":1800,"refresh_token":"R1","token_type":"Bearer"}"#,
        );
        let t = exchange_code(&url, "http://192.0.2.5:8124/", "c/1").unwrap();
        let req = h.join().unwrap();
        assert!(req.starts_with("POST /auth/token HTTP/1.1\r\n"));
        assert!(req.ends_with(
            "grant_type=authorization_code&code=c%2F1&client_id=http%3A%2F%2F192.0.2.5%3A8124%2F"
        ));
        assert_eq!(t.access_token, "A1");
        assert_eq!(t.refresh_token, "R1");
        assert_eq!(t.client_id, "http://192.0.2.5:8124/");
        assert!(t.expires_at >= now_secs() + 1700);
    }

    #[test]
    fn refresh_and_revoked() {
        let d = tmpdir("refresh");
        let token_file = d.join("ha-token");
        std::fs::write(&token_file, "long-lived\n").unwrap();
        // A long-lived token only.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let auth = Auth::new(&url, &token_file, &d);
        assert_eq!(auth.access_token().unwrap(), "long-lived");
        assert!(!auth.is_linked() && auth.has_token());
        // Linked, expired: refreshed, the refresh token kept.
        let old = Tokens {
            hass_url: url.clone(),
            client_id: "http://192.0.2.5:8124/".into(),
            access_token: "old".into(),
            refresh_token: "R1".into(),
            expires_at: now_secs() + 10,
        };
        save(auth.auth_file(), &old).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(auth.auth_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let h = serve_once(
            l,
            200,
            r#"{"access_token":"A2","expires_in":1800,"token_type":"Bearer"}"#,
        );
        assert_eq!(auth.access_token().unwrap(), "A2");
        let req = h.join().unwrap();
        assert!(req.contains(
            "grant_type=refresh_token&client_id=http%3A%2F%2F192.0.2.5%3A8124%2F&refresh_token=R1"
        ));
        let saved = load(auth.auth_file()).unwrap().unwrap();
        assert_eq!(
            (saved.access_token.as_str(), saved.refresh_token.as_str()),
            ("A2", "R1")
        );
        // Valid: no request.
        assert_eq!(auth.access_token().unwrap(), "A2");
        // Revoked: the file goes, back to the long-lived token.
        save(auth.auth_file(), &old).unwrap();
        let l = TcpListener::bind((
            "127.0.0.1",
            url.rsplit(':').next().unwrap().parse::<u16>().unwrap(),
        ))
        .unwrap();
        let h = serve_once(l, 400, r#"{"error":"invalid_grant"}"#);
        assert!(matches!(auth.access_token(), Err(TokenError::Refused(_))));
        h.join().unwrap();
        assert!(!auth.is_linked());
        assert_eq!(auth.access_token().unwrap(), "long-lived");
        let _ = std::fs::remove_dir_all(&d);
    }
}
