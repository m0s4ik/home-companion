// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Long-term memory, on the device.
//!
//! - **Blocks**: `profile.md` (who the user is, preferences) and `notes.md` (lasting facts about
//!   the house and the character), plain Markdown files in the memory directory, always in the
//!   prompt, with a size cap; the model edits them with the `memory` tool, the user by hand.
//! - **`brain.db`** (SQLite, FTS5, no embeddings): `sessions` (a conversation: exchanges less than
//!   `session_gap_minutes` apart, with a summary written when it closes), `exchanges` (raw text,
//!   kept `keep_days`), `facts` (one sentence each, extracted when a session closes or added with
//!   "ricordati che…"; never deleted by the model, only invalidated).
//!
//! Recall: the words of the request (and their stems, a prefix match: FTS5 has no Italian
//! stemmer) against the active facts and the session summaries, BM25 plus importance.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

pub const PROFILE: &str = "profile";
pub const NOTES: &str = "notes";

#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub id: i64,
    pub text: String,
    pub kind: String,
    pub importance: i64,
}

/// A fact as the "Ricordi" page and the nightly tidy-up see it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FactRow {
    pub id: i64,
    pub text: String,
    pub kind: String,
    pub importance: i64,
    pub created: i64,
    /// Last change (= created when never changed).
    pub updated: i64,
}

/// The nightly tidy-up's decisions (the model's JSON, see `agent::consolidate`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tidy {
    /// Several facts saying the same thing → one sentence.
    pub merge: Vec<(Vec<i64>, String)>,
    pub update: Vec<(i64, String)>,
    /// Facts no longer true (an appointment last week).
    pub invalidate: Vec<i64>,
    /// The blocks rewritten whole (shorter, reordered); None = unchanged.
    pub profile: Option<String>,
    pub notes: Option<String>,
}

/// What a tidy-up did (counts only: they are logged and shown on the "Ricordi" page).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TidyResult {
    pub at: i64,
    pub ok: bool,
    pub merged: usize,
    pub updated: usize,
    pub invalidated: usize,
    pub blocks: usize,
    /// Why it did nothing ("" = it ran).
    pub skipped: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exchange {
    pub user: String,
    pub answer: String,
}

/// One change decided at the end of a session (mem0-style).
#[derive(Debug, Clone, PartialEq)]
pub enum FactOp {
    Add {
        text: String,
        kind: String,
        importance: i64,
    },
    Update {
        id: i64,
        text: String,
    },
    Delete {
        id: i64,
    },
}

pub struct Memory {
    db: Connection,
    dir: PathBuf,
    pub profile_max: usize,
    pub notes_max: usize,
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
const STOP: &[&str] = &[
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
    pub fn open(dir: &Path, profile_max: usize, notes_max: usize) -> rusqlite::Result<Memory> {
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
        Ok(Memory {
            db,
            dir: dir.to_path_buf(),
            profile_max,
            notes_max,
        })
    }

    /// The database, for the other tables of `brain.db` (`jobs`).
    pub fn db(&self) -> &Connection {
        &self.db
    }

    // ------------------------------------------------------------ blocks

    fn block_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.md"))
    }

    pub fn block(&self, name: &str) -> String {
        std::fs::read_to_string(self.block_path(name)).unwrap_or_default()
    }

    pub fn block_max(&self, name: &str) -> usize {
        if name == PROFILE {
            self.profile_max
        } else {
            self.notes_max
        }
    }

    /// The model's `memory` tool: add a line, replace or remove one (found by a unique substring).
    /// Over the cap = an error with the current content, so the model can make room.
    pub fn edit_block(
        &self,
        name: &str,
        action: &str,
        text: &str,
        old: &str,
    ) -> Result<String, String> {
        if name != PROFILE && name != NOTES {
            return Err(format!("unknown block {name:?}: profile or notes"));
        }
        let cur = self.block(name);
        let mut lines: Vec<String> = cur
            .lines()
            .map(str::to_string)
            .filter(|l| !l.trim().is_empty())
            .collect();
        let find = |lines: &[String], old: &str| -> Result<usize, String> {
            let hits: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| !old.is_empty() && l.contains(old))
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [i] => Ok(*i),
                [] => Err(format!("{old:?} not found in {name}")),
                _ => Err(format!(
                    "{old:?} is in more than one line of {name}: be more precise"
                )),
            }
        };
        let line = |t: &str| format!("- {}", t.trim().trim_start_matches("- "));
        match action {
            "add" => lines.push(line(text)),
            "replace" | "patch" => {
                let i = find(&lines, old)?;
                lines[i] = line(text);
            }
            "remove" => {
                let i = find(&lines, old)?;
                lines.remove(i);
            }
            _ => return Err(format!("unknown action {action:?}: add, patch or remove")),
        }
        let new = lines.join("\n") + "\n";
        let max = self.block_max(name);
        if new.chars().count() > max {
            return Err(format!(
                "{name} would be {} characters, over its limit of {max}: consolidate first (merge or shorten lines with patch, remove what is no longer true), then add. Now:\n{cur}",
                new.chars().count()
            ));
        }
        std::fs::write(self.block_path(name), &new).map_err(|e| e.to_string())?;
        Ok(format!("{name} updated"))
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
            "SELECT user, answer FROM exchanges WHERE session = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let mut v: Vec<Exchange> = st
            .query_map(params![session, n as i64], |r| {
                Ok(Exchange {
                    user: r.get(0)?,
                    answer: r.get(1)?,
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

    pub fn add_fact(
        &self,
        text: &str,
        kind: &str,
        importance: i64,
        session: Option<i64>,
    ) -> rusqlite::Result<i64> {
        self.db.execute(
            "INSERT INTO facts(text, kind, importance, created, source_session) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![text.trim(), kind, importance.clamp(1, 10), now(), session],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// Applies the end-of-session decisions; ids that are not active facts are skipped.
    pub fn apply(&self, ops: &[FactOp], session: Option<i64>) -> rusqlite::Result<usize> {
        let mut n = 0;
        for op in ops {
            n += match op {
                FactOp::Add {
                    text,
                    kind,
                    importance,
                } if !text.trim().is_empty() => {
                    self.add_fact(text, kind, *importance, session)?;
                    1
                }
                FactOp::Add { .. } => 0,
                FactOp::Update { id, text } => self.db.execute(
                    "UPDATE facts SET text = ?2, updated = ?3 WHERE id = ?1 AND invalid IS NULL",
                    params![id, text.trim(), now()],
                )?,
                FactOp::Delete { id } => self.db.execute(
                    "UPDATE facts SET invalid = ?2 WHERE id = ?1 AND invalid IS NULL",
                    params![id, now()],
                )?,
            };
        }
        Ok(n)
    }

    /// Active facts matching `text`, best first (BM25 + importance); marks them recalled.
    pub fn recall(&self, text: &str, limit: usize) -> rusqlite::Result<Vec<Fact>> {
        let Some(q) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let mut st = self.db.prepare(
            "SELECT f.id, f.text, f.kind, f.importance FROM facts_fts
             JOIN facts f ON f.id = facts_fts.rowid
             WHERE facts_fts MATCH ?1 AND f.invalid IS NULL
             ORDER BY bm25(facts_fts) - f.importance * 0.1 LIMIT ?2",
        )?;
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
        for f in &v {
            self.db.execute(
                "UPDATE facts SET recalls = recalls + 1, last_recalled = ?2 WHERE id = ?1",
                params![f.id, now()],
            )?;
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

    /// "Dimentica tutto": facts, conversations and both blocks.
    pub fn forget_all(&self) -> rusqlite::Result<()> {
        self.db.execute_batch(
            "DELETE FROM facts; DELETE FROM exchanges; DELETE FROM sessions;
             INSERT INTO facts_fts(facts_fts) VALUES ('rebuild');
             INSERT INTO summaries_fts(summaries_fts) VALUES ('rebuild');",
        )?;
        for b in [PROFILE, NOTES] {
            let _ = std::fs::remove_file(self.block_path(b));
        }
        let _ = self.db.execute_batch("VACUUM;");
        Ok(())
    }

    /// Active facts, newest first (the "Ricordi" page) or oldest first (the tidy-up).
    pub fn list_facts(&self, limit: usize, newest_first: bool) -> rusqlite::Result<Vec<FactRow>> {
        let order = if newest_first { "DESC" } else { "ASC" };
        let mut st = self.db.prepare(&format!(
            "SELECT id, text, kind, importance, created, coalesce(updated, created) FROM facts
             WHERE invalid IS NULL ORDER BY coalesce(updated, created) {order}, id {order} LIMIT ?1"
        ))?;
        st.query_map(params![limit as i64], |r| {
            Ok(FactRow {
                id: r.get(0)?,
                text: r.get(1)?,
                kind: r.get(2)?,
                importance: r.get(3)?,
                created: r.get(4)?,
                updated: r.get(5)?,
            })
        })?
        .collect()
    }

    /// The user deleted a fact on the screen: it goes for good (not just invalidated).
    pub fn delete_fact(&self, id: i64) -> rusqlite::Result<usize> {
        self.db
            .execute("DELETE FROM facts WHERE id = ?1", params![id])
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

    /// Applies a tidy-up in one transaction (the blocks after it, within their caps); ids
    /// that are not active facts are skipped.
    pub fn apply_tidy(&mut self, t: &Tidy) -> rusqlite::Result<TidyResult> {
        let at = now();
        let mut r = TidyResult {
            at,
            ok: true,
            ..Default::default()
        };
        let tx = self.db.transaction()?;
        let active =
            |tx: &rusqlite::Transaction, id: i64| -> rusqlite::Result<Option<(String, i64)>> {
                tx.query_row(
                    "SELECT kind, importance FROM facts WHERE id = ?1 AND invalid IS NULL",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
            };
        for (ids, text) in &t.merge {
            let mut found = Vec::new();
            for id in ids {
                if let Some(k) = active(&tx, *id)? {
                    found.push((*id, k));
                }
            }
            if found.len() < 2 || text.trim().is_empty() {
                continue;
            }
            let kind = found[0].1.0.clone();
            let importance = found.iter().map(|f| f.1.1).max().unwrap_or(5);
            tx.execute(
                "INSERT INTO facts(text, kind, importance, created, updated) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![text.trim(), kind, importance, at],
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
                "UPDATE facts SET text = ?2, updated = ?3 WHERE id = ?1 AND invalid IS NULL",
                params![id, text.trim(), at],
            )?;
        }
        for id in &t.invalidate {
            r.invalidated += tx.execute(
                "UPDATE facts SET invalid = ?2 WHERE id = ?1 AND invalid IS NULL",
                params![id, at],
            )?;
        }
        tx.commit()?;
        for (name, text) in [(PROFILE, &t.profile), (NOTES, &t.notes)] {
            if let Some(text) = text {
                let text = text.trim().to_string() + "\n";
                if text.chars().count() <= self.block_max(name)
                    && std::fs::write(self.block_path(name), text).is_ok()
                {
                    r.blocks += 1;
                }
            }
        }
        Ok(r)
    }

    pub fn count_facts(&self) -> i64 {
        self.db
            .query_row(
                "SELECT count(*) FROM facts WHERE invalid IS NULL",
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
            .join(format!("home-companion-brain-mem-{}-{}", std::process::id(), now()))
            .join(format!("{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        let _ = std::fs::remove_dir_all(&d);
        d
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
        let m = Memory::open(&d, 1400, 2200).unwrap();
        let a = m
            .add_fact("Andrea ha il dentista martedì alle 15", "event", 7, None)
            .unwrap();
        m.add_fact("La caldaia è in cantina", "house", 4, None)
            .unwrap();
        let r = m.recall("quando ho il dentista?", 5).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, a);
        // Inflections: "dentisti" still finds "dentista" through the stem.
        assert_eq!(m.recall("i dentisti", 5).unwrap().len(), 1);
        m.apply(
            &[FactOp::Update {
                id: a,
                text: "Andrea ha il dentista mercoledì".into(),
            }],
            None,
        )
        .unwrap();
        assert!(
            m.recall("dentista", 5).unwrap()[0]
                .text
                .contains("mercoledì")
        );
        m.apply(&[FactOp::Delete { id: a }], None).unwrap();
        assert!(m.recall("dentista", 5).unwrap().is_empty());
        m.forget_all().unwrap();
        assert_eq!(m.count_facts(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sessions_gap_and_summaries() {
        let d = tmp();
        let m = Memory::open(&d, 1400, 2200).unwrap();
        let s1 = m.session(600, 1000).unwrap();
        m.add_exchange(s1, 1000, "ciao", "ciao Andrea", "x", "", 10, (1, 1))
            .unwrap();
        assert_eq!(m.session(600, 1300).unwrap(), s1);
        assert_eq!(m.sessions_to_close(600, 1300).unwrap(), Vec::<i64>::new());
        assert_eq!(m.sessions_to_close(600, 1700).unwrap(), vec![s1]);
        m.close_session(s1, "Andrea ha chiesto della caldaia")
            .unwrap();
        let s2 = m.session(600, 1700).unwrap();
        assert_ne!(s1, s2);
        assert_eq!(m.recall_sessions("caldaia", 3).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn blocks_with_caps() {
        let d = tmp();
        let m = Memory::open(&d, 200, 200).unwrap();
        m.edit_block(PROFILE, "add", "Si chiama Andrea", "")
            .unwrap();
        m.edit_block(
            PROFILE,
            "replace",
            "Si chiama Andrea, vive a casa con il gatto",
            "Si chiama",
        )
        .unwrap();
        assert!(m.block(PROFILE).contains("gatto"));
        let long = "x".repeat(250);
        assert!(
            m.edit_block(PROFILE, "add", &long, "")
                .unwrap_err()
                .contains("limit")
        );
        m.edit_block(PROFILE, "remove", "", "gatto").unwrap();
        assert_eq!(m.block(PROFILE).trim(), "");
        assert!(m.edit_block("other", "add", "a", "").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
