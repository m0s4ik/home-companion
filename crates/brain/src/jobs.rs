// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Reminders and other things the brain says at a time ("ricordamelo alle 18"): the `jobs`
//! table of `brain.db` and the rules of their delivery.
//!
//! - Made by the model's `schedule` tool (the voice) or by the brain itself (the morning
//!   summary); a job never makes jobs, and delivering one never calls a tool with side effects.
//!   A plain reminder never calls the language model.
//! - Delivery: a card on the body's screen first (`notify` over the control socket); if it is
//!   not touched within `voice_after_seconds` and `how` is `card_then_voice`, Home Assistant
//!   speaks it on the device (`assist_satellite.announce`), but never inside the quiet hours:
//!   then the voice waits for their end. `how = voice` ("svegliami") is spoken at once, always.
//! - Late (the device was off or suspended): up to an hour late it is delivered marked late;
//!   older ones become `missed` and one card lists them. Nothing is retried silently.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const LATE_MAX: i64 = 3600;
/// Up to this late, a job counts as on time (the scheduler runs once a minute).
pub const ON_TIME: i64 = 120;

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY, due_at INTEGER NOT NULL, repeat TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL, how TEXT NOT NULL DEFAULT 'card_then_voice',
    status TEXT NOT NULL DEFAULT 'pending', created_at INTEGER NOT NULL,
    source TEXT NOT NULL DEFAULT 'voice', delivered_at INTEGER, voiced INTEGER NOT NULL DEFAULT 0,
    late INTEGER NOT NULL DEFAULT 0);
";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub id: i64,
    pub due_at: i64,
    /// "" (once), "daily" or "weekly": the same local time again.
    pub repeat: String,
    pub text: String,
    /// "card", "card_then_voice" or "voice".
    pub how: String,
    /// pending, delivered, acknowledged, missed, cancelled, interrupted.
    pub status: String,
    pub source: String,
    pub late: bool,
}

const COLS: &str = "id, due_at, repeat, text, how, status, source, late";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get(0)?,
        due_at: r.get(1)?,
        repeat: r.get(2)?,
        text: r.get(3)?,
        how: r.get(4)?,
        status: r.get(5)?,
        source: r.get(6)?,
        late: r.get::<_, i64>(7)? != 0,
    })
}

pub fn valid_how(how: &str) -> bool {
    matches!(how, "card" | "card_then_voice" | "voice")
}

pub fn valid_repeat(r: &str) -> bool {
    matches!(r, "" | "daily" | "weekly")
}

pub fn add(
    db: &Connection,
    due_at: i64,
    repeat: &str,
    text: &str,
    how: &str,
    source: &str,
    at: i64,
) -> rusqlite::Result<i64> {
    db.execute(
        "INSERT INTO jobs(due_at, repeat, text, how, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![due_at, repeat, text.trim(), how, source, at],
    )?;
    Ok(db.last_insert_rowid())
}

pub fn count_pending(db: &Connection) -> i64 {
    db.query_row(
        "SELECT count(*) FROM jobs WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Pending jobs, soonest first.
pub fn pending(db: &Connection) -> rusqlite::Result<Vec<Job>> {
    let mut st = db.prepare(&format!(
        "SELECT {COLS} FROM jobs WHERE status = 'pending' ORDER BY due_at, id"
    ))?;
    st.query_map([], row)?.collect()
}

/// Delivered and not yet touched (shown again to a body that reconnects).
pub fn delivered(db: &Connection) -> rusqlite::Result<Vec<Job>> {
    let mut st = db.prepare(&format!(
        "SELECT {COLS} FROM jobs WHERE status = 'delivered' ORDER BY due_at, id"
    ))?;
    st.query_map([], row)?.collect()
}

pub fn get(db: &Connection, id: i64) -> rusqlite::Result<Option<Job>> {
    db.query_row(
        &format!("SELECT {COLS} FROM jobs WHERE id = ?1"),
        params![id],
        row,
    )
    .optional()
}

/// The user (by voice or on the screen) cancels a pending or delivered job.
pub fn cancel(db: &Connection, id: i64) -> rusqlite::Result<usize> {
    db.execute(
        "UPDATE jobs SET status = 'cancelled' WHERE id = ?1 AND status IN ('pending', 'delivered')",
        params![id],
    )
}

/// The card was touched: no voice any more.
pub fn ack(db: &Connection, id: i64) -> rusqlite::Result<usize> {
    db.execute(
        "UPDATE jobs SET status = 'acknowledged' WHERE id = ?1 AND status = 'delivered'",
        params![id],
    )
}

/// "In ten minutes" on the screen: the delivered job is acknowledged and the same words come
/// back once, `minutes` from `at` (a repeating one keeps its own next time). The new job's id.
pub fn snooze(db: &Connection, id: i64, minutes: i64, at: i64) -> rusqlite::Result<Option<i64>> {
    let Some(j) = get(db, id)? else {
        return Ok(None);
    };
    if j.status != "delivered" {
        return Ok(None);
    }
    ack(db, id)?;
    add(
        db,
        at + minutes.clamp(1, 24 * 60) * 60,
        "",
        &j.text,
        &j.how,
        "snooze",
        at,
    )
    .map(Some)
}

/// What one scheduler pass decided (the brain carries it out: events to the body, voice).
#[derive(Debug, Default, PartialEq)]
pub struct Pass {
    /// Shown now as cards (late ones marked).
    pub deliver: Vec<Job>,
    /// Spoken now through Home Assistant.
    pub speak: Vec<Job>,
    /// Too late: listed on one card.
    pub missed: Vec<Job>,
}

/// One scheduler pass at `at`: due jobs delivered (or missed), unanswered ones spoken outside
/// the quiet hours; repeating jobs get their next time. Changes the table; returns what to do.
pub fn pass(
    db: &Connection,
    at: i64,
    quiet_now: bool,
    voice_after: i64,
    can_speak: bool,
) -> rusqlite::Result<Pass> {
    let mut p = Pass::default();
    let mut st = db.prepare(&format!(
        "SELECT {COLS} FROM jobs WHERE status = 'pending' AND due_at <= ?1 ORDER BY due_at, id"
    ))?;
    let due: Vec<Job> = st
        .query_map(params![at], row)?
        .collect::<rusqlite::Result<_>>()?;
    drop(st);
    for mut j in due {
        let lateness = at - j.due_at;
        if !j.repeat.is_empty() {
            let step = if j.repeat == "weekly" {
                7 * 86400
            } else {
                86400
            };
            let mut next = j.due_at + step;
            while next <= at {
                next += step;
            }
            add(db, next, &j.repeat, &j.text, &j.how, &j.source, at)?;
        }
        if lateness > LATE_MAX {
            db.execute(
                "UPDATE jobs SET status = 'missed' WHERE id = ?1",
                params![j.id],
            )?;
            j.status = "missed".into();
            p.missed.push(j);
            continue;
        }
        j.late = lateness > ON_TIME;
        db.execute(
            "UPDATE jobs SET status = 'delivered', delivered_at = ?2, late = ?3 WHERE id = ?1",
            params![j.id, at, j.late as i64],
        )?;
        j.status = "delivered".into();
        // "Svegliami": spoken at once, quiet hours or not (the user asked for it).
        if j.how == "voice" && can_speak {
            db.execute("UPDATE jobs SET voiced = 1 WHERE id = ?1", params![j.id])?;
            p.speak.push(j.clone());
        }
        p.deliver.push(j);
    }
    if can_speak && !quiet_now {
        let mut st = db.prepare(&format!(
            "SELECT {COLS} FROM jobs WHERE status = 'delivered' AND how = 'card_then_voice'
             AND voiced = 0 AND delivered_at <= ?1 ORDER BY due_at, id"
        ))?;
        let waiting: Vec<Job> = st
            .query_map(params![at - voice_after], row)?
            .collect::<rusqlite::Result<_>>()?;
        drop(st);
        for j in waiting {
            db.execute("UPDATE jobs SET voiced = 1 WHERE id = ?1", params![j.id])?;
            p.speak.push(j);
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn snooze_brings_the_words_back_once() {
        let c = db();
        let t0 = 1_000_000;
        let id = add(
            &c,
            t0,
            "daily",
            "pillola",
            "card_then_voice",
            "voice",
            t0 - 100,
        )
        .unwrap();
        // Not delivered yet: nothing to snooze.
        assert_eq!(snooze(&c, id, 10, t0 - 50).unwrap(), None);
        let p = pass(&c, t0 + 5, false, 120, true).unwrap();
        assert_eq!(p.deliver.len(), 1);
        let again = snooze(&c, id, 10, t0 + 20).unwrap().unwrap();
        let j = get(&c, again).unwrap().unwrap();
        assert_eq!(
            (j.due_at, j.repeat.as_str(), j.text.as_str()),
            (t0 + 620, "", "pillola")
        );
        assert_eq!(j.status, "pending");
        assert_eq!(get(&c, id).unwrap().unwrap().status, "acknowledged");
        // Twice: the first one is acknowledged already.
        assert_eq!(snooze(&c, id, 10, t0 + 30).unwrap(), None);
    }

    #[test]
    fn card_then_voice_outside_quiet_hours_only() {
        let c = db();
        let t0 = 1_000_000;
        let id = add(
            &c,
            t0,
            "",
            "chiamare la mamma",
            "card_then_voice",
            "voice",
            t0 - 100,
        )
        .unwrap();
        let p = pass(&c, t0 + 30, false, 120, true).unwrap();
        assert_eq!(p.deliver.len(), 1);
        assert!(!p.deliver[0].late);
        assert!(p.speak.is_empty());
        // Unanswered after 2 minutes, but quiet hours: no voice.
        let p = pass(&c, t0 + 200, true, 120, true).unwrap();
        assert!(p.speak.is_empty());
        // Quiet hours over: spoken once.
        let p = pass(&c, t0 + 4000, false, 120, true).unwrap();
        assert_eq!(p.speak.len(), 1);
        assert!(
            pass(&c, t0 + 5000, false, 120, true)
                .unwrap()
                .speak
                .is_empty()
        );
        assert_eq!(get(&c, id).unwrap().unwrap().status, "delivered");
    }

    #[test]
    fn touched_cards_are_not_spoken() {
        let c = db();
        let id = add(&c, 100, "", "x", "card_then_voice", "voice", 0).unwrap();
        pass(&c, 100, false, 120, true).unwrap();
        assert_eq!(ack(&c, id).unwrap(), 1);
        assert!(pass(&c, 1000, false, 120, true).unwrap().speak.is_empty());
        assert_eq!(get(&c, id).unwrap().unwrap().status, "acknowledged");
    }

    #[test]
    fn wake_up_voice_even_at_night_and_cards_never() {
        let c = db();
        add(&c, 100, "", "svegliami", "voice", "voice", 0).unwrap();
        add(&c, 100, "", "solo card", "card", "voice", 0).unwrap();
        let p = pass(&c, 110, true, 120, true).unwrap();
        assert_eq!(p.deliver.len(), 2);
        assert_eq!(p.speak.len(), 1);
        assert_eq!(p.speak[0].text, "svegliami");
        assert!(pass(&c, 10_000, false, 120, true).unwrap().speak.is_empty());
    }

    #[test]
    fn late_and_missed_after_a_long_sleep() {
        let c = db();
        add(&c, 1000, "", "in ritardo", "card", "voice", 0).unwrap();
        add(&c, 1000 - 5000, "", "perso", "card_then_voice", "voice", 0).unwrap();
        let p = pass(&c, 1000 + 1800, false, 120, true).unwrap();
        assert_eq!(p.deliver.len(), 1);
        assert!(p.deliver[0].late);
        assert_eq!(p.missed.len(), 1);
        assert_eq!(p.missed[0].text, "perso");
        // The missed one is never spoken later.
        assert!(
            pass(&c, 100_000, false, 120, true)
                .unwrap()
                .speak
                .is_empty()
        );
    }

    #[test]
    fn repeating_jobs_get_their_next_time_and_cancel() {
        let c = db();
        let id = add(&c, 1000, "daily", "pillola", "card", "voice", 0).unwrap();
        pass(&c, 1010, false, 120, true).unwrap();
        let next = pending(&c).unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].due_at, 1000 + 86400);
        assert_eq!(cancel(&c, next[0].id).unwrap(), 1);
        assert!(pending(&c).unwrap().is_empty());
        assert_eq!(
            cancel(&c, id).unwrap(),
            1,
            "the delivered one can be cancelled too"
        );
        assert!(!valid_how("loud") && valid_repeat("weekly") && !valid_repeat("hourly"));
    }

    #[test]
    fn no_voice_without_a_satellite() {
        let c = db();
        add(&c, 100, "", "x", "voice", "voice", 0).unwrap();
        let p = pass(&c, 100, false, 120, false).unwrap();
        assert_eq!(p.deliver.len(), 1);
        assert!(p.speak.is_empty());
    }
}
