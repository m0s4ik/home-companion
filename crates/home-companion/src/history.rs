// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What was said to the brain and what it answered, for the Conversazioni page: text only (never
//! audio, design.md section 11), on the device in `<state_dir>/conversations.json` (readable by
//! the `home-companion` user only), the last 7 days or 50 exchanges, whichever is less. Settings → "Salva
//! le conversazioni" off keeps nothing (and deletes the file). The brain keeps its own history
//! regardless.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const KEEP_SECONDS: i64 = 7 * 24 * 3600;
pub const KEEP_EXCHANGES: usize = 50;
/// A pause longer than this starts a new session (a separator line on the page).
pub const SESSION_GAP: i64 = 10 * 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exchange {
    /// Seconds since the epoch.
    pub at: i64,
    /// Who answered ("Home Assistant").
    pub brain: String,
    pub said: String,
    pub answer: String,
    /// "Da capo": the next exchange starts a new session whatever the time.
    #[serde(default)]
    pub new_session: bool,
}

#[derive(Debug, Default)]
pub struct History {
    list: Vec<Exchange>,
    path: Option<PathBuf>,
    keep: bool,
    /// The next exchange starts a new session ("Da capo").
    fresh: bool,
}

impl History {
    pub fn in_memory() -> History {
        History {
            keep: true,
            ..Default::default()
        }
    }

    /// Loads `path` (nothing if `keep` is false: then the file is deleted).
    pub fn load(path: &Path, keep: bool, now: i64) -> History {
        let mut h = History {
            list: Vec::new(),
            path: Some(path.to_path_buf()),
            keep,
            fresh: false,
        };
        if keep {
            if let Ok(s) = std::fs::read_to_string(path) {
                h.list = serde_json::from_str(&s).unwrap_or_else(|e| {
                    log::warn!("{}: {e}: conversations ignored", path.display());
                    Vec::new()
                });
            }
            h.prune(now);
        } else {
            let _ = std::fs::remove_file(path);
        }
        h
    }

    pub fn exchanges(&self) -> &[Exchange] {
        &self.list
    }

    pub fn keeps(&self) -> bool {
        self.keep
    }

    /// Keep or not from now on (off: everything goes).
    pub fn set_keep(&mut self, keep: bool) {
        self.keep = keep;
        if !keep {
            self.clear();
        }
    }

    pub fn push(&mut self, at: i64, brain: &str, said: &str, answer: &str) {
        if !self.keep || (said.trim().is_empty() && answer.trim().is_empty()) {
            return;
        }
        self.list.push(Exchange {
            at,
            brain: brain.to_string(),
            said: said.trim().to_string(),
            answer: answer.trim().to_string(),
            new_session: std::mem::take(&mut self.fresh),
        });
        self.prune(at);
        self.save();
    }

    /// "Da capo": what comes next is a new session.
    pub fn start_over(&mut self) {
        self.fresh = true;
    }

    pub fn clear(&mut self) {
        self.list.clear();
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }

    fn prune(&mut self, now: i64) {
        self.list.retain(|e| now - e.at <= KEEP_SECONDS);
        if self.list.len() > KEEP_EXCHANGES {
            let extra = self.list.len() - KEEP_EXCHANGES;
            self.list.drain(..extra);
        }
    }

    /// The page's lines, oldest first: ("sep", "Oggi · 14:02 · Home Assistant"), ("user", …),
    /// ("character", …). `when` writes a session's start (seconds since the epoch).
    pub fn lines(&self, when: impl Fn(i64) -> String) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut last: Option<&Exchange> = None;
        for e in &self.list {
            let new = match last {
                None => true,
                Some(l) => e.new_session || e.at - l.at > SESSION_GAP || e.brain != l.brain,
            };
            if new {
                out.push(("sep".into(), format!("{} · {}", when(e.at), e.brain)));
            }
            if !e.said.is_empty() {
                out.push(("user".into(), e.said.clone()));
            }
            if !e.answer.is_empty() {
                out.push(("character".into(), e.answer.clone()));
            }
            last = Some(e);
        }
        out
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let r = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            let text = serde_json::to_string(&self.list).map_err(std::io::Error::other)?;
            std::fs::write(&tmp, text)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = r {
            log::warn!("{}: {e}: conversations not saved", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_7_days_or_50_exchanges() {
        let mut h = History::in_memory();
        for i in 0..60 {
            h.push(1000 + i, "HA", &format!("q{i}"), "a");
        }
        assert_eq!(h.exchanges().len(), KEEP_EXCHANGES);
        assert_eq!(h.exchanges()[0].said, "q10");
        h.push(1000 + KEEP_SECONDS + 100, "HA", "late", "a");
        assert_eq!(h.exchanges().len(), 1);
    }

    #[test]
    fn sessions_by_pause_brain_and_start_over() {
        let mut h = History::in_memory();
        h.push(0, "HA", "one", "1");
        h.push(60, "HA", "two", "2");
        h.push(60 + SESSION_GAP + 1, "HA", "three", "");
        h.start_over();
        h.push(60 + SESSION_GAP + 30, "HA", "four", "4");
        let lines = h.lines(|t| t.to_string());
        let kinds: Vec<&str> = lines.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "sep",
                "user",
                "character",
                "user",
                "character",
                "sep",
                "user",
                "sep",
                "user",
                "character"
            ]
        );
        assert_eq!(lines[0].1, "0 · HA");
    }

    #[test]
    fn nothing_kept_when_off() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-history-{}", std::process::id()));
        let path = dir.join("conversations.json");
        let mut h = History::load(&path, true, 0);
        h.push(10, "HA", "hello", "hi");
        assert!(path.exists());
        assert_eq!(History::load(&path, true, 20).exchanges().len(), 1);
        let h2 = History::load(&path, false, 20);
        assert!(h2.exchanges().is_empty());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
