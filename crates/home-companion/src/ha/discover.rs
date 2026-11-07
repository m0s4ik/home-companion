// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Finding Home Assistant on the network for the first-start page: Home Assistant announces
//! `_home-assistant._tcp` over mDNS (its zeroconf integration, on by default), and the system's
//! avahi-daemon already hears the network. `avahi-browse -rpt _home-assistant._tcp` (package
//! avahi-utils) dumps the resolved services and ends; [`parse`] turns its lines into one
//! [`Found`] per instance. No new dependency: the same "run a system helper and read its
//! output" as AirPlay's `busctl`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// The service type Home Assistant announces.
pub const SERVICE: &str = "_home-assistant._tcp";
/// avahi-browse is killed after this long (it normally ends in a second or two).
pub const TIMEOUT: Duration = Duration::from_secs(12);

/// One Home Assistant on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The name it announces (its location's name, "Home").
    pub name: String,
    /// The address the body will use: its `internal_url`, else `http://<address>:<port>`.
    pub url: String,
}

/// How a search ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Zero or more instances.
    Found(Vec<Found>),
    /// avahi-browse is not installed (package avahi-utils).
    NotInstalled,
    /// It ran and failed (avahi-daemon not running…): its message.
    Failed(String),
}

/// avahi-browse's escaping in parsable output: `\DDD` (a decimal byte) and `\X` (X itself).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let digits = &b[i + 1..(i + 4).min(b.len())];
            if digits.len() == 3 && digits.iter().all(u8::is_ascii_digit) {
                let v = digits.iter().fold(0u32, |a, d| a * 10 + (d - b'0') as u32);
                if let Ok(v) = u8::try_from(v) {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
            out.push(b[i + 1]);
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The TXT record as avahi-browse prints it: `"k=v" "k2=v2"`; the value of `key`.
fn txt_value(txt: &str, key: &str) -> Option<String> {
    let mut items = Vec::new();
    let mut cur: Option<String> = None;
    let mut chars = txt.chars();
    while let Some(c) = chars.next() {
        match (c, cur.as_mut()) {
            ('"', None) => cur = Some(String::new()),
            ('"', Some(_)) => items.extend(cur.take()),
            ('\\', Some(s)) => {
                // Keep the escape for unescape() (\DDD) or take the next character.
                s.push('\\');
                if let Some(n) = chars.next() {
                    s.push(n);
                }
            }
            (c, Some(s)) => s.push(c),
            (_, None) => {}
        }
    }
    items.iter().find_map(|i| {
        let i = unescape(i);
        let (k, v) = i.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// A usable http(s) address without its trailing slash.
fn clean_url(u: &str) -> Option<String> {
    let u = u.trim().trim_end_matches('/');
    let rest = u
        .strip_prefix("http://")
        .or_else(|| u.strip_prefix("https://"))?;
    (!rest.is_empty()).then(|| u.to_string())
}

/// The URL of one resolved line: `internal_url` when Home Assistant announces one, else the
/// address and the port (IPv6 in brackets; link-local IPv6 is useless without its interface).
pub fn choose_url(address: &str, port: &str, txt: &str) -> Option<String> {
    if let Some(u) = txt_value(txt, "internal_url")
        .as_deref()
        .and_then(clean_url)
    {
        return Some(u);
    }
    let ip: std::net::IpAddr = address.parse().ok()?;
    let port: u16 = port.parse().ok()?;
    match ip {
        std::net::IpAddr::V4(v4) => Some(format!("http://{v4}:{port}")),
        std::net::IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => None,
        std::net::IpAddr::V6(v6) => Some(format!("http://[{v6}]:{port}")),
    }
}

/// `avahi-browse -rpt` output → the instances: only resolved lines
/// (`=;iface;IPv4;name;type;domain;host;address;port;txt`), IPv4 preferred (an instance seen
/// over IPv4 drops its IPv6 lines), one row per URL, in the order first seen.
pub fn parse(output: &str) -> Vec<Found> {
    struct Line {
        v4: bool,
        name: String,
        url: String,
    }
    let lines: Vec<Line> = output
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(10, ';').collect();
            if f.len() < 9 || f[0] != "=" {
                return None;
            }
            let url = choose_url(f[7], f[8], f.get(9).copied().unwrap_or(""))?;
            Some(Line {
                v4: f[2] == "IPv4",
                name: unescape(f[3]),
                url,
            })
        })
        .collect();
    let mut found: Vec<Found> = Vec::new();
    for l in &lines {
        if !l.v4 && lines.iter().any(|o| o.v4 && o.name == l.name) {
            continue;
        }
        if found.iter().any(|f| f.url == l.url) {
            continue;
        }
        found.push(Found {
            name: l.name.clone(),
            url: l.url.clone(),
        });
    }
    found
}

/// Runs avahi-browse once (blocking, at most [`TIMEOUT`]).
pub fn browse() -> Outcome {
    let child = Command::new("avahi-browse")
        .args(["-r", "-p", "-t", SERVICE])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Outcome::NotInstalled,
        Err(e) => return Outcome::Failed(format!("avahi-browse: {e}")),
    };
    // Read both pipes on their own threads so a full pipe never blocks the child.
    let reader = |mut p: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = p.read_to_string(&mut s);
            s
        })
    };
    let out = child.stdout.take().map(|p| reader(Box::new(p)));
    let err = child.stderr.take().map(|p| reader(Box::new(p)));
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                log::warn!(
                    "avahi-browse: no end after {} s: stopped",
                    TIMEOUT.as_secs()
                );
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Outcome::Failed(format!("avahi-browse: {e}")),
        }
    };
    let out = out.and_then(|h| h.join().ok()).unwrap_or_default();
    let err = err.and_then(|h| h.join().ok()).unwrap_or_default();
    let found = parse(&out);
    match status {
        Some(s) if !s.success() && found.is_empty() => {
            let msg = err
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            Outcome::Failed(if msg.is_empty() {
                format!("avahi-browse: {s}")
            } else {
                format!("avahi-browse: {msg}")
            })
        }
        _ => Outcome::Found(found),
    }
}

/// [`browse`] on its own thread; the outcome arrives once on the receiver.
pub fn spawn() -> Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    let started = std::thread::Builder::new()
        .name("ha-discover".into())
        .spawn(move || {
            let o = browse();
            match &o {
                Outcome::Found(f) => log::info!(event = "ha_discover", found = f.len() as u64; "Home Assistant on the network: {f:?}"),
                Outcome::NotInstalled => log::warn!(event = "ha_discover"; "avahi-browse not installed (package avahi-utils): Home Assistant cannot be searched"),
                Outcome::Failed(e) => log::warn!(event = "ha_discover"; "{e}"),
            }
            let _ = tx.send(o);
        });
    if let Err(e) = started {
        log::warn!("ha-discover thread: {e}");
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped like real avahi-browse -rpt output (addresses from the documentation ranges).
    const SAMPLE: &str = concat!(
        "+;wlan0;IPv6;Home;_home-assistant._tcp;local\n",
        "+;wlan0;IPv4;Home;_home-assistant._tcp;local\n",
        "=;wlan0;IPv6;Home;_home-assistant._tcp;local;ha.example;2001:db8::10;8123;",
        "\"version=2026.11.0\" \"base_url=http://192.0.2.10:8123\" \"requires_api_password=True\" \"uuid=0123\"\n",
        "=;wlan0;IPv4;Home;_home-assistant._tcp;local;ha.example;192.0.2.10;8123;",
        "\"version=2026.11.0\" \"base_url=http://192.0.2.10:8123\" \"requires_api_password=True\" \"uuid=0123\"\n",
        "=;wlan0;IPv4;My\\032Cottage;_home-assistant._tcp;local;cottage.example;192.0.2.20;8123;",
        "\"internal_url=http://ha.example:8123/\" \"external_url=\" \"uuid=4567\"\n",
        "=;eth0;IPv4;Home;_home-assistant._tcp;local;ha.example;192.0.2.10;8123;\"uuid=0123\"\n",
    );

    #[test]
    fn parses_resolved_lines() {
        assert_eq!(
            parse(SAMPLE),
            vec![
                Found {
                    name: "Home".into(),
                    url: "http://192.0.2.10:8123".into()
                },
                Found {
                    name: "My Cottage".into(),
                    url: "http://ha.example:8123".into()
                },
            ]
        );
        assert!(parse("").is_empty());
        assert!(parse("Failed to create client object: Daemon not running\n").is_empty());
    }

    #[test]
    fn ipv6_only_and_link_local() {
        let v6 =
            "=;wlan0;IPv6;Lab;_home-assistant._tcp;local;lab.example;2001:db8::5;8123;\"uuid=1\"\n";
        assert_eq!(parse(v6)[0].url, "http://[2001:db8::5]:8123");
        let ll =
            "=;wlan0;IPv6;Lab;_home-assistant._tcp;local;lab.example;fe80::1;8123;\"uuid=1\"\n";
        assert!(parse(ll).is_empty());
    }

    #[test]
    fn url_choice() {
        assert_eq!(
            choose_url("192.0.2.1", "8123", "\"internal_url=https://ha.example\""),
            Some("https://ha.example".into())
        );
        // An empty or odd internal_url falls back to the address.
        assert_eq!(
            choose_url(
                "192.0.2.1",
                "8123",
                "\"internal_url=\" \"base_url=http://x\""
            ),
            Some("http://192.0.2.1:8123".into())
        );
        assert_eq!(
            choose_url("192.0.2.1", "8123", "\"internal_url=ftp://x\""),
            Some("http://192.0.2.1:8123".into())
        );
        assert_eq!(choose_url("nonsense", "8123", ""), None);
        assert_eq!(choose_url("192.0.2.1", "port", ""), None);
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape("Casa\\032al\\032mare"), "Casa al mare");
        assert_eq!(unescape("a\\.b\\\\c"), "a.b\\c");
        // UTF-8 bytes escaped one by one.
        assert_eq!(unescape("Caff\\195\\168"), "Caffè");
        assert_eq!(
            txt_value(
                "\"a=1\" \"name=x\\\"y\" \"internal_url=http://h:1\"",
                "name"
            )
            .as_deref(),
            Some("x\"y")
        );
        assert_eq!(txt_value("\"a=1\"", "b"), None);
    }
}
