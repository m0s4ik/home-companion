// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The logger: one line per record on stderr (or a file), in one of three formats:
//! - `journal`: `<6>ts=… level=info target=home-companion msg="…" k=v …` (the sd-daemon `<N>` prefix gives
//!   journald the priority);
//! - `text`: `12:34:56.789 INFO  home-companion: message k=v` for a terminal;
//! - `json`: one object per line, key-values as fields (CI reads these).
//!
//! `auto` is `text` on a terminal and `journal` otherwise. `RUST_LOG` overrides the level:
//! `debug`, or `info,home_companion::core=debug` (per target prefix).

use std::fmt::Write as _;
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::kv::{Key, Value, VisitSource};
use log::{Level, LevelFilter, Log, Metadata, Record};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Journal,
    Text,
    Json,
}

pub struct Logger {
    format: Format,
    default: LevelFilter,
    targets: Vec<(String, LevelFilter)>,
    out: Mutex<Box<dyn Write + Send>>,
}

fn parse_level(s: &str) -> Option<LevelFilter> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// `level` from the configuration, `RUST_LOG` (if set) on top.
fn levels(level: &str, rust_log: Option<&str>) -> (LevelFilter, Vec<(String, LevelFilter)>) {
    let mut default = parse_level(level).unwrap_or(LevelFilter::Info);
    let mut targets = Vec::new();
    for part in rust_log
        .unwrap_or("")
        .split(',')
        .filter(|p| !p.trim().is_empty())
    {
        match part.split_once('=') {
            Some((t, l)) => {
                if let Some(l) = parse_level(l) {
                    targets.push((t.trim().to_string(), l));
                }
            }
            None => {
                if let Some(l) = parse_level(part) {
                    default = l;
                }
            }
        }
    }
    // Longest prefix first.
    targets.sort_by_key(|t| std::cmp::Reverse(t.0.len()));
    (default, targets)
}

impl Logger {
    pub fn new(format: &str, level: &str, file: &str) -> Result<Logger, String> {
        let out: Box<dyn Write + Send> = if file.is_empty() {
            Box::new(std::io::stderr())
        } else {
            if let Some(dir) = std::path::Path::new(file).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            Box::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(file)
                    .map_err(|e| format!("log.file {file}: {e}"))?,
            )
        };
        // SAFETY: isatty only reads the descriptor's state.
        let tty = file.is_empty() && unsafe { libc::isatty(2) } == 1;
        let format = match format {
            "journal" => Format::Journal,
            "text" => Format::Text,
            "json" => Format::Json,
            _ if tty => Format::Text,
            _ => Format::Journal,
        };
        let rust_log = std::env::var("RUST_LOG").ok();
        let (default, targets) = levels(level, rust_log.as_deref());
        Ok(Logger {
            format,
            default,
            targets,
            out: Mutex::new(out),
        })
    }

    /// Installs it as the `log` logger.
    pub fn install(self) -> Result<(), String> {
        let max = self
            .targets
            .iter()
            .map(|t| t.1)
            .chain([self.default])
            .max()
            .unwrap_or(LevelFilter::Info);
        log::set_boxed_logger(Box::new(self)).map_err(|e| format!("logger: {e}"))?;
        log::set_max_level(max);
        Ok(())
    }

    fn level_for(&self, target: &str) -> LevelFilter {
        self.targets
            .iter()
            .find(|(t, _)| target.starts_with(t.as_str()))
            .map_or(self.default, |t| t.1)
    }

    /// Formats one record (without the newline).
    pub fn format(&self, record: &Record) -> String {
        let mut kv = Collect(Vec::new());
        let _ = record.key_values().visit(&mut kv);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let level = record.level().as_str().to_ascii_lowercase();
        let msg = record.args().to_string();
        let mut s = String::with_capacity(160);
        match self.format {
            Format::Journal => {
                let _ = write!(
                    s,
                    "<{}>ts={} level={level} target={}",
                    priority(record.level()),
                    rfc3339(now),
                    record.target()
                );
                let _ = write!(s, " msg={}", logfmt(&msg));
                for (k, v) in &kv.0 {
                    let _ = write!(s, " {k}={}", logfmt(&v.text()));
                }
            }
            Format::Text => {
                let secs = now.as_secs();
                let (h, m, sec) = local_hms(secs as libc::time_t);
                let _ = write!(
                    s,
                    "{h:02}:{m:02}:{sec:02}.{:03} {:<5} {}: {msg}",
                    now.subsec_millis(),
                    record.level(),
                    record.target()
                );
                for (k, v) in &kv.0 {
                    let _ = write!(s, " {k}={}", logfmt(&v.text()));
                }
            }
            Format::Json => {
                let mut obj = serde_json::Map::new();
                obj.insert("ts".into(), rfc3339(now).into());
                obj.insert("t".into(), serde_json::json!(now.as_secs_f64()));
                obj.insert("level".into(), level.into());
                obj.insert("target".into(), record.target().into());
                obj.insert("msg".into(), msg.into());
                for (k, v) in kv.0 {
                    obj.insert(k, v.json());
                }
                s = serde_json::Value::Object(obj).to_string();
            }
        }
        s
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut line = self.format(record);
        line.push('\n');
        if let Ok(mut out) = self.out.lock() {
            let _ = out.write_all(line.as_bytes());
            let _ = out.flush();
        }
    }

    fn flush(&self) {
        if let Ok(mut out) = self.out.lock() {
            let _ = out.flush();
        }
    }
}

/// sd-daemon priority of a level.
fn priority(l: Level) -> u8 {
    match l {
        Level::Error => 3,
        Level::Warn => 4,
        Level::Info => 6,
        Level::Debug | Level::Trace => 7,
    }
}

/// A key-value as text or JSON.
enum Val {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl Val {
    fn from(v: &Value) -> Val {
        if let Some(b) = v.to_bool() {
            Val::Bool(b)
        } else if let Some(i) = v.to_i64() {
            Val::Int(i)
        } else if let Some(f) = v.to_f64() {
            Val::Float(f)
        } else {
            Val::Str(v.to_string())
        }
    }

    fn text(&self) -> String {
        match self {
            Val::Int(i) => i.to_string(),
            Val::Float(f) => format!("{f:.3}"),
            Val::Bool(b) => b.to_string(),
            Val::Str(s) => s.clone(),
        }
    }

    fn json(self) -> serde_json::Value {
        match self {
            Val::Int(i) => i.into(),
            Val::Float(f) if f.is_finite() => serde_json::json!(f),
            Val::Float(_) => serde_json::Value::Null,
            Val::Bool(b) => b.into(),
            Val::Str(s) => s.into(),
        }
    }
}

struct Collect(Vec<(String, Val)>);

impl<'kvs> VisitSource<'kvs> for Collect {
    fn visit_pair(&mut self, key: Key<'kvs>, value: Value<'kvs>) -> Result<(), log::kv::Error> {
        self.0.push((key.as_str().to_string(), Val::from(&value)));
        Ok(())
    }
}

/// logfmt quoting: bare when safe, else a JSON string.
fn logfmt(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '=' && c != '\\')
    {
        s.to_string()
    } else {
        serde_json::Value::String(s.to_string()).to_string()
    }
}

/// `2026-11-03T12:34:56.789Z`
pub fn rfc3339(since_epoch: std::time::Duration) -> String {
    let secs = since_epoch.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since_epoch.subsec_millis()
    )
}

/// Days since 1970-01-01 → (year, month, day) (H. Hinnant's algorithm).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn local_hms(t: libc::time_t) -> (i32, i32, i32) {
    // SAFETY: localtime_r only writes into the struct we pass.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(
            rfc3339(std::time::Duration::from_millis(1_793_708_096_789)),
            "2026-11-03T12:14:56.789Z"
        );
    }

    #[test]
    fn rust_log_levels() {
        let (d, t) = levels("info", Some("warn,home_companion::core=debug"));
        assert_eq!(d, LevelFilter::Warn);
        assert_eq!(
            t,
            vec![("home_companion::core".to_string(), LevelFilter::Debug)]
        );
        let (d, t) = levels("debug", None);
        assert_eq!(d, LevelFilter::Debug);
        assert!(t.is_empty());
    }

    #[test]
    fn formats() {
        let l = Logger {
            format: Format::Json,
            default: LevelFilter::Info,
            targets: vec![],
            out: Mutex::new(Box::new(std::io::sink())),
        };
        let kvs: &[(&str, Value)] = &[
            ("event", Value::from("timer_started")),
            ("n", Value::from(3u64)),
            ("ok", Value::from(true)),
        ];
        let rec = Record::builder()
            .args(format_args!("hello"))
            .level(Level::Info)
            .target("home_companion::core")
            .key_values(&kvs)
            .build();
        let v: serde_json::Value = serde_json::from_str(&l.format(&rec)).unwrap();
        assert_eq!(v["event"], "timer_started");
        assert_eq!(v["n"], 3);
        assert_eq!(v["ok"], true);
        assert_eq!(v["msg"], "hello");
        let l = Logger {
            format: Format::Journal,
            ..l
        };
        let s = l.format(&rec);
        assert!(s.starts_with("<6>ts="), "{s}");
        assert!(
            s.contains(" target=home_companion::core msg=hello event=timer_started n=3 ok=true"),
            "{s}"
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(logfmt("plain"), "plain");
        assert_eq!(logfmt("two words"), "\"two words\"");
        assert_eq!(logfmt(""), "\"\"");
    }
}
