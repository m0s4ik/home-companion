// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What may reach the model and what the model may do.
//!
//! - **External text** (web search, Home Assistant's answers and states) is cleaned before the model
//!   sees it: Unicode TAG characters (U+E0000-U+E007F) and invisible format characters, used to hide
//!   instructions, are removed, control characters too, and the length is capped.
//! - **House commands** go through a gate (OpenDots' read-only/mutating split): domains that are
//!   never controlled by voice (default: alarms and locks) are refused; sensitive ones (default:
//!   covers: shutters, garage doors, gates) need a spoken "Confermi?" and an explicit yes in the
//!   next turn, executed by the brain itself, not by the model. The command is free text, so the
//!   domains are recognised by their words (Italian and English).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SafetyCfg {
    /// Never controlled by voice.
    pub never_domains: Vec<String>,
    /// Done only after a spoken confirmation.
    pub confirm_domains: Vec<String>,
    /// More words that need a confirmation (beginnings of words, lower case).
    pub confirm_words: Vec<String>,
    /// More words that are never allowed.
    pub never_words: Vec<String>,
    /// `cover` device classes never controlled by voice (someone shouting from outside could
    /// open them): Home Assistant's standard classes, the same in every installation.
    pub never_cover_classes: Vec<String>,
}

impl Default for SafetyCfg {
    fn default() -> Self {
        SafetyCfg {
            never_domains: vec!["alarm_control_panel".into(), "lock".into()],
            confirm_domains: vec!["cover".into(), "valve".into()],
            confirm_words: Vec::new(),
            never_words: Vec::new(),
            never_cover_classes: vec!["garage".into(), "gate".into(), "door".into()],
        }
    }
}

/// The words of a domain, as beginnings of words.
fn domain_words(domain: &str) -> &'static [&'static str] {
    match domain {
        "alarm_control_panel" => &["allarm", "antifurt", "alarm", "disarm"],
        "lock" => &["serratur", "lucchett", "chiavistell", "lock", "unlock"],
        "door" => &["garage", "cancell", "portone", "portoncin", "basculant", "serrand", "porta", "door", "gate"],
        "cover" => &[
            "tapparell",
            "persian",
            "serrand",
            "tende",
            "tenda",
            "cancell",
            "garage",
            "portone",
            "basculant",
            "shutter",
            "blind",
            "gate",
            "awning",
        ],
        _ => &[],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Allow,
    Confirm,
    Never,
}

impl SafetyCfg {
    /// The gate of one entity, from Home Assistant's standard labels (domain, device class),
    /// the same in every installation. Names count only for covers and for the generic
    /// on/off things a door can hide behind (a garage door on a relay is a `switch` called
    /// "Garage"); a light in the garage is still a light.
    pub fn entity_gate(&self, entity_id: &str, device_class: &str, name: &str) -> Gate {
        let domain = entity_id.split('.').next().unwrap_or("");
        if self.never_domains.iter().any(|d| d == domain) {
            return Gate::Never;
        }
        let words: Vec<String> = format!("{name} {}", entity_id.replace(['.', '_'], " "))
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        let named_door = words.iter().any(|w| {
            matches!(w.as_str(), "porta" | "porte" | "door" | "doors")
                || domain_words("door")
                    .iter()
                    .filter(|p| **p != "porta" && **p != "door")
                    .any(|p| w.starts_with(p))
        });
        match domain {
            "cover" if self.never_cover_classes.iter().any(|c| c == device_class) => Gate::Never,
            "cover" if device_class.is_empty() && named_door => Gate::Never,
            "switch" | "button" | "input_boolean" | "input_button" | "script" => {
                match self.classify(&words.join(" ")) {
                    Gate::Never => Gate::Never,
                    _ if named_door => Gate::Never,
                    g => g,
                }
            }
            d if self.confirm_domains.iter().any(|c| c == d) => Gate::Confirm,
            _ => Gate::Allow,
        }
    }

    pub fn classify(&self, command: &str) -> Gate {
        let words: Vec<String> = command
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        let hit = |prefixes: &mut dyn Iterator<Item = &str>| {
            let p: Vec<&str> = prefixes.collect();
            words.iter().any(|w| p.iter().any(|p| w.starts_with(p)))
        };
        let never = self
            .never_domains
            .iter()
            .flat_map(|d| domain_words(d).iter().copied())
            .chain(self.never_words.iter().map(String::as_str));
        if hit(&mut never.into_iter()) {
            return Gate::Never;
        }
        let confirm = self
            .confirm_domains
            .iter()
            .flat_map(|d| domain_words(d).iter().copied())
            .chain(self.confirm_words.iter().map(String::as_str));
        if hit(&mut confirm.into_iter()) {
            return Gate::Confirm;
        }
        Gate::Allow
    }
}

/// An explicit yes / no to "Confermi?".
pub fn answer_to_confirm(text: &str) -> Option<bool> {
    let t = text
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_lowercase()
        .replace('ì', "i");
    let first = t
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| !w.is_empty())
        .unwrap_or("");
    const YES: &[&str] = &[
        "si", "confermo", "conferma", "certo", "ok", "okay", "yes", "confirm", "vai", "procedi",
    ];
    const NO: &[&str] = &[
        "no", "annulla", "lascia", "niente", "stop", "basta", "cancel",
    ];
    if YES.contains(&first) || t == "va bene" {
        Some(true)
    } else if NO.contains(&first) {
        Some(false)
    } else {
        None
    }
}

fn invisible(c: char) -> bool {
    matches!(c,
        '\u{E0000}'..='\u{E007F}'
        | '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{E0100}'..='\u{E01EF}')
        || (c.is_control() && c != '\n' && c != '\t')
}

/// External text for the model: invisible and control characters out, at most `max` characters.
pub fn sanitize(s: &str, max: usize) -> String {
    let clean: String = s.chars().filter(|c| !invisible(*c)).collect();
    if clean.chars().count() <= max {
        return clean;
    }
    let mut t: String = clean.chars().take(max).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_by_words() {
        let s = SafetyCfg::default();
        assert_eq!(s.classify("accendi la luce in cucina"), Gate::Allow);
        assert_eq!(
            s.classify("chiudi le tapparelle del soggiorno"),
            Gate::Confirm
        );
        assert_eq!(s.classify("apri il garage"), Gate::Confirm);
        assert_eq!(s.classify("disattiva l'allarme"), Gate::Never);
        assert_eq!(s.classify("unlock the front door"), Gate::Never);
        assert_eq!(s.classify("apri la serratura"), Gate::Never);
    }

    #[test]
    fn confirmations() {
        assert_eq!(answer_to_confirm("Sì."), Some(true));
        assert_eq!(answer_to_confirm("si confermo"), Some(true));
        assert_eq!(answer_to_confirm("va bene"), Some(true));
        assert_eq!(answer_to_confirm("no, lascia stare"), Some(false));
        assert_eq!(answer_to_confirm("che ore sono"), None);
        assert_eq!(answer_to_confirm("sicuro che piove?"), None);
    }

    #[test]
    fn hidden_characters_go() {
        let tagged = format!("ciao{}fine", "\u{E0049}\u{E0047}\u{E004E}");
        assert_eq!(sanitize(&tagged, 100), "ciaofine");
        assert_eq!(sanitize("a\u{200B}b\u{202E}c\u{0007}", 100), "abc");
        assert_eq!(sanitize("abcdef", 3), "abc…");
    }

    #[test]
    fn entity_gates_from_standard_labels() {
        let c = SafetyCfg::default();
        assert_eq!(c.entity_gate("lock.front", "", "Front door"), Gate::Never);
        assert_eq!(c.entity_gate("alarm_control_panel.home", "", "Allarme"), Gate::Never);
        assert_eq!(c.entity_gate("cover.garage", "garage", "Basculante"), Gate::Never);
        assert_eq!(c.entity_gate("cover.ingresso", "gate", "Ingresso"), Gate::Never);
        assert_eq!(c.entity_gate("cover.x", "", "Cancello"), Gate::Never);
        assert_eq!(c.entity_gate("cover.soggiorno", "shutter", "Tapparella"), Gate::Confirm);
        assert_eq!(c.entity_gate("cover.x", "", "Tenda"), Gate::Confirm);
        assert_eq!(c.entity_gate("valve.acqua", "water", "Acqua"), Gate::Confirm);
        assert_eq!(c.entity_gate("switch.relay_3", "", "Garage"), Gate::Never);
        assert_eq!(c.entity_gate("switch.shelly", "", "Apri portone"), Gate::Never);
        assert_eq!(c.entity_gate("switch.tapparelle", "", "Tapparelle"), Gate::Confirm);
        assert_eq!(c.entity_gate("switch.p1", "", "Pulsante Serranda Switch 1"), Gate::Never);
        assert_eq!(c.entity_gate("switch.portatile", "", "Portatile"), Gate::Allow);
        assert_eq!(c.entity_gate("light.garage", "", "Luce garage"), Gate::Allow);
        assert_eq!(c.entity_gate("light.cucina", "", "Cucina"), Gate::Allow);
        assert_eq!(c.entity_gate("climate.casa", "", "Termostato"), Gate::Allow);
    }
}
