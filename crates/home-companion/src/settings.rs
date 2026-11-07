// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the user chose: `<state_dir>/settings.toml`, `language = "it"` (the same small file
//! companion-sim writes), `volume = 40` (percent, set with the volume keys) and
//! `[casa_home_button] edge = "right", offset = 720` (where the user dragged the home button
//! of "Casa": an edge and the offset along it, in the view's logical pixels),
//! `ha_url = "http://ha.example:8123"` (Home Assistant picked on the first-start page
//! when the configuration has no `[home_assistant] url`).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Settings {
    pub path: PathBuf,
}

impl Settings {
    pub fn in_dir(state_dir: &Path) -> Settings {
        Settings {
            path: state_dir.join("settings.toml"),
        }
    }

    pub fn language(&self) -> Option<String> {
        self.table()?
            .get("language")?
            .as_str()
            .filter(|l| !l.is_empty())
            .map(String::from)
    }

    /// Home Assistant's address picked on the first-start page (the configuration's wins).
    pub fn ha_url(&self) -> Option<String> {
        self.table()?
            .get("ha_url")?
            .as_str()
            .filter(|u| !u.is_empty())
            .map(String::from)
    }

    pub fn save_ha_url(&self, url: &str) -> std::io::Result<()> {
        self.save("ha_url", url.into())?;
        log::info!(event = "ha_url", url = url; "Home Assistant {url} saved in {}", self.path.display());
        Ok(())
    }

    fn table(&self) -> Option<toml::Table> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        toml::from_str(&text)
            .map_err(|e| log::warn!("{}: {e}", self.path.display()))
            .ok()
    }

    /// The saved volume in percent (0-100).
    pub fn volume(&self) -> Option<u8> {
        let v = self.table()?.get("volume")?.as_integer()?;
        u8::try_from(v).ok().filter(|v| *v <= 100)
    }

    pub fn save_volume(&self, percent: u8) {
        if let Err(e) = self.save("volume", (percent as i64).into()) {
            log::warn!("{}: {e}", self.path.display());
        }
    }

    /// Where the user left "Casa"'s home button: (edge, offset), unchecked.
    pub fn casa_home_button(&self) -> Option<(String, f32)> {
        let t = self.table()?;
        let b = t.get("casa_home_button")?.as_table()?;
        let edge = b.get("edge")?.as_str()?.to_string();
        let offset = b
            .get("offset")
            .and_then(|o| o.as_float().or_else(|| o.as_integer().map(|i| i as f64)))?;
        Some((edge, offset as f32))
    }

    pub fn save_casa_home_button(&self, edge: &str, offset: f32) {
        let mut b = toml::Table::new();
        b.insert("edge".into(), edge.into());
        b.insert("offset".into(), (offset.round() as i64).into());
        if let Err(e) = self.save("casa_home_button", b.into()) {
            log::warn!("{}: {e}", self.path.display());
        }
    }

    /// Sets one key, keeping the others (written to a temporary file, then renamed).
    /// Night mode (Settings → Screen): on, start hour, end hour; by default 23 to 7.
    pub fn night(&self) -> (bool, u32, u32) {
        let t = self
            .table()
            .and_then(|t| t.get("night")?.as_table().cloned());
        let get = |k: &str, d: i64| {
            t.as_ref()
                .and_then(|t| t.get(k)?.as_integer())
                .unwrap_or(d)
                .rem_euclid(24) as u32
        };
        let on = t
            .as_ref()
            .and_then(|t| t.get("on")?.as_bool())
            .unwrap_or(true);
        (on, get("start", 23), get("end", 7))
    }

    pub fn save_night(&self, on: bool, start: u32, end: u32) {
        let mut n = toml::Table::new();
        n.insert("on".into(), on.into());
        n.insert("start".into(), (start as i64).into());
        n.insert("end".into(), (end as i64).into());
        self.save_logged("night", n.into());
    }

    /// Settings → "Salva le conversazioni" (history.rs); on by default.
    pub fn keep_conversations(&self) -> bool {
        self.flag("keep_conversations").unwrap_or(true)
    }

    pub fn save_keep_conversations(&self, keep: bool) {
        self.save_logged("keep_conversations", keep.into());
    }

    /// The control center's microphone switch: muted, the wake word is not listened for.
    pub fn mic_muted(&self) -> bool {
        self.flag("mic_muted").unwrap_or(false)
    }

    pub fn save_mic_muted(&self, muted: bool) {
        self.save_logged("mic_muted", muted.into());
    }

    /// The control center's AirPlay switch (when AirPlay is installed); on by default.
    pub fn airplay_on(&self) -> bool {
        self.flag("airplay").unwrap_or(true)
    }

    pub fn save_airplay_on(&self, on: bool) {
        self.save_logged("airplay", on.into());
    }

    /// "Più tardi" on the setup page: it does not come back by itself at start.
    pub fn setup_later(&self) -> bool {
        self.flag("setup_later").unwrap_or(false)
    }

    pub fn save_setup_later(&self) {
        self.save_logged("setup_later", true.into());
    }

    /// Home Assistant picked on screen after a "Più tardi": the link page comes at start again.
    pub fn clear_setup_later(&self) {
        if self.setup_later() {
            self.save_logged("setup_later", false.into());
        }
    }

    fn flag(&self, key: &str) -> Option<bool> {
        self.table()?.get(key)?.as_bool()
    }

    fn save_logged(&self, key: &str, value: toml::Value) {
        if let Err(e) = self.save(key, value) {
            log::warn!("{}: {e}", self.path.display());
        }
    }

    fn save(&self, key: &str, value: toml::Value) -> std::io::Result<()> {
        let mut table: toml::Table = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        table.insert(key.into(), value);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("toml.new");
        std::fs::write(&tmp, toml::to_string(&table).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.path)
    }

    pub fn save_language(&self, lang: &str) {
        let mut table: toml::Table = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        table.insert("language".into(), lang.into());
        let result = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&self.path, toml::to_string(&table).unwrap_or_default()));
        match result {
            Ok(()) => {
                log::info!(event = "language", language = lang; "language {lang} saved in {}", self.path.display())
            }
            Err(e) => log::warn!("{}: {e}", self.path.display()),
        }
    }
}

/// What the first start still has to ask, after [`apply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstStart {
    /// No language in the configuration nor in the settings: the welcome page.
    pub language: bool,
    /// No Home Assistant address in either: the page that finds it on the network.
    pub home_assistant: bool,
}

/// The language and Home Assistant's address in use: the language chosen on screen wins over
/// the configuration's (Settings → Language), the configuration's address wins over the one
/// picked on screen (the installer's `--ha-url` or a hand edit is deliberate). `None` / "" =
/// not chosen yet.
pub fn resolve(
    cfg_language: &str,
    saved_language: Option<String>,
    cfg_url: &str,
    saved_url: Option<String>,
) -> (Option<String>, String) {
    let language = saved_language
        .filter(|l| !l.is_empty())
        .or_else(|| (!cfg_language.is_empty()).then(|| cfg_language.to_string()));
    let url = if cfg_url.is_empty() {
        saved_url.unwrap_or_default()
    } else {
        cfg_url.to_string()
    };
    (language, url)
}

/// Puts the settings' language and Home Assistant address into `cfg` (see [`resolve`]); a
/// language never chosen becomes [`locale::DEFAULT`] until the welcome page.
pub fn apply(cfg: &mut crate::config::Config, s: &Settings) -> FirstStart {
    let (language, url) = resolve(
        &cfg.character.language,
        s.language(),
        &cfg.home_assistant.url,
        s.ha_url(),
    );
    let first = FirstStart {
        language: language.is_none(),
        home_assistant: url.is_empty(),
    };
    cfg.character.language = language.unwrap_or_else(|| locale::DEFAULT.to_string());
    cfg.home_assistant.url = url;
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_reads_back() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        assert_eq!(s.language(), None);
        s.save_language("it");
        assert_eq!(s.language().as_deref(), Some("it"));
        assert_eq!(s.volume(), None);
        s.save_volume(35);
        assert_eq!(s.volume(), Some(35));
        assert_eq!(s.language().as_deref(), Some("it"));
        s.save_language("en");
        assert_eq!(s.volume(), Some(35));
        assert_eq!(s.casa_home_button(), None);
        s.save_casa_home_button("left", 300.4);
        assert_eq!(s.casa_home_button(), Some(("left".into(), 300.0)));
        assert_eq!(s.volume(), Some(35));
        assert_eq!(s.language().as_deref(), Some("en"));
        s.save_volume(40);
        assert_eq!(s.casa_home_button(), Some(("left".into(), 300.0)));
        assert_eq!(s.night(), (true, 23, 7));
        s.save_night(false, 22, 6);
        assert_eq!(s.night(), (false, 22, 6));
        assert!(s.keep_conversations());
        s.save_keep_conversations(false);
        assert!(!s.keep_conversations());
        assert!(!s.setup_later());
        s.save_setup_later();
        assert!(s.setup_later());
        assert_eq!(s.volume(), Some(40));
        std::fs::write(&s.path, "volume = 300\n").unwrap();
        assert_eq!(s.volume(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_url_and_resolution() {
        let dir = std::env::temp_dir().join(format!(
            "home-companion-settings-url-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        assert_eq!(s.ha_url(), None);
        s.save_volume(30);
        s.save_ha_url("http://ha.example:8123").unwrap();
        assert_eq!(s.ha_url().as_deref(), Some("http://ha.example:8123"));
        assert_eq!(s.volume(), Some(30));

        let some = |v: &str| Some(v.to_string());
        // Nothing anywhere: both still to ask.
        assert_eq!(resolve("", None, "", None), (None, String::new()));
        // An old install: the configuration's language, no settings.
        assert_eq!(resolve("it", None, "", None).0, some("it"));
        // The screen's language wins over the configuration's.
        assert_eq!(resolve("it", some("en"), "", None).0, some("en"));
        assert_eq!(resolve("", some(""), "", None).0, None);
        // The configuration's address wins; the saved one fills an empty configuration.
        assert_eq!(
            resolve(
                "",
                None,
                "http://192.0.2.10:8123",
                some("http://192.0.2.11:8123")
            )
            .1,
            "http://192.0.2.10:8123"
        );
        assert_eq!(
            resolve("", None, "", some("http://192.0.2.11:8123")).1,
            "http://192.0.2.11:8123"
        );

        let mut cfg = crate::config::Config::default();
        cfg.character.language = String::new();
        cfg.home_assistant.url = String::new();
        assert_eq!(
            apply(&mut cfg, &s),
            FirstStart {
                language: true,
                home_assistant: false
            }
        );
        assert_eq!(cfg.character.language, locale::DEFAULT);
        assert_eq!(cfg.home_assistant.url, "http://ha.example:8123");
        s.save_language("it");
        let mut cfg = crate::config::Config::default();
        cfg.home_assistant.url = "http://192.0.2.10:8123".into();
        assert_eq!(
            apply(&mut cfg, &s),
            FirstStart {
                language: false,
                home_assistant: false
            }
        );
        assert_eq!(cfg.character.language, "it");
        assert_eq!(cfg.home_assistant.url, "http://192.0.2.10:8123");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
