// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The smallest HTTP/1.1 server pieces home-companion needs, on `std` only: a request read with
//! limits ([`read_request`]) and an answer written ([`Response`]). One request per connection,
//! `Connection: close` always: no keep-alive, no pipelining, no upgrade.
//!
//! Used by the Home Assistant link (the QR code's callback), home-companion-brain's MCP server and
//! the browser panel (`home-companion-panel`). What is refused, and why:
//! a head over `Limits::head` (431), a body over `Limits::body` (413), a request line or a header
//! that is not HTTP/1.x (400), a `Content-Length` that is not a number or appears twice with
//! two values, `Content-Length` together with `Transfer-Encoding`, a transfer coding other than
//! `chunked`, folded headers (all 400: each is a way to make two parsers see two different
//! requests).

use std::io::{self, Read, Write};
use std::net::IpAddr;

/// How much a request may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The request line and the headers, with their line ends.
    pub head: usize,
    /// The body (after chunked decoding).
    pub body: usize,
}

impl Limits {
    pub const fn new(head: usize, body: usize) -> Limits {
        Limits { head, body }
    }
}

/// At most this many headers.
const MAX_HEADERS: usize = 64;

/// A request as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The request target as sent ("/api/pair/ab12?x=1").
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The first header named `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The target without its query.
    pub fn path(&self) -> &str {
        self.target.split_once('?').map_or(&self.target, |(p, _)| p)
    }

    /// The query, without `?` ("" when none).
    pub fn query(&self) -> &str {
        self.target.split_once('?').map_or("", |(_, q)| q)
    }
}

/// Why a request was not read.
#[derive(Debug)]
pub enum Error {
    /// The peer closed before a whole request (nothing to answer).
    Closed,
    /// The head is over the limit.
    HeadTooLarge,
    /// The body is over the limit.
    BodyTooLarge,
    /// Not a request we read (the reason, for the log).
    Bad(&'static str),
    Io(io::Error),
}

impl Error {
    /// The status to answer with, when there is one to send.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Closed | Error::Io(_) => None,
            Error::HeadTooLarge => Some(431),
            Error::BodyTooLarge => Some(413),
            Error::Bad(_) => Some(400),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Closed => write!(f, "closed before a whole request"),
            Error::HeadTooLarge => write!(f, "request head too large"),
            Error::BodyTooLarge => write!(f, "request body too large"),
            Error::Bad(why) => write!(f, "bad request: {why}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        match e.kind() {
            io::ErrorKind::UnexpectedEof => Error::Closed,
            _ => Error::Io(e),
        }
    }
}

/// Bytes read past what was asked for: the head's read may take some of the body.
struct Buffered<'a, R: Read> {
    r: &'a mut R,
    pending: Vec<u8>,
    at: usize,
}

impl<R: Read> Buffered<'_, R> {
    fn byte(&mut self) -> Result<u8, Error> {
        let mut b = [0u8; 1];
        self.exact(&mut b)?;
        Ok(b[0])
    }

    fn exact(&mut self, out: &mut [u8]) -> Result<(), Error> {
        let mut filled = 0;
        while filled < out.len() {
            if self.at < self.pending.len() {
                let n = (self.pending.len() - self.at).min(out.len() - filled);
                out[filled..filled + n].copy_from_slice(&self.pending[self.at..self.at + n]);
                self.at += n;
                filled += n;
                continue;
            }
            let n = self.r.read(&mut out[filled..])?;
            if n == 0 {
                return Err(Error::Closed);
            }
            filled += n;
        }
        Ok(())
    }

    /// A line of at most `max` bytes, without its end (CRLF or LF).
    fn line(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        loop {
            let b = self.byte()?;
            if b == b'\n' {
                if out.last() == Some(&b'\r') {
                    out.pop();
                }
                return Ok(out);
            }
            out.push(b);
            if out.len() > max {
                return Err(Error::BodyTooLarge);
            }
        }
    }
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Where the head ends in `buf` (the index after the blank line), if it does.
fn head_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len() {
        if buf[i] == b'\n' {
            if buf[i + 1..].starts_with(b"\r\n") {
                return Some(i + 3);
            }
            if buf[i + 1..].starts_with(b"\n") {
                return Some(i + 2);
            }
        }
    }
    None
}

/// Reads one request from `r` within `limits`. Set a read timeout on the stream first: this
/// waits as long as the reader does.
pub fn read_request<R: Read>(r: &mut R, limits: Limits) -> Result<Request, Error> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        if let Some(e) = head_end(&buf) {
            break e;
        }
        if buf.len() > limits.head {
            return Err(Error::HeadTooLarge);
        }
        let n = r.read(&mut chunk)?;
        if n == 0 {
            return Err(Error::Closed);
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    if end > limits.head {
        return Err(Error::HeadTooLarge);
    }
    let rest = buf.split_off(end);
    let head = std::str::from_utf8(&buf).map_err(|_| Error::Bad("head not UTF-8"))?;
    let mut lines = head.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let line = lines.next().unwrap_or_default();
    let mut parts = line.split(' ');
    let (method, target, version) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v), None) => (m, t, v),
        _ => return Err(Error::Bad("request line")),
    };
    if !is_token(method) || method.len() > 16 {
        return Err(Error::Bad("method"));
    }
    if !target.starts_with('/') || target.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(Error::Bad("target"));
    }
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(Error::Bad("version"));
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    for l in lines {
        if l.is_empty() {
            continue;
        }
        if l.starts_with([' ', '\t']) {
            return Err(Error::Bad("folded header"));
        }
        let (name, value) = l.split_once(':').ok_or(Error::Bad("header"))?;
        if !is_token(name) {
            return Err(Error::Bad("header name"));
        }
        let value = value.trim_matches([' ', '\t']);
        if value.bytes().any(|b| b < b' ' && b != b'\t' || b == 0x7f) {
            return Err(Error::Bad("header value"));
        }
        headers.push((name.to_string(), value.to_string()));
        if headers.len() > MAX_HEADERS {
            return Err(Error::HeadTooLarge);
        }
    }
    let mut length: Option<usize> = None;
    let mut chunked = false;
    for (n, v) in &headers {
        if n.eq_ignore_ascii_case("content-length") {
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::Bad("content-length"));
            }
            let l: usize = v.parse().map_err(|_| Error::BodyTooLarge)?;
            if length.is_some_and(|x| x != l) {
                return Err(Error::Bad("two content-lengths"));
            }
            length = Some(l);
        } else if n.eq_ignore_ascii_case("transfer-encoding") {
            if !v.eq_ignore_ascii_case("chunked") || chunked {
                return Err(Error::Bad("transfer-encoding"));
            }
            chunked = true;
        }
    }
    if chunked && length.is_some() {
        return Err(Error::Bad("content-length with transfer-encoding"));
    }
    let mut b = Buffered {
        r,
        pending: rest,
        at: 0,
    };
    let mut body = Vec::new();
    if chunked {
        loop {
            let size = b.line(64)?;
            let size = std::str::from_utf8(&size).map_err(|_| Error::Bad("chunk size"))?;
            let size = size.split(';').next().unwrap_or("").trim();
            if size.is_empty() || size.len() > 8 {
                return Err(Error::Bad("chunk size"));
            }
            let n = usize::from_str_radix(size, 16).map_err(|_| Error::Bad("chunk size"))?;
            if n == 0 {
                // Trailers, up to the blank line.
                let mut trailer_bytes = 0;
                loop {
                    let t = b.line(limits.head)?;
                    if t.is_empty() {
                        break;
                    }
                    trailer_bytes += t.len();
                    if trailer_bytes > limits.head {
                        return Err(Error::HeadTooLarge);
                    }
                }
                break;
            }
            if body.len() + n > limits.body {
                return Err(Error::BodyTooLarge);
            }
            let at = body.len();
            body.resize(at + n, 0);
            b.exact(&mut body[at..])?;
            if !b.line(2)?.is_empty() {
                return Err(Error::Bad("chunk end"));
            }
        }
    } else if let Some(n) = length {
        if n > limits.body {
            return Err(Error::BodyTooLarge);
        }
        body.resize(n, 0);
        b.exact(&mut body)?;
    }
    Ok(Request {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body,
    })
}

/// The reason phrase of the statuses we send.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        302 => "Found",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        421 => "Misdirected Request",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// An answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// A body of type `content_type`.
    pub fn body(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Response {
        self.headers
            .push(("Content-Type".into(), content_type.into()));
        self.body = body.into();
        self
    }

    /// The answer's bytes; without the body for a HEAD (`head_only`), with its length still.
    pub fn to_bytes(&self, head_only: bool) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status)).into_bytes();
        for (n, v) in &self.headers {
            // Never a header split by a value from elsewhere.
            if n.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
                continue;
            }
            out.extend_from_slice(format!("{n}: {v}\r\n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                self.body.len()
            )
            .as_bytes(),
        );
        if !head_only {
            out.extend_from_slice(&self.body);
        }
        out
    }

    pub fn write_to<W: Write>(&self, w: &mut W, head_only: bool) -> io::Result<()> {
        w.write_all(&self.to_bytes(head_only))?;
        w.flush()
    }
}

/// An address of the local network or of the device itself: loopback, the private ranges
/// (RFC 1918, fc00::/7), link-local, and IPv4 inside IPv6 the same way.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_private() || a.is_loopback() || a.is_link_local(),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_private(IpAddr::V4(v4));
            }
            let s = a.segments();
            a.is_loopback() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Limits = Limits::new(1024, 64);

    fn read(s: &[u8]) -> Result<Request, Error> {
        read_request(&mut &s[..], L)
    }

    /// A reader that gives one byte at a time (a request split over many packets).
    struct Trickle<'a>(&'a [u8]);
    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() || out.is_empty() {
                return Ok(0);
            }
            out[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    #[test]
    fn a_get_and_a_post() {
        let r =
            read(b"GET /a/b?x=1&y=2 HTTP/1.1\r\nHost: 192.0.2.5:8125\r\nX-A:  v \r\n\r\n").unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path(), "/a/b");
        assert_eq!(r.query(), "x=1&y=2");
        assert_eq!(r.header("host"), Some("192.0.2.5:8125"));
        assert_eq!(r.header("x-a"), Some("v"));
        assert!(r.body.is_empty());
        let r = read(b"POST /api HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello").unwrap();
        assert_eq!(r.body, b"hello");
        // LF only, as some clients write.
        let r = read(b"POST /api HTTP/1.0\nContent-Length: 2\n\nok").unwrap();
        assert_eq!(r.body, b"ok");
    }

    #[test]
    fn split_requests() {
        let raw = b"POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\nT: x\r\n\r\n";
        let r = read_request(&mut Trickle(raw), L).unwrap();
        assert_eq!(r.body, b"abcde");
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 3\r\n\r\nxyz";
        assert_eq!(read_request(&mut Trickle(raw), L).unwrap().body, b"xyz");
    }

    #[test]
    fn refused() {
        let status = |s: &[u8]| read(s).err().and_then(|e| e.status());
        // Too long.
        let mut big = b"GET /".to_vec();
        big.extend(std::iter::repeat_n(b'a', 2000));
        big.extend_from_slice(b" HTTP/1.1\r\n\r\n");
        assert_eq!(status(&big), Some(431));
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 65\r\n\r\n"),
            Some(413)
        );
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n"),
            Some(413)
        );
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n41\r\n"),
            Some(413)
        );
        // Lying or doubled lengths, smuggling shapes.
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 5x\r\n\r\n"),
            Some(400)
        );
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n"),
            Some(400)
        );
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 3\r\nContent-Length: 4\r\n\r\nabcd"),
            Some(400)
        );
        assert!(
            read(b"POST / HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nab").is_ok()
        );
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Some(400)
        );
        assert_eq!(
            status(b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n"),
            Some(400)
        );
        assert_eq!(
            status(b"GET / HTTP/1.1\r\nA: b\r\n folded\r\n\r\n"),
            Some(400)
        );
        assert_eq!(status(b"GET / HTTP/1.1\r\nBad Name: b\r\n\r\n"), Some(400));
        // Not HTTP/1.x, odd methods and targets.
        assert_eq!(status(b"GET / HTTP/2.0\r\n\r\n"), Some(400));
        assert_eq!(status(b"GET http://x/ HTTP/1.1\r\n\r\n"), Some(400));
        assert_eq!(status(b"G(T / HTTP/1.1\r\n\r\n"), Some(400));
        assert_eq!(status(b"GET  / HTTP/1.1\r\n\r\n"), Some(400));
        // A method we do not know is read: the caller answers 405.
        assert_eq!(read(b"BREW / HTTP/1.1\r\n\r\n").unwrap().method, "BREW");
        // Closed early: nothing to answer.
        assert!(matches!(read(b"GET / HTTP/1.1\r\n"), Err(Error::Closed)));
        assert!(matches!(
            read(b"POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\nab"),
            Err(Error::Closed)
        ));
    }

    #[test]
    fn answers() {
        let r = Response::new(200)
            .header("X-A", "1")
            .header("Evil", "a\r\nSet-Cookie: x")
            .body("text/plain", "hi");
        let s = String::from_utf8(r.to_bytes(false)).unwrap();
        assert_eq!(
            s,
            "HTTP/1.1 200 OK\r\nX-A: 1\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi"
        );
        assert!(
            String::from_utf8(r.to_bytes(true))
                .unwrap()
                .ends_with("\r\n\r\n")
        );
    }

    #[test]
    fn private_addresses() {
        for a in [
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(is_private(a.parse().unwrap()), "{a}");
        }
        for a in [
            "192.0.2.5",
            "8.8.8.8",
            "172.32.0.1",
            "2001:db8::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_private(a.parse().unwrap()), "{a}");
        }
    }

    /// Random bytes and damaged requests never make the reader panic, and never let a body past
    /// its limit.
    #[test]
    fn random_input_never_panics() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let seeds: [&[u8]; 4] = [
            b"GET /a?b=c HTTP/1.1\r\nHost: x\r\n\r\n",
            b"POST /api/set HTTP/1.1\r\nContent-Length: 10\r\nContent-Type: application/json\r\n\r\n{\"a\":true}",
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n0\r\n\r\n",
            b"\r\n\r\n",
        ];
        for round in 0..20_000 {
            let mut v: Vec<u8> = if round % 3 == 0 {
                (0..next() % 300).map(|_| next() as u8).collect()
            } else {
                seeds[(next() % 4) as usize].to_vec()
            };
            for _ in 0..(next() % 6) {
                if v.is_empty() {
                    break;
                }
                let i = (next() as usize) % v.len();
                match next() % 3 {
                    0 => v[i] = next() as u8,
                    1 => {
                        v.remove(i);
                    }
                    _ => v.insert(
                        i,
                        [b'\r', b'\n', b':', b' ', b'0', b'f'][(next() % 6) as usize],
                    ),
                }
            }
            if let Ok(r) = read_request(&mut &v[..], L) {
                assert!(r.body.len() <= L.body);
            }
            let _ = read_request(&mut Trickle(&v), L);
        }
    }
}
