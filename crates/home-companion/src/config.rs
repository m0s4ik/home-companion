// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The configuration file (TOML, versioned). Unknown keys are errors, so typos are caught.
//! Relative paths are resolved against the directory of the file; empty paths mean "find it"
//! (see [`Config::resolve`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The format version written by this program.
pub const VERSION: u32 = 1;

/// Where the device install keeps its data (characters, locales).
pub const SHARE_DIR: &str = "/usr/share/home-companion";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub version: u32,
    pub satellite: Satellite,
    pub character: CharacterCfg,
    pub wake_word: WakeWordCfg,
    pub audio: AudioCfg,
    pub ui: UiCfg,
    pub screen: ScreenCfg,
    pub timers: TimersCfg,
    pub home_assistant: HomeAssistantCfg,
    pub weather: WeatherCfg,
    /// Lists in Home Assistant (`todo.*` entities) shown on the screen.
    pub lists: ListsCfg,
    /// "Casa": Home Assistant's frontend through the web helper (crates/home-companion/src/ui/webview).
    pub web: crate::ui::webview::WebCfg,
    /// Local reflexes: Vosk in the `home-companion-reflex` helper.
    pub reflexes: ReflexesCfg,
    /// AirPlay receiver (crates/home-companion/src/airplay.rs).
    pub airplay: crate::airplay::AirplayCfg,
    pub paths: Paths,
    pub log: LogCfg,
}

/// Home Assistant's WebSocket API (crates/home-companion/src/ha), for what Wyoming does not carry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HomeAssistantCfg {
    /// "http://homeassistant.example.home:8123"; "" = found on the screen at the first start
    /// (mDNS, saved in <state_dir>/settings.toml as `ha_url`); until then no connection.
    pub url: String,
    /// A file holding a long-lived access token, readable only by the body's user; relative to
    /// the configuration file's directory. Used while the body is not linked by QR code
    /// (Settings → Home Assistant: the tokens go to `<state_dir>/ha-auth.json` and win).
    pub token_file: String,
    /// The TCP port Home Assistant sends the phone back to while the link page is shown
    /// (crates/home-companion/src/ha/link.rs); nothing listens on it otherwise. The firewall must let the
    /// LAN reach it.
    pub link_port: u16,
    /// home-companion-brain's MCP server, as the setup tells Home Assistant (`http://<body>:<port>/mcp`).
    pub mcp_port: u16,
}

/// Local reflexes: after the wake word the utterance is first decoded by Vosk against the
/// character's `grammar/<lang>.toml` in the `home-companion-reflex` helper; a confident, exact phrase is
/// done by the body (time, timers, alarms, volume), anything else goes to Home Assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReflexesCfg {
    /// Off: every utterance streams to Home Assistant at once, as before M3.
    pub enabled: bool,
    /// Vosk models, one directory per language: `<model_dir>/<lang>` (e.g.
    /// vosk-model-small-it-0.22 as `it`). A language without a model has no reflexes.
    pub model_dir: String,
    /// The helper; "" = `home-companion-reflex` next to the `home-companion` binary.
    pub helper: String,
    /// Every word of the phrase must reach this confidence (0..1).
    pub threshold: f32,
    /// Waiting for the end of speech longer than this, the utterance goes to Home Assistant as
    /// it is and keeps streaming (a long question is not held back).
    pub max_seconds: f32,
}

/// Lists in Home Assistant shown on the screen.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListsCfg {
    /// The shopping list: a `todo.*` entity (Home Assistant's own shopping list, Bring!, …);
    /// "" = no "Spesa" card, tile or page. The same key as in home-companion-brain's brain.toml.
    pub shopping: String,
}

/// The weather on the home screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WeatherCfg {
    /// A `weather.*` entity of Home Assistant; "" = no weather.
    pub entity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Satellite {
    /// Shown in Home Assistant.
    pub name: String,
    /// Suggested area ("" = none).
    pub area: String,
    pub bind: String,
    /// Home Assistant's address(es): "192.0.2.10" or "192.0.2.0/24". Besides these, the addresses
    /// learned from Home Assistant after the QR link (ha-setup.json). Both empty = nobody.
    pub allowed: Vec<String>,
    /// Also ping Home Assistant every 2 s (offline after 5 s without pong).
    pub ping: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CharacterCfg {
    /// "" = the repository's characters/alfred when run from the build tree, else
    /// /usr/share/home-companion/characters/alfred.
    pub dir: String,
    /// Used unless the user picked one on screen (<state_dir>/settings.toml); "" = chosen on
    /// the screen at the first start (English until then).
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WakeWordCfg {
    /// "" = <character.dir>/wakeword/<first *.json>; else the path of a manifest.
    pub model: String,
    /// The stop words ("basta", "stop"), heard only while the character thinks, speaks, rings or makes an
    /// announcement: "" = every `*.json` in <character.dir>/stopword/ (any of them stops); "off" =
    /// none; else manifest paths separated by commas.
    pub stop_model: String,
    /// 0.0 = the manifest's.
    pub cutoff: f32,
    pub refractory_seconds: f32,
    pub cooloff_seconds: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AudioCfg {
    /// alsa | cpal | null | file
    pub backend: String,
    pub playback_device: String,
    pub capture_device: String,
    pub capture_channels: u16,
    /// left | right | average
    pub capture_channel: String,
    pub period_ms: u32,
    pub keep_open: bool,
    pub agc: bool,
    pub realtime: bool,
    pub volume: f32,
    /// The speaker volume (src/volume.rs): the card's mixer control it goes on ("" = the
    /// software gain of the mix, 100 % = `volume`).
    pub volume_control: String,
    /// The mixer device of that control.
    pub volume_card: String,
    /// The control's value at 100 %: the ceiling, never exceeded.
    pub volume_max_amp: i64,
    /// The control's value at the lowest step (`volume_step` %); linear in between.
    pub volume_min_amp: i64,
    /// One press of a volume key, in percent (also the lowest volume).
    pub volume_step: u8,
    /// backend = file only.
    pub capture_file: String,
    /// backend = file only.
    pub playback_file: String,
    /// backend = file only: pace like a card (false = as fast as there is something to play).
    pub file_realtime: bool,
    pub record: RecordCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RecordCfg {
    pub enabled: bool,
    pub dir: String,
    pub keep: usize,
    pub pre_roll_ms: u32,
    /// The audio before the wake word, in `<name>-wake.wav` next to each recording (0 = none):
    /// to see what woke the character when nobody called him.
    pub wake_context_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiCfg {
    /// auto | gpu | software | window | headless
    pub renderer: String,
    pub framebuffer: String,
    /// The companion screen is landscape: "auto" turns it by 90 degrees on a portrait panel;
    /// else 0, 90, 180 or 270 (clockwise).
    pub rotation: String,
    /// Touch screen: "auto" (the first /dev/input/event* with multi-touch axes), "" (none) or
    /// a device path. Not used in the simulator (the mouse is the touch there).
    pub touch: String,
    /// Wave refresh while a conversation is on screen.
    pub fps: u32,
    /// "" = <state_dir>/screenshots
    pub screenshot_dir: String,
}

/// Backlight, awake period, animations (crates/home-companion/src/ui/backlight.rs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScreenCfg {
    /// "auto" (the first /sys/class/backlight/*), "" (none: levels are only logged) or the
    /// backlight's directory.
    pub backlight: String,
    /// The home screen while nobody is around (0 = dark; the panel stays on).
    pub idle_brightness: u32,
    /// Every other screen, and the home screen while awake.
    pub active_brightness: u32,
    /// Night mode (its hours are in Settings → Screen): the levels never go above these.
    pub night_brightness: u32,
    pub night_active_brightness: u32,
    /// The levels follow the ambient light sensor (VCNL4200; ui/light.rs) on a curve through
    /// three points: `[lux, idle, active]` in the dark, in a lit room, in daylight. Without a
    /// sensor (or false) `idle_brightness` and `active_brightness` are used.
    pub auto_brightness: bool,
    pub auto_dark: [f32; 3],
    pub auto_room: [f32; 3],
    pub auto_bright: [f32; 3],
    /// How long a touch, a volume key, a movement or the end of a conversation keeps the home
    /// screen bright and the character moving.
    pub awake_seconds: f32,
    /// The volume keys (change the volume and wake the screen).
    pub keys: bool,
    /// "auto" (the first IIO device with in_accel_x_raw), "" (none) or the IIO device's directory.
    pub accelerometer: String,
    /// Movement between two readings (every 0.5 s) that wakes the screen, m/s² (0 = off).
    pub motion_threshold: f32,
    /// The character's animations (false: still poses only).
    pub animations: bool,
    /// The loading animation plays from start-up until Home Assistant connects, at most this long.
    pub loading_seconds: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TimersCfg {
    pub bell_repeats: u32,
    pub bell_gap_seconds: f32,
    /// A timer found due by more than this at start is dropped silently.
    pub overdue_limit_seconds: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Paths {
    /// "" = /var/lib/home-companion (simulator: $XDG_STATE_HOME/home-companion).
    pub state_dir: String,
    /// "" = the repository's locales/ when run from the build tree, else
    /// /usr/share/home-companion/locales.
    pub locales_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LogCfg {
    /// error | warn | info | debug | trace (RUST_LOG overrides)
    pub level: String,
    /// auto | journal | text | json
    pub format: String,
    /// "" = stderr; else a file the lines are appended to.
    pub file: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: VERSION,
            satellite: Satellite::default(),
            character: CharacterCfg::default(),
            wake_word: WakeWordCfg::default(),
            audio: AudioCfg::default(),
            ui: UiCfg::default(),
            screen: ScreenCfg::default(),
            timers: TimersCfg::default(),
            home_assistant: HomeAssistantCfg::default(),
            weather: WeatherCfg::default(),
            lists: ListsCfg::default(),
            web: Default::default(),
            reflexes: ReflexesCfg::default(),
            airplay: Default::default(),
            paths: Paths::default(),
            log: LogCfg::default(),
        }
    }
}

impl Default for Satellite {
    fn default() -> Self {
        Satellite {
            name: "ThinkSmart View".into(),
            area: String::new(),
            bind: "0.0.0.0:10700".into(),
            allowed: Vec::new(),
            ping: true,
        }
    }
}

impl Default for WakeWordCfg {
    fn default() -> Self {
        WakeWordCfg {
            model: String::new(),
            cutoff: 0.0,
            stop_model: String::new(),
            refractory_seconds: 2.0,
            cooloff_seconds: 1.0,
        }
    }
}

impl Default for AudioCfg {
    fn default() -> Self {
        AudioCfg {
            backend: if cfg!(target_os = "linux") {
                "alsa"
            } else {
                "cpal"
            }
            .into(),
            playback_device: "hw:0,0".into(),
            capture_device: "hw:0,1".into(),
            capture_channels: 2,
            capture_channel: "left".into(),
            period_ms: 20,
            keep_open: true,
            agc: false,
            realtime: true,
            volume: 1.0,
            volume_control: "Speaker Amp Master".into(),
            volume_card: "hw:0".into(),
            volume_max_amp: 85,
            volume_min_amp: 47,
            volume_step: 5,
            capture_file: String::new(),
            playback_file: String::new(),
            file_realtime: true,
            record: RecordCfg::default(),
        }
    }
}

impl Default for RecordCfg {
    fn default() -> Self {
        RecordCfg {
            enabled: false,
            dir: "/var/lib/home-companion/utterances".into(),
            keep: 20,
            pre_roll_ms: 0,
            wake_context_ms: 3500,
        }
    }
}

impl Default for UiCfg {
    fn default() -> Self {
        UiCfg {
            renderer: "auto".into(),
            framebuffer: "/dev/fb0".into(),
            rotation: "auto".into(),
            touch: "auto".into(),
            fps: 15,
            screenshot_dir: String::new(),
        }
    }
}

impl Default for ScreenCfg {
    fn default() -> Self {
        ScreenCfg {
            backlight: "auto".into(),
            idle_brightness: 600,
            active_brightness: 4095,
            night_brightness: 40,
            night_active_brightness: 400,
            auto_brightness: true,
            // In the sensor's own lux: behind the dark bezel glass the VCNL4200 reads about ten
            // times less than the room (measured on the device, 2026-11-05: ~1.5 for a room lit
            // in the evening, ~300 for a phone torch held close).
            auto_dark: [0.2, 40.0, 400.0],
            auto_room: [20.0, 600.0, 2200.0],
            auto_bright: [100.0, 1200.0, 4095.0],
            awake_seconds: 16.0,
            keys: true,
            accelerometer: "auto".into(),
            motion_threshold: 1.5,
            animations: true,
            loading_seconds: 30.0,
        }
    }
}

impl Default for TimersCfg {
    fn default() -> Self {
        TimersCfg {
            bell_repeats: 6,
            bell_gap_seconds: 1.0,
            overdue_limit_seconds: 600,
        }
    }
}

impl Default for HomeAssistantCfg {
    fn default() -> Self {
        HomeAssistantCfg {
            url: String::new(),
            token_file: "ha-token".into(),
            link_port: 8124,
            mcp_port: 10701,
        }
    }
}

impl Default for ReflexesCfg {
    fn default() -> Self {
        ReflexesCfg {
            enabled: true,
            model_dir: "/opt/home-companion/reflex".into(),
            helper: String::new(),
            // 0 false accepts and 93 % of the commands on the synthetic set,
            // provisional until real recordings tune it.
            threshold: 0.80,
            max_seconds: 8.0,
        }
    }
}

impl Default for WeatherCfg {
    fn default() -> Self {
        WeatherCfg {
            entity: "weather.home".into(),
        }
    }
}

impl Default for LogCfg {
    fn default() -> Self {
        LogCfg {
            level: "info".into(),
            format: "auto".into(),
            file: String::new(),
        }
    }
}

/// The default configuration file: /etc/home-companion/home-companion.toml on the device,
/// `$XDG_CONFIG_HOME/home-companion/home-companion.toml` (else ~/.config/…) for the simulator.
pub fn default_path(simulator: bool) -> PathBuf {
    if simulator {
        xdg_dir("XDG_CONFIG_HOME", ".config")
            .join("home-companion")
            .join("home-companion.toml")
    } else {
        PathBuf::from("/etc/home-companion/home-companion.toml")
    }
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// How the configuration was found (for the start-up log).
#[derive(Debug, Clone)]
pub enum Source {
    File(PathBuf),
    /// The default file does not exist: defaults.
    Missing(PathBuf),
}

/// Loads the configuration: defaults, then the simulator's overrides, then the file (the file
/// wins), then the version migration and [`Config::resolve`]. `explicit` is `--config`: a
/// missing explicit file is an error, a missing default file is not.
pub fn load(explicit: Option<&Path>, simulator: bool) -> Result<(Config, Source), String> {
    let path = explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_path(simulator));
    let mut base = Config::default();
    if simulator {
        base.audio.backend = "cpal".into();
        base.audio.capture_channels = 1;
        base.audio.capture_device = String::new();
        base.audio.playback_device = String::new();
        base.audio.volume_control = String::new();
        base.ui.renderer = "window".into();
        base.ui.touch = String::new();
        base.screen.backlight = String::new();
        base.screen.accelerometer = String::new();
        base.screen.keys = false;
        base.audio.record.dir = String::new();
    }
    let (mut cfg, source) = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let file: toml::Table =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let file = migrate(file).map_err(|e| format!("{}: {e}", path.display()))?;
            let mut merged = toml::Table::try_from(&base).map_err(|e| format!("defaults: {e}"))?;
            merge(&mut merged, file);
            let cfg: Config = merged
                .try_into()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            (cfg, Source::File(path.clone()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
            (base, Source::Missing(path.clone()))
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if simulator && !cfg.satellite.allowed.iter().any(|a| a == "127.0.0.1") {
        cfg.satellite.allowed.push("127.0.0.1".into());
    }
    let base_dir = match &source {
        Source::File(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
        Source::Missing(_) => std::env::current_dir().unwrap_or_default(),
    };
    cfg.resolve(&base_dir, simulator);
    cfg.validate()?;
    Ok((cfg, source))
}

/// Version migrations of the file's table (none yet: format 1 is the first).
fn migrate(mut t: toml::Table) -> Result<toml::Table, String> {
    let v = t
        .get("version")
        .and_then(|v| v.as_integer())
        .unwrap_or(VERSION as i64);
    if v < 1 || v > VERSION as i64 {
        return Err(format!(
            "version = {v}: this program reads version {VERSION}"
        ));
    }
    t.insert("version".into(), toml::Value::Integer(VERSION as i64));
    Ok(t)
}

/// Deep merge: tables are merged key by key, everything else is replaced.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

fn resolve_path(base: &Path, p: &mut String) {
    if !p.is_empty() && Path::new(p.as_str()).is_relative() {
        *p = base.join(p.as_str()).to_string_lossy().into_owned();
    }
}

impl Config {
    /// Relative paths against `base` (the file's directory), then the empty "find it" paths.
    pub fn resolve(&mut self, base: &Path, simulator: bool) {
        for p in [
            &mut self.character.dir,
            &mut self.wake_word.model,
            &mut self.audio.capture_file,
            &mut self.audio.playback_file,
            &mut self.audio.record.dir,
            &mut self.ui.screenshot_dir,
            &mut self.paths.state_dir,
            &mut self.paths.locales_dir,
            &mut self.log.file,
            &mut self.home_assistant.token_file,
        ] {
            resolve_path(base, p);
        }
        if self.character.dir.is_empty() {
            let repo = character::repo_characters().join("alfred");
            self.character.dir = if repo.is_dir() {
                clean(&repo)
            } else {
                format!("{SHARE_DIR}/characters/alfred")
            };
        }
        if self.paths.locales_dir.is_empty() {
            let repo = locale::repo_locales();
            self.paths.locales_dir = if repo.is_dir() {
                clean(&repo)
            } else {
                format!("{SHARE_DIR}/locales")
            };
        }
        if self.paths.state_dir.is_empty() {
            self.paths.state_dir = if simulator {
                xdg_dir("XDG_STATE_HOME", ".local/state")
                    .join("home-companion")
                    .to_string_lossy()
                    .into_owned()
            } else {
                "/var/lib/home-companion".into()
            };
        }
        if self.ui.screenshot_dir.is_empty() {
            self.ui.screenshot_dir = Path::new(&self.paths.state_dir)
                .join("screenshots")
                .to_string_lossy()
                .into_owned();
        }
        if self.audio.record.dir.is_empty() {
            self.audio.record.dir = Path::new(&self.paths.state_dir)
                .join("utterances")
                .to_string_lossy()
                .into_owned();
        }
        if self.wake_word.model.is_empty() {
            if let Some(m) = first_manifest(&Path::new(&self.character.dir).join("wakeword")) {
                self.wake_word.model = m.to_string_lossy().into_owned();
            }
        }
        if self.wake_word.stop_model.is_empty() {
            let all = all_manifests(&Path::new(&self.character.dir).join("stopword"));
            self.wake_word.stop_model = all
                .iter()
                .map(|m| m.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(",");
        }
    }

    fn validate(&self) -> Result<(), String> {
        let one_of = |key: &str, v: &str, ok: &[&str]| {
            if ok.contains(&v) {
                Ok(())
            } else {
                Err(format!("{key} = {v:?}: expected one of {}", ok.join(", ")))
            }
        };
        if !(0.0..=1.0).contains(&self.reflexes.threshold) {
            return Err(format!(
                "reflexes.threshold = {}: expected 0..1",
                self.reflexes.threshold
            ));
        }
        if !(2.0..=30.0).contains(&self.reflexes.max_seconds) {
            return Err(format!(
                "reflexes.max_seconds = {}: expected 2..30",
                self.reflexes.max_seconds
            ));
        }
        one_of(
            "audio.backend",
            &self.audio.backend,
            &["alsa", "cpal", "null", "file"],
        )?;
        one_of(
            "audio.capture_channel",
            &self.audio.capture_channel,
            &["left", "right", "average"],
        )?;
        one_of(
            "ui.renderer",
            &self.ui.renderer,
            &["auto", "gpu", "software", "window", "headless"],
        )?;
        one_of(
            "ui.rotation",
            &self.ui.rotation,
            &["auto", "0", "90", "180", "270"],
        )?;
        one_of(
            "log.level",
            &self.log.level,
            &["error", "warn", "info", "debug", "trace"],
        )?;
        one_of(
            "log.format",
            &self.log.format,
            &["auto", "journal", "text", "json"],
        )?;
        self.bind_addr()?;
        self.allowed_nets()?;
        if !self.home_assistant.url.is_empty() {
            crate::ha::endpoint(&self.home_assistant.url)
                .map_err(|e| format!("home_assistant.url = {e}"))?;
        }
        if !self.weather.entity.is_empty() && !self.weather.entity.starts_with("weather.") {
            return Err(format!(
                "weather.entity = {:?}: a weather.* entity",
                self.weather.entity
            ));
        }
        if !(0.0..=1.0).contains(&self.audio.volume) {
            return Err(format!("audio.volume = {}: 0.0 to 1.0", self.audio.volume));
        }
        let a = &self.audio;
        if !(1..=50).contains(&a.volume_step) {
            return Err(format!("audio.volume_step = {}: 1 to 50", a.volume_step));
        }
        if a.volume_min_amp < 0 || a.volume_min_amp > a.volume_max_amp {
            return Err(format!(
                "audio.volume_min_amp = {}: 0 to volume_max_amp ({})",
                a.volume_min_amp, a.volume_max_amp
            ));
        }
        if !(1..=2).contains(&self.audio.capture_channels) {
            return Err(format!(
                "audio.capture_channels = {}: 1 or 2",
                self.audio.capture_channels
            ));
        }
        if self.ui.fps == 0 || self.ui.fps > 60 {
            return Err(format!("ui.fps = {}: 1 to 60", self.ui.fps));
        }
        let sc = &self.screen;
        if !(0.0..=3600.0).contains(&sc.awake_seconds) {
            return Err(format!(
                "screen.awake_seconds = {}: 0 to 3600",
                sc.awake_seconds
            ));
        }
        if !(0.0..=600.0).contains(&sc.loading_seconds) {
            return Err(format!(
                "screen.loading_seconds = {}: 0 to 600",
                sc.loading_seconds
            ));
        }
        if sc.motion_threshold.is_nan() || sc.motion_threshold < 0.0 {
            return Err(format!(
                "screen.motion_threshold = {}: 0 (off) or more",
                sc.motion_threshold
            ));
        }
        self.web.validate()?;
        if self.home_assistant.link_port == 0 {
            return Err("home_assistant.link_port = 0: a free TCP port, 8124 by default".into());
        }
        if self
            .bind_addr()
            .is_ok_and(|b| b.port() == self.home_assistant.link_port)
        {
            return Err(format!(
                "home_assistant.link_port = {}: the satellite's port",
                self.home_assistant.link_port
            ));
        }
        if !(0.0..=1.0).contains(&self.wake_word.cutoff) {
            return Err(format!(
                "wake_word.cutoff = {}: 0.0 (the manifest's) to 1.0",
                self.wake_word.cutoff
            ));
        }
        Ok(())
    }

    /// The weather thread's settings; None = no weather (no `home_assistant.url` or no
    /// `weather.entity`).
    /// Home Assistant's address and a token are configured (else Settings → Home Assistant and
    /// the first start show the setup page).
    pub fn ha_linked(&self) -> bool {
        !self.home_assistant.url.is_empty() && self.ha_auth().has_token()
    }

    /// The body's tokens for Home Assistant (linked by QR code, or the token file).
    pub fn ha_auth(&self) -> crate::ha::auth::Auth {
        let ha = &self.home_assistant;
        crate::ha::auth::Auth::new(&ha.url, Path::new(&ha.token_file), &self.state_dir())
    }

    /// Home Assistant's host name ("ha.example.org"), for the setup page.
    pub fn ha_host(&self) -> Option<String> {
        let url = &self.home_assistant.url;
        let rest = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
        let host = rest.split(['/', ':']).next()?;
        (!host.is_empty()).then(|| host.to_string())
    }

    /// The automatic brightness curve, if on.
    pub fn brightness_curve(&self) -> Option<crate::ui::light::Curve> {
        let s = &self.screen;
        let point = |p: [f32; 3]| {
            (
                p[0],
                crate::ui::backlight::Levels {
                    idle: p[1].max(0.0) as u32,
                    active: p[2].max(0.0) as u32,
                },
            )
        };
        s.auto_brightness.then(|| crate::ui::light::Curve {
            dark: point(s.auto_dark),
            room: point(s.auto_room),
            bright: point(s.auto_bright),
        })
    }

    /// The setup of Home Assistant (Settings → Home Assistant), sharing `auth`.
    pub fn setup_config(
        &self,
        auth: std::sync::Arc<crate::ha::auth::Auth>,
    ) -> Result<crate::ha::setup::SetupConfig, String> {
        let character = Path::new(&self.character.dir);
        Ok(crate::ha::setup::SetupConfig {
            url: self.home_assistant.url.clone(),
            auth,
            state_file: self.state_dir().join(crate::ha::setup::STATE_FILE),
            name: self.satellite.name.clone(),
            wyoming_port: self.bind_addr()?.port(),
            mcp_port: self.home_assistant.mcp_port,
            prompt: crate::ha::setup::agent_prompt(character, &self.character.language),
            brain: crate::brain_link::brain_info,
            learned: None,
        })
    }

    /// The weather thread's settings, sharing `auth` with the rest of the body.
    pub fn weather_config(
        &self,
        auth: std::sync::Arc<crate::ha::auth::Auth>,
    ) -> Option<crate::ha::WeatherConfig> {
        let ha = &self.home_assistant;
        if ha.url.is_empty() || self.weather.entity.is_empty() {
            return None;
        }
        Some(crate::ha::WeatherConfig::new(
            &ha.url,
            auth,
            &self.weather.entity,
        ))
    }

    pub fn bind_addr(&self) -> Result<std::net::SocketAddr, String> {
        self.satellite
            .bind
            .parse()
            .map_err(|e| format!("satellite.bind = {:?}: {e}", self.satellite.bind))
    }

    pub fn allowed_nets(&self) -> Result<Vec<wyoming::Net>, String> {
        self.satellite
            .allowed
            .iter()
            .map(|a| wyoming::Net::parse(a).map_err(|e| format!("satellite.allowed {a:?}: {e}")))
            .collect()
    }

    /// The audio crate's configuration.
    pub fn audio_config(&self) -> audio::AudioConfig {
        let a = &self.audio;
        let opt = |s: &str| {
            if s.is_empty() {
                None
            } else {
                Some(PathBuf::from(s))
            }
        };
        let backend = match a.backend.as_str() {
            "cpal" => audio::Backend::Cpal,
            "null" => audio::Backend::Null,
            "file" => audio::Backend::File {
                capture: opt(&a.capture_file),
                playback: opt(&a.playback_file),
                realtime: a.file_realtime,
            },
            _ => audio::Backend::Alsa,
        };
        audio::AudioConfig {
            backend,
            playback_device: a.playback_device.clone(),
            capture_device: a.capture_device.clone(),
            capture_channels: a.capture_channels,
            capture_channel: match a.capture_channel.as_str() {
                "right" => audio::ChannelPick::Right,
                "average" => audio::ChannelPick::Average,
                _ => audio::ChannelPick::Left,
            },
            period_ms: a.period_ms,
            keep_open: a.keep_open,
            idle_close: Duration::from_secs(30),
            agc: a.agc,
            realtime: a.realtime,
            volume: a.volume,
            recorder: a.record.enabled.then(|| audio::RecorderConfig {
                dir: PathBuf::from(&a.record.dir),
                keep: a.record.keep,
                pre_roll_ms: a.record.pre_roll_ms,
                wake_context_ms: a.record.wake_context_ms,
            }),
        }
    }

    /// The volume's configuration.
    pub fn volume_config(&self) -> crate::volume::VolumeConfig {
        let a = &self.audio;
        crate::volume::VolumeConfig {
            // Only the real card's control: the file and null backends (tests) never touch it.
            control: if a.backend == "alsa" {
                a.volume_control.clone()
            } else {
                String::new()
            },
            card: a.volume_card.clone(),
            curve: crate::volume::Curve {
                min_amp: a.volume_min_amp,
                max_amp: a.volume_max_amp,
                step: a.volume_step,
            },
            base_gain: a.volume,
        }
    }

    /// The wake word detector's configuration.
    pub fn detector_config(&self) -> wakeword::DetectorConfig {
        let w = &self.wake_word;
        wakeword::DetectorConfig {
            cutoff: (w.cutoff > 0.0).then_some(w.cutoff),
            cooloff: Duration::from_secs_f32(w.cooloff_seconds.max(0.0)),
            refractory: Duration::from_secs_f32(w.refractory_seconds.max(0.0)),
            ..Default::default()
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        PathBuf::from(&self.paths.state_dir)
    }

    /// The effective configuration as TOML (`--print-config`).
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }
}

/// `a/b/../c` → `a/c` for display (the repository paths go through `crates/x/../../`).
fn clean(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Every `*.json` of a directory, alphabetically (none if it does not exist).
fn all_manifests(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The first `*.json` of a directory, alphabetically.
pub fn first_manifest(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    v.sort();
    v.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("home-companion.toml");
        std::fs::write(&p, text).unwrap();
        p
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "home-companion-config-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn defaults_round_trip_and_validate() {
        let c = Config::default();
        let t = c.to_toml();
        let back: Config = toml::from_str(&t).unwrap();
        assert_eq!(back.satellite.bind, "0.0.0.0:10700");
        assert!(back.satellite.allowed.is_empty());
        assert_eq!(back.timers.bell_repeats, 6);
        assert_eq!(back.screen.idle_brightness, 600);
        assert_eq!(back.screen.active_brightness, 4095);
    }

    #[test]
    fn weather_needs_url_and_entity() {
        let d = tmp("weather");
        let (c, _) = load(Some(&write(&d, "version = 1\n")), false).unwrap();
        assert!(c.weather_config(std::sync::Arc::new(c.ha_auth())).is_none());
        let p = write(
            &d,
            "version = 1\n[home_assistant]\nurl = \"http://192.0.2.10:8123\"\n[weather]\nentity = \"weather.home\"\n",
        );
        let (c, _) = load(Some(&p), false).unwrap();
        let w = c.weather_config(std::sync::Arc::new(c.ha_auth())).unwrap();
        assert!(!w.auth.has_token());
        assert_eq!(w.entity, "weather.home");
        let p = write(
            &d,
            "version = 1\n[home_assistant]\nurl = \"http://192.0.2.10:8123\"\n[weather]\nentity = \"\"\n",
        );
        let c = load(Some(&p), false).unwrap().0;
        assert!(c.weather_config(std::sync::Arc::new(c.ha_auth())).is_none());
        for bad in [
            "[home_assistant]\nurl = \"https://192.0.2.10:8123\"\n",
            "[weather]\nentity = \"sensor.outside\"\n",
        ] {
            let p = write(&d, &format!("version = 1\n{bad}"));
            assert!(load(Some(&p), false).is_err(), "{bad}");
        }
    }

    #[test]
    fn unknown_keys_are_errors() {
        let d = tmp("unknown");
        let p = write(&d, "version = 1\n[audio]\nvolumee = 0.5\n");
        let e = load(Some(&p), false).unwrap_err();
        assert!(e.contains("volumee"), "{e}");
        let p = write(&d, "colour = 1\n");
        assert!(load(Some(&p), false).is_err());
    }

    #[test]
    fn file_wins_over_simulator_overrides_and_paths_resolve() {
        let d = tmp("sim");
        let p = write(
            &d,
            "version = 1\n[audio]\nbackend = \"null\"\n[paths]\nstate_dir = \"state\"\n[satellite]\nallowed = [\"192.0.2.10\"]\n",
        );
        let (c, _) = load(Some(&p), true).unwrap();
        assert_eq!(c.audio.backend, "null");
        assert_eq!(c.audio.capture_channels, 1);
        assert_eq!(c.ui.renderer, "window");
        assert_eq!(c.screen.backlight, "");
        assert_eq!(
            c.satellite.allowed,
            vec!["192.0.2.10".to_string(), "127.0.0.1".to_string()]
        );
        assert_eq!(PathBuf::from(&c.paths.state_dir), d.join("state"));
        assert!(
            c.wake_word.model.ends_with("okay_alfred.json"),
            "{}",
            c.wake_word.model
        );
    }

    #[test]
    fn bad_values_and_versions() {
        let d = tmp("bad");
        for text in [
            "version = 2\n",
            "[audio]\nbackend = \"pulse\"\n",
            "[satellite]\nallowed = [\"not an address\"]\n",
            "[satellite]\nbind = \"nowhere\"\n",
            "[audio]\nvolume = 1.5\n",
            "[screen]\nawake_seconds = -1\n",
            "[screen]\nidle_brightness = -5\n",
        ] {
            let p = write(&d, text);
            assert!(load(Some(&p), false).is_err(), "{text}");
        }
    }

    #[test]
    fn missing_explicit_file_is_an_error() {
        assert!(load(Some(Path::new("/nonexistent/home-companion.toml")), false).is_err());
    }
}
