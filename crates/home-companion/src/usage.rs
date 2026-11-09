// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the cloud services cost, roughly: per day, the questions that reached Home
//! Assistant's agent, the seconds of speech sent to be transcribed and the seconds of voice
//! that came back (or the answer's length when its voice was not played). Prices per unit come
//! from `[usage]` (whatever provider the user chose in Home Assistant: the body only counts),
//! so the screen can say "about 0.05 $ today": an order of magnitude, not the bill.
//!
//! Kept in `<state_dir>/usage.json`, the last [`KEEP_DAYS`] days.
//!
//! When the agent lives under the "OpenAI with Token & Cost Tracking" integration
//! (`openai_usage`), that integration counts what really went through it: its sensors are read
//! instead ([`Precise`], fetched by `ha::usage`) and the screen says which numbers it shows.

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
    /// Where the numbers come from.
    pub source: Source,
}

/// The body's estimate, or the precise numbers of the `openai_usage` integration.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Source {
    #[default]
    Estimate,
    /// Today's requests and tokens, when their sensors are on.
    Precise {
        requests: Option<u64>,
        tokens: Option<u64>,
    },
}

/// The `openai_usage` integration's numbers, as read from Home Assistant (US dollars).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Precise {
    pub today_cost: f64,
    pub month_cost: f64,
    pub requests: Option<u64>,
    pub tokens: Option<u64>,
    /// The last [`VIEW_DAYS`] days, oldest first: (day of the month, cost).
    pub days: Vec<(u32, f64)>,
}

impl View {
    /// What the screen shows: the integration's numbers when there are some, else the estimate.
    pub fn choose(estimate: &View, precise: Option<&Precise>) -> View {
        let Some(p) = precise else {
            return View {
                source: Source::Estimate,
                ..estimate.clone()
            };
        };
        View {
            today: Day::default(),
            month: Day::default(),
            today_cost: p.today_cost,
            month_cost: p.month_cost,
            days: p.days.clone(),
            source: Source::Precise {
                requests: p.requests,
                tokens: p.tokens,
            },
        }
    }

    pub fn is_precise(&self) -> bool {
        matches!(self.source, Source::Precise { .. })
    }
}

/// The last [`VIEW_DAYS`] local days up to `now`, oldest first, each with the highest value
/// a daily counter reached that day (`points`: seconds since the epoch, value). A daily counter
/// only grows until midnight, so its maximum is the day's total.
pub fn daily_max(points: &[(i64, f64)], now: i64) -> Vec<(u32, f64)> {
    let mut by_day: BTreeMap<String, f64> = BTreeMap::new();
    for &(t, v) in points {
        if v.is_finite() && v >= 0.0 {
            let e = by_day.entry(local_day(t)).or_insert(0.0);
            *e = e.max(v);
        }
    }
    (0..VIEW_DAYS as i64)
        .rev()
        .map(|k| {
            let day = local_day(now - k * 86_400);
            let n = day.get(8..).and_then(|d| d.parse().ok()).unwrap_or(0);
            (n, by_day.get(&day).copied().unwrap_or(0.0))
        })
        .collect()
}

/// Local midnight of the day `t` falls in (seconds since the epoch).
pub fn local_midnight(t: i64) -> i64 {
    // SAFETY: localtime_r and mktime only read and write the struct we pass.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = t as libc::time_t;
        libc::localtime_r(&tt, &mut tm);
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        libc::mktime(&mut tm) as i64
    }
}

/// Home Assistant's times ("2026-11-09T10:12:13.123456+00:00", "...Z") → seconds since the
/// epoch.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Skip the fraction; then Z, nothing (UTC) or ±HH:MM.
    let rest = s[19..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hm = rest[1..].replace(':', "");
            let oh: i64 = hm.get(..2)?.parse().ok()?;
            let om: i64 = match hm.get(2..4) {
                Some(m) => m.parse().ok()?,
                None => 0,
            };
            sign * (oh * 3600 + om * 60)
        }
    };
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset)
}

/// Days since 1970-01-01 of a civil date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
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
            source: Source::Estimate,
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

/// An amount on the screen with the language's decimal separator: an estimate as
/// [`dollars`] with "~" in front ("~0,04"), a precise one with a digit more ("0,018", "12,34").
pub fn amount(x: f64, precise: bool, sep: &str) -> String {
    let n = match (precise, x < 1.0) {
        (false, _) => format!("~{}", dollars(x)),
        (true, true) => format!("{x:.3}"),
        (true, false) => format!("{x:.2}"),
    };
    n.replace('.', sep)
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
        assert_eq!(v.source, Source::Estimate);
    }

    #[test]
    fn times_of_home_assistant() {
        assert_eq!(parse_time("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_time("2026-11-09T10:12:13.123456+00:00"),
            Some(1_794_219_133)
        );
        assert_eq!(
            parse_time("2026-11-09T11:12:13.5+01:00"),
            Some(1_794_219_133)
        );
        assert_eq!(parse_time("2026-11-09T05:12:13-05:00"), Some(1_794_219_133));
        assert_eq!(parse_time("2026-11-09"), None);
        assert_eq!(parse_time("unknown"), None);
    }

    #[test]
    fn a_daily_counter_gives_each_days_maximum() {
        let now = 1_794_200_000;
        let today = local_midnight(now);
        let yesterday = local_midnight(today - 3600);
        let points = [
            // Yesterday it grew to 0.30, then reset at midnight.
            (yesterday + 3600, 0.10),
            (yesterday + 7200, 0.30),
            (today, 0.0),
            (today + 60, 0.02),
            (today + 120, 0.05),
            (today + 180, f64::NAN),
        ];
        let d = daily_max(&points, now);
        assert_eq!(d.len(), VIEW_DAYS);
        assert_eq!(d[VIEW_DAYS - 1].1, 0.05);
        assert_eq!(d[VIEW_DAYS - 2].1, 0.30);
        assert_eq!(d[0].1, 0.0);
        let n: u32 = local_day(now)[8..].parse().unwrap();
        assert_eq!(d[VIEW_DAYS - 1].0, n);
        assert!(local_midnight(now) <= now && now - local_midnight(now) < 86_400 + 3600);
    }

    #[test]
    fn precise_numbers_win_when_there_are_some() {
        let est = View {
            today_cost: 0.5,
            days: vec![(1, 0.5)],
            ..Default::default()
        };
        assert_eq!(View::choose(&est, None), est);
        let p = Precise {
            today_cost: 0.018,
            month_cost: 1.234,
            requests: Some(7),
            tokens: None,
            days: vec![(1, 0.018)],
        };
        let v = View::choose(&est, Some(&p));
        assert!(v.is_precise());
        assert_eq!(v.today_cost, 0.018);
        assert_eq!(v.days, p.days);
        assert_eq!(
            v.source,
            Source::Precise {
                requests: Some(7),
                tokens: None
            }
        );
        assert_eq!(amount(0.0181, true, ","), "0,018");
        assert_eq!(amount(12.345, true, "."), "12.35");
        assert_eq!(amount(0.0412, false, ","), "~0,04");
    }
}
