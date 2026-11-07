// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Alarms set by touch (Timer e sveglie → Nuova sveglia): an hour and a minute, the days of the
//! week, on or off. They belong to the body (Home Assistant knows nothing of them) and ring like
//! a timer: when one is due the core starts a local timer that is already over. Kept in
//! `<state_dir>/alarms.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Monday to Friday, as the `days` bits (bit 0 = Monday … bit 6 = Sunday).
pub const WEEKDAYS: u8 = 0b001_1111;
pub const WEEKEND: u8 = 0b110_0000;
pub const EVERY_DAY: u8 = 0b111_1111;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub id: u32,
    pub hour: u8,
    pub minute: u8,
    /// The days it rings (bit 0 = Monday); 0 = once, at the next time it comes.
    pub days: u8,
    pub on: bool,
    /// The minute it last rang (minutes since the epoch), so a minute rings once.
    #[serde(default)]
    pub rung_minute: i64,
}

impl Alarm {
    /// "07:00".
    pub fn time(&self) -> String {
        format!("{:02}:{:02}", self.hour, self.minute)
    }

    /// Minutes from `now` until it rings next (`weekday` 0 = Monday, `minute_of_day`), if on.
    pub fn minutes_until(&self, weekday: usize, minute_of_day: u32) -> Option<u32> {
        if !self.on {
            return None;
        }
        let at = self.hour as u32 * 60 + self.minute as u32;
        (0..8u32)
            .map(|d| d * 1440 + at)
            .filter(|&m| m > minute_of_day)
            .find(|&m| {
                let day = (weekday + (m / 1440) as usize) % 7;
                self.days == 0 || self.days & (1 << day) != 0
            })
            .map(|m| m - minute_of_day)
    }
}

/// Local time as the alarms need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    /// 0 = Monday.
    pub weekday: usize,
    pub hour: u32,
    pub minute: u32,
    /// Minutes since the epoch (the same minute rings once).
    pub epoch_minute: i64,
}

impl Now {
    pub fn local() -> Now {
        // SAFETY: time(NULL) only returns the time.
        Now::at(unsafe { libc::time(std::ptr::null_mut()) })
    }

    /// Local time at `t` (seconds since the epoch).
    pub fn at(t: i64) -> Now {
        // SAFETY: localtime_r only writes into the struct we pass.
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let tt = t as libc::time_t;
            libc::localtime_r(&tt, &mut tm);
            tm
        };
        Now {
            weekday: ((tm.tm_wday + 6) % 7) as usize,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            epoch_minute: t / 60,
        }
    }

    fn minute_of_day(&self) -> u32 {
        self.hour * 60 + self.minute
    }
}

#[derive(Debug, Default)]
pub struct Alarms {
    list: Vec<Alarm>,
    path: Option<PathBuf>,
}

impl Alarms {
    /// No file: kept in memory only (tests).
    pub fn in_memory() -> Alarms {
        Alarms::default()
    }

    /// A copy of a list (the screen's, to find the next one).
    pub fn from_list(list: Vec<Alarm>) -> Alarms {
        Alarms { list, path: None }
    }

    pub fn load(path: &Path) -> Alarms {
        let list = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                log::warn!("{}: {e}: alarms ignored", path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Alarms {
            list,
            path: Some(path.to_path_buf()),
        }
    }

    /// Sorted by time of day.
    pub fn list(&self) -> &[Alarm] {
        &self.list
    }

    pub fn add(&mut self, hour: u8, minute: u8, days: u8) -> u32 {
        let id = self.list.iter().map(|a| a.id).max().unwrap_or(0) + 1;
        self.list.push(Alarm {
            id,
            hour: hour.min(23),
            minute: minute.min(59),
            days: days & EVERY_DAY,
            on: true,
            rung_minute: 0,
        });
        self.list.sort_by_key(|a| (a.hour, a.minute, a.id));
        self.save();
        id
    }

    pub fn set_on(&mut self, id: u32, on: bool) {
        if let Some(a) = self.list.iter_mut().find(|a| a.id == id) {
            a.on = on;
            self.save();
        }
    }

    pub fn remove(&mut self, id: u32) -> Option<Alarm> {
        let i = self.list.iter().position(|a| a.id == id)?;
        let a = self.list.remove(i);
        self.save();
        Some(a)
    }

    /// Puts back an alarm removed by mistake (the "Annulla" chip).
    pub fn restore(&mut self, a: Alarm) {
        if !self.list.iter().any(|x| x.id == a.id) {
            self.list.push(a);
            self.list.sort_by_key(|a| (a.hour, a.minute, a.id));
            self.save();
        }
    }

    /// The alarms that ring now (each minute once); a one-off alarm turns itself off.
    pub fn due(&mut self, now: Now) -> Vec<Alarm> {
        let mut rung = Vec::new();
        for a in &mut self.list {
            let today = a.days == 0 || a.days & (1 << now.weekday) != 0;
            if a.on
                && today
                && a.hour as u32 == now.hour
                && a.minute as u32 == now.minute
                && a.rung_minute != now.epoch_minute
            {
                a.rung_minute = now.epoch_minute;
                if a.days == 0 {
                    a.on = false;
                }
                rung.push(a.clone());
            }
        }
        if !rung.is_empty() {
            self.save();
        }
        rung
    }

    /// The next alarm within `within` minutes: (alarm, minutes until it rings).
    pub fn next_within(&self, now: Now, within: u32) -> Option<(&Alarm, u32)> {
        self.list
            .iter()
            .filter_map(|a| Some((a, a.minutes_until(now.weekday, now.minute_of_day())?)))
            .filter(|(_, m)| *m <= within)
            .min_by_key(|(_, m)| *m)
    }

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
            log::warn!("{}: {e}: alarms not saved", path.display());
        }
    }
}

/// The days of an alarm in words: `names` are the locale's day names, Sunday first (as
/// locales/*.toml `days`); `words` gives "every_day", "weekdays", "weekend", "once".
pub fn days_text(days: u8, names: &[String], words: impl Fn(&str) -> String) -> String {
    match days & EVERY_DAY {
        0 => words("once"),
        EVERY_DAY => words("every_day"),
        WEEKDAYS => words("weekdays"),
        WEEKEND => words("weekend"),
        d => (0..7)
            .filter(|i| d & (1 << i) != 0)
            .map(|i| {
                let n = names.get((i + 1) % 7).map_or("", String::as_str);
                n.chars().take(3).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(weekday: usize, hour: u32, minute: u32) -> Now {
        Now {
            weekday,
            hour,
            minute,
            epoch_minute: (weekday as i64 * 1440) + (hour * 60 + minute) as i64,
        }
    }

    #[test]
    fn rings_on_its_days_once_a_minute() {
        let mut a = Alarms::in_memory();
        let id = a.add(7, 0, WEEKDAYS);
        assert!(a.due(at(0, 6, 59)).is_empty());
        assert_eq!(a.due(at(0, 7, 0)).len(), 1);
        // The same minute again: not twice.
        assert!(a.due(at(0, 7, 0)).is_empty());
        // Saturday: no.
        assert!(a.due(at(5, 7, 0)).is_empty());
        a.set_on(id, false);
        assert!(a.due(at(1, 7, 0)).is_empty());
    }

    #[test]
    fn a_one_off_alarm_turns_itself_off() {
        let mut a = Alarms::in_memory();
        a.add(9, 30, 0);
        assert_eq!(a.due(at(3, 9, 30)).len(), 1);
        assert!(!a.list()[0].on);
        assert!(a.due(at(4, 9, 30)).is_empty());
    }

    #[test]
    fn next_alarm_within_hours() {
        let mut a = Alarms::in_memory();
        a.add(7, 0, WEEKDAYS);
        // Sunday 23:00: Monday 7:00 is 8 hours away.
        let (alarm, m) = a.next_within(at(6, 23, 0), 12 * 60).unwrap();
        assert_eq!((alarm.time(), m), ("07:00".to_string(), 480));
        // Friday 23:00: the next is Monday.
        assert!(a.next_within(at(4, 23, 0), 12 * 60).is_none());
    }

    #[test]
    fn remove_and_restore() {
        let mut a = Alarms::in_memory();
        let id = a.add(7, 0, EVERY_DAY);
        let gone = a.remove(id).unwrap();
        assert!(a.list().is_empty());
        a.restore(gone);
        assert_eq!(a.list().len(), 1);
    }

    #[test]
    fn days_in_words() {
        let names: Vec<String> = [
            "domenica",
            "lunedì",
            "martedì",
            "mercoledì",
            "giovedì",
            "venerdì",
            "sabato",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let w = |k: &str| k.to_string();
        assert_eq!(days_text(WEEKDAYS, &names, w), "weekdays");
        assert_eq!(days_text(0, &names, w), "once");
        assert_eq!(days_text(0b000_0101, &names, w), "lun mer");
        assert_eq!(days_text(0b100_0000, &names, w), "dom");
    }
}
