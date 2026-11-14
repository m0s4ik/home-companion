// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Long-term memory, on the device: `brain.db` (SQLite, FTS5, no embeddings).
//!
//! - `facts`: one sentence each, the only store. `status` = `active` (told to Home Assistant's
//!   agent in its instructions, see context.rs), `candidate` (heard once, not told until the
//!   user confirms it: collect.rs) or `rejected` (deleted by the user: kept a while as a
//!   tombstone so it is not learned again). `invalid` set = not alive. `origin` = `explicit`
//!   ("remember that…"), `inferred` (from a conversation), `screen` (confirmed on the screen),
//!   `legacy` (before 0.3).
//! - `sessions` (a conversation: exchanges less than `session_gap_minutes` apart, with a summary
//!   written when it closes) and `exchanges` (raw text and the tools used, kept `keep_days`).
//!
//! Recall: the words of the request (and their stems, a prefix match: FTS5 has no Italian
//! stemmer) against the active facts and the session summaries, BM25 plus importance.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::collect::{self, Kind};
use crate::context::{self, ContextFact};

#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub id: i64,
    pub text: String,
    pub kind: String,
    pub importance: i64,
}

/// A fact as the "Ricordi" page, the context and the nightly tidy-up see it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FactRow {
    pub id: i64,
    pub text: String,
    pub kind: String,
    pub importance: i64,
    pub created: i64,
    /// Last change (= created when never changed).
    pub updated: i64,
    /// active | candidate
    pub status: String,
    /// explicit | inferred | screen | legacy
    pub origin: String,
}

impl FactRow {
    fn context(&self) -> ContextFact<'_> {
        ContextFact {
            id: self.id,
            text: &self.text,
            kind: Kind::of(&self.kind),
            importance: self.importance,
            explicit: matches!(self.origin.as_str(), "explicit" | "screen"),
            updated: self.updated,
        }
    }
}

/// The nightly tidy-up's decisions (the model's JSON, see `agent::consolidate`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tidy {
    /// Several facts saying the same thing → one sentence.
    pub merge: Vec<(Vec<i64>, String)>,
    pub update: Vec<(i64, String)>,
    /// Facts no longer true, or records of a past day.
    pub invalidate: Vec<i64>,
}

/// What a tidy-up did (counts only: they are logged and shown on the "Ricordi" page).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TidyResult {
    pub at: i64,
    pub ok: bool,
    pub merged: usize,
    pub updated: usize,
    pub invalidated: usize,
    /// Unused since 0.3 (the profile and notes blocks are gone); kept so older results parse.
    #[serde(default)]
    pub blocks: usize,
    /// Why it did nothing ("" = it ran).
    pub skipped: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub user: String,
    pub answer: String,
    /// The tools the agent called in this exchange, comma-separated.
    pub tools: String,
}

/// One change decided at the end of a session (mem0-style).
#[derive(Debug, Clone, PartialEq)]
pub enum FactOp {
    Add {
        text: String,
        kind: String,
        importance: i64,
        /// The user's words it rests on.
        quote: String,
    },
    Update {
        id: i64,
        text: String,
        quote: String,
    },
    Delete {
        id: i64,
    },
    /// A candidate heard again.
    Confirm {
        id: i64,
    },
}

/// A fact to store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewFact<'a> {
    pub text: &'a str,
    pub kind: Kind,
    pub importance: i64,
    /// explicit | inferred | screen
    pub origin: &'a str,
    /// active | candidate
    pub status: &'a str,
    pub quote: Option<&'a str>,
    pub session: Option<i64>,
}

/// What [`Memory::add_checked`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Added {
    New(i64),
    /// The same fact was there: touched.
    Same(i64),
    /// A candidate heard again on another day (or now said plainly): active.
    Confirmed(i64),
    /// The user said it is wrong (a tombstone): not stored.
    Refused,
}

/// A fact `add_checked` compares with: id, text, status, invalid, seen_day.
type Known = (i64, String, String, Option<i64>, Option<i64>);

/// How long a rejected fact is remembered as rejected.
const TOMBSTONE_DAYS: i64 = 180;

/// What becomes of a fact stored before 0.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Legacy {
    /// A record of a day (a year in it) or a plan: invalid.
    Drop,
    /// Kept, to be confirmed on the screen.
    Candidate,
}

pub fn migrate_legacy(text: &str, kind: &str) -> Legacy {
    if collect::has_year(text) || matches!(kind, "event" | "commitment") {
        Legacy::Drop
    } else {
        Legacy::Candidate
    }
}

/// The local day number (days since the epoch on the local calendar) of `ts`.
pub fn day(ts: i64) -> i64 {
    crate::clock::local(ts).day
}

pub struct Memory {
    db: Connection,
    dir: PathBuf,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA cache_size = -2000;
CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY, started INTEGER NOT NULL, last INTEGER NOT NULL,
    closed INTEGER NOT NULL DEFAULT 0, summary TEXT);
CREATE TABLE IF NOT EXISTS exchanges (
    id INTEGER PRIMARY KEY, session INTEGER NOT NULL, ts INTEGER NOT NULL,
    user TEXT NOT NULL, answer TEXT NOT NULL, model TEXT, tools TEXT, ms INTEGER,
    tokens_in INTEGER, tokens_out INTEGER);
CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY, text TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'fact',
    importance INTEGER NOT NULL DEFAULT 5, created INTEGER NOT NULL, invalid INTEGER,
    recalls INTEGER NOT NULL DEFAULT 0, last_recalled INTEGER, source_session INTEGER);
CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    text, content='facts', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS facts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, text) VALUES (new.id, new.text); END;
CREATE TRIGGER IF NOT EXISTS facts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, text) VALUES ('delete', old.id, old.text); END;
CREATE TRIGGER IF NOT EXISTS facts_au AFTER UPDATE OF text ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO facts_fts(rowid, text) VALUES (new.id, new.text); END;
CREATE VIRTUAL TABLE IF NOT EXISTS summaries_fts USING fts5(
    summary, content='sessions', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS sessions_ai AFTER INSERT ON sessions BEGIN
    INSERT INTO summaries_fts(rowid, summary) VALUES (new.id, coalesce(new.summary, '')); END;
CREATE TRIGGER IF NOT EXISTS sessions_au AFTER UPDATE OF summary ON sessions BEGIN
    INSERT INTO summaries_fts(summaries_fts, rowid, summary) VALUES ('delete', old.id, coalesce(old.summary, ''));
    INSERT INTO summaries_fts(rowid, summary) VALUES (new.id, coalesce(new.summary, '')); END;
CREATE TRIGGER IF NOT EXISTS sessions_ad AFTER DELETE ON sessions BEGIN
    INSERT INTO summaries_fts(summaries_fts, rowid, summary) VALUES ('delete', old.id, coalesce(old.summary, '')); END;
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Words that carry no meaning for recall.
pub const STOP: &[&str] = &[
    "che", "chi", "cosa", "come", "quando", "dove", "perché", "perche", "non", "una", "uno", "del",
    "della", "dei", "delle", "nel", "nella", "per", "con", "sono", "sei", "hai", "ho", "mi", "ti",
    "il", "lo", "la", "le", "gli", "un", "di", "da", "in", "su", "tra", "fra", "e", "o", "ma",
    "the", "and", "what", "when", "where", "who", "how", "you", "your", "are", "was", "did",
    "ricordi", "ricorda", "detto", "avevo", "alfred", "dimmi", "sai",
];

/// An FTS5 query from free text: each meaningful word as a quoted prefix of its stem, OR-ed.
pub fn fts_query(text: &str) -> Option<String> {
    let mut terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .map(|w| {
            let n = w.chars().count();
            let keep = if n > 5 { n - 2 } else { n };
            let stem: String = w.chars().take(keep).collect();
            format!("\"{stem}\"*")
        })
        .collect();
    terms.sort();
    terms.dedup();
    terms.truncate(12);
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

impl Memory {
    pub fn open(dir: &Path) -> rusqlite::Result<Memory> {
        let _ = std::fs::create_dir_all(dir);
        let db = Connection::open(dir.join("brain.db"))?;
        db.execute_batch(SCHEMA)?;
        db.execute_batch(crate::jobs::SCHEMA)?;
        // Added after the first release: the last change of a fact.
        let has_updated: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('facts') WHERE name = 'updated'")?
            .exists([])?;
        if !has_updated {
            db.execute_batch("ALTER TABLE facts ADD COLUMN updated INTEGER")?;
        }
        let mut m = Memory {
            db,
            dir: dir.to_path_buf(),
        };
        let version: i64 = m.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            m.migrate_v1()?;
        }
        Ok(m)
    }

    /// Version 1 (0.3): status, origin, quote and day of the facts; the facts before it either
    /// dropped (dated, plans) or candidates; the profile and notes blocks become facts.
    fn migrate_v1(&mut self) -> rusqlite::Result<()> {
        let at = now();
        let tx = self.db.transaction()?;
        for (col, def) in [
            ("status", "TEXT NOT NULL DEFAULT 'active'"),
            ("origin", "TEXT NOT NULL DEFAULT 'legacy'"),
            ("quote", "TEXT"),
            ("seen_day", "INTEGER"),
        ] {
            let has: bool = tx
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('facts') WHERE name = '{col}'"
                ))?
                .exists([])?;
            if !has {
                tx.execute_batch(&format!("ALTER TABLE facts ADD COLUMN {col} {def}"))?;
            }
        }
        let alive: Vec<(i64, String, String, i64)> = {
            let mut st =
                tx.prepare("SELECT id, text, kind, created FROM facts WHERE invalid IS NULL")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let (mut dropped, mut candidates, mut imported) = (0, 0, 0);
        let mut texts: Vec<String> = Vec::new();
        for (id, text, kind, created) in alive {
            match migrate_legacy(&text, &kind) {
                Legacy::Drop => {
                    tx.execute(
                        "UPDATE facts SET invalid = ?2 WHERE id = ?1",
                        params![id, at],
                    )?;
                    dropped += 1;
                }
                Legacy::Candidate => {
                    tx.execute(
                        "UPDATE facts SET status = 'candidate', origin = 'legacy', seen_day = ?2 WHERE id = ?1",
                        params![id, day(created)],
                    )?;
                    candidates += 1;
                    texts.push(text);
                }
            }
        }
        // Only the user by hand or a deliberate tool call ever wrote the blocks: their lines
        // are the user's own facts.
        for (name, kind) in [("profile", Kind::Person), ("notes", Kind::House)] {
            let path = self.dir.join(format!("{name}.md"));
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in body.lines() {
                let line = line.trim().trim_start_matches("- ").trim();
                if line.is_empty() || texts.iter().any(|t| collect::similar(t, line)) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO facts(text, kind, importance, created, updated, status, origin, seen_day)
                     VALUES (?1, ?2, 7, ?3, ?3, 'active', 'explicit', ?4)",
                    params![line, kind.name(), at, day(at)],
                )?;
                texts.push(line.to_string());
                imported += 1;
            }
            let _ = std::fs::rename(&path, self.dir.join(format!("{name}.md.migrated")));
        }
        tx.execute_batch("PRAGMA user_version = 1")?;
        tx.commit()?;
        self.bump_rev()?;
        log::info!(event = "brain_memory", dropped = dropped, candidates = candidates, imported = imported;
            "memory version 1: {dropped} dated facts dropped, {candidates} to confirm, {imported} from the blocks");
        Ok(())
    }

    /// The database, for the other tables of `brain.db` (`jobs`).
    pub fn db(&self) -> &Connection {
        &self.db
    }

    // ------------------------------------------------------------ revisions

    /// The revision of the alive facts: +1 at every change (the context is published again).
    pub fn facts_rev(&self) -> i64 {
        self.meta("facts_rev")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }

    fn bump_rev(&self) -> rusqlite::Result<()> {
        let r = self.facts_rev() + 1;
        self.set_meta("facts_rev", &r.to_string())
    }

    // ------------------------------------------------------------ sessions and exchanges

    /// The open session if its last exchange is recent enough, else a new one.
    pub fn session(&self, gap_secs: i64, at: i64) -> rusqlite::Result<i64> {
        let open: Option<(i64, i64)> = self
            .db
            .query_row(
                "SELECT id, last FROM sessions WHERE closed = 0 ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, last)) = open
            && at - last < gap_secs
        {
            return Ok(id);
        }
        self.db.execute(
            "INSERT INTO sessions(started, last) VALUES (?1, ?1)",
            params![at],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_exchange(
        &self,
        session: i64,
        at: i64,
        user: &str,
        answer: &str,
        model: &str,
        tools: &str,
        ms: i64,
        tokens: (u64, u64),
    ) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO exchanges(session, ts, user, answer, model, tools, ms, tokens_in, tokens_out)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![session, at, user, answer, model, tools, ms, tokens.0 as i64, tokens.1 as i64],
        )?;
        self.db.execute(
            "UPDATE sessions SET last = ?2 WHERE id = ?1",
            params![session, at],
        )?;
        Ok(())
    }

    /// The last `n` exchanges of a session, oldest first.
    pub fn recent(&self, session: i64, n: usize) -> rusqlite::Result<Vec<Exchange>> {
        let mut st = self.db.prepare(
            "SELECT user, answer, coalesce(tools, '') FROM exchanges WHERE session = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let mut v: Vec<Exchange> = st
            .query_map(params![session, n as i64], |r| {
                Ok(Exchange {
                    user: r.get(0)?,
                    answer: r.get(1)?,
                    tools: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        v.reverse();
        Ok(v)
    }

    /// Open sessions quiet for `gap_secs`: to summarise and close.
    pub fn sessions_to_close(&self, gap_secs: i64, at: i64) -> rusqlite::Result<Vec<i64>> {
        let mut st = self
            .db
            .prepare("SELECT id FROM sessions WHERE closed = 0 AND last <= ?1 ORDER BY id")?;
        st.query_map(params![at - gap_secs], |r| r.get(0))?
            .collect()
    }

    pub fn exchanges_of(&self, session: i64) -> rusqlite::Result<Vec<Exchange>> {
        self.recent(session, 200)
    }

    pub fn close_session(&self, session: i64, summary: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE sessions SET closed = 1, summary = ?2 WHERE id = ?1",
            params![session, summary],
        )?;
        Ok(())
    }

    /// Raw exchanges older than `days` go; summaries and facts stay.
    pub fn prune(&self, days: u32, at: i64) -> rusqlite::Result<usize> {
        self.db.execute(
            "DELETE FROM exchanges WHERE ts < ?1",
            params![at - days as i64 * 86400],
        )
    }

    // ------------------------------------------------------------ facts

    fn insert(&self, f: &NewFact, at: i64) -> rusqlite::Result<i64> {
        self.db.execute(
            "INSERT INTO facts(text, kind, importance, created, updated, source_session, status, origin, quote, seen_day)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                f.text.trim(),
                f.kind.name(),
                f.importance.clamp(1, 10),
                at,
                f.session,
                f.status,
                f.origin,
                f.quote,
                day(at)
            ],
        )?;
        let id = self.db.last_insert_rowid();
        self.bump_rev()?;
        Ok(id)
    }

    /// A fact stored as it is, active (tests, the seed of a tidy-up).
    pub fn add_fact(
        &self,
        text: &str,
        kind: &str,
        importance: i64,
        session: Option<i64>,
    ) -> rusqlite::Result<i64> {
        self.insert(
            &NewFact {
                text,
                kind: Kind::of(kind),
                importance,
                origin: "inferred",
                status: "active",
                quote: None,
                session,
            },
            now(),
        )
    }

    /// A new fact against the ones there are (alive, and rejected in the last 180 days):
    /// the same as an active one = touched; as a candidate = confirmed when heard on another
    /// day than `today` or now said plainly (`explicit`, or `f.status` active), else touched;
    /// as a rejected one = refused, unless `explicit` (the user says it now); else stored.
    pub fn add_checked(&self, f: &NewFact, explicit: bool, at: i64) -> rusqlite::Result<Added> {
        let today = day(at);
        let rows: Vec<Known> = {
            let mut st = self.db.prepare(
                "SELECT id, text, status, invalid, seen_day FROM facts
                 WHERE invalid IS NULL OR (status = 'rejected' AND invalid > ?1)
                 ORDER BY invalid IS NOT NULL, id",
            )?;
            st.query_map(params![at - TOMBSTONE_DAYS * 86400], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let similar: Vec<_> = rows
            .iter()
            .filter(|r| collect::similar(&r.1, f.text))
            .collect();
        let alive = |s: &str| {
            similar
                .iter()
                .find(|r| r.3.is_none() && r.2 == s)
                .map(|r| (r.0, r.4))
        };
        if let Some((id, _)) = alive("active") {
            self.db.execute(
                "UPDATE facts SET updated = ?2, seen_day = ?3,
                 origin = CASE WHEN ?4 THEN 'explicit' ELSE origin END WHERE id = ?1",
                params![id, at, today, explicit],
            )?;
            self.bump_rev()?;
            return Ok(Added::Same(id));
        }
        if let Some((id, seen)) = alive("candidate") {
            let promote = explicit || f.status == "active" || seen.is_some_and(|d| d != today);
            if promote {
                self.db.execute(
                    "UPDATE facts SET status = 'active', updated = ?2, seen_day = ?3,
                     origin = CASE WHEN ?4 THEN 'explicit' WHEN origin = 'legacy' THEN 'inferred' ELSE origin END
                     WHERE id = ?1",
                    params![id, at, today, explicit],
                )?;
                self.bump_rev()?;
                return Ok(Added::Confirmed(id));
            }
            self.db.execute(
                "UPDATE facts SET updated = ?2, seen_day = ?3 WHERE id = ?1",
                params![id, at, today],
            )?;
            return Ok(Added::Same(id));
        }
        if !explicit && similar.iter().any(|r| r.2 == "rejected") {
            return Ok(Added::Refused);
        }
        self.insert(f, at).map(Added::New)
    }

    /// A candidate the conversation mentioned again: active when heard on another day than
    /// `today`. True when it changed.
    pub fn confirm_candidate(&self, id: i64, today: i64) -> rusqlite::Result<bool> {
        let n = self.db.execute(
            "UPDATE facts SET status = 'active', updated = ?2,
             origin = CASE WHEN origin = 'legacy' THEN 'inferred' ELSE origin END
             WHERE id = ?1 AND invalid IS NULL AND status = 'candidate' AND coalesce(seen_day, -1) != ?3",
            params![id, now(), today],
        )?;
        if n > 0 {
            self.bump_rev()?;
        }
        Ok(n > 0)
    }

    /// "Right" on the screen: active, the user's own.
    pub fn confirm_fact(&self, id: i64) -> rusqlite::Result<usize> {
        let n = self.db.execute(
            "UPDATE facts SET status = 'active', origin = 'screen', updated = ?2
             WHERE id = ?1 AND invalid IS NULL",
            params![id, now()],
        )?;
        if n > 0 {
            self.bump_rev()?;
        }
        Ok(n)
    }

    /// Deleted on the screen, "Wrong", or the `forget` tool: not alive, and remembered as
    /// rejected so it is not learned again.
    pub fn reject_fact(&self, id: i64) -> rusqlite::Result<usize> {
        let n = self.db.execute(
            "UPDATE facts SET invalid = ?2, status = 'rejected' WHERE id = ?1 AND invalid IS NULL",
            params![id, now()],
        )?;
        if n > 0 {
            self.bump_rev()?;
        }
        Ok(n)
    }

    /// No longer true (a conversation said so): not alive, not a tombstone.
    pub fn invalidate(&self, id: i64) -> rusqlite::Result<usize> {
        let n = self.db.execute(
            "UPDATE facts SET invalid = ?2 WHERE id = ?1 AND invalid IS NULL",
            params![id, now()],
        )?;
        if n > 0 {
            self.bump_rev()?;
        }
        Ok(n)
    }

    /// A known fact changed (its status stays).
    pub fn update_fact(&self, id: i64, text: &str) -> rusqlite::Result<usize> {
        let n = self.db.execute(
            "UPDATE facts SET text = ?2, updated = ?3 WHERE id = ?1 AND invalid IS NULL",
            params![id, text.trim(), now()],
        )?;
        if n > 0 {
            self.bump_rev()?;
        }
        Ok(n)
    }

    /// Candidates untouched for `candidate_days` go; tombstones older than 180 days are
    /// deleted; over `max_facts` alive, the oldest inferred facts of least importance go
    /// (never the user's own). The number of facts that went.
    pub fn expire(
        &self,
        at: i64,
        candidate_days: u32,
        max_facts: usize,
    ) -> rusqlite::Result<usize> {
        let mut n = self.db.execute(
            "UPDATE facts SET invalid = ?1 WHERE invalid IS NULL AND status = 'candidate'
             AND coalesce(updated, created) < ?2",
            params![at, at - candidate_days as i64 * 86400],
        )?;
        n += self.db.execute(
            "DELETE FROM facts WHERE status = 'rejected' AND invalid < ?1",
            params![at - TOMBSTONE_DAYS * 86400],
        )?;
        let alive: i64 = self.db.query_row(
            "SELECT count(*) FROM facts WHERE invalid IS NULL",
            [],
            |r| r.get(0),
        )?;
        let over = alive - max_facts as i64;
        if over > 0 {
            n += self.db.execute(
                "UPDATE facts SET invalid = ?1 WHERE id IN (
                    SELECT id FROM facts WHERE invalid IS NULL AND origin IN ('inferred', 'legacy')
                    ORDER BY importance, coalesce(updated, created), id LIMIT ?2)",
                params![at, over],
            )?;
        }
        if n > 0 {
            self.bump_rev()?;
        }
        Ok(n)
    }

    /// Active facts matching `text`, best first (BM25 + importance); `mark` (the agent's
    /// `recall` tool) counts them as recalled.
    pub fn recall(&self, text: &str, limit: usize, mark: bool) -> rusqlite::Result<Vec<Fact>> {
        self.search(text, limit, "f.status = 'active'", mark)
    }

    /// Alive facts (active and candidates) matching `text`, best first: what `forget` looks in.
    pub fn search_alive(&self, text: &str, limit: usize) -> rusqlite::Result<Vec<Fact>> {
        self.search(text, limit, "1", false)
    }

    fn search(
        &self,
        text: &str,
        limit: usize,
        cond: &str,
        mark: bool,
    ) -> rusqlite::Result<Vec<Fact>> {
        let Some(q) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut st = self.db.prepare(&format!(
            "SELECT f.id, f.text, f.kind, f.importance FROM facts_fts
             JOIN facts f ON f.id = facts_fts.rowid
             WHERE facts_fts MATCH ?1 AND f.invalid IS NULL AND {cond}
             ORDER BY bm25(facts_fts) - f.importance * 0.1 LIMIT ?2"
        ))?;
        let v: Vec<Fact> = st
            .query_map(params![q, limit as i64], |r| {
                Ok(Fact {
                    id: r.get(0)?,
                    text: r.get(1)?,
                    kind: r.get(2)?,
                    importance: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        if mark {
            for f in &v {
                self.db.execute(
                    "UPDATE facts SET recalls = recalls + 1, last_recalled = ?2 WHERE id = ?1",
                    params![f.id, now()],
                )?;
            }
        }
        Ok(v)
    }

    /// Session summaries matching `text`, newest first: (started, summary).
    pub fn recall_sessions(
        &self,
        text: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, String)>> {
        let Some(q) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut st = self.db.prepare(
            "SELECT s.started, s.summary FROM summaries_fts JOIN sessions s ON s.id = summaries_fts.rowid
             WHERE summaries_fts MATCH ?1 ORDER BY bm25(summaries_fts) LIMIT ?2",
        )?;
        st.query_map(params![q, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }

    /// "Dimentica tutto": facts (tombstones too), conversations, and the old blocks' files.
    pub fn forget_all(&self) -> rusqlite::Result<()> {
        self.db.execute_batch(
            "DELETE FROM facts; DELETE FROM exchanges; DELETE FROM sessions;
             INSERT INTO facts_fts(facts_fts) VALUES ('rebuild');
             INSERT INTO summaries_fts(summaries_fts) VALUES ('rebuild');
             DELETE FROM meta WHERE key LIKE 'explicit_%';",
        )?;
        for b in ["profile", "notes"] {
            let _ = std::fs::remove_file(self.dir.join(format!("{b}.md")));
            let _ = std::fs::remove_file(self.dir.join(format!("{b}.md.migrated")));
        }
        self.bump_rev()?;
        let _ = self.db.execute_batch("VACUUM;");
        Ok(())
    }

    fn rows(&self, cond: &str, order: &str, limit: usize) -> rusqlite::Result<Vec<FactRow>> {
        let mut st = self.db.prepare(&format!(
            "SELECT id, text, kind, importance, created, coalesce(updated, created), status, origin
             FROM facts WHERE invalid IS NULL AND {cond} ORDER BY {order} LIMIT ?1"
        ))?;
        st.query_map(params![limit as i64], |r| {
            Ok(FactRow {
                id: r.get(0)?,
                text: r.get(1)?,
                kind: r.get(2)?,
                importance: r.get(3)?,
                created: r.get(4)?,
                updated: r.get(5)?,
                status: r.get(6)?,
                origin: r.get(7)?,
            })
        })?
        .collect()
    }

    /// Active facts, oldest first (the tidy-up).
    pub fn list_facts(&self, limit: usize) -> rusqlite::Result<Vec<FactRow>> {
        self.rows("status = 'active'", "coalesce(updated, created), id", limit)
    }

    /// Active facts in the context's order (what the agent reads, and the "Ricordi" page).
    pub fn list_context(&self) -> rusqlite::Result<Vec<FactRow>> {
        let mut v = self.rows("status = 'active'", "id", 100_000)?;
        v.sort_by_key(|f| context::key(&f.context()));
        Ok(v)
    }

    /// Candidates, newest first.
    pub fn list_candidates(&self, limit: usize) -> rusqlite::Result<Vec<FactRow>> {
        self.rows(
            "status = 'candidate'",
            "coalesce(updated, created) DESC, id DESC",
            limit,
        )
    }

    /// The context for the agent: (text, facts shown, active facts).
    pub fn context(&self, max_chars: usize) -> rusqlite::Result<(String, usize, usize)> {
        let rows = self.list_context()?;
        let facts: Vec<ContextFact> = rows.iter().map(FactRow::context).collect();
        let (text, shown) = context::render(&facts, max_chars);
        Ok((text, shown, rows.len()))
    }

    /// When the last conversation was active, and whether one is still open.
    pub fn last_activity(&self) -> (Option<i64>, bool) {
        let last = self
            .db
            .query_row("SELECT max(last) FROM sessions", [], |r| r.get(0))
            .unwrap_or(None);
        let open = self
            .db
            .query_row("SELECT count(*) FROM sessions WHERE closed = 0", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0)
            > 0;
        (last, open)
    }

    pub fn meta(&self, key: &str) -> Option<String> {
        self.db
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn del_meta(&self, key: &str) -> rusqlite::Result<()> {
        self.db
            .execute("DELETE FROM meta WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Applies a tidy-up in one transaction; ids that are not active facts are skipped. A
    /// merged fact takes the strongest origin of its sources.
    pub fn apply_tidy(&mut self, t: &Tidy) -> rusqlite::Result<TidyResult> {
        let at = now();
        let mut r = TidyResult {
            at,
            ok: true,
            ..Default::default()
        };
        let tx = self.db.transaction()?;
        let active = |tx: &rusqlite::Transaction,
                      id: i64|
         -> rusqlite::Result<Option<(String, i64, String)>> {
            tx.query_row(
                "SELECT kind, importance, origin FROM facts
                 WHERE id = ?1 AND invalid IS NULL AND status = 'active'",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
        };
        let rank = |o: &str| match o {
            "explicit" => 3,
            "screen" => 2,
            "inferred" => 1,
            _ => 0,
        };
        for (ids, text) in &t.merge {
            let mut found = Vec::new();
            for id in ids {
                if let Some(k) = active(&tx, *id)?
                    && !found.iter().any(|(f, _)| f == id)
                {
                    found.push((*id, k));
                }
            }
            if found.len() < 2 || text.trim().is_empty() {
                continue;
            }
            let kind = found[0].1.0.clone();
            let importance = found.iter().map(|f| f.1.1).max().unwrap_or(5);
            let origin = found
                .iter()
                .map(|f| f.1.2.as_str())
                .max_by_key(|o| rank(o))
                .unwrap_or("inferred")
                .to_string();
            tx.execute(
                "INSERT INTO facts(text, kind, importance, created, updated, status, origin, seen_day)
                 VALUES (?1, ?2, ?3, ?4, ?4, 'active', ?5, ?6)",
                params![text.trim(), kind, importance, at, origin, day(at)],
            )?;
            for (id, _) in &found {
                tx.execute(
                    "UPDATE facts SET invalid = ?2 WHERE id = ?1",
                    params![id, at],
                )?;
            }
            r.merged += found.len();
        }
        for (id, text) in &t.update {
            if text.trim().is_empty() {
                continue;
            }
            r.updated += tx.execute(
                "UPDATE facts SET text = ?2, updated = ?3 WHERE id = ?1 AND invalid IS NULL AND status = 'active'",
                params![id, text.trim(), at],
            )?;
        }
        for id in &t.invalidate {
            r.invalidated += tx.execute(
                "UPDATE facts SET invalid = ?2 WHERE id = ?1 AND invalid IS NULL AND status = 'active'",
                params![id, at],
            )?;
        }
        tx.commit()?;
        if r.merged + r.updated + r.invalidated > 0 {
            self.bump_rev()?;
        }
        Ok(r)
    }

    /// Active facts (the tile, `status`).
    pub fn count_facts(&self) -> i64 {
        self.db
            .query_row(
                "SELECT count(*) FROM facts WHERE invalid IS NULL AND status = 'active'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!(
                "home-companion-brain-mem-{}-{}",
                std::process::id(),
                now()
            ))
            .join(format!("{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn new<'a>(text: &'a str, kind: Kind, status: &'a str, origin: &'a str) -> NewFact<'a> {
        NewFact {
            text,
            kind,
            importance: 5,
            origin,
            status,
            quote: None,
            session: None,
        }
    }

    fn status_of(m: &Memory, id: i64) -> (String, bool) {
        m.db.query_row(
            "SELECT status, invalid IS NULL FROM facts WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn query_stems_and_stopwords() {
        assert_eq!(fts_query("che ore sono"), Some("\"ore\"*".into()));
        let q = fts_query("Cosa ti avevo detto del dentista?").unwrap();
        assert_eq!(q, "\"dentis\"*");
        assert_eq!(fts_query("e o ma"), None);
    }

    #[test]
    fn facts_recall_update_and_forget() {
        let d = tmp();
        let m = Memory::open(&d).unwrap();
        let a = m
            .add_fact("Marco va dal dentista ogni martedì", "routine", 7, None)
            .unwrap();
        m.add_fact("La caldaia è in cantina", "house", 4, None)
            .unwrap();
        let r = m.recall("quando ho il dentista?", 5, true).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, a);
        // Inflections: "dentisti" still finds "dentista" through the stem.
        assert_eq!(m.recall("i dentisti", 5, false).unwrap().len(), 1);
        m.update_fact(a, "Marco va dal dentista ogni mercoledì")
            .unwrap();
        assert!(
            m.recall("dentista", 5, false).unwrap()[0]
                .text
                .contains("mercoledì")
        );
        m.invalidate(a).unwrap();
        assert!(m.recall("dentista", 5, false).unwrap().is_empty());
        let rev = m.facts_rev();
        m.forget_all().unwrap();
        assert_eq!(m.count_facts(), 0);
        assert!(m.facts_rev() > rev);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sessions_gap_and_summaries() {
        let d = tmp();
        let m = Memory::open(&d).unwrap();
        let s1 = m.session(600, 1000).unwrap();
        m.add_exchange(s1, 1000, "ciao", "ciao Marco", "x", "timer", 10, (1, 1))
            .unwrap();
        assert_eq!(m.exchanges_of(s1).unwrap()[0].tools, "timer");
        assert_eq!(m.session(600, 1300).unwrap(), s1);
        assert_eq!(m.sessions_to_close(600, 1300).unwrap(), Vec::<i64>::new());
        assert_eq!(m.sessions_to_close(600, 1700).unwrap(), vec![s1]);
        m.close_session(s1, "Marco ha chiesto della caldaia")
            .unwrap();
        let s2 = m.session(600, 1700).unwrap();
        assert_ne!(s1, s2);
        assert_eq!(m.recall_sessions("caldaia", 3).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A version 0 database (before 0.3) with the device's eight facts of 14/11 (the name
    /// replaced), as the snapshot has them.
    fn legacy_db(d: &Path) {
        std::fs::create_dir_all(d).unwrap();
        let db = Connection::open(d.join("brain.db")).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db.execute_batch("ALTER TABLE facts ADD COLUMN updated INTEGER")
            .unwrap();
        let rows: [(i64, &str, &str, Option<i64>); 8] = [
            (
                1,
                "Il 2026-11-08 l'utente ha segnalato un problema di permessi con la lampada dello studio, che ha ripreso a funzionare regolarmente il 2026-11-09.",
                "house",
                Some(1794277816),
            ),
            (
                2,
                "Il 2026-11-09 è stato risolto l'errore di sincronizzazione delle luci dello studio, che ora funzionano correttamente dopo la segnalazione di problemi di permessi avvenuta il 2026-11-08.",
                "house",
                Some(1794277816),
            ),
            (3, "L'utente si chiama Mario R.", "fact", None),
            (
                4,
                "Il 9 novembre 2026 l'utente ha confermato la chiusura della tapparella sinistra del salone, ha spostato la tapparella al 10% il 13 novembre 2026 e non ha confermato di volerla chiudere il 11 novembre 2026.",
                "house",
                None,
            ),
            (
                5,
                "La lista della spesa dell'utente ora include latte, pane, gorgonzola e rotoloni.",
                "house",
                None,
            ),
            (
                6,
                "Il 9 novembre 2026 è stato risolto un problema di permessi con la lampada dello studio e l'errore di sincronizzazione delle luci, che ora funzionano correttamente.",
                "house",
                None,
            ),
            (
                7,
                "L'utente ha aggiunto rotoloni alla lista della spesa il 13 novembre 2026.",
                "preference",
                None,
            ),
            (
                8,
                "L'utente vuole essere ricordato di controllare il forno.",
                "commitment",
                None,
            ),
        ];
        for (id, text, kind, invalid) in rows {
            db.execute(
                "INSERT INTO facts(id, text, kind, importance, created, invalid) VALUES (?1, ?2, ?3, 5, 1794222247, ?4)",
                params![id, text, kind, invalid],
            )
            .unwrap();
        }
    }

    #[test]
    fn m1_migration_of_the_device_snapshot() {
        let d = tmp();
        legacy_db(&d);
        let m = Memory::open(&d).unwrap();
        let alive: Vec<(i64, String)> = {
            let mut st =
                m.db.prepare("SELECT id, status FROM facts WHERE invalid IS NULL ORDER BY id")
                    .unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            alive,
            vec![(3, "candidate".to_string()), (5, "candidate".to_string())]
        );
        let v: i64 =
            m.db.query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
        assert_eq!(v, 1);
        assert_eq!(m.context(1500).unwrap(), (String::new(), 0, 0));
        assert_eq!(m.count_facts(), 0);
        assert_eq!(m.list_candidates(20).unwrap().len(), 2);
        // Candidates are not recalled.
        assert!(m.recall("Mario", 5, false).unwrap().is_empty());
        drop(m);
        // Opening again changes nothing.
        let m = Memory::open(&d).unwrap();
        assert_eq!(m.list_candidates(20).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn m2_blocks_become_facts() {
        let d = tmp();
        legacy_db(&d);
        std::fs::write(
            d.join("profile.md"),
            "- Si chiama Marco\n- Beve il caffè senza zucchero\n\n",
        )
        .unwrap();
        let m = Memory::open(&d).unwrap();
        let ctx = m.list_context().unwrap();
        assert_eq!(ctx.len(), 2);
        assert!(
            ctx.iter()
                .all(|f| f.origin == "explicit" && f.kind == "person")
        );
        assert!(!d.join("profile.md").exists());
        assert!(d.join("profile.md.migrated").exists());
        drop(m);
        let m = Memory::open(&d).unwrap();
        assert_eq!(m.list_context().unwrap().len(), 2);
        m.forget_all().unwrap();
        assert!(!d.join("profile.md.migrated").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn d2_dedupe_confirm_and_tombstones() {
        let d = tmp();
        let m = Memory::open(&d).unwrap();
        let t0 = now();
        let cand = new(
            "La vicina si chiama Greta",
            Kind::Person,
            "candidate",
            "inferred",
        );
        let Added::New(id) = m.add_checked(&cand, false, t0).unwrap() else {
            panic!()
        };
        // The same day: still a candidate.
        assert_eq!(m.add_checked(&cand, false, t0).unwrap(), Added::Same(id));
        assert_eq!(status_of(&m, id).0, "candidate");
        // Another day: active.
        assert_eq!(
            m.add_checked(
                &new(
                    "La vicina di casa si chiama Greta",
                    Kind::Person,
                    "candidate",
                    "inferred"
                ),
                false,
                t0 + 86400
            )
            .unwrap(),
            Added::Confirmed(id)
        );
        assert_eq!(status_of(&m, id), ("active".to_string(), true));
        // Rejected: not learned again, unless the user says it.
        m.reject_fact(id).unwrap();
        assert_eq!(status_of(&m, id), ("rejected".to_string(), false));
        assert_eq!(m.add_checked(&cand, false, t0).unwrap(), Added::Refused);
        let explicit = new(
            "La vicina si chiama Greta",
            Kind::Person,
            "active",
            "explicit",
        );
        let Added::New(again) = m.add_checked(&explicit, true, t0).unwrap() else {
            panic!()
        };
        assert_eq!(status_of(&m, again).0, "active");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn e1_expiry() {
        let d = tmp();
        let m = Memory::open(&d).unwrap();
        let t0 = now();
        let old = t0 - 31 * 86400;
        let Added::New(c) = m
            .add_checked(
                &new(
                    "Il cane si chiama Fido",
                    Kind::Person,
                    "candidate",
                    "inferred",
                ),
                false,
                old,
            )
            .unwrap()
        else {
            panic!()
        };
        let Added::New(e) = m
            .add_checked(
                &new(
                    "Il gatto si chiama Neve",
                    Kind::Person,
                    "active",
                    "explicit",
                ),
                true,
                old,
            )
            .unwrap()
        else {
            panic!()
        };
        let Added::New(i) = m
            .add_checked(
                &new("La caldaia è in cantina", Kind::House, "active", "inferred"),
                false,
                old,
            )
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(m.expire(t0, 30, 200).unwrap(), 1);
        assert!(!status_of(&m, c).1);
        // Over the limit: the inferred one goes, never the user's own.
        assert_eq!(m.expire(t0, 30, 1).unwrap(), 1);
        assert!(!status_of(&m, i).1);
        assert_eq!(m.expire(t0, 30, 0).unwrap(), 0);
        assert!(status_of(&m, e).1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn y1_an_older_tidy_result_still_reads() {
        let r: TidyResult = serde_json::from_str(
            r#"{"at":1,"ok":true,"merged":2,"updated":1,"invalidated":0,"blocks":1,"skipped":""}"#,
        )
        .unwrap();
        assert_eq!(r.merged, 2);
        let r: TidyResult = serde_json::from_str(
            r#"{"at":1,"ok":true,"merged":0,"updated":0,"invalidated":0,"skipped":"x"}"#,
        )
        .unwrap();
        assert_eq!(r.blocks, 0);
    }

    #[test]
    fn r1_recall_marks_only_when_asked() {
        let d = tmp();
        let m = Memory::open(&d).unwrap();
        let a = m
            .add_fact("Il gatto si chiama Neve", "person", 5, None)
            .unwrap();
        let recalls = |m: &Memory| -> i64 {
            m.db.query_row("SELECT recalls FROM facts WHERE id = ?1", params![a], |r| {
                r.get(0)
            })
            .unwrap()
        };
        m.recall("gatto", 5, false).unwrap();
        assert_eq!(recalls(&m), 0);
        m.recall("gatto", 5, true).unwrap();
        assert_eq!(recalls(&m), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
