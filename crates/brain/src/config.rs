// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `/etc/home-companion/brain.toml`: every key has a default, `home-companion-brain --print-config` shows them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The MCP tool server: where Home Assistant's MCP client connects (`http://<device>:10701/mcp`).
    pub listen: String,
    /// Addresses allowed to connect (Home Assistant's); empty = anyone (tests only).
    pub allow: Vec<String>,
    /// The character: its name and `persona.<lang>.md` (optional).
    pub character_dir: String,
    /// Language when Home Assistant does not say ("it", "en"); the language chosen on the body's
    /// screen (handed over with `ha.token`) wins.
    pub language: String,
    /// Home Assistant's AI: the memory jobs think through its AI task.
    pub ai: AiCfg,
    pub home_assistant: HaCfg,
    pub memory: MemoryCfg,
    pub safety: crate::safety::SafetyCfg,
    /// The body's control socket (the "Ricordi" page, reminders): a Unix socket, group
    /// `home-companion-brain` may connect; "" = none.
    pub control_socket: String,
    pub jobs: JobsCfg,
    pub briefing: BriefingCfg,
    pub lists: ListsCfg,
}

/// Lists in Home Assistant the brain may change by voice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListsCfg {
    /// The shopping list: a `todo.*` entity (Home Assistant's own, Bring!, …); "" = no list
    /// tools. The same key as in home-companion.toml (the "Spesa" card and page).
    pub shopping: String,
}

/// Reminders ("ricordamelo alle 18"): a card on the screen first, then the voice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct JobsCfg {
    /// Unacknowledged this long, a `card_then_voice` reminder is spoken.
    pub voice_after_seconds: u32,
    /// The satellite Home Assistant speaks through (`assist_satellite.announce`); "" = cards only.
    pub satellite_entity: String,
    /// Quiet hours ("23:00"-"07:00"): only the card; the voice waits for the end.
    pub quiet_start: String,
    pub quiet_end: String,
    /// At most this many pending jobs.
    pub max_active: usize,
}

/// The morning summary: a card at `at` ("07:30"; "" = off).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BriefingCfg {
    pub at: String,
    /// A `weather.*` entity read through Home Assistant ("" = none).
    pub weather_entity: String,
    /// `calendar.*` entities whose events of the day are listed.
    pub calendars: Vec<String>,
}

impl Default for JobsCfg {
    fn default() -> Self {
        JobsCfg {
            voice_after_seconds: 120,
            satellite_entity: String::new(),
            quiet_start: "23:00".into(),
            quiet_end: "07:00".into(),
            max_active: 20,
        }
    }
}

/// "07:30" → minutes after midnight.
pub fn hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AiCfg {
    /// The AI task entity for the memory jobs; "" = the one chosen in Home Assistant's
    /// settings, else the first `ai_task.*` there is.
    pub task_entity: String,
    /// One memory job's answer, at most.
    pub timeout_seconds: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HaCfg {
    /// "http://homeassistant.example.home:8123"; "" = no house tools.
    pub url: String,
    /// A long-lived access token, used only while the body has handed over none of its own
    /// (`ha.token` on the control socket, sent by home-companion every few minutes).
    pub token_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MemoryCfg {
    /// The SQLite database (`brain.db`).
    pub dir: String,
    /// Unused since 0.3 (the profile and notes blocks became facts); still read so an older
    /// brain.toml loads.
    pub profile_max_chars: usize,
    /// Unused since 0.3, as `profile_max_chars`.
    pub notes_max_chars: usize,
    /// What the agent always knows (the facts in its instructions), at most (200-4000; ≈ 4
    /// characters a token). The rest is found with `recall`.
    pub context_max_chars: usize,
    /// A fact heard once (a candidate) goes after this many days untouched.
    pub candidate_days: u32,
    /// At most this many facts alive; over it the oldest inferred ones of least importance go.
    pub max_facts: usize,
    /// Raw exchanges are kept this long; session summaries and facts stay.
    pub keep_days: u32,
    /// A pause this long closes a conversation (its summary and facts are extracted then).
    pub session_gap_minutes: u32,
    /// Exchanges of the current conversation sent with each request.
    pub recent_turns: usize,
    /// Facts recalled into each request.
    pub recall_facts: usize,
    /// The nightly tidy-up ("03:30", local time; "" = off): merges, updates and retires
    /// facts, with Home Assistant's AI task.
    pub consolidate_at: String,
    /// At most this many facts are shown to the model in one tidy-up (the oldest first).
    pub consolidate_max_facts: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "0.0.0.0:10701".into(),
            allow: Vec::new(),
            character_dir: "/opt/home-companion/characters/alfred".into(),
            language: "it".into(),
            ai: AiCfg::default(),
            home_assistant: HaCfg::default(),
            memory: MemoryCfg::default(),
            safety: crate::safety::SafetyCfg::default(),
            control_socket: "/run/home-companion-brain/brain.sock".into(),
            jobs: JobsCfg::default(),
            briefing: BriefingCfg::default(),
            lists: ListsCfg::default(),
        }
    }
}

impl Default for AiCfg {
    fn default() -> Self {
        AiCfg {
            task_entity: String::new(),
            timeout_seconds: 60.0,
        }
    }
}

impl Default for HaCfg {
    fn default() -> Self {
        HaCfg {
            url: String::new(),
            token_file: "/etc/home-companion/brain-ha-token".into(),
        }
    }
}

impl MemoryCfg {
    /// `context_max_chars` within 200..=4000.
    pub fn context_chars(&self) -> usize {
        self.context_max_chars.clamp(200, 4000)
    }
}

impl Default for MemoryCfg {
    fn default() -> Self {
        MemoryCfg {
            dir: "/var/lib/home-companion-brain".into(),
            profile_max_chars: 1400,
            notes_max_chars: 2200,
            context_max_chars: 1500,
            candidate_days: 30,
            max_facts: 200,
            keep_days: 30,
            session_gap_minutes: 10,
            recent_turns: 10,
            recall_facts: 6,
            consolidate_at: "03:30".into(),
            consolidate_max_facts: 150,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let c: Config =
                    toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
                c.validate()?;
                Ok(c)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::warn!("{} not found: defaults used", path.display());
                Ok(Config::default())
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(1.0..=300.0).contains(&self.ai.timeout_seconds) {
            return Err("ai.timeout_seconds: 1-300".into());
        }
        for (k, v) in [
            ("memory.consolidate_at", &self.memory.consolidate_at),
            ("briefing.at", &self.briefing.at),
        ] {
            if !v.is_empty() && hm(v).is_none() {
                return Err(format!("{k} {v:?}: \"HH:MM\" or \"\""));
            }
        }
        for (k, v) in [
            ("jobs.quiet_start", &self.jobs.quiet_start),
            ("jobs.quiet_end", &self.jobs.quiet_end),
        ] {
            if hm(v).is_none() {
                return Err(format!("{k} {v:?}: \"HH:MM\""));
            }
        }
        Ok(())
    }

    pub fn memory_dir(&self) -> PathBuf {
        PathBuf::from(&self.memory.dir)
    }
}

/// The first non-empty line of a secret file, never logged.
pub fn read_secret(path: &str) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_valid_and_round_trip() {
        let c = Config::default();
        c.validate().unwrap();
        let s = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.ai.task_entity, c.ai.task_entity);
        let bad: Config = toml::from_str("[ai]\ntimeout_seconds = 0.0").unwrap();
        assert!(bad.validate().is_err());
        // The key-on-the-device way is gone (branch standby/brain-direct-llm): refused, not ignored.
        assert!(toml::from_str::<Config>("[llm]\nprovider = \"gemini\"").is_err());
    }
}
