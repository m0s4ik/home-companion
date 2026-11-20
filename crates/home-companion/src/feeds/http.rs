// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The one place that asks the internet for the widget's data: `curl` as a child process (the
//! body has no TLS of its own; Debian keeps curl and the certificates up to date, as for
//! update.rs). The host is a constant per source; what the user typed only ever travels as a
//! query value we percent-encode, inside the one `--url` argument; no redirects, no shell, no
//! environment but `PATH`, one request at a time, never from the screen's thread.

use std::process::{Command, Stdio};

/// The hosts the widget's data comes from: nothing else is ever asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// Yahoo Finance's chart and search (unofficial).
    Yahoo,
    /// The same API on Yahoo's second host: the fallback when the first refuses us.
    Yahoo2,
    CoinGecko,
    OpenMeteo,
    OpenMeteoGeo,
}

impl Host {
    pub fn name(self) -> &'static str {
        match self {
            Host::Yahoo => "query1.finance.yahoo.com",
            Host::Yahoo2 => "query2.finance.yahoo.com",
            Host::CoinGecko => "api.coingecko.com",
            Host::OpenMeteo => "api.open-meteo.com",
            Host::OpenMeteoGeo => "geocoding-api.open-meteo.com",
        }
    }
}

/// What a request gave.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// HTTP 200 and its body.
    Ok(Vec<u8>),
    /// Another status, and Retry-After in seconds when the server sent one.
    Http(u16, Option<u64>),
    Timeout,
    Failed(String),
}

/// Something that fetches a URL (curl; a fake in the tests).
pub trait Fetch: Send {
    fn get(&mut self, url: &Url) -> Outcome;
}

/// A URL we built: `https://<host constant>/<path>?<encoded query>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub host: Host,
    text: String,
}

impl Url {
    /// `path` is ours (a constant, or a symbol we checked: [`path_segment`]); every query value is
    /// percent-encoded here.
    pub fn new(host: Host, path: &str, query: &[(&str, &str)]) -> Url {
        let mut text = format!("https://{}/{}", host.name(), path.trim_start_matches('/'));
        for (i, (k, v)) in query.iter().enumerate() {
            text.push(if i == 0 { '?' } else { '&' });
            text.push_str(&encode(k));
            text.push('=');
            text.push_str(&encode(v));
        }
        Url { host, text }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// Percent-encoding of a query value or a path segment: only `A-Z a-z 0-9 - . _ ~` stay.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A symbol as one path segment (Yahoo's chart takes it in the path): encoded, so `/`, `..` or
/// `?` can never leave the segment.
pub fn path_segment(s: &str) -> String {
    let e = encode(s);
    if e.is_empty() || e.chars().all(|c| c == '.') {
        return "_".into();
    }
    e
}

/// The body's user agent.
pub fn user_agent() -> String {
    format!("home-companion/{}", env!("CARGO_PKG_VERSION"))
}

/// The longest answer read (512 KiB): a bigger one is an error, never half a datum.
pub const MAX_BODY: u64 = 512 * 1024;

/// curl's arguments for one GET: fixed options, then the URL as the last argument and only
/// there (`--url`, so it can never be taken for an option).
pub fn curl_args(ua: &str, url: &Url) -> Vec<String> {
    let mut a: Vec<String> = [
        "-q",
        "-sS",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--max-time",
        "10",
        "--connect-timeout",
        "5",
        "--max-filesize",
        "524288",
        "--compressed",
        "-A",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push(ua.to_string());
    a.push("-w".into());
    a.push("\n%{http_code} %header{retry-after}".into());
    a.push("--url".into());
    a.push(url.as_str().to_string());
    a
}

/// curl's exit and output → the outcome: the last line is "<status> <retry-after>".
pub fn outcome(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Outcome {
    match code {
        Some(0) => {}
        Some(28) => return Outcome::Timeout,
        Some(63) => return Outcome::Failed("answer too big".into()),
        other => {
            let e = String::from_utf8_lossy(stderr);
            let e: String = e.trim().chars().take(160).collect();
            return Outcome::Failed(format!("curl {other:?}: {e}"));
        }
    }
    let Some(nl) = stdout.iter().rposition(|b| *b == b'\n') else {
        return Outcome::Failed("no status".into());
    };
    let (body, tail) = (&stdout[..nl], &stdout[nl + 1..]);
    let tail = String::from_utf8_lossy(tail);
    let mut parts = tail.split_whitespace();
    let status: u16 = match parts.next().and_then(|s| s.parse().ok()) {
        Some(s) => s,
        None => return Outcome::Failed("no status".into()),
    };
    if body.len() as u64 > MAX_BODY {
        return Outcome::Failed("answer too big".into());
    }
    if status == 200 {
        return Outcome::Ok(body.to_vec());
    }
    let retry = parts.next().and_then(|s| s.parse::<u64>().ok());
    Outcome::Http(status, retry)
}

/// The real thing: one curl per request.
pub struct Curl {
    ua: String,
    /// The program run (`curl` in PATH; a fake one in the tests).
    program: String,
}

impl Default for Curl {
    fn default() -> Self {
        Curl {
            ua: user_agent(),
            program: "curl".into(),
        }
    }
}

impl Curl {
    #[cfg(test)]
    pub fn with_program(program: &str) -> Curl {
        Curl {
            ua: user_agent(),
            program: program.into(),
        }
    }
}

impl Fetch for Curl {
    fn get(&mut self, url: &Url) -> Outcome {
        let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
        let out = Command::new(&self.program)
            .args(curl_args(&self.ua, url))
            .env_clear()
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match out {
            Ok(o) => outcome(o.status.code(), &o.stdout, &o.stderr),
            Err(e) => Outcome::Failed(format!("curl: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_text_stays_a_value() {
        assert_eq!(encode("S&P 500"), "S%26P%20500");
        assert_eq!(encode("^GSPC"), "%5EGSPC");
        assert_eq!(encode("EURUSD=X"), "EURUSD%3DX");
        assert_eq!(encode("Zürich"), "Z%C3%BCrich");
        assert_eq!(path_segment("../etc"), "..%2Fetc");
        assert_eq!(path_segment(".."), "_");
        assert_eq!(path_segment(""), "_");
        let u = Url::new(
            Host::Yahoo,
            "v1/finance/search",
            &[("q", "-o /etc/passwd"), ("quotesCount", "6")],
        );
        assert_eq!(
            u.as_str(),
            "https://query1.finance.yahoo.com/v1/finance/search?q=-o%20%2Fetc%2Fpasswd&quotesCount=6"
        );
        let a = curl_args("ua", &u);
        // The URL is the last argument, after --url, and nothing else holds user text.
        assert_eq!(a[a.len() - 2], "--url");
        assert_eq!(a.last().unwrap(), u.as_str());
        assert!(
            a.last()
                .unwrap()
                .starts_with("https://query1.finance.yahoo.com/")
        );
        assert!(a[..a.len() - 1].iter().all(|x| !x.contains("passwd")));
        assert_eq!(a[0], "-q");
        assert!(!a.iter().any(|x| x == "-L" || x == "--location"));
        for host in [
            Host::Yahoo,
            Host::Yahoo2,
            Host::CoinGecko,
            Host::OpenMeteo,
            Host::OpenMeteoGeo,
        ] {
            assert!(
                Url::new(host, "x", &[])
                    .as_str()
                    .starts_with(&format!("https://{}/", host.name()))
            );
        }
    }

    #[test]
    fn outcomes() {
        assert_eq!(
            outcome(Some(0), b"{\"a\":1}\n200 ", b""),
            Outcome::Ok(b"{\"a\":1}".to_vec())
        );
        assert_eq!(
            outcome(Some(0), b"slow down\n429 30", b""),
            Outcome::Http(429, Some(30))
        );
        assert_eq!(outcome(Some(0), b"\n404 ", b""), Outcome::Http(404, None));
        assert_eq!(outcome(Some(28), b"", b"timeout"), Outcome::Timeout);
        assert!(matches!(
            outcome(Some(6), b"", b"could not resolve"),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            outcome(Some(0), b"no newline", b""),
            Outcome::Failed(_)
        ));
        assert!(matches!(outcome(None, b"", b""), Outcome::Failed(_)));
        let big = vec![b'x'; MAX_BODY as usize + 1];
        let mut out = big.clone();
        out.extend_from_slice(b"\n200 ");
        assert!(matches!(outcome(Some(0), &out, b""), Outcome::Failed(_)));
    }

    /// A fake curl in a directory of its own: it prints its arguments and the status line, so
    /// the arguments as the program received them can be checked.
    #[test]
    fn a_fake_curl_gets_our_arguments_and_nothing_of_the_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hc-fake-curl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("curl");
        std::fs::write(
            &prog,
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s|' \"$a\"; done\nprintf 'HOME=%s|' \"$HOME\"\nprintf '\\n200 '\n",
        )
        .unwrap();
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut c = Curl::with_program(prog.to_str().unwrap());
        let u = Url::new(
            Host::OpenMeteoGeo,
            "v1/search",
            &[("name", "Berlin; rm -rf /")],
        );
        let Outcome::Ok(body) = c.get(&u) else {
            panic!("no answer");
        };
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains(
            "|--url|https://geocoding-api.open-meteo.com/v1/search?name=Berlin%3B%20rm%20-rf%20%2F|"
        ));
        assert!(body.contains("HOME=|"), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
