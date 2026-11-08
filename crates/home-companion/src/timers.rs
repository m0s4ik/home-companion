// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Local timers (design section 5): Home Assistant's timers are mirrored from the Wyoming
//! timer events and rung by the body with its own clock, whether or not `timer-finished`
//! arrives. Kept in `<state_dir>/timers.json`, so they survive a restart.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timer {
    pub id: String,
    pub name: Option<String>,
    /// When it rings, seconds since the epoch (meaningful while active).
    pub due_unix: f64,
    pub total_seconds: u64,
    pub active: bool,
    /// Seconds left when paused (shown frozen).
    pub remaining_at_pause: f64,
    /// Already rung by us: a late `timer-finished` must not ring it again.
    pub finished_locally: bool,
}

impl Timer {
    pub fn remaining(&self, now: f64) -> f64 {
        if self.active {
            (self.due_unix - now).max(0.0)
        } else {
            self.remaining_at_pause.max(0.0)
        }
    }

    /// The name shown ("Timer" when Home Assistant gave none).
    pub fn label(&self) -> String {
        self.name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Timer".into())
    }
}

/// `mm:ss`, or `h:mm:ss` from one hour; rounded up, so 0:00 shows only when it rings.
pub fn format_left(secs: f64) -> String {
    let s = secs.max(0.0).ceil() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// A timer ticks softly each second of its last this many seconds.
pub const FINAL_SECONDS: u64 = 10;

pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// A timer as the screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct TimerView {
    pub id: String,
    pub name: String,
    /// "07:42".
    pub left: String,
    /// What is left of the whole, 1 → 0 (the ring).
    pub fraction: f32,
    pub paused: bool,
    /// When it rings (seconds since the epoch; meaningful while running).
    pub due_unix: f64,
}

#[derive(Debug, Default)]
pub struct Timers {
    list: Vec<Timer>,
    path: Option<PathBuf>,
    /// Ids rung and removed recently (a late `timer-finished` is matched against these).
    rung: VecDeque<String>,
}

/// What [`Timers::load`] found.
#[derive(Debug, Default)]
pub struct Loaded {
    pub kept: usize,
    /// Overdue by more than the limit: dropped silently.
    pub dropped: Vec<String>,
}

impl Timers {
    /// No file: kept in memory only (tests).
    pub fn in_memory() -> Timers {
        Timers::default()
    }

    /// Loads `path` (missing = no timers); drops timers overdue by more than `overdue_limit`
    /// seconds. Due ones stay and ring at the next tick.
    pub fn load(path: &Path, overdue_limit: u64, now: f64) -> (Timers, Loaded) {
        let mut t = Timers {
            path: Some(path.to_path_buf()),
            ..Default::default()
        };
        let mut loaded = Loaded::default();
        let list: Vec<Timer> = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                log::warn!("{}: {e}: timers ignored", path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        for timer in list {
            if timer.finished_locally
                || (timer.active && now - timer.due_unix > overdue_limit as f64)
            {
                loaded.dropped.push(timer.id);
            } else {
                t.list.push(timer);
            }
        }
        loaded.kept = t.list.len();
        if !loaded.dropped.is_empty() {
            t.save();
        }
        (t, loaded)
    }

    pub fn get(&self, id: &str) -> Option<&Timer> {
        self.list.iter().find(|t| t.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// `timer-started`: a new timer (or a restart of a known id).
    pub fn start(&mut self, t: &wyoming::TimerStarted, now: f64) {
        self.list.retain(|x| x.id != t.id);
        self.list.push(Timer {
            id: t.id.clone(),
            name: t.name.clone(),
            due_unix: now + t.total_seconds as f64,
            total_seconds: t.total_seconds,
            active: true,
            remaining_at_pause: t.total_seconds as f64,
            finished_locally: false,
        });
        self.save();
    }

    /// `timer-updated`: `total_seconds` is the time left. An unknown id is created (Home
    /// Assistant restarted before us). Returns true when it was created.
    pub fn update(&mut self, u: &wyoming::TimerUpdated, now: f64) -> bool {
        let left = u.total_seconds as f64;
        let created = match self.list.iter_mut().find(|x| x.id == u.id) {
            Some(t) => {
                t.due_unix = now + left;
                t.active = u.is_active;
                t.remaining_at_pause = left;
                if left > 0.0 {
                    t.finished_locally = false;
                }
                false
            }
            None => {
                self.list.push(Timer {
                    id: u.id.clone(),
                    name: None,
                    due_unix: now + left,
                    total_seconds: u.total_seconds,
                    active: u.is_active,
                    remaining_at_pause: left,
                    finished_locally: false,
                });
                true
            }
        };
        self.save();
        created
    }

    pub fn remove(&mut self, id: &str) -> Option<Timer> {
        let i = self.list.iter().position(|t| t.id == id)?;
        let t = self.list.remove(i);
        if t.finished_locally {
            self.remember_rung(id);
        }
        self.save();
        Some(t)
    }

    /// Active timers whose time has come and that were not rung yet.
    pub fn due(&self, now: f64) -> Vec<String> {
        self.list
            .iter()
            .filter(|t| t.active && !t.finished_locally && t.due_unix <= now)
            .map(|t| t.id.clone())
            .collect()
    }

    /// When the next active timer rings (seconds since the epoch), if any.
    pub fn next_due(&self) -> Option<f64> {
        self.list
            .iter()
            .filter(|t| t.active && !t.finished_locally)
            .map(|t| t.due_unix)
            .min_by(f64::total_cmp)
    }

    /// A running timer is in its last [`FINAL_SECONDS`] (the soft tick each second).
    pub fn in_final_seconds(&self, now: f64) -> bool {
        self.list.iter().any(|t| {
            let left = t.due_unix - now;
            t.active && !t.finished_locally && left > 0.0 && left <= FINAL_SECONDS as f64
        })
    }

    /// The ids of the timers shown with this name (a reflex's "il timer della pasta"), any case.
    pub fn named(&self, name: &str) -> Vec<String> {
        self.list
            .iter()
            .filter(|t| !t.finished_locally && t.label().to_lowercase() == name.to_lowercase())
            .map(|t| t.id.clone())
            .collect()
    }

    pub fn mark_rung(&mut self, id: &str) {
        if let Some(t) = self.list.iter_mut().find(|t| t.id == id) {
            t.finished_locally = true;
            self.save();
        }
        self.remember_rung(id);
    }

    /// Rung by us (still listed, or removed recently).
    pub fn was_rung(&self, id: &str) -> bool {
        self.get(id).is_some_and(|t| t.finished_locally) || self.rung.iter().any(|r| r == id)
    }

    fn remember_rung(&mut self, id: &str) {
        if !self.rung.iter().any(|r| r == id) {
            self.rung.push_back(id.to_string());
            while self.rung.len() > 32 {
                self.rung.pop_front();
            }
        }
    }

    /// The timers for the screen, soonest first; rung timers are left out (the ringing screen
    /// shows them).
    pub fn rows(&self, now: f64) -> Vec<TimerView> {
        let mut v: Vec<&Timer> = self.list.iter().filter(|t| !t.finished_locally).collect();
        v.sort_by(|a, b| a.remaining(now).total_cmp(&b.remaining(now)));
        v.into_iter()
            .map(|t| {
                let left = t.remaining(now);
                TimerView {
                    id: t.id.clone(),
                    name: t.label(),
                    left: format_left(left),
                    fraction: if t.total_seconds > 0 {
                        (left / t.total_seconds as f64).clamp(0.0, 1.0) as f32
                    } else {
                        0.0
                    },
                    paused: !t.active,
                    due_unix: t.due_unix,
                }
            })
            .collect()
    }

    /// A timer of the body's own (set by touch, "+1 minuto", an alarm): `local-<n>` ids, never
    /// sent to Home Assistant.
    pub fn start_local(&mut self, name: Option<String>, seconds: u64, now: f64) -> String {
        let n = (0..)
            .find(|n| !self.list.iter().any(|t| t.id == format!("local-{n}")))
            .unwrap_or(0);
        let id = format!("local-{n}");
        self.start(
            &wyoming::TimerStarted {
                id: id.clone(),
                total_seconds: seconds,
                name,
                start_hours: None,
                start_minutes: None,
                start_seconds: None,
            },
            now,
        );
        id
    }

    /// Pauses a running timer or resumes a paused one (by touch). For Home Assistant's timers
    /// this is local only: Wyoming has no way to tell Home Assistant.
    pub fn toggle_pause(&mut self, id: &str, now: f64) {
        let Some(t) = self.list.iter_mut().find(|t| t.id == id) else {
            return;
        };
        if t.active {
            t.remaining_at_pause = (t.due_unix - now).max(0.0);
            t.active = false;
        } else {
            t.due_unix = now + t.remaining_at_pause;
            t.active = true;
        }
        self.save();
    }

    /// Cancelled by touch: gone, and a late `timer-finished` for it is ignored.
    pub fn cancel(&mut self, id: &str) -> Option<Timer> {
        self.remember_rung(id);
        let i = self.list.iter().position(|t| t.id == id)?;
        let t = self.list.remove(i);
        self.save();
        Some(t)
    }

    /// Written on every change: to `.tmp`, then renamed.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let r = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            let text = serde_json::to_string_pretty(&self.list).map_err(std::io::Error::other)?;
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = r {
            log::warn!("{}: {e}: timers not saved", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(id: &str, secs: u64) -> wyoming::TimerStarted {
        wyoming::TimerStarted {
            id: id.into(),
            total_seconds: secs,
            name: Some("Pasta".into()),
            start_hours: None,
            start_minutes: None,
            start_seconds: Some(secs as u32),
        }
    }

    #[test]
    fn mirror_ring_and_late_finish() {
        let mut t = Timers::in_memory();
        t.start(&started("a", 90), 1000.0);
        let r = &t.rows(1000.0)[0];
        assert_eq!(
            (r.name.as_str(), r.left.as_str(), r.fraction),
            ("Pasta", "01:30", 1.0)
        );
        assert_eq!(t.rows(1045.0)[0].fraction, 0.5);
        assert_eq!(t.next_due(), Some(1090.0));
        t.start(&started("b", 30), 1000.0);
        assert_eq!(t.next_due(), Some(1030.0));
        t.remove("b");
        assert!(t.due(1089.0).is_empty());
        assert_eq!(t.due(1090.0), vec!["a".to_string()]);
        t.mark_rung("a");
        assert!(t.due(1100.0).is_empty(), "rung once only");
        assert_eq!(t.next_due(), None);
        assert!(t.rows(1100.0).is_empty());
        t.remove("a");
        assert!(t.was_rung("a"), "remembered after removal");
        assert!(!t.was_rung("b"));
    }

    #[test]
    fn updates_pause_and_unknown_ids() {
        let mut t = Timers::in_memory();
        t.start(&started("a", 60), 0.0);
        let paused = wyoming::TimerUpdated {
            id: "a".into(),
            is_active: false,
            total_seconds: 42,
        };
        assert!(!t.update(&paused, 10.0));
        assert_eq!(t.rows(500.0)[0].left, "00:42", "frozen while paused");
        assert!(t.rows(500.0)[0].paused);
        assert!(t.due(500.0).is_empty());
        let resumed = wyoming::TimerUpdated {
            id: "a".into(),
            is_active: true,
            total_seconds: 42,
        };
        t.update(&resumed, 500.0);
        assert_eq!(t.due(542.0), vec!["a".to_string()]);
        let unknown = wyoming::TimerUpdated {
            id: "z".into(),
            is_active: true,
            total_seconds: 3700,
        };
        assert!(t.update(&unknown, 0.0));
        assert_eq!(t.get("z").unwrap().label(), "Timer");
        assert_eq!(format_left(3700.0), "1:01:40");
        assert_eq!(format_left(0.2), "00:01");
    }

    #[test]
    fn persisted_and_overdue_dropped() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-timers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("timers.json");
        let (mut t, l) = Timers::load(&path, 600, 0.0);
        assert_eq!(l.kept, 0);
        t.start(&started("soon", 10), 1000.0);
        t.start(&started("old", 10), 0.0);
        t.start(&started("later", 100), 1000.0);
        let (t2, l) = Timers::load(&path, 600, 1020.0);
        assert_eq!(l.dropped, vec!["old".to_string()]);
        assert_eq!(l.kept, 2);
        assert_eq!(
            t2.due(1020.0),
            vec!["soon".to_string()],
            "due at start: rings at the next tick"
        );
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_timers_by_touch() {
        let mut t = Timers::in_memory();
        let a = t.start_local(Some("Uova".into()), 60, 0.0);
        let b = t.start_local(None, 120, 0.0);
        assert_eq!((a.as_str(), b.as_str()), ("local-0", "local-1"));
        t.toggle_pause(&a, 30.0);
        assert!(t.due(100.0).is_empty(), "paused");
        t.toggle_pause(&a, 100.0);
        assert_eq!(t.next_due(), Some(120.0));
        assert!(t.cancel(&b).is_some());
        assert!(t.was_rung(&b), "a late timer-finished is ignored");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn final_seconds_and_names() {
        let mut t = Timers::in_memory();
        let a = t.start_local(Some("Pasta".into()), 60, 0.0);
        t.start_local(None, 100, 0.0);
        assert!(!t.in_final_seconds(49.0));
        assert!(t.in_final_seconds(50.0));
        assert!(t.in_final_seconds(59.5));
        assert!(!t.in_final_seconds(60.0), "ringing, not ticking");
        t.toggle_pause(&a, 55.0);
        assert!(!t.in_final_seconds(55.0), "paused");
        assert_eq!(t.named("pasta"), vec![a.clone()]);
        assert_eq!(t.named("Timer").len(), 1);
        assert!(t.named("Uova").is_empty());
        t.mark_rung(&a);
        assert!(
            t.named("Pasta").is_empty(),
            "rung timers are not asked about"
        );
    }
}
