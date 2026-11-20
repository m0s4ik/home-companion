// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Who may use the browser panel: the access requests waiting for a yes on the screen, the QR
//! code's one-time ticket, the devices let in, and the limits that keep a flood of requests from
//! becoming a flood of pop-ups. Pure: the time (`now`, seconds since the epoch) and the random
//! bytes come from the caller, the file is written only by [`Sessions::save`].
//!
//! - A request: an id of 128 random bits (the browser keeps it) and a 4-digit code shown on the
//!   screen and in the browser, to tell two requests apart (not a password). It waits
//!   [`REQUEST_TTL`]. One at a time; after a no, the same address waits [`REFUSED_WAIT`]; after
//!   [`FAIL_LIMIT`] noes or timeouts within [`FAIL_WINDOW`] nobody may ask for [`BLOCK`] (the home
//!   shows a card).
//! - The QR code: a ticket of 128 random bits, good once and for [`QR_TTL`]; whoever scans it has
//!   the screen in front of them, so it lets in without the code. Compared in constant time.
//! - A device let in: a token of 256 random bits, handed to the browser once (its cookie); only
//!   its SHA-256 is kept, in `panel-sessions.json` (0600). Good for [`SESSION_TTL`] from its last
//!   use; at most [`MAX_SESSIONS`] (the sixth must have one revoked on the screen first).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::sha256;

pub const FILE: &str = "panel-sessions.json";
pub const REQUEST_TTL: u64 = 60;
pub const QR_TTL: u64 = 120;
pub const SESSION_TTL: u64 = 30 * 86400;
pub const MAX_SESSIONS: usize = 5;
pub const REFUSED_WAIT: u64 = 10 * 60;
pub const FAIL_WINDOW: u64 = 10 * 60;
pub const FAIL_LIMIT: usize = 3;
pub const BLOCK: u64 = 10 * 60;
/// A decided request is remembered this long, for the browser to learn the answer.
const DECIDED_KEEP: u64 = 120;
/// `last_used` is written to disk at most this often per device.
const TOUCH_EVERY: u64 = 3600;

/// Random bytes (the system's in the body, a fixed sequence in the tests).
pub type Rng<'a> = &'a mut dyn FnMut(&mut [u8]);

/// The system's random bytes.
pub fn system_random(out: &mut [u8]) {
    use std::io::Read;
    if let Err(e) = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(out)) {
        // Without randomness there is no safe token: nothing may be let in.
        panic!("/dev/urandom: {e}");
    }
}

fn random_hex(rng: Rng, bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rng(&mut b);
    sha256::hex(&b)
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
    pub created: u64,
    decision: Decision,
    decided: u64,
}

/// Why a request is not put to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Another request is waiting.
    Busy,
    /// Too many noes or timeouts lately: nobody may ask for a while.
    Blocked,
    /// This address was told no a moment ago.
    Wait,
    /// Not now (night, do not disturb, a conversation, a dialog…).
    CannotAsk,
    /// Five devices already.
    Full,
    /// The QR ticket is not (or no longer) good.
    BadTicket,
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
    created: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    sessions: Vec<Session>,
}

/// The panel's access state (the body keeps one, on the screen's thread).
#[derive(Debug, Default)]
pub struct Sessions {
    path: Option<PathBuf>,
    list: Vec<Session>,
    requests: Vec<Request>,
    refused: HashMap<String, u64>,
    failures: Vec<u64>,
    blocked_until: u64,
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
    pub fn list(&mut self, now: u64) -> &[Session] {
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
    pub fn blocked(&self, now: u64) -> bool {
        now < self.blocked_until
    }

    fn fail(&mut self, now: u64) {
        self.failures.retain(|t| now < t + FAIL_WINDOW);
        self.failures.push(now);
        if self.failures.len() >= FAIL_LIMIT {
            self.blocked_until = now + BLOCK;
            self.failures.clear();
            log::warn!(event = "panel"; "browser access: {FAIL_LIMIT} requests refused or unanswered, no requests for {} min", BLOCK / 60);
        }
    }

    /// Time passing: a request waited too long (true: the screen's dialog closes), old
    /// decisions and devices unused for [`SESSION_TTL`] go.
    pub fn expire(&mut self, now: u64) -> bool {
        let mut timed_out = false;
        for r in &mut self.requests {
            if r.decision == Decision::Waiting && now >= r.created + REQUEST_TTL {
                r.decision = Decision::Expired;
                r.decided = now;
                timed_out = true;
            }
        }
        if timed_out {
            self.fail(now);
        }
        self.requests
            .retain(|r| r.decision == Decision::Waiting || now < r.decided + DECIDED_KEEP);
        self.refused.retain(|_, until| now < *until);
        let before = self.list.len();
        self.list.retain(|s| now < s.last_used + SESSION_TTL);
        if self.list.len() != before {
            self.changed = true;
            self.save();
        }
        if self
            .ticket
            .as_ref()
            .is_some_and(|t| now >= t.created + QR_TTL)
        {
            self.ticket = None;
        }
        timed_out
    }

    /// A browser asks to be let in: Ok(the request to show) or why not. `can_ask`: the screen
    /// may show a dialog now.
    pub fn request(
        &mut self,
        peer: &str,
        agent: Agent,
        can_ask: bool,
        now: u64,
        rng: Rng,
    ) -> Result<Request, Refusal> {
        self.expire(now);
        if self.blocked(now) {
            return Err(Refusal::Blocked);
        }
        if self.refused.contains_key(peer) {
            return Err(Refusal::Wait);
        }
        if self.waiting().is_some() {
            return Err(Refusal::Busy);
        }
        if self.list.len() >= MAX_SESSIONS {
            return Err(Refusal::Full);
        }
        if !can_ask {
            return Err(Refusal::CannotAsk);
        }
        let mut c = [0u8; 4];
        rng(&mut c);
        let r = Request {
            id: random_hex(rng, 16),
            code: format!("{:04}", u32::from_le_bytes(c) % 10_000),
            peer: peer.to_string(),
            agent,
            created: now,
            decision: Decision::Waiting,
            decided: 0,
        };
        self.requests.push(r.clone());
        Ok(r)
    }

    /// The screen's answer to the waiting request (`id`): a yes lets the device in.
    pub fn answer(&mut self, id: &str, yes: bool, now: u64, rng: Rng) {
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
            self.refused.insert(peer, now + REFUSED_WAIT);
            self.fail(now);
        }
        self.requests[i].decided = now;
    }

    /// What the browser that made request `id` learns; a yes hands it the token once.
    pub fn status(&mut self, id: &str, now: u64) -> Status {
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
        now: u64,
        rng: Rng,
    ) -> Result<String, Refusal> {
        if self.list.len() >= MAX_SESSIONS {
            return Err(Refusal::Full);
        }
        let token = random_hex(rng, 32);
        self.list.insert(
            0,
            Session {
                hash: hash(&token),
                agent,
                peer: peer.to_string(),
                created: now,
                last_used: now,
                qr,
            },
        );
        self.changed = true;
        self.save();
        log::info!(event = "panel", qr = qr; "browser access: a device let in ({} now)", self.list.len());
        Ok(token)
    }

    /// The QR code's ticket while the page shows it: the one in force, or a new one when there
    /// is none (used, expired). `None`: no ticket can let anyone in now (five devices).
    pub fn ticket(&mut self, now: u64, rng: Rng) -> Option<&str> {
        self.expire(now);
        if self.list.len() >= MAX_SESSIONS {
            self.ticket = None;
            return None;
        }
        if self.ticket.is_none() {
            self.ticket = Some(Ticket {
                value: random_hex(rng, 16),
                created: now,
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
        now: u64,
        rng: Rng,
    ) -> Result<String, Refusal> {
        self.expire(now);
        let good = self
            .ticket
            .as_ref()
            .is_some_and(|t| same(&t.value, ticket) && now < t.created + QR_TTL);
        if !good {
            return Err(Refusal::BadTicket);
        }
        self.ticket = None;
        self.admit(peer, agent, true, now, rng)
    }

    /// The device holding `token`, if it is let in (its last use noted).
    pub fn check(&mut self, token: &str, now: u64) -> Option<&Session> {
        if token.len() != 64 {
            return None;
        }
        self.expire(now);
        let h = hash(token);
        let i = self.list.iter().position(|s| same(&s.hash, &h))?;
        if now >= self.list[i].last_used + TOUCH_EVERY {
            self.list[i].last_used = now;
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

    /// A counter as randomness: different, predictable bytes.
    pub fn counter() -> impl FnMut(&mut [u8]) {
        let mut n: u8 = 0;
        move |out: &mut [u8]| {
            for b in out {
                n = n.wrapping_add(37);
                *b = n;
            }
        }
    }

    fn agent() -> Agent {
        Agent::clean("Chrome", "macOS")
    }

    const T0: u64 = 1_800_000_000;

    #[test]
    fn yes_lets_in_once() {
        let mut rng = counter();
        let mut s = Sessions::default();
        let r = s
            .request("192.0.2.24", agent(), true, T0, &mut rng)
            .unwrap();
        assert_eq!(r.id.len(), 32);
        assert_eq!(r.code.len(), 4);
        assert!(r.code.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(s.waiting().map(|w| w.id.clone()), Some(r.id.clone()));
        assert_eq!(s.status(&r.id, T0 + 1), Status::Waiting);
        s.answer(&r.id, true, T0 + 2, &mut rng);
        let Status::Allowed(token) = s.status(&r.id, T0 + 3) else {
            panic!("not allowed")
        };
        assert_eq!(token.len(), 64);
        // Handed once.
        assert_eq!(s.status(&r.id, T0 + 4), Status::Unknown);
        assert!(s.check(&token, T0 + 5).is_some());
        assert!(s.check(&"0".repeat(64), T0 + 5).is_none());
        assert!(s.check("short", T0 + 5).is_none());
        // Only the hash is kept.
        assert_ne!(s.list(T0)[0].hash, token);
        s.end(&token);
        assert!(s.check(&token, T0 + 6).is_none());
    }

    #[test]
    fn no_and_timeouts_are_limited() {
        let mut rng = counter();
        let mut s = Sessions::default();
        // One at a time.
        let a = s.request("192.0.2.1", agent(), true, T0, &mut rng).unwrap();
        assert_eq!(
            s.request("192.0.2.2", agent(), true, T0, &mut rng),
            Err(Refusal::Busy)
        );
        s.answer(&a.id, false, T0 + 5, &mut rng);
        assert_eq!(s.status(&a.id, T0 + 6), Status::Refused);
        // The same address waits 10 minutes; another may ask.
        assert_eq!(
            s.request("192.0.2.1", agent(), true, T0 + 60, &mut rng),
            Err(Refusal::Wait)
        );
        let b = s
            .request("192.0.2.2", agent(), true, T0 + 60, &mut rng)
            .unwrap();
        // Unanswered for 60 s: expired (the second failure).
        assert!(!s.expire(T0 + 119));
        assert!(s.expire(T0 + 120));
        assert_eq!(s.status(&b.id, T0 + 121), Status::Expired);
        assert!(!s.blocked(T0 + 121));
        // A third: nobody may ask for 10 minutes, and the home says so.
        let c = s
            .request("192.0.2.3", agent(), true, T0 + 130, &mut rng)
            .unwrap();
        s.answer(&c.id, false, T0 + 131, &mut rng);
        assert!(s.blocked(T0 + 131));
        assert_eq!(
            s.request("192.0.2.4", agent(), true, T0 + 200, &mut rng),
            Err(Refusal::Blocked)
        );
        assert!(!s.blocked(T0 + 131 + BLOCK));
        assert!(
            s.request("192.0.2.4", agent(), true, T0 + 131 + BLOCK, &mut rng)
                .is_ok()
        );
        // Not now (night, a conversation…): nothing shown, nothing counted.
        let mut s = Sessions::default();
        for _ in 0..5 {
            assert_eq!(
                s.request("192.0.2.9", agent(), false, T0, &mut rng),
                Err(Refusal::CannotAsk)
            );
        }
        assert!(s.waiting().is_none() && !s.blocked(T0));
        // A wrong id answers nothing.
        let r = s.request("192.0.2.9", agent(), true, T0, &mut rng).unwrap();
        s.answer("nope", true, T0, &mut rng);
        assert_eq!(s.status(&r.id, T0), Status::Waiting);
        assert_eq!(s.status("nope", T0), Status::Unknown);
    }

    #[test]
    fn five_devices_and_thirty_days() {
        let mut rng = counter();
        let mut s = Sessions::default();
        let mut tokens = Vec::new();
        for i in 0..MAX_SESSIONS {
            let t = s.ticket(T0, &mut rng).unwrap().to_string();
            tokens.push(
                s.redeem(&t, &format!("192.0.2.{i}"), agent(), T0, &mut rng)
                    .unwrap(),
            );
        }
        assert_eq!(
            s.request("192.0.2.9", agent(), true, T0, &mut rng),
            Err(Refusal::Full)
        );
        assert_eq!(s.ticket(T0, &mut rng), None);
        // Revoke one: room again.
        let id = s.list(T0)[0].id().to_string();
        assert!(s.revoke(&id));
        assert!(!s.revoke(&id));
        assert!(s.check(&tokens[4], T0).is_none());
        assert!(s.ticket(T0, &mut rng).is_some());
        // Used now and then: still in after 40 days; unused for 30 days: gone.
        assert!(s.check(&tokens[0], T0 + 20 * 86400).is_some());
        assert!(s.check(&tokens[0], T0 + 40 * 86400).is_some());
        assert!(s.check(&tokens[1], T0 + 40 * 86400).is_none());
        assert_eq!(s.list(T0 + 40 * 86400).len(), 1);
        s.revoke_all();
        assert!(s.list(T0 + 40 * 86400).is_empty());
    }

    #[test]
    fn the_qr_ticket_is_good_once_and_for_two_minutes() {
        let mut rng = counter();
        let mut s = Sessions::default();
        let t = s.ticket(T0, &mut rng).unwrap().to_string();
        assert_eq!(t.len(), 32);
        // The same while it is good.
        assert_eq!(s.ticket(T0 + 60, &mut rng), Some(t.as_str()));
        assert_eq!(
            s.redeem("x", "192.0.2.5", agent(), T0, &mut rng),
            Err(Refusal::BadTicket)
        );
        let mut wrong = t.clone();
        wrong.replace_range(0..1, if t.starts_with('0') { "1" } else { "0" });
        assert_eq!(
            s.redeem(&wrong, "192.0.2.5", agent(), T0, &mut rng),
            Err(Refusal::BadTicket)
        );
        let token = s
            .redeem(&t, "192.0.2.5", agent(), T0 + 10, &mut rng)
            .unwrap();
        assert!(s.check(&token, T0 + 11).unwrap().qr);
        // Spent: a second scan of the same code lets nobody in; a new one is made.
        assert_eq!(
            s.redeem(&t, "192.0.2.6", agent(), T0 + 12, &mut rng),
            Err(Refusal::BadTicket)
        );
        let t2 = s.ticket(T0 + 12, &mut rng).unwrap().to_string();
        assert_ne!(t2, t);
        // Expired after two minutes: refused, and a new one in its place.
        assert_eq!(
            s.redeem(&t2, "192.0.2.6", agent(), T0 + 12 + QR_TTL, &mut rng),
            Err(Refusal::BadTicket)
        );
        let t3 = s.ticket(T0 + 12 + QR_TTL, &mut rng).unwrap().to_string();
        assert_ne!(t3, t2);
        s.drop_ticket();
        assert_eq!(
            s.redeem(&t3, "192.0.2.6", agent(), T0 + 13 + QR_TTL, &mut rng),
            Err(Refusal::BadTicket)
        );
    }

    #[test]
    fn the_file_holds_hashes_only_0600() {
        use std::os::unix::fs::PermissionsExt;
        let mut rng = counter();
        let dir = std::env::temp_dir().join(format!("home-companion-panel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Sessions::load(&dir);
        let t = s.ticket(T0, &mut rng).unwrap().to_string();
        let token = s.redeem(&t, "192.0.2.5", agent(), T0, &mut rng).unwrap();
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!text.contains(&token));
        assert!(text.contains(&hash(&token)));
        let mode = std::fs::metadata(dir.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mut again = Sessions::load(&dir);
        assert!(again.check(&token, T0 + 1).is_some());
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
