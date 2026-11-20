// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Who may use the browser panel: the access requests waiting for a yes on the screen, the QR
//! code's one-time ticket, the devices let in, and the limits that keep a flood of requests from
//! becoming a flood of pop-ups. Pure: the time ([`Now`]: the wall clock for what is kept on disk,
//! a monotonic one for every limit) and the random bytes come from the caller, the file is
//! written only by [`Sessions::save`].
//!
//! - A request: an id of 128 random bits (the browser keeps it) and a 4-digit code shown on the
//!   screen and in the browser, to tell two requests apart (not a password). It waits
//!   [`REQUEST_TTL`]. One at a time, and [`PAUSE`] after the last one ended (a new dialog never
//!   replaces the previous one under a finger). After a no or a timeout the same address waits
//!   [`REFUSED_WAIT`]; after [`FAIL_LIMIT`] of them within [`FAIL_WINDOW`] nobody may ask: for
//!   10 minutes, then an hour, then until someone opens Settings → Access from the browser on
//!   the screen (the home shows a card; the QR code there still lets in).
//! - The QR code: a ticket of 256 random bits, good once and for [`QR_TTL`]; whoever scans it has
//!   the screen in front of them, so it lets in without the code. Compared in constant time.
//! - A device let in: a token of 256 random bits, handed to the browser once (its cookie); only
//!   its SHA-256 is kept, in `panel-sessions.json` (0600). Good for [`SESSION_TTL`] from its last
//!   use; one never used goes after [`GHOST_TTL`] (a token handed to nobody); at most
//!   [`MAX_SESSIONS`] (the sixth must have one revoked on the screen first).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::sha256;

pub const FILE: &str = "panel-sessions.json";
pub const REQUEST_TTL: Duration = Duration::from_secs(60);
pub const QR_TTL: Duration = Duration::from_secs(120);
/// From the last use, wall clock (kept on disk).
pub const SESSION_TTL: u64 = 30 * 86400;
/// A device let in that never made a request: its token went nowhere.
pub const GHOST_TTL: u64 = 60;
pub const MAX_SESSIONS: usize = 5;
pub const REFUSED_WAIT: Duration = Duration::from_secs(10 * 60);
pub const FAIL_WINDOW: Duration = Duration::from_secs(10 * 60);
pub const FAIL_LIMIT: usize = 3;
/// The first stop, then the second; after that it lasts until the screen's page is opened.
pub const BLOCKS: [Duration; 2] = [Duration::from_secs(10 * 60), Duration::from_secs(3600)];
/// Calls without a device let in, per second, from everyone.
pub const OPEN_CALLS: u32 = 30;
/// Between the end of one request and the next dialog.
pub const PAUSE: Duration = Duration::from_secs(4);
/// A decided request is remembered this long, for the browser to learn the answer.
const DECIDED_KEEP: Duration = Duration::from_secs(120);
/// `last_used` is written to disk at most this often per device.
const TOUCH_EVERY: u64 = 3600;

/// The time: `wall` in seconds since the epoch (what goes on disk), `mono` for the limits (a
/// clock set back or forward changes none of them).
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub wall: u64,
    pub mono: Instant,
}

impl Now {
    pub fn system() -> Now {
        Now {
            wall: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            mono: Instant::now(),
        }
    }

    /// `d` later on both clocks.
    pub fn plus(self, d: Duration) -> Now {
        Now {
            wall: self.wall + d.as_secs(),
            mono: self.mono + d,
        }
    }
}

/// Random bytes (the system's in the body, a fixed sequence in the tests); false: none to be
/// had, and then nothing may be let in.
pub type Rng<'a> = &'a mut dyn FnMut(&mut [u8]) -> bool;

/// The system's random bytes.
pub fn system_random(out: &mut [u8]) -> bool {
    use std::io::Read;
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(out)) {
        Ok(()) => true,
        Err(e) => {
            log::error!(event = "panel"; "/dev/urandom: {e}: nobody let in");
            false
        }
    }
}

fn random_hex(rng: Rng, bytes: usize) -> Option<String> {
    let mut b = vec![0u8; bytes];
    rng(&mut b).then(|| sha256::hex(&b))
}

/// Constant-time equality of two strings of the same length (different lengths: false).
pub fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hash(token: &str) -> String {
    sha256::hex(&sha256::digest(token.as_bytes()))
}

/// What the browser said it is ("Chrome", "macOS"), as the panel guessed it from User-Agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub browser: String,
    pub os: String,
}

impl Agent {
    /// Short and printable only: it is shown on the screen.
    pub fn clean(browser: &str, os: &str) -> Agent {
        let c = |s: &str| -> String {
            s.chars()
                .filter(|c| c.is_alphanumeric() || " .-_".contains(*c))
                .take(24)
                .collect::<String>()
                .trim()
                .to_string()
        };
        Agent {
            browser: c(browser),
            os: c(os),
        }
    }
}

fn yes() -> bool {
    true
}

/// A device let in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// SHA-256 of the token (hex).
    pub hash: String,
    pub agent: Agent,
    /// The address it was let in from (only to recognise it).
    pub peer: String,
    pub created: u64,
    pub last_used: u64,
    /// Let in with the QR code (no code compared).
    #[serde(default)]
    pub qr: bool,
    /// The browser came back with its cookie at least once.
    #[serde(default = "yes")]
    pub used: bool,
}

impl Session {
    /// The id the screen and the panel revoke it by: the first 16 hex digits of the hash
    /// (never the token).
    pub fn id(&self) -> &str {
        &self.hash[..16.min(self.hash.len())]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    Waiting,
    /// Let in: the token, until the browser takes it.
    Allowed(String),
    Refused,
    Expired,
}

/// An access request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub code: String,
    pub peer: String,
    pub agent: Agent,
    created: Instant,
    decision: Decision,
    decided: Instant,
}

/// Why a request is not put to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Another request is waiting, or one just ended.
    Busy,
    /// Too many noes or timeouts lately: nobody may ask for a while.
    Blocked,
    /// This address was told no (or not answered) a moment ago.
    Wait,
    /// Not now (night, do not disturb, a conversation, a dialog…).
    CannotAsk,
    /// Five devices already.
    Full,
    /// The QR ticket is not (or no longer) good.
    BadTicket,
    /// No random bytes: nobody can be let in safely.
    Unavailable,
}

impl Refusal {
    pub fn key(self) -> &'static str {
        match self {
            Refusal::Busy => "busy",
            Refusal::Blocked => "blocked",
            Refusal::Wait => "wait",
            Refusal::CannotAsk => "cannot_ask",
            Refusal::Full => "full",
            Refusal::BadTicket => "bad_ticket",
            Refusal::Unavailable => "error",
        }
    }
}

/// What the browser learns about its request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Waiting,
    /// Let in: the token (handed once).
    Allowed(String),
    Refused,
    Expired,
    Unknown,
}

#[derive(Debug, Clone)]
struct Ticket {
    value: String,
    created: Instant,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    sessions: Vec<Session>,
}

/// Requests stopped: until when (None: until the screen's page is opened).
#[derive(Debug, Clone, Copy)]
struct Block {
    until: Option<Instant>,
}

/// The panel's access state (the body keeps one, on the screen's thread).
#[derive(Debug, Default)]
pub struct Sessions {
    path: Option<PathBuf>,
    list: Vec<Session>,
    requests: Vec<Request>,
    refused: HashMap<String, Instant>,
    failures: Vec<Instant>,
    block: Option<Block>,
    /// How many stops so far (each one longer); back to none once a device is let in or the
    /// screen's page is opened.
    stops: usize,
    /// When the last request ended (answered, expired, cancelled).
    last_end: Option<Instant>,
    /// Calls that need no device let in, this second, for everyone together.
    open_calls: (Option<Instant>, u32),
    ticket: Option<Ticket>,
    /// The device list changed since the screen last drew it.
    pub changed: bool,
    /// `last_used` moved by more than [`TOUCH_EVERY`] since the file was written.
    dirty: bool,
}

impl Sessions {
    /// From `<dir>/panel-sessions.json` (none or unreadable: no device let in).
    pub fn load(dir: &Path) -> Sessions {
        let path = dir.join(FILE);
        let list = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Stored>(&t).ok())
            .map(|s| s.sessions)
            .unwrap_or_default();
        Sessions {
            path: Some(path),
            list,
            ..Default::default()
        }
    }

    /// Written (0600) when something changed; a no-op without a file (tests, demos).
    pub fn save(&mut self) {
        self.dirty = false;
        let Some(path) = &self.path else { return };
        let text = serde_json::to_string_pretty(&Stored {
            sessions: self.list.clone(),
        })
        .unwrap_or_default();
        let tmp = path.with_extension("json.new");
        let r = (|| {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            let _ = std::fs::remove_file(&tmp);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = r {
            log::warn!(event = "panel"; "{}: {e}", path.display());
        }
    }

    /// The devices let in, newest first; expired ones are dropped first.
    pub fn list(&mut self, now: Now) -> &[Session] {
        self.expire(now);
        &self.list
    }

    /// The request waiting for the screen's answer.
    pub fn waiting(&self) -> Option<&Request> {
        self.requests
            .iter()
            .find(|r| r.decision == Decision::Waiting)
    }

    /// Too many noes lately: the home's card says so.
    pub fn blocked(&self, now: Now) -> bool {
        self.block
            .is_some_and(|b| b.until.is_none_or(|u| now.mono < u))
    }

    /// One more call that needs no device let in (hello, words, pairing): at most
    /// [`OPEN_CALLS`] a second for everyone together.
    pub fn open_call(&mut self, now: Now) -> bool {
        let (since, n) = &mut self.open_calls;
        if since.is_none_or(|s| now.mono >= s + Duration::from_secs(1)) {
            *since = Some(now.mono);
            *n = 0;
        }
        *n += 1;
        *n <= OPEN_CALLS
    }

    /// Tests elsewhere: the pause after a request is over.
    #[cfg(test)]
    pub fn skip_pause(&mut self) {
        self.last_end = None;
    }

    /// The screen's page opened: whoever looks at it may let devices in again.
    pub fn unblock(&mut self) {
        if self.block.is_some() || self.stops > 0 {
            log::info!(event = "panel"; "browser access: requests allowed again (the page was opened)");
        }
        self.block = None;
        self.stops = 0;
        self.failures.clear();
    }

    fn fail(&mut self, now: Now, peer: &str) {
        self.refused
            .insert(peer.to_string(), now.mono + REFUSED_WAIT);
        self.failures.retain(|t| now.mono < *t + FAIL_WINDOW);
        self.failures.push(now.mono);
        if self.failures.len() >= FAIL_LIMIT {
            let until = BLOCKS.get(self.stops).map(|d| now.mono + *d);
            self.stops += 1;
            self.block = Some(Block { until });
            self.failures.clear();
            match until {
                Some(_) => {
                    log::warn!(event = "panel"; "browser access: {FAIL_LIMIT} requests refused or unanswered, none for {} min", BLOCKS[self.stops - 1].as_secs() / 60)
                }
                None => {
                    log::warn!(event = "panel"; "browser access: {FAIL_LIMIT} requests refused or unanswered again, none until the page is opened on the screen")
                }
            }
        }
    }

    /// Time passing: a request waited too long (true: the screen's dialog closes), old
    /// decisions, devices unused for [`SESSION_TTL`] and never used for [`GHOST_TTL`] go.
    pub fn expire(&mut self, now: Now) -> bool {
        let mut timed_out = Vec::new();
        for r in &mut self.requests {
            if r.decision == Decision::Waiting && now.mono >= r.created + REQUEST_TTL {
                r.decision = Decision::Expired;
                r.decided = now.mono;
                timed_out.push(r.peer.clone());
            }
        }
        for peer in &timed_out {
            self.last_end = Some(now.mono);
            self.fail(now, peer);
        }
        self.requests
            .retain(|r| r.decision == Decision::Waiting || now.mono < r.decided + DECIDED_KEEP);
        self.refused.retain(|_, until| now.mono < *until);
        if self
            .block
            .is_some_and(|b| b.until.is_some_and(|u| now.mono >= u))
        {
            self.block = None;
        }
        let before = self.list.len();
        self.list.retain(|s| {
            now.wall < s.last_used + SESSION_TTL && (s.used || now.wall < s.created + GHOST_TTL)
        });
        if self.list.len() != before {
            self.changed = true;
            self.save();
        }
        if self
            .ticket
            .as_ref()
            .is_some_and(|t| now.mono >= t.created + QR_TTL)
        {
            self.ticket = None;
        }
        !timed_out.is_empty()
    }

    /// A browser asks to be let in: Ok(the request to show) or why not. `can_ask`: the screen
    /// may show a dialog now.
    pub fn request(
        &mut self,
        peer: &str,
        agent: Agent,
        can_ask: bool,
        now: Now,
        rng: Rng,
    ) -> Result<Request, Refusal> {
        self.expire(now);
        if self.blocked(now) {
            return Err(Refusal::Blocked);
        }
        if self.refused.contains_key(peer) {
            return Err(Refusal::Wait);
        }
        if self.waiting().is_some() || self.last_end.is_some_and(|t| now.mono < t + PAUSE) {
            return Err(Refusal::Busy);
        }
        if self.list.len() >= MAX_SESSIONS {
            return Err(Refusal::Full);
        }
        if !can_ask {
            return Err(Refusal::CannotAsk);
        }
        let mut c = [0u8; 4];
        if !rng(&mut c) {
            return Err(Refusal::Unavailable);
        }
        let r = Request {
            id: random_hex(rng, 16).ok_or(Refusal::Unavailable)?,
            code: format!("{:04}", u32::from_le_bytes(c) % 10_000),
            peer: peer.to_string(),
            agent,
            created: now.mono,
            decision: Decision::Waiting,
            decided: now.mono,
        };
        self.requests.push(r.clone());
        Ok(r)
    }

    /// The screen's answer to the waiting request (`id`): a yes lets the device in.
    pub fn answer(&mut self, id: &str, yes: bool, now: Now, rng: Rng) {
        let Some(i) = self
            .requests
            .iter()
            .position(|r| r.decision == Decision::Waiting && same(&r.id, id))
        else {
            return;
        };
        if yes {
            let r = self.requests[i].clone();
            match self.admit(&r.peer, r.agent.clone(), false, now, rng) {
                Ok(token) => self.requests[i].decision = Decision::Allowed(token),
                Err(_) => self.requests[i].decision = Decision::Refused,
            }
        } else {
            self.requests[i].decision = Decision::Refused;
            let peer = self.requests[i].peer.clone();
            self.fail(now, &peer);
        }
        self.requests[i].decided = now.mono;
        self.last_end = Some(now.mono);
    }

    /// The panel switched off with a request waiting: refused, and counted against nobody.
    pub fn cancel_waiting(&mut self, now: Now) -> bool {
        let mut any = false;
        for r in &mut self.requests {
            if r.decision == Decision::Waiting {
                r.decision = Decision::Refused;
                r.decided = now.mono;
                any = true;
            }
        }
        if any {
            self.last_end = Some(now.mono);
        }
        any
    }

    /// What the browser that made request `id` learns; a yes hands it the token once.
    pub fn status(&mut self, id: &str, now: Now) -> Status {
        self.expire(now);
        let Some(i) = self.requests.iter().position(|r| same(&r.id, id)) else {
            return Status::Unknown;
        };
        match &self.requests[i].decision {
            Decision::Waiting => Status::Waiting,
            Decision::Refused => Status::Refused,
            Decision::Expired => Status::Expired,
            Decision::Allowed(t) => {
                let t = t.clone();
                self.requests.remove(i);
                Status::Allowed(t)
            }
        }
    }

    fn admit(
        &mut self,
        peer: &str,
        agent: Agent,
        qr: bool,
        now: Now,
        rng: Rng,
    ) -> Result<String, Refusal> {
        if self.list.len() >= MAX_SESSIONS {
            return Err(Refusal::Full);
        }
        let token = random_hex(rng, 32).ok_or(Refusal::Unavailable)?;
        self.list.insert(
            0,
            Session {
                hash: hash(&token),
                agent,
                peer: peer.to_string(),
                created: now.wall,
                last_used: now.wall,
                qr,
                used: false,
            },
        );
        // Someone at the screen let a device in: the stops start over.
        self.stops = 0;
        self.changed = true;
        self.save();
        log::info!(event = "panel", qr = qr; "browser access: a device let in ({} now)", self.list.len());
        Ok(token)
    }

    /// The QR code's ticket while the page shows it: the one in force, or a new one when there
    /// is none (used, expired). `None`: no ticket can let anyone in now (five devices, no
    /// randomness).
    pub fn ticket(&mut self, now: Now, rng: Rng) -> Option<&str> {
        self.expire(now);
        if self.list.len() >= MAX_SESSIONS {
            self.ticket = None;
            return None;
        }
        if self.ticket.is_none() {
            self.ticket = Some(Ticket {
                value: random_hex(rng, 32)?,
                created: now.mono,
            });
        }
        self.ticket.as_ref().map(|t| t.value.as_str())
    }

    /// The page with the QR code closed: its ticket is good no more.
    pub fn drop_ticket(&mut self) {
        self.ticket = None;
    }

    /// The QR code scanned: a good ticket lets the device in (and is spent).
    pub fn redeem(
        &mut self,
        ticket: &str,
        peer: &str,
        agent: Agent,
        now: Now,
        rng: Rng,
    ) -> Result<String, Refusal> {
        self.expire(now);
        let good = self
            .ticket
            .as_ref()
            .is_some_and(|t| same(&t.value, ticket) && now.mono < t.created + QR_TTL);
        if !good {
            return Err(Refusal::BadTicket);
        }
        self.ticket = None;
        self.admit(peer, agent, true, now, rng)
    }

    /// The device holding `token`, if it is let in (its use noted).
    pub fn check(&mut self, token: &str, now: Now) -> Option<&Session> {
        if token.len() != 64 {
            return None;
        }
        self.expire(now);
        let h = hash(token);
        let i = self.list.iter().position(|s| same(&s.hash, &h))?;
        if !self.list[i].used || now.wall >= self.list[i].last_used + TOUCH_EVERY {
            self.list[i].used = true;
            self.list[i].last_used = now.wall;
            self.dirty = true;
        }
        if self.dirty {
            self.save();
        }
        self.list.get(i)
    }

    /// "Log out on this device".
    pub fn end(&mut self, token: &str) {
        let h = hash(token);
        self.remove(|s| same(&s.hash, &h));
    }

    /// Revoked on the screen or from another device; true if there was one.
    pub fn revoke(&mut self, id: &str) -> bool {
        if id.len() != 16 {
            return false;
        }
        self.remove(|s| same(s.id(), id))
    }

    pub fn revoke_all(&mut self) {
        self.remove(|_| true);
    }

    fn remove(&mut self, f: impl Fn(&Session) -> bool) -> bool {
        let before = self.list.len();
        self.list.retain(|s| !f(s));
        let gone = self.list.len() != before;
        if gone {
            self.changed = true;
            self.save();
            log::info!(event = "panel"; "browser access: {} device(s) revoked", before - self.list.len());
        }
        gone
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// A counter as randomness: different, predictable bytes, never repeating in a test.
    pub fn counter() -> impl FnMut(&mut [u8]) -> bool {
        let mut n: u64 = 0;
        move |out: &mut [u8]| {
            for chunk in out.chunks_mut(8) {
                n += 1;
                let v = n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
            }
            true
        }
    }

    fn agent() -> Agent {
        Agent::clean("Chrome", "macOS")
    }

    /// `secs` after the tests' start, on both clocks.
    pub fn t(secs: u64) -> Now {
        static BASE: OnceLock<Instant> = OnceLock::new();
        Now {
            wall: 1_800_000_000,
            mono: *BASE.get_or_init(Instant::now),
        }
        .plus(Duration::from_secs(secs))
    }

    #[test]
    fn yes_lets_in_once() {
        let mut rng = counter();
        let mut s = Sessions::default();
        let r = s
            .request("192.0.2.24", agent(), true, t(0), &mut rng)
            .unwrap();
        assert_eq!(r.id.len(), 32);
        assert_eq!(r.code.len(), 4);
        assert!(r.code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(s.waiting().map(|w| w.id.clone()), Some(r.id.clone()));
        assert_eq!(s.status(&r.id, t(1)), Status::Waiting);
        s.answer(&r.id, true, t(2), &mut rng);
        let Status::Allowed(token) = s.status(&r.id, t(3)) else {
            panic!("not allowed")
        };
        assert_eq!(token.len(), 64);
        // Handed once.
        assert_eq!(s.status(&r.id, t(4)), Status::Unknown);
        assert!(s.check(&token, t(5)).is_some());
        assert!(s.check(&"0".repeat(64), t(5)).is_none());
        assert!(s.check("short", t(5)).is_none());
        // Only the hash is kept.
        assert_ne!(s.list(t(0))[0].hash, token);
        s.end(&token);
        assert!(s.check(&token, t(6)).is_none());
    }

    #[test]
    fn no_and_timeouts_are_limited() {
        let mut rng = counter();
        let mut s = Sessions::default();
        // One at a time, and a pause after each.
        let a = s
            .request("192.0.2.1", agent(), true, t(0), &mut rng)
            .unwrap();
        assert_eq!(
            s.request("192.0.2.2", agent(), true, t(0), &mut rng),
            Err(Refusal::Busy)
        );
        s.answer(&a.id, false, t(5), &mut rng);
        assert_eq!(s.status(&a.id, t(6)), Status::Refused);
        assert_eq!(
            s.request("192.0.2.2", agent(), true, t(7), &mut rng),
            Err(Refusal::Busy)
        );
        // The same address waits 10 minutes; another may ask.
        assert_eq!(
            s.request("192.0.2.1", agent(), true, t(60), &mut rng),
            Err(Refusal::Wait)
        );
        let b = s
            .request("192.0.2.2", agent(), true, t(60), &mut rng)
            .unwrap();
        // Unanswered for 60 s: expired (the second failure), and that address waits too.
        assert!(!s.expire(t(119)));
        assert!(s.expire(t(120)));
        assert_eq!(s.status(&b.id, t(121)), Status::Expired);
        assert!(!s.blocked(t(121)));
        assert_eq!(
            s.request("192.0.2.2", agent(), true, t(130), &mut rng),
            Err(Refusal::Wait)
        );
        // A third: nobody may ask for 10 minutes, and the home says so.
        let c = s
            .request("192.0.2.3", agent(), true, t(130), &mut rng)
            .unwrap();
        s.answer(&c.id, false, t(131), &mut rng);
        assert!(s.blocked(t(131)));
        assert_eq!(
            s.request("192.0.2.4", agent(), true, t(200), &mut rng),
            Err(Refusal::Blocked)
        );
        // Ten minutes: free; three more noes: an hour.
        let mut at = 131 + 600;
        assert!(!s.blocked(t(at)));
        for peer in ["192.0.2.10", "192.0.2.11", "192.0.2.12"] {
            let r = s.request(peer, agent(), true, t(at), &mut rng).unwrap();
            s.answer(&r.id, false, t(at), &mut rng);
            at += 10;
        }
        assert!(s.blocked(t(at + 3000)));
        assert!(!s.blocked(t(at + 3600)));
        // Three more: until the page is opened on the screen.
        at += 3600;
        for peer in ["192.0.2.13", "192.0.2.14", "192.0.2.15"] {
            let r = s.request(peer, agent(), true, t(at), &mut rng).unwrap();
            s.answer(&r.id, false, t(at), &mut rng);
            at += 10;
        }
        assert!(s.blocked(t(at + 86400)));
        s.unblock();
        assert!(!s.blocked(t(at)));
        assert!(
            s.request("192.0.2.16", agent(), true, t(at), &mut rng)
                .is_ok()
        );
        // Not now (night, a conversation…): nothing shown, nothing counted.
        let mut s = Sessions::default();
        for _ in 0..5 {
            assert_eq!(
                s.request("192.0.2.9", agent(), false, t(0), &mut rng),
                Err(Refusal::CannotAsk)
            );
        }
        assert!(s.waiting().is_none() && !s.blocked(t(0)));
        // A wrong id answers nothing.
        let r = s
            .request("192.0.2.9", agent(), true, t(0), &mut rng)
            .unwrap();
        s.answer("nope", true, t(0), &mut rng);
        assert_eq!(s.status(&r.id, t(0)), Status::Waiting);
        assert_eq!(s.status("nope", t(0)), Status::Unknown);
        // Switched off while it waits: refused, counted against nobody.
        assert!(s.cancel_waiting(t(1)));
        assert_eq!(s.status(&r.id, t(1)), Status::Refused);
        assert!(
            s.request("192.0.2.9", agent(), true, t(10), &mut rng)
                .is_ok()
        );
        // No randomness: nobody let in.
        let mut s = Sessions::default();
        let mut none = |_: &mut [u8]| false;
        assert_eq!(
            s.request("192.0.2.9", agent(), true, t(0), &mut none),
            Err(Refusal::Unavailable)
        );
        assert_eq!(s.ticket(t(0), &mut none), None);
    }

    #[test]
    fn five_devices_thirty_days_and_ghosts() {
        let mut rng = counter();
        let mut s = Sessions::default();
        let mut tokens = Vec::new();
        for i in 0..MAX_SESSIONS {
            let tk = s.ticket(t(0), &mut rng).unwrap().to_string();
            let token = s
                .redeem(&tk, &format!("192.0.2.{i}"), agent(), t(0), &mut rng)
                .unwrap();
            assert!(s.check(&token, t(1)).is_some());
            tokens.push(token);
        }
        assert_eq!(
            s.request("192.0.2.9", agent(), true, t(2), &mut rng),
            Err(Refusal::Full)
        );
        assert_eq!(s.ticket(t(2), &mut rng), None);
        // Revoke one: room again.
        let id = s.list(t(2))[0].id().to_string();
        assert!(s.revoke(&id));
        assert!(!s.revoke(&id));
        assert!(s.check(&tokens[4], t(2)).is_none());
        // A token handed to nobody (the browser never came back): gone after a minute.
        let tk = s.ticket(t(2), &mut rng).unwrap().to_string();
        s.redeem(&tk, "192.0.2.20", agent(), t(2), &mut rng)
            .unwrap();
        assert_eq!(s.list(t(30)).len(), 5);
        assert_eq!(s.list(t(2 + GHOST_TTL)).len(), 4);
        // Used now and then: still in after 40 days; unused for 30 days: gone.
        assert!(s.check(&tokens[0], t(20 * 86400)).is_some());
        assert!(s.check(&tokens[0], t(40 * 86400)).is_some());
        assert!(s.check(&tokens[1], t(40 * 86400)).is_none());
        assert_eq!(s.list(t(40 * 86400)).len(), 1);
        s.revoke_all();
        assert!(s.list(t(40 * 86400)).is_empty());
    }

    #[test]
    fn the_qr_ticket_is_good_once_and_for_two_minutes() {
        let mut rng = counter();
        let mut s = Sessions::default();
        let tk = s.ticket(t(0), &mut rng).unwrap().to_string();
        assert_eq!(tk.len(), 64);
        // The same while it is good.
        assert_eq!(s.ticket(t(60), &mut rng), Some(tk.as_str()));
        assert_eq!(
            s.redeem("x", "192.0.2.5", agent(), t(0), &mut rng),
            Err(Refusal::BadTicket)
        );
        let mut wrong = tk.clone();
        wrong.replace_range(0..1, if tk.starts_with('0') { "1" } else { "0" });
        assert_eq!(
            s.redeem(&wrong, "192.0.2.5", agent(), t(0), &mut rng),
            Err(Refusal::BadTicket)
        );
        let token = s
            .redeem(&tk, "192.0.2.5", agent(), t(10), &mut rng)
            .unwrap();
        assert!(s.check(&token, t(11)).unwrap().qr);
        // Spent: a second scan of the same code lets nobody in; a new one is made.
        assert_eq!(
            s.redeem(&tk, "192.0.2.6", agent(), t(12), &mut rng),
            Err(Refusal::BadTicket)
        );
        let t2 = s.ticket(t(12), &mut rng).unwrap().to_string();
        assert_ne!(t2, tk);
        // Expired after two minutes: refused, and a new one in its place.
        let after = 12 + QR_TTL.as_secs();
        assert_eq!(
            s.redeem(&t2, "192.0.2.6", agent(), t(after), &mut rng),
            Err(Refusal::BadTicket)
        );
        let t3 = s.ticket(t(after), &mut rng).unwrap().to_string();
        assert_ne!(t3, t2);
        s.drop_ticket();
        assert_eq!(
            s.redeem(&t3, "192.0.2.6", agent(), t(after + 1), &mut rng),
            Err(Refusal::BadTicket)
        );
        // The QR code lets in while requests are stopped.
        let mut s = Sessions::default();
        for p in ["192.0.2.1", "192.0.2.2", "192.0.2.3"] {
            let r = s.request(p, agent(), true, t(0), &mut rng).unwrap();
            s.answer(&r.id, false, t(0), &mut rng);
            s.last_end = None;
        }
        assert!(s.blocked(t(1)));
        let tk = s.ticket(t(1), &mut rng).unwrap().to_string();
        assert!(s.redeem(&tk, "192.0.2.7", agent(), t(1), &mut rng).is_ok());
    }

    #[test]
    fn the_file_holds_hashes_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let mut rng = counter();
        let dir = std::env::temp_dir().join(format!("home-companion-panel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Sessions::load(&dir);
        let tk = s.ticket(t(0), &mut rng).unwrap().to_string();
        let token = s.redeem(&tk, "192.0.2.5", agent(), t(0), &mut rng).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!text.contains(&token));
        assert!(text.contains(&hash(&token)));
        let mode = std::fs::metadata(dir.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mut again = Sessions::load(&dir);
        assert!(again.check(&token, t(1)).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agents_are_cleaned() {
        let a = Agent::clean("Chrome<script>", "a\nb\u{202e}cdefghijklmnopqrstuvwxyz0123");
        assert_eq!(a.browser, "Chromescript");
        assert_eq!(a.os.chars().count(), 24);
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "ab"));
    }
}
