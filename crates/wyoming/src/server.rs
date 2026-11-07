// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The satellite side of Wyoming: Home Assistant is the TCP client and connects to us.
//!
//! Policy:
//! - only the addresses in [`ServerConfig::allowed`] may connect (empty = nobody);
//! - any number of allowed connections at once (up to 16), like wyoming's `AsyncServer`, which
//!   runs one handler per connection;
//! - every connection gets `describe` answered with the configured [`Info`] and `ping` with
//!   `pong` by its reader thread, at once;
//! - only a connection that sends `run-satellite` becomes the **satellite link**: the sink sees
//!   `Connected` for it (just before its `run-satellite`), all its messages, and `Disconnected`
//!   when it ends. A `run-satellite` on another connection replaces the link: the old reader
//!   reports `Disconnected(.., Replaced)` **before** the new `Connected` (Home Assistant
//!   reconnects this way after unmute, reload and restart, and the old socket may linger);
//! - connections that never send `run-satellite` (Home Assistant's short `describe -> info ->
//!   close` probes: `load_wyoming_info` from the config flow, zeroconf rediscovery and entry
//!   setup) are invisible to the sink: they never disturb the link, and their close is only
//!   logged. Their other messages are ignored. wyoming-satellite behaves the same way: its
//!   handler answers `describe` without claiming the satellite and only clears the server it
//!   claimed;
//! - with [`ServerConfig::ping_interval`] the server also pings Home Assistant on the link, 2 s
//!   after `run-satellite` and after each `pong` (mirroring Home Assistant's
//!   `_send_delayed_ping`), not while the satellite is paused (Home Assistant stops reading a
//!   muted satellite's connection), and closes the link with [`Disconnect::PongTimeout`] when no
//!   `pong` comes within [`ServerConfig::pong_timeout`] (15 s).
//!
//! Why the ping matters: Home Assistant's `_run_pipeline_loop` waits at most `_PING_TIMEOUT = 5`
//! s for *any* event from the satellite and sends `ping` 2 s after each `pong`; a satellite that
//! does not answer is dropped and reconnected (every ~8 s, what wyoming-satellite 1.0 suffered).

use std::collections::HashMap;
use std::io::{BufReader, ErrorKind, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::event::{encode_event, encode_raw, read_event_limited, with_buffers};
use crate::info::Info;
use crate::message::{AudioFormat, Message};
use crate::net::{self, Net};
use crate::{Error, MAX_FRAME};

/// Configuration of [`Server`]. Build it with `..Default::default()` for the hidden fields.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Default `0.0.0.0:10700`.
    pub bind: SocketAddr,
    /// Who may connect: Home Assistant's address(es). Empty = nobody (warned once a minute).
    pub allowed: Vec<Net>,
    /// More addresses that may connect, changed while it runs (Home Assistant's own addresses,
    /// learned by the body after the QR link).
    pub learned: Option<std::sync::Arc<std::sync::RwLock<Vec<Net>>>>,
    /// Answered to every `describe`.
    pub info: Info,
    /// `Some(2 s)`: also ping Home Assistant on the satellite link (not while paused); no `pong`
    /// within `pong_timeout` = disconnect.
    pub ping_interval: Option<Duration>,
    /// No `pong` within this after our `ping` = [`Disconnect::PongTimeout`] (default 15 s:
    /// generous, the link is also watched by Home Assistant's own 5 s rule).
    pub pong_timeout: Duration,
    /// Largest data block or payload accepted (default 16 MiB).
    pub max_frame: usize,
    /// Answer `ping` with `pong` (always true outside the tests of a misbehaving satellite).
    #[doc(hidden)]
    pub answer_pings: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: SocketAddr::from(([0, 0, 0, 0], 10700)),
            allowed: Vec::new(),
            learned: None,
            info: Info::default(),
            ping_interval: Some(Duration::from_secs(2)),
            max_frame: MAX_FRAME,
            pong_timeout: Duration::from_secs(15),
            answer_pings: true,
        }
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disconnect {
    /// The peer closed it.
    Eof,
    /// Read or write error, bad frame, write timeout (2 s).
    Io(String),
    /// Another connection sent `run-satellite` and took its place.
    Replaced,
    /// No `pong` to our `ping` in time.
    PongTimeout,
    /// [`Server::shutdown`].
    Shutdown,
}

/// What the server reports to its sink.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// A connection became the satellite link (it sent `run-satellite`, reported next).
    Connected(SocketAddr),
    /// The satellite link ended. Probe connections (no `run-satellite`) are not reported.
    Disconnected(SocketAddr, Disconnect),
    /// Not in the allow-list: closed at once (the log line is rate-limited, the event is not).
    Rejected(SocketAddr),
    /// Every message from the satellite link, `describe` and `ping` included (already answered);
    /// the `pong` to our own `ping` is swallowed.
    Message(Message),
}

type Sink = Box<dyn Fn(ServerEvent) + Send + Sync + 'static>;

/// One accepted connection.
struct Conn {
    id: u64,
    peer: SocketAddr,
    /// Write half (a clone of the socket); writes are serialized by this mutex.
    writer: Mutex<TcpStream>,
    /// Set by whoever closes the connection, read by the reader when it exits.
    reason: Mutex<Option<Disconnect>>,
    ping: Mutex<PingState>,
    /// Became the satellite link (sent `run-satellite`) at some point: its end is reported.
    link: AtomicBool,
}

#[derive(Debug, Clone, Copy)]
struct PingState {
    /// When to send the next ping (None while one is outstanding, before `run-satellite` and
    /// while paused).
    next_at: Option<Instant>,
    /// When the outstanding ping was sent.
    sent_at: Option<Instant>,
}

impl Conn {
    /// Closes the socket; the reader wakes up and reports `reason` (the first one set wins).
    fn close(&self, reason: Disconnect) {
        {
            let mut r = lock(&self.reason);
            if r.is_none() {
                *r = Some(reason);
            }
        }
        let _ = lock(&self.writer).shutdown(Shutdown::Both);
    }

    /// Writes a frame; on failure closes the connection (a write timeout included).
    fn write_frame(&self, frame: &[u8]) -> Result<(), Error> {
        let res = {
            let mut w = lock(&self.writer);
            w.write_all(frame).and_then(|_| w.flush())
        };
        if let Err(e) = res {
            let why = match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => "write timeout".to_string(),
                _ => e.to_string(),
            };
            self.close(Disconnect::Io(why));
            return Err(Error::Io(e));
        }
        Ok(())
    }

    fn send(&self, m: &Message) -> Result<(), Error> {
        let e = m.to_event();
        with_buffers(|buf, scratch| {
            encode_event(buf, scratch, &e.kind, &e.data, e.payload.as_deref());
            self.write_frame(buf)
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

struct Shared {
    cfg: ServerConfig,
    sink: Sink,
    /// The `info` frame, encoded once.
    info_frame: Vec<u8>,
    /// The satellite link.
    live: Mutex<Option<Arc<Conn>>>,
    /// Every open connection (the link included).
    conns: Mutex<Vec<Arc<Conn>>>,
    /// Serializes link take-overs.
    claim: Mutex<()>,
    readers: Mutex<Vec<(u64, JoinHandle<()>)>>,
    shutdown: AtomicBool,
    next_id: AtomicU64,
}

impl Shared {
    fn live(&self) -> Option<Arc<Conn>> {
        lock(&self.live).clone()
    }

    fn is_link(&self, conn: &Conn) -> bool {
        lock(&self.live).as_ref().is_some_and(|c| c.id == conn.id)
    }

    /// Makes `conn` the satellite link, replacing the current one (whose reader reports
    /// `Disconnected(.., Replaced)` before `Connected` is reported for `conn`).
    fn claim(&self, conn: &Arc<Conn>) {
        let _serial = lock(&self.claim);
        if let Some(old) = self.live() {
            log::info!(
                "wyoming: {} replaces {} as the satellite link",
                conn.peer,
                old.peer
            );
            old.close(Disconnect::Replaced);
            join_reader(self, old.id);
        }
        *lock(&conn.ping) = PingState {
            next_at: self.cfg.ping_interval.map(|i| Instant::now() + i),
            sent_at: None,
        };
        conn.link.store(true, Ordering::SeqCst);
        *lock(&self.live) = Some(conn.clone());
        log::info!("wyoming: {} is the satellite link", conn.peer);
        (self.sink)(ServerEvent::Connected(conn.peer));
    }
}

/// The satellite-side server. Dropping it shuts it down.
pub struct Server {
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
    ticker: Option<JoinHandle<()>>,
    bound: SocketAddr,
}

/// Writes to the satellite link; cheap to clone, usable from any thread.
#[derive(Clone)]
pub struct Sender(Arc<Shared>);

const TICK: Duration = Duration::from_millis(100);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const LOG_EVERY: Duration = Duration::from_secs(60);
/// Open connections at most (Home Assistant needs two: the link and a probe).
const MAX_CONNS: usize = 16;

impl Server {
    /// Binds `cfg.bind` and starts the accept thread (`wyoming-accept`).
    pub fn start(
        cfg: ServerConfig,
        sink: Box<dyn Fn(ServerEvent) + Send + Sync + 'static>,
    ) -> Result<Server, Error> {
        // std sets SO_REUSEADDR on Unix listeners.
        let listener = TcpListener::bind(cfg.bind)?;
        let local = listener.local_addr()?;
        let info_frame = with_buffers(|buf, scratch| {
            encode_event(buf, scratch, "info", &cfg.info.raw, None);
            buf.clone()
        });
        let learned: Vec<Net> = cfg
            .learned
            .as_ref()
            .and_then(|l| l.read().ok().map(|v| v.clone()))
            .unwrap_or_default();
        if cfg.allowed.is_empty() && learned.is_empty() {
            log::warn!("wyoming: the allow-list is empty, nobody can connect to {local}");
        }
        log::info!(
            "wyoming: listening on {local}, allowed: [{}]{}",
            cfg.allowed
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            if cfg.learned.is_some() {
                format!(
                    ", learned from Home Assistant: [{}]",
                    learned
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            } else {
                String::new()
            }
        );
        let shared = Arc::new(Shared {
            cfg,
            sink,
            info_frame,
            live: Mutex::new(None),
            conns: Mutex::new(Vec::new()),
            claim: Mutex::new(()),
            readers: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        });
        let s2 = shared.clone();
        let accept = std::thread::Builder::new()
            .name("wyoming-accept".into())
            .spawn(move || accept_loop(s2, listener))?;
        let s3 = shared.clone();
        let ticker = std::thread::Builder::new()
            .name("wyoming-ping".into())
            .spawn(move || tick_loop(s3))?;
        Ok(Server {
            shared,
            accept: Some(accept),
            ticker: Some(ticker),
            bound: local,
        })
    }

    pub fn sender(&self) -> Sender {
        Sender(self.shared.clone())
    }

    /// The bound address (useful with port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.bound
    }

    /// Closes the listener and every connection (`Disconnected(.., Shutdown)` for the satellite
    /// link), joins the threads. Returns within about 200 ms.
    pub fn shutdown(self) {
        drop(self)
    }

    fn stop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Wake the blocking accept with a connection to ourselves.
        let wake = match self.bound {
            SocketAddr::V4(a) if a.ip().is_unspecified() => {
                SocketAddr::from(([127, 0, 0, 1], a.port()))
            }
            SocketAddr::V6(a) if a.ip().is_unspecified() => {
                SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, a.port()))
            }
            a => a,
        };
        let woke = TcpStream::connect_timeout(&wake, Duration::from_millis(500)).is_ok();
        if let Some(h) = self.accept.take() {
            if woke {
                let _ = h.join();
            } else {
                log::warn!("wyoming: could not wake the accept thread, leaving it behind");
            }
        }
        if let Some(h) = self.ticker.take() {
            let _ = h.join();
        }
        let conns = lock(&self.shared.conns).clone();
        for c in conns {
            c.close(Disconnect::Shutdown);
        }
        let readers = std::mem::take(&mut *lock(&self.shared.readers));
        for (_, h) in readers {
            let _ = h.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Sender {
    /// Sends a message. `Err(NotConnected)` when nobody is connected (the caller treats it as
    /// "offline"); a failed or timed-out (2 s) write closes the connection.
    pub fn send(&self, m: &Message) -> Result<(), Error> {
        let c = self.0.live().ok_or(Error::NotConnected)?;
        c.send(m)
    }

    /// Sends one `audio-chunk` without building a `Message` (no allocation per chunk).
    pub fn send_chunk(
        &self,
        format: &AudioFormat,
        timestamp_ms: Option<i64>,
        pcm: &[u8],
    ) -> Result<(), Error> {
        let c = self.0.live().ok_or(Error::NotConnected)?;
        with_buffers(|buf, data| {
            // Same keys and separators as the Python writer (keys sorted, as serde_json writes
            // every other data block).
            let _ = write!(
                data,
                "{{\"channels\": {}, \"rate\": {}, \"timestamp\": ",
                format.channels, format.rate
            );
            match timestamp_ms {
                Some(t) => {
                    let _ = write!(data, "{t}");
                }
                None => data.extend_from_slice(b"null"),
            }
            let _ = write!(data, ", \"width\": {}}}", format.width);
            encode_raw(buf, "audio-chunk", data, pcm);
            c.write_frame(buf)
        })
    }

    pub fn is_connected(&self) -> bool {
        self.0.live().is_some()
    }

    /// Address of the satellite link.
    pub fn peer(&self) -> Option<SocketAddr> {
        self.0.live().map(|c| c.peer)
    }
}

/// Rate limiter for log lines: at most one per key per minute.
struct LogLimit(HashMap<Option<IpAddr>, Instant>);

impl LogLimit {
    fn allow(&mut self, key: Option<IpAddr>) -> bool {
        let now = Instant::now();
        self.0.retain(|_, t| now.duration_since(*t) < LOG_EVERY);
        if self.0.contains_key(&key) {
            return false;
        }
        self.0.insert(key, now);
        true
    }
}

fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    let mut limit = LogLimit(HashMap::new());
    loop {
        let accepted = listener.accept();
        if shared.shutdown.load(Ordering::SeqCst) {
            return; // the listener closes here
        }
        match accepted {
            Ok((stream, peer)) => {
                if let Err(e) = accept_one(&shared, stream, peer, &mut limit) {
                    log::warn!("wyoming: could not set up the connection from {peer}: {e}");
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                // EMFILE and friends: keep serving, do not spin.
                log::warn!("wyoming: accept failed: {e}");
                std::thread::sleep(TICK);
            }
        }
    }
}

/// Our pings and the reaping of finished readers.
fn tick_loop(shared: Arc<Shared>) {
    while !shared.shutdown.load(Ordering::SeqCst) {
        std::thread::sleep(TICK);
        ping_tick(&shared);
        reap_readers(&shared);
    }
}

fn accept_one(
    shared: &Arc<Shared>,
    stream: TcpStream,
    peer: SocketAddr,
    limit: &mut LogLimit,
) -> std::io::Result<()> {
    let cfg = &shared.cfg;
    let learned: Vec<Net> = cfg
        .learned
        .as_ref()
        .and_then(|l| l.read().ok().map(|v| v.clone()))
        .unwrap_or_default();
    if !net::allowed(&cfg.allowed, &peer.ip()) && !net::allowed(&learned, &peer.ip()) {
        let _ = stream.shutdown(Shutdown::Both);
        drop(stream);
        if cfg.allowed.is_empty() && learned.is_empty() {
            if limit.allow(None) {
                log::warn!(
                    "wyoming: rejected {peer}: the allow-list is empty (set satellite.allowed)"
                );
            }
        } else if limit.allow(Some(peer.ip())) {
            log::warn!("wyoming: rejected {peer}: not in the allow-list");
        }
        (shared.sink)(ServerEvent::Rejected(peer));
        return Ok(());
    }
    if lock(&shared.conns).len() >= MAX_CONNS {
        let _ = stream.shutdown(Shutdown::Both);
        if limit.allow(Some(peer.ip())) {
            log::warn!("wyoming: refused {peer}: {MAX_CONNS} connections already open");
        }
        return Ok(());
    }
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let writer = stream.try_clone()?;
    let id = shared.next_id.fetch_add(1, Ordering::SeqCst);
    let conn = Arc::new(Conn {
        id,
        peer,
        writer: Mutex::new(writer),
        reason: Mutex::new(None),
        // No pings before run-satellite.
        ping: Mutex::new(PingState {
            next_at: None,
            sent_at: None,
        }),
        link: AtomicBool::new(false),
    });
    log::debug!("wyoming: {peer} connected");
    lock(&shared.conns).push(conn.clone());

    let s2 = shared.clone();
    let reader = std::thread::Builder::new()
        .name("wyoming-read".into())
        .spawn(move || read_loop(s2, conn, stream))?;
    lock(&shared.readers).push((id, reader));
    Ok(())
}

fn join_reader(shared: &Shared, id: u64) {
    let h = {
        let mut r = lock(&shared.readers);
        r.iter()
            .position(|(i, _)| *i == id)
            .map(|p| r.swap_remove(p).1)
    };
    if let Some(h) = h {
        let _ = h.join();
    }
}

fn reap_readers(shared: &Shared) {
    let done: Vec<JoinHandle<()>> = {
        let mut r = lock(&shared.readers);
        let mut done = Vec::new();
        let mut i = 0;
        while i < r.len() {
            if r[i].1.is_finished() {
                done.push(r.swap_remove(i).1);
            } else {
                i += 1;
            }
        }
        done
    };
    for h in done {
        let _ = h.join();
    }
}

fn ping_tick(shared: &Shared) {
    if shared.cfg.ping_interval.is_none() {
        return;
    }
    let Some(conn) = shared.live() else { return };
    let now = Instant::now();
    let state = *lock(&conn.ping);
    if let Some(sent) = state.sent_at {
        if now.duration_since(sent) >= shared.cfg.pong_timeout {
            log::warn!(
                "wyoming: no pong from {} within {:?}, closing",
                conn.peer,
                shared.cfg.pong_timeout
            );
            conn.close(Disconnect::PongTimeout);
        }
        return;
    }
    if state.next_at.is_some_and(|t| now >= t) {
        {
            let mut p = lock(&conn.ping);
            p.next_at = None;
            p.sent_at = Some(now);
        }
        if conn.send(&Message::Ping { text: None }).is_err() {
            log::debug!("wyoming: ping to {} failed", conn.peer);
        }
    }
}

fn read_loop(shared: Arc<Shared>, conn: Arc<Conn>, stream: TcpStream) {
    let mut reader = BufReader::with_capacity(64 * 1024, stream);
    let max = shared.cfg.max_frame;
    let ended = loop {
        let event = match read_event_limited(&mut reader, max) {
            Ok(Some(e)) => e,
            Ok(None) => break Disconnect::Eof,
            Err(Error::Io(e)) => break Disconnect::Io(e.to_string()),
            Err(e) => break Disconnect::Io(e.to_string()),
        };
        let msg = Message::from_event(event);
        if !msg.is_audio() {
            log::debug!("wyoming: <- {}", msg.event_type());
        }
        match &msg {
            Message::RunSatellite => {
                if shared.is_link(&conn) {
                    // Resumed after pause-satellite: ping again.
                    let mut p = lock(&conn.ping);
                    if p.sent_at.is_none() && p.next_at.is_none() {
                        p.next_at = shared.cfg.ping_interval.map(|i| Instant::now() + i);
                    }
                } else {
                    shared.claim(&conn);
                }
            }
            Message::PauseSatellite if shared.is_link(&conn) => {
                // Home Assistant stops reading a paused satellite: no pings until run-satellite.
                *lock(&conn.ping) = PingState {
                    next_at: None,
                    sent_at: None,
                };
            }
            Message::Describe => {
                if conn.write_frame(&shared.info_frame).is_err() {
                    continue; // the read side reports the end
                }
            }
            Message::Ping { text } if shared.cfg.answer_pings => {
                let _ = conn.send(&Message::Pong { text: text.clone() });
            }
            Message::Pong { .. } => {
                let mut p = lock(&conn.ping);
                if p.sent_at.is_some() {
                    p.sent_at = None;
                    p.next_at = shared.cfg.ping_interval.map(|i| Instant::now() + i);
                    continue; // our ping's answer: swallowed
                }
            }
            _ => {}
        }
        if shared.is_link(&conn) {
            (shared.sink)(ServerEvent::Message(msg));
        } else if !matches!(
            msg,
            Message::Describe | Message::Ping { .. } | Message::Pong { .. }
        ) {
            log::debug!(
                "wyoming: {} ignored from {}: not the satellite link (no run-satellite)",
                msg.event_type(),
                conn.peer
            );
        }
    };
    let reason = lock(&conn.reason).clone().unwrap_or(ended);
    // Close our side too and leave the link slot if we still hold it.
    let _ = lock(&conn.writer).shutdown(Shutdown::Both);
    {
        let mut live = lock(&shared.live);
        if live.as_ref().is_some_and(|c| c.id == conn.id) {
            *live = None;
        }
    }
    lock(&shared.conns).retain(|c| c.id != conn.id);
    if conn.link.load(Ordering::SeqCst) {
        log::info!("wyoming: {} disconnected: {reason:?}", conn.peer);
        (shared.sink)(ServerEvent::Disconnected(conn.peer, reason));
    } else {
        log::debug!("wyoming: {} (probe) disconnected: {reason:?}", conn.peer);
    }
}
