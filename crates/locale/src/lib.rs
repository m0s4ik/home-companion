// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's own words per language: `locales/<code>.toml` (format in
//! `locales/README.md`). English is the default and the reference: a key another file
//! lacks is taken from English, one by one (a new feature ships in English and Italian,
//! and the other languages catch up when someone translates it); a key English does not
//! have is an error (a typo). [`load_or_english`] falls back to English as a whole only
//! when the file is missing or broken.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The `format` this crate reads.
pub const FORMAT: u32 = 1;
pub const DEFAULT: &str = "en";

#[derive(Clone, Debug)]
pub struct Locale {
    pub code: String,
    /// The language named in itself ("Italiano").
    pub name: String,
    /// From Sunday.
    pub days: Vec<String>,
    pub months: Vec<String>,
    text: BTreeMap<String, String>,
    sample: BTreeMap<String, String>,
    /// Keys this language lacks, shown in English ("text.key", "sample.key").
    pub missing: Vec<String>,
}

impl Locale {
    /// A word of the interface (`[text]`); the key itself if missing.
    pub fn text<'a>(&'a self, key: &'a str) -> &'a str {
        self.text.get(key).map_or(key, String::as_str)
    }

    /// Sample data for the simulator (`[sample]`).
    pub fn sample<'a>(&'a self, key: &'a str) -> &'a str {
        self.sample.get(key).map_or(key, String::as_str)
    }

    /// "Tuesday 3 November" (as the locale writes the names) (day of week from Sunday = 0, month from 0).
    pub fn date(&self, weekday: usize, day: u32, month: usize) -> String {
        format!("{} {day} {}", self.days[weekday % 7], self.months[month % 12])
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    format: u32,
    name: String,
    days: Vec<String>,
    months: Vec<String>,
    text: BTreeMap<String, String>,
    sample: BTreeMap<String, String>,
}

fn read(dir: &Path, code: &str) -> Result<Raw, String> {
    let file = dir.join(format!("{code}.toml"));
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))
}

/// Loads `dir/<code>.toml` and checks it against `dir/en.toml`; `Err` lists all the problems.
pub fn load(dir: &Path, code: &str) -> Result<Locale, Vec<String>> {
    let raw = read(dir, code).map_err(|e| vec![e])?;
    let mut errors = Vec::new();
    if raw.format != FORMAT {
        errors.push(format!("format {} (this body reads format {FORMAT})", raw.format));
    }
    if raw.days.len() != 7 {
        errors.push(format!("days: {} names, 7 needed (from Sunday)", raw.days.len()));
    }
    if raw.months.len() != 12 {
        errors.push(format!("months: {} names, 12 needed", raw.months.len()));
    }
    let mut raw = raw;
    let mut missing = Vec::new();
    if code != DEFAULT {
        match read(dir, DEFAULT) {
            Ok(en) => {
                for (table, mine, reference) in
                    [("text", &mut raw.text, &en.text), ("sample", &mut raw.sample, &en.sample)]
                {
                    for key in mine.keys().filter(|k| !reference.contains_key(*k)) {
                        errors.push(format!("[{table}] {key} is not in {DEFAULT}.toml"));
                    }
                    // Not translated yet: English, key by key.
                    let lacking: Vec<(String, String)> = reference
                        .iter()
                        .filter(|(k, _)| !mine.contains_key(*k))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    for (key, value) in lacking {
                        missing.push(format!("{table}.{key}"));
                        mine.insert(key, value);
                    }
                }
            }
            Err(e) => errors.push(e),
        }
    }
    let all = std::iter::once(("name".to_string(), &raw.name))
        .chain(raw.days.iter().map(|d| ("days".to_string(), d)))
        .chain(raw.months.iter().map(|m| ("months".to_string(), m)))
        .chain(raw.text.iter().map(|(k, v)| (format!("text.{k}"), v)))
        .chain(raw.sample.iter().map(|(k, v)| (format!("sample.{k}"), v)));
    for (what, s) in all {
        if let Some(c) = s.chars().find(|c| !character::printable(*c)) {
            errors.push(format!("{what}: character {c:?} (U+{:04X}) is not in the screen fonts", c as u32));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Locale {
        code: code.into(),
        name: raw.name,
        days: raw.days,
        months: raw.months,
        text: raw.text,
        sample: raw.sample,
        missing,
    })
}

/// The locale for `code`, or English when there is none (or it is broken).
pub fn load_or_english(dir: &Path, code: &str) -> Result<Locale, Vec<String>> {
    load(dir, code)
        .inspect(|l| {
            if !l.missing.is_empty() {
                eprintln!("locale {code}: {} key(s) not translated yet, shown in {DEFAULT}: {}", l.missing.len(), l.missing.join(", "));
            }
        })
        .or_else(|e| {
        if code != DEFAULT {
            eprintln!("locale {code}: {}; using {DEFAULT}", e.join("; "));
        }
        load(dir, DEFAULT)
    })
}

/// The codes of the locale files in `dir`, sorted.
pub fn available(dir: &Path) -> Vec<String> {
    let mut codes: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "toml").then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    codes.sort();
    codes
}

/// The `locales/` directory of this repository.
pub fn repo_locales() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every locale of the repository is valid (the check for pull requests); the keys a
    /// language has not translated yet are printed, not an error.
    #[test]
    fn repo_locales_are_valid() {
        let codes = available(&repo_locales());
        assert!(codes.contains(&DEFAULT.to_string()), "{DEFAULT}.toml is missing");
        for code in codes {
            match load(&repo_locales(), &code) {
                Err(errors) => panic!("locales/{code}.toml:\n  {}", errors.join("\n  ")),
                Ok(l) if !l.missing.is_empty() => {
                    println!("locales/{code}.toml: not translated yet: {}", l.missing.join(", "))
                }
                Ok(_) => {}
            }
        }
    }

    #[test]
    fn italian_and_fallback() {
        let it = load(&repo_locales(), "it").unwrap();
        assert_eq!(it.text("talk"), "Parla");
        assert_eq!(it.date(2, 3, 10), "martedì 3 novembre");
        assert_eq!(load_or_english(&repo_locales(), "xx").unwrap().code, "en");
    }

    #[test]
    fn missing_keys_are_listed() {
        let dir = std::env::temp_dir().join(format!("locale-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(repo_locales().join("en.toml"), dir.join("en.toml")).unwrap();
        std::fs::write(
            dir.join("xx.toml"),
            "format = 1\nname = \"X\"\ndays = [\"A\"]\nmonths = []\n[text]\nhome = \"“H”\"\nextra = \"E\"\n[sample]\n",
        )
        .unwrap();
        let errors = load(&dir, "xx").unwrap_err().join("\n");
        for want in ["days: 1", "months: 0", "[text] extra is not in", "U+201C"] {
            assert!(errors.contains(want), "missing {want:?} in:\n{errors}");
        }
        assert!(!errors.contains("talk"), "a missing key is not an error:\n{errors}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_key_is_english_alone() {
        let dir = std::env::temp_dir().join(format!("locale-fallback-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(repo_locales().join("en.toml"), dir.join("en.toml")).unwrap();
        // Italian without one key: the rest stays Italian, that key is English.
        let it = std::fs::read_to_string(repo_locales().join("it.toml")).unwrap();
        let it: String = it.lines().filter(|l| !l.starts_with("talk =")).map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join("it.toml"), it).unwrap();
        let l = load_or_english(&dir, "it").unwrap();
        assert_eq!(l.code, "it");
        assert_eq!(l.text("talk"), "Talk");
        assert_eq!(l.text("home_compact"), "Home compatta");
        assert_eq!(l.missing, ["text.talk"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
