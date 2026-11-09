// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the cloud services cost, roughly: per day, the questions that reached Home
//! Assistant's agent, the seconds of speech sent to be transcribed and the seconds of voice
//! that came back (or the answer's length when its voice was not played). Prices per unit come
//! from `[usage]` (whatever provider the user chose in Home Assistant: the body only counts),
//! so the screen can say "about 0.05 $ today": an order of magnitude, not the bill.
//!
//! Kept in `<state_dir>/usage.json`, the last [`KEEP_DAYS`] days.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Days kept in the file.
pub const KEEP_DAYS: usize = 62;
/// Characters per second of speech, to price an answer whose voice was not played.
const CHARS_PER_SECOND: f64 = 15.0;

/// Prices in US dollars per unit (`[usage]` in the configuration).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Prices {
    /// Speech-to-text, per minute of audio sent.
    pub stt_per_minute: f64,
    /// Text-to-speech, per minute of voice.
    pub tts_per_minute: f64,
    /// The agent, per question (a prompt with the tools and a short answer; a few thousand
    /// tokens).
    pub agent_per_question: f64,
}

impl Default for Prices {
    /// Rough list prices of small hosted models (late 2025), on the high side: the point is the
    /// order of magnitude.
    fn default() -> Self {
        Prices {
            stt_per_minute: 0.006,
            tts_per_minute: 0.015,
            agent_per_question: 0.005,
        }
    }
}

/// One day's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Day {
    pub questions: u32,
    pub stt_seconds: f64,
    pub tts_seconds: f64,
}

impl Day {
    pub fn cost(&self, p: &Prices) -> f64 {
        self.questions as f64 * p.agent_per_question
            + self.stt_seconds / 60.0 * p.stt_per_minute
            + self.tts_seconds / 60.0 * p.tts_per_minute
    }
}

/// The counts by day ("2026-11-09").
#[derive(Debug, Clone, Default)]
pub struct Usage {
    path: Option<PathBuf>,
    pub days: BTreeMap<String, Day>,
}

impl Usage {
    /// The counts in `dir/usage.json` (none yet: empty).
    pub fn in_dir(dir: &Path) -> Usage {
        let path = dir.join("usage.json");
        let days = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Usage {
            path: Some(path),
            days,
        }
    }

    /// A conversation that went to Home Assistant: `sent` seconds of speech, the answer's voice
    /// (`voice` seconds played, or `answer` characters when it was not played).
    pub fn add(&mut self, day: &str, sent: f64, voice: f64, answer: &str) {
        let d = self.days.entry(day.to_string()).or_default();
        d.questions += 1;
        d.stt_seconds += sent.max(0.0);
        d.tts_seconds += voice.max(answer.chars().count() as f64 / CHARS_PER_SECOND);
        while self.days.len() > KEEP_DAYS {
            let first = self.days.keys().next().cloned().unwrap_or_default();
            self.days.remove(&first);
        }
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text = serde_json::to_string(&self.days).unwrap_or_default();
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path)) {
            log::warn!("{}: {e}", path.display());
        }
    }

    /// (questions today, cost today, cost this month): `day` is today, its first 7 characters
    /// ("2026-11") the month.
    pub fn summary(&self, day: &str, p: &Prices) -> (u32, f64, f64) {
        let today = self.days.get(day).copied().unwrap_or_default();
        let month = day.get(..7).unwrap_or(day);
        let m: f64 = self
            .days
            .iter()
            .filter(|(k, _)| k.starts_with(month))
            .map(|(_, d)| d.cost(p))
            .sum();
        (today.questions, today.cost(p), m)
    }
}

/// What the screen shows: today, this month, the last days.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub today: Day,
    pub month: Day,
    pub today_cost: f64,
    pub month_cost: f64,
    /// The last [`VIEW_DAYS`] days, oldest first: (day of the month, cost).
    pub days: Vec<(u32, f64)>,
}

/// Days in the chart.
pub const VIEW_DAYS: usize = 30;

impl Usage {
    /// The view at `now` (seconds since the epoch, local days).
    pub fn view(&self, now: i64, p: &Prices) -> View {
        let today = local_day(now);
        let month = today.get(..7).unwrap_or(&today).to_string();
        let mut m = Day::default();
        for (_, d) in self.days.iter().filter(|(k, _)| k.starts_with(&month)) {
            m.questions += d.questions;
            m.stt_seconds += d.stt_seconds;
            m.tts_seconds += d.tts_seconds;
        }
        let days = (0..VIEW_DAYS as i64)
            .rev()
            .map(|k| {
                let day = local_day(now - k * 86_400);
                let n = day.get(8..).and_then(|d| d.parse().ok()).unwrap_or(0);
                (n, self.days.get(&day).map_or(0.0, |d| d.cost(p)))
            })
            .collect();
        let t = self.days.get(&today).copied().unwrap_or_default();
        View {
            today: t,
            month: m,
            today_cost: t.cost(p),
            month_cost: m.cost(p),
            days,
        }
    }
}

/// Today's date in local time, "2026-11-09".
pub fn local_day(t: i64) -> String {
    // SAFETY: localtime_r only writes into the struct we pass.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = t as libc::time_t;
        libc::localtime_r(&tt, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday
    )
}

/// "$" amounts on the screen: cents below a dollar ("0.04"), else one decimal.
pub fn dollars(x: f64) -> String {
    if x < 1.0 {
        format!("{x:.2}")
    } else {
        format!("{x:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_prices_by_day_and_month() {
        let p = Prices {
            stt_per_minute: 0.006,
            tts_per_minute: 0.015,
            agent_per_question: 0.01,
        };
        let mut u = Usage::default();
        // 30 s spoken, 60 s of voice back.
        u.add("2026-11-09", 30.0, 60.0, "x");
        // A quiet answer: its 150 characters count as 10 s of voice.
        u.add("2026-11-09", 0.0, 0.0, &"a".repeat(150));
        u.add("2026-10-31", 60.0, 0.0, "");
        let (q, today, month) = u.summary("2026-11-09", &p);
        assert_eq!(q, 2);
        let want = 0.02 + 0.5 * 0.006 + (70.0 / 60.0) * 0.015;
        assert!((today - want).abs() < 1e-9, "{today} {want}");
        assert!((month - today).abs() < 1e-9, "October is not November");
        assert_eq!(dollars(0.0412), "0.04");
        assert_eq!(dollars(12.34), "12.3");
    }

    #[test]
    fn keeps_two_months_and_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("hc-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut u = Usage::in_dir(&dir);
        for d in 1..=70 {
            u.add(
                &format!("2026-{:02}-{:02}", 1 + d / 28, 1 + d % 28),
                1.0,
                1.0,
                "",
            );
        }
        assert_eq!(u.days.len(), KEEP_DAYS);
        let again = Usage::in_dir(&dir);
        assert_eq!(again.days, u.days);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_view_has_thirty_days_ending_today() {
        let p = Prices::default();
        let now = 1_794_200_000; // 2026-11-09
        let mut u = Usage::default();
        let today = local_day(now);
        u.add(&today, 60.0, 60.0, "");
        u.add(&local_day(now - 2 * 86_400), 0.0, 0.0, "");
        let v = u.view(now, &p);
        assert_eq!(v.days.len(), VIEW_DAYS);
        assert!(v.days[VIEW_DAYS - 1].1 > 0.0, "today last");
        assert_eq!(v.days[VIEW_DAYS - 2].1, 0.0);
        assert!(v.days[VIEW_DAYS - 3].1 > 0.0);
        assert_eq!(v.today.questions, 1);
        assert!(v.month.questions >= 1);
    }
}
