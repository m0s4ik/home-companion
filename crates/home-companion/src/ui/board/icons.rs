// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The tiles' icons: Material Design Icons (Pictogrammers, Apache-2.0) as SVG path data in a
//! 24 x 24 view box, drawn by the screen with a `Path` (as the weather page's).
//!
//! - The default icons of [`super::default_icon`] and the demos' are built into the binary
//!   (`icon_table.rs`, written by tools/make-board-icons.py): the board works without any file.
//! - Any other icon a tile names is read from the bundle's `icons/mdi.json` (every icon, ~2.6
//!   MB; next to the `locales/` directory, [`file_beside`]) only when the board changes and
//!   names one that is missing; only the paths in use stay in memory.
//! - Names are compared without their hyphens ([`key`]): @mdi/js has camel-case names, so the
//!   file's kebab-case names are a best guess ("molecule-co-2" for "molecule-co2").

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::icon_table::TABLE;

/// The icon drawn when a tile's is in neither the table nor the file.
pub const FALLBACK: &str = "help-circle-outline";

/// A name's key: lower case, without hyphens ("mdi:" taken off).
pub fn key(name: &str) -> String {
    name.trim()
        .trim_start_matches("mdi:")
        .chars()
        .filter(|c| *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// A built-in icon's path data.
pub fn builtin(name: &str) -> Option<&'static str> {
    let k = key(name);
    TABLE
        .binary_search_by(|(t, _)| (*t).cmp(k.as_str()))
        .ok()
        .map(|i| TABLE[i].1)
}

/// The built-in icons' names (their keys: without hyphens), sorted.
pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    TABLE.iter().map(|(k, _)| *k)
}

/// The icons file beside a `locales/` directory: `<parent>/icons/mdi.json`.
pub fn file_beside(locales: &Path) -> PathBuf {
    locales
        .parent()
        .unwrap_or(Path::new("."))
        .join("icons")
        .join("mdi.json")
}

/// The icons in use: the built-in ones, and those read from the file.
#[derive(Debug, Clone, Default)]
pub struct Icons {
    file: PathBuf,
    /// Read from the file: key → path data (only the ones in use).
    extra: HashMap<String, String>,
}

impl Icons {
    pub fn new(file: PathBuf) -> Icons {
        Icons {
            file,
            extra: HashMap::new(),
        }
    }

    /// An icon's path data, the fallback's when it is not known.
    pub fn path(&self, name: &str) -> &str {
        builtin(name)
            .or_else(|| self.extra.get(&key(name)).map(String::as_str))
            .or_else(|| builtin(FALLBACK))
            .unwrap_or("")
    }

    /// Makes the icons read from the file exactly those of `names` that are not built in: the
    /// file is read only when one is missing. The names found nowhere are returned (drawn as
    /// the fallback).
    pub fn prepare<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let wanted: HashSet<String> = names
            .into_iter()
            .filter(|n| builtin(n).is_none())
            .map(key)
            .collect();
        self.extra.retain(|k, _| wanted.contains(k));
        let missing: HashSet<String> = wanted
            .iter()
            .filter(|k| !self.extra.contains_key(*k))
            .cloned()
            .collect();
        if missing.is_empty() {
            return Vec::new();
        }
        match read_file(&self.file, &missing) {
            Ok(found) => {
                log::info!(event = "board_icons", read = found.len(), wanted = missing.len();
                    "board: {} of {} icons read from {}", found.len(), missing.len(), self.file.display());
                self.extra.extend(found);
            }
            Err(e) => log::warn!("board: icons {}: {e}", self.file.display()),
        }
        let mut gone: Vec<String> = missing
            .into_iter()
            .filter(|k| !self.extra.contains_key(k))
            .collect();
        gone.sort();
        gone
    }
}

#[derive(serde::Deserialize)]
struct IconsFile {
    icons: HashMap<String, String>,
}

/// The icons of `keys` found in the file (by key).
pub fn read_file(path: &Path, keys: &HashSet<String>) -> Result<HashMap<String, String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_file(&text, keys)
}

/// The icons of `keys` in the file's text.
pub fn parse_file(text: &str, keys: &HashSet<String>) -> Result<HashMap<String, String>, String> {
    let f: IconsFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(f.icons
        .into_iter()
        .filter_map(|(name, d)| {
            let k = key(&name);
            (keys.contains(&k) && valid_path(&d)).then_some((k, d))
        })
        .collect())
}

/// Path data: SVG path commands and numbers only, not too long.
fn valid_path(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 16 * 1024
        && d.bytes().all(|b| {
            b.is_ascii_digit() || b" .,-eE".contains(&b) || b"MmLlHhVvCcSsQqTtAaZz".contains(&b)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_has_every_default() {
        assert!(TABLE.windows(2).all(|w| w[0].0 < w[1].0));
        let domains = [
            "light",
            "switch",
            "fan",
            "input_boolean",
            "automation",
            "scene",
            "script",
            "button",
            "input_button",
            "cover",
            "valve",
            "media_player",
            "sensor",
            "binary_sensor",
            "input_number",
            "number",
            "nothing",
        ];
        let classes = " outlet blind curtain awning shade window garage gate door water gas tv \
            receiver temperature humidity moisture power energy battery illuminance pressure \
            carbon_dioxide pm25 voltage current signal_strength wind_speed precipitation \
            timestamp monetary garage_door opening motion occupancy smoke connectivity plug \
            problem";
        let classes: Vec<&str> = classes.split(' ').collect();
        for d in domains {
            for &c in &classes {
                let name = super::super::default_icon(d, c);
                assert!(builtin(name).is_some(), "{d}/{c}: {name} is not built in");
            }
        }
        assert!(builtin(FALLBACK).is_some());
    }

    #[test]
    fn names_match_without_hyphens() {
        assert_eq!(key("mdi:Molecule-CO2"), "moleculeco2");
        assert!(builtin("molecule-co2").is_some());
        assert!(builtin("molecule-co-2").is_some());
        assert_eq!(builtin("ceiling-light"), builtin("ceilinglight"));
        assert!(builtin("no-such-icon").is_none());
    }

    #[test]
    fn only_the_missing_icons_are_read_and_kept() {
        let dir = std::env::temp_dir().join(format!("hc-icons-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mdi.json");
        std::fs::write(
            &file,
            r#"{"version":"7.4.47","icons":{"robot-vacuum":"M1 2L3 4Z","rocket":"M5 6Z",
               "bad":"<script>","fan":"M0 0Z"}}"#,
        )
        .unwrap();
        let mut icons = Icons::new(file.clone());
        assert_eq!(icons.prepare(["fan", "lightbulb"]), Vec::<String>::new());
        assert_eq!(icons.extra.len(), 0, "built in: the file is not read");
        assert_eq!(icons.prepare(["robot-vacuum", "bad", "fan"]), vec!["bad"]);
        assert_eq!(icons.path("robot-vacuum"), "M1 2L3 4Z");
        assert_eq!(
            icons.path("rocket"),
            builtin(FALLBACK).unwrap(),
            "not asked"
        );
        assert_eq!(icons.path("bad"), builtin(FALLBACK).unwrap());
        assert_ne!(icons.path("fan"), "M0 0Z", "the built-in one wins");
        std::fs::remove_file(&file).unwrap();
        // Still in use: kept without the file.
        assert!(icons.prepare(["robot-vacuum"]).is_empty());
        assert_eq!(icons.path("robot-vacuum"), "M1 2L3 4Z");
        // No longer in use: dropped.
        icons.prepare(["fan"]);
        assert!(icons.extra.is_empty());
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn the_file_sits_beside_the_locales() {
        assert_eq!(
            file_beside(Path::new("/opt/home-companion/locales")),
            Path::new("/opt/home-companion/icons/mdi.json")
        );
    }
}
