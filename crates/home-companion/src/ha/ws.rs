// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A minimal WebSocket client (RFC 6455) for Home Assistant's `/api/websocket`: plain TCP
//! (`ws://`), text messages, ping/pong and close; no extensions, no TLS. Small on purpose: the
//! body has no async runtime and Home Assistant only needs text frames, so this costs no
//! dependency. Reads never block for long: with a read timeout on the stream, [`WebSocket::read`]
//! returns `Ok(None)` when nothing complete arrived and keeps the partial frame for the next call.
//!
//! The server's `Sec-WebSocket-Accept` is not checked (it would need SHA-1 and only protects
//! against caching proxies, which a LAN connection to Home Assistant does not cross).

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Larger messages are refused (a filtered `subscribe_entities` or a forecast is a few KiB).
pub const MAX_MESSAGE: usize = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    /// The server closed the connection (close frame).
    Close,
}

pub struct WebSocket<S: Read + Write> {
    stream: S,
    /// Received bytes not yet consumed (a partial frame after a timeout).
    rbuf: Vec<u8>,
    /// The fragments of a message split over several frames.
    partial: Option<Vec<u8>>,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Pseudo-random bytes for the handshake key and the frame masks (they need not be secret: the
/// mask only keeps intermediaries from reading frames as HTTP).
fn random_u64() -> u64 {
    static STATE: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    // splitmix64 over a counter mixed with the clock.
    let mut z = STATE
        .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
        .wrapping_add(nanos)
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One frame, masked as a client must.
pub fn encode_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = Vec::with_capacity(payload.len() + 14);
    f.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n <= 0xFFFF => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// A frame read from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
    /// Bytes of the buffer it took.
    pub used: usize,
}

/// A complete frame at the start of `buf`; None = more bytes needed.
pub fn decode_frame(buf: &[u8]) -> io::Result<Option<Frame>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let fin = buf[0] & 0x80 != 0;
    if buf[0] & 0x70 != 0 {
        return Err(invalid(
            "websocket: reserved bits set (no extension was negotiated)",
        ));
    }
    let opcode = buf[0] & 0x0F;
    let masked = buf[1] & 0x80 != 0;
    let (len, mut at) = match buf[1] & 0x7F {
        126 => {
            if buf.len() < 4 {
                return Ok(None);
            }
            (u16::from_be_bytes([buf[2], buf[3]]) as u64, 4)
        }
        127 => {
            if buf.len() < 10 {
                return Ok(None);
            }
            (u64::from_be_bytes(buf[2..10].try_into().unwrap()), 10)
        }
        n => (n as u64, 2),
    };
    if len > MAX_MESSAGE as u64 {
        return Err(invalid(format!("websocket: frame of {len} bytes")));
    }
    let mask = if masked {
        if buf.len() < at + 4 {
            return Ok(None);
        }
        let m = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
        at += 4;
        Some(m)
    } else {
        None
    };
    let end = at + len as usize;
    if buf.len() < end {
        return Ok(None);
    }
    let mut payload = buf[at..end].to_vec();
    if let Some(m) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    Ok(Some(Frame {
        fin,
        opcode,
        payload,
        used: end,
    }))
}

impl WebSocket<TcpStream> {
    /// Connects to `host:port`, upgrades `path` and leaves a 1 s read timeout on the stream.
    pub fn connect(host: &str, port: u16, path: &str, timeout: Duration) -> io::Result<Self> {
        let addr = (host, port).to_socket_addrs()?.next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{host}: no address"))
        })?;
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let host_header = if port == 80 {
            host.to_string()
        } else {
            format!("{host}:{port}")
        };
        let ws = WebSocket::handshake(stream, &host_header, path)?;
        ws.stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        Ok(ws)
    }
}

impl<S: Read + Write> WebSocket<S> {
    /// The HTTP upgrade on an open stream.
    pub fn handshake(mut stream: S, host: &str, path: &str) -> io::Result<Self> {
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&random_u64().to_le_bytes());
        key[8..].copy_from_slice(&random_u64().to_le_bytes());
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            base64(&key)
        );
        stream.write_all(req.as_bytes())?;
        stream.flush()?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if buf.len() > 16 * 1024 {
                return Err(invalid("websocket: response header too long"));
            }
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "websocket: closed during the handshake",
                ));
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]);
        let status = head.lines().next().unwrap_or("");
        if status.split_whitespace().nth(1) != Some("101") {
            return Err(invalid(format!("websocket: upgrade refused: {status}")));
        }
        let rbuf = buf[head_end..].to_vec();
        Ok(WebSocket {
            stream,
            rbuf,
            partial: None,
        })
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mask = (random_u64() as u32).to_le_bytes();
        self.stream
            .write_all(&encode_frame(opcode, payload, mask))?;
        self.stream.flush()
    }

    pub fn send_text(&mut self, text: &str) -> io::Result<()> {
        self.send(0x1, text.as_bytes())
    }

    /// Sends a close frame (best effort).
    pub fn close(&mut self) {
        let _ = self.send(0x8, &1000u16.to_be_bytes());
    }

    /// The next message; `Ok(None)` when the read timed out first. Pings are answered here.
    pub fn read(&mut self) -> io::Result<Option<Message>> {
        loop {
            while let Some(Frame {
                fin,
                opcode,
                payload,
                used,
            }) = decode_frame(&self.rbuf)?
            {
                self.rbuf.drain(..used);
                match opcode {
                    0x0 => {
                        let Some(p) = self.partial.as_mut() else {
                            return Err(invalid("websocket: continuation without a message"));
                        };
                        p.extend_from_slice(&payload);
                        if p.len() > MAX_MESSAGE {
                            return Err(invalid("websocket: message too large"));
                        }
                        if fin {
                            let p = self.partial.take().unwrap_or_default();
                            return text(p).map(Some);
                        }
                    }
                    0x1 if fin => return text(payload).map(Some),
                    0x1 => self.partial = Some(payload),
                    // Binary: Home Assistant sends none; skipped.
                    0x2 => {}
                    0x8 => return Ok(Some(Message::Close)),
                    0x9 => self.send(0xA, &payload)?,
                    0xA => {}
                    op => return Err(invalid(format!("websocket: opcode {op}"))),
                }
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "websocket: connection closed",
                    ));
                }
                Ok(n) => self.rbuf.extend_from_slice(&chunk[..n]),
                Err(e) if is_timeout(&e) => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

fn text(p: Vec<u8>) -> io::Result<Message> {
    String::from_utf8(p)
        .map(Message::Text)
        .map_err(|_| invalid("websocket: text is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0u8; 16]).len(), 24);
    }

    #[test]
    fn frames_round_trip_at_every_length_encoding() {
        for len in [0usize, 5, 125, 126, 300, 65535, 65536, 70000] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let f = encode_frame(0x1, &payload, [1, 2, 3, 4]);
            // Every prefix is "not yet".
            assert!(decode_frame(&f[..f.len() - 1]).unwrap().is_none());
            let fr = decode_frame(&f).unwrap().unwrap();
            assert!(fr.fin);
            assert_eq!(fr.opcode, 1);
            assert_eq!(fr.payload, payload);
            assert_eq!(fr.used, f.len());
        }
    }

    #[test]
    fn rejects_huge_frames_and_reserved_bits() {
        let mut f = vec![0x81, 127];
        f.extend_from_slice(&(u64::MAX).to_be_bytes());
        assert!(decode_frame(&f).is_err());
        assert!(decode_frame(&[0xC1, 0]).is_err());
    }

    /// An in-memory stream: what the "server" sent, what the client wrote.
    struct Pipe {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.input.read(buf)?;
            if n == 0 && !buf.is_empty() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            Ok(n)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![if fin { 0x80 } else { 0 } | opcode, payload.len() as u8];
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn handshake_then_fragments_pings_and_close() {
        let mut input = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
        input.extend(server_frame(false, 0x1, b"{\"type\":"));
        input.extend(server_frame(true, 0x9, b"hi"));
        input.extend(server_frame(true, 0x0, b"\"auth_required\"}"));
        input.extend(server_frame(true, 0x8, &[3, 232]));
        let pipe = Pipe {
            input: io::Cursor::new(input),
            output: Vec::new(),
        };
        let mut ws = WebSocket::handshake(pipe, "example.home:8123", "/api/websocket").unwrap();
        let req = String::from_utf8(ws.stream.output.clone()).unwrap();
        assert!(req.starts_with("GET /api/websocket HTTP/1.1\r\n"));
        assert!(req.contains("Sec-WebSocket-Version: 13\r\n"));
        ws.stream.output.clear();
        assert_eq!(
            ws.read().unwrap(),
            Some(Message::Text("{\"type\":\"auth_required\"}".into()))
        );
        // The ping was answered with a masked pong carrying the same payload.
        let fr = decode_frame(&ws.stream.output).unwrap().unwrap();
        assert_eq!((fr.opcode, fr.payload.as_slice()), (0xA, b"hi".as_slice()));
        assert_eq!(ws.read().unwrap(), Some(Message::Close));
        // Nothing more: a timeout, not an error.
        assert_eq!(ws.read().unwrap(), None);
    }

    #[test]
    fn handshake_refused() {
        let pipe = Pipe {
            input: io::Cursor::new(b"HTTP/1.1 404 Not Found\r\n\r\n".to_vec()),
            output: Vec::new(),
        };
        assert!(WebSocket::handshake(pipe, "h", "/api/websocket").is_err());
    }
}
