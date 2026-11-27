// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The board: short notes for whoever is home ("the plumber comes on Tuesday"), in the same
//! database as the memory but apart from it. They are not facts (never in the agent's
//! instructions nor in `sensor.alfred_memory`) and not reminders (no time). At most
//! [`MAX_NOTES`] of [`MAX_CHARS`] characters, one line each. Written by voice (the agent's
//! `note_add`, only when the user asked), from the browser panel and removed from the screen;
//! the agent reads them with `note_list` as the user's text, never as instructions.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub const MAX_NOTES: usize = 20;
pub const MAX_CHARS: usize = 280;

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS notes (
    id INTEGER PRIMARY KEY, text TEXT NOT NULL, created INTEGER NOT NULL);";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub id: i64,
    pub text: String,
    pub created: i64,
}

/// Why a note was not written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteError {
    /// Empty, too long, or no text left after cleaning.
    Invalid,
    /// There are [`MAX_NOTES`] already.
    Full,
    /// No such note.
    Unknown,
    Db(String),
}

impl NoteError {
    pub fn key(&self) -> String {
        match self {
            NoteError::Invalid => "invalid".into(),
            NoteError::Full => "full".into(),
            NoteError::Unknown => "unknown note".into(),
            NoteError::Db(e) => e.clone(),
        }
    }
}

impl From<rusqlite::Error> for NoteError {
    fn from(e: rusqlite::Error) -> Self {
        NoteError::Db(e.to_string())
    }
}

/// A note's text: line breaks and tabs become spaces, other control characters and the
/// bidirectional overrides go, spaces are squeezed; 1 to [`MAX_CHARS`] characters.
pub fn clean(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control()
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
        {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    let n = out.chars().count();
    (1..=MAX_CHARS).contains(&n).then_some(out)
}

pub fn list(db: &Connection) -> rusqlite::Result<Vec<Note>> {
    let mut st = db.prepare("SELECT id, text, created FROM notes ORDER BY id LIMIT ?1")?;
    st.query_map(params![MAX_NOTES as i64], |r| {
        Ok(Note {
            id: r.get(0)?,
            text: r.get(1)?,
            created: r.get(2)?,
        })
    })?
    .collect()
}

pub fn add(db: &Connection, text: &str, now: i64) -> Result<Note, NoteError> {
    let text = clean(text).ok_or(NoteError::Invalid)?;
    let n: i64 = db.query_row("SELECT count(*) FROM notes", [], |r| r.get(0))?;
    if n as usize >= MAX_NOTES {
        return Err(NoteError::Full);
    }
    db.execute(
        "INSERT INTO notes (text, created) VALUES (?1, ?2)",
        params![text, now],
    )?;
    Ok(Note {
        id: db.last_insert_rowid(),
        text,
        created: now,
    })
}

pub fn edit(db: &Connection, id: i64, text: &str) -> Result<(), NoteError> {
    let text = clean(text).ok_or(NoteError::Invalid)?;
    match db.execute(
        "UPDATE notes SET text = ?1 WHERE id = ?2",
        params![text, id],
    )? {
        0 => Err(NoteError::Unknown),
        _ => Ok(()),
    }
}

pub fn delete(db: &Connection, id: i64) -> Result<(), NoteError> {
    match db.execute("DELETE FROM notes WHERE id = ?1", params![id])? {
        0 => Err(NoteError::Unknown),
        _ => Ok(()),
    }
}

/// The note that best holds `words` (every word of three letters or more, in any case); None
/// when none or more than one do.
pub fn find(db: &Connection, words: &str) -> rusqlite::Result<Option<Note>> {
    let want: Vec<String> = words
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    if want.is_empty() {
        return Ok(None);
    }
    let hits: Vec<Note> = list(db)?
        .into_iter()
        .filter(|n| {
            let t = n.text.to_lowercase();
            want.iter().all(|w| t.contains(w.as_str()))
        })
        .collect();
    Ok((hits.len() == 1).then(|| hits.into_iter().next()).flatten())
}

pub fn get(db: &Connection, id: i64) -> rusqlite::Result<Option<Note>> {
    db.query_row(
        "SELECT id, text, created FROM notes WHERE id = ?1",
        params![id],
        |r| {
            Ok(Note {
                id: r.get(0)?,
                text: r.get(1)?,
                created: r.get(2)?,
            })
        },
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db
    }

    #[test]
    fn cleaning() {
        assert_eq!(
            clean("  The plumber\non Tuesday\t "),
            Some("The plumber on Tuesday".into())
        );
        assert_eq!(clean("a\u{202e}b\u{7}c"), Some("abc".into()));
        assert_eq!(clean("Pizza 🍕 stasera"), Some("Pizza 🍕 stasera".into()));
        assert_eq!(clean(" \n "), None);
        assert!(clean(&"x".repeat(MAX_CHARS)).is_some());
        assert!(clean(&"x".repeat(MAX_CHARS + 1)).is_none());
    }

    #[test]
    fn add_edit_find_delete() {
        let db = db();
        let a = add(&db, "The plumber comes on Tuesday", 10).unwrap();
        let b = add(&db, "Water the plants", 11).unwrap();
        assert_eq!(list(&db).unwrap(), [a.clone(), b.clone()]);
        assert_eq!(add(&db, "\n", 12), Err(NoteError::Invalid));
        edit(&db, a.id, "The plumber comes on Wednesday").unwrap();
        assert_eq!(
            get(&db, a.id).unwrap().unwrap().text,
            "The plumber comes on Wednesday"
        );
        assert_eq!(edit(&db, 99, "x"), Err(NoteError::Unknown));
        assert_eq!(find(&db, "plumber").unwrap().unwrap().id, a.id);
        assert_eq!(find(&db, "the").unwrap(), None, "both hold it");
        assert_eq!(find(&db, "xx").unwrap(), None);
        delete(&db, a.id).unwrap();
        assert_eq!(delete(&db, a.id), Err(NoteError::Unknown));
        for i in 0..MAX_NOTES - 1 {
            add(&db, &format!("note {i}"), 20).unwrap();
        }
        assert_eq!(add(&db, "one too many", 30), Err(NoteError::Full));
        assert_eq!(list(&db).unwrap().len(), MAX_NOTES);
    }
}
