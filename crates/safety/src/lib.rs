// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Which Home Assistant entities may be controlled, and which only after a yes. One table for
//! the brain (house commands by voice, free text or entity ids) and the body (the screen's
//! tiles): domains that are never controlled (default: alarms and locks, and covers of the
//! classes `garage`, `gate`, `door`) are refused; sensitive ones (default: covers and valves)
//! need a yes first. Pure, no I/O.

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
        "lock" => &[
            "serratur",
            "lucchett",
            "chiavistell",
            "lock",
            "unlock",
            "verriegel",
            "entriegel",
        ],
        // Beginnings only where no common word begins so: a short word ("tor", "tür",
        // "deur", "hek") is matched whole ([`DOOR_WORDS`]), its compounds here, so that
        // "motor", "kleiderschrank" or "deurbel" stay what they are.
        "door" => &[
            "garage",
            "cancell",
            "portone",
            "portoncin",
            "basculant",
            "serrand",
            "porta",
            "door",
            "gate",
            // de
            "garagentor",
            "hoftor",
            "gartentor",
            "einfahrtstor",
            "rolltor",
            "schiebetor",
            "toröffn",
            "toroeffn",
            "pforte",
            "gartenpfort",
            "schranke",
            "haustür",
            "haustuer",
            "eingangstür",
            "eingangstuer",
            "türöffn",
            "tueroeffn",
            "torantrieb",
            "torsteuer",
            "toreinfahrt",
            "türsummer",
            "tuersummer",
            // nl
            "tuinpoort",
            "garagedeur",
            "voordeur",
            "achterdeur",
            "deuropen",
            "tuinhek",
            "toegangshek",
            "slagboom",
        ],
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
            // de
            "rollladen",
            "rolladen",
            "jalousie",
            "markise",
            "garagentor",
            "hoftor",
            "gartentor",
            "einfahrtstor",
            "rolltor",
            "schiebetor",
            "schranke",
            // nl
            "rolluik",
            "zonnescherm",
            "tuinpoort",
            "slagboom",
        ],
        _ => &[],
    }
}

/// Door and gate words matched whole (too short to be beginnings): it, en, de, nl.
const DOOR_WORDS: &[&str] = &[
    "porta", "porte", "door", "doors", "tor", "tore", "tür", "türe", "türen", "tuer", "deur",
    "deuren", "hek", "hekken", "poort", "poorten",
];

/// German and Dutch put the door or the gate at the end of a compound (Kellertür, Eingangstor,
/// Buitendeur, Schuifpoort): words ending so are doors, but for the few common words that end
/// the same ([`NOT_DOOR_ENDINGS`]).
const DOOR_ENDINGS: &[&str] = &["tür", "tuer", "deur", "poort", "hek", "tor"];
/// Motor, Monitor, Ventilator, Rotor, Detector, Transistor, Pastor; Bibliothek; Ambassadeur.
/// ("Kontor", an office, and "Mentor" still read as a gate: rare in a home, on the safe side;
/// "-entor" and "-stor" are Tiefgaragentor's and Eingangstor's endings too.)
const NOT_DOOR_ENDINGS: &[&str] = &[
    "ator", "itor", "otor", "ktor", "ctor", "utor", "ptor", "istor", "astor", "thek", "adeur",
];

/// A word of a door or a gate, in any of the languages.
fn door_word(w: &str) -> bool {
    DOOR_WORDS.contains(&w)
        || domain_words("door")
            .iter()
            .filter(|p| **p != "porta" && **p != "door")
            .any(|p| w.starts_with(p))
        || (DOOR_ENDINGS.iter().any(|e| w.ends_with(e))
            && !NOT_DOOR_ENDINGS.iter().any(|e| w.ends_with(e)))
}

/// A lock's word that is no beginning: «Schloss», «Türschloss», «slot», «deurslot», «nachtslot»
/// (endings), «Riegel» (whole). A castle called «Schloss» reads as a lock: the safe side.
fn lock_word(w: &str) -> bool {
    w == "riegel" || w.ends_with("schloss") || w.ends_with("slot")
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
        let named_door = words.iter().any(|w| door_word(w));
        match domain {
            "cover" if self.never_cover_classes.iter().any(|c| c == device_class) => Gate::Never,
            "cover" if device_class.is_empty() && named_door => Gate::Never,
            "switch" | "button" | "input_boolean" | "input_button" | "script" | "scene"
            | "automation" => match self.classify(&words.join(" ")) {
                Gate::Never => Gate::Never,
                _ if named_door => Gate::Never,
                g => g,
            },
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
        let locks = self.never_domains.iter().any(|d| d == "lock");
        if hit(&mut never.into_iter()) || (locks && words.iter().any(|w| lock_word(w))) {
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

    /// A domain whose actions need a yes by default (covers, valves).
    pub fn confirm_domain(&self, domain: &str) -> bool {
        self.confirm_domains.iter().any(|d| d == domain)
    }
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
    fn entity_gates_from_standard_labels() {
        let c = SafetyCfg::default();
        assert_eq!(c.entity_gate("lock.front", "", "Front door"), Gate::Never);
        assert_eq!(
            c.entity_gate("alarm_control_panel.home", "", "Allarme"),
            Gate::Never
        );
        assert_eq!(
            c.entity_gate("cover.garage", "garage", "Basculante"),
            Gate::Never
        );
        assert_eq!(
            c.entity_gate("cover.ingresso", "gate", "Ingresso"),
            Gate::Never
        );
        assert_eq!(c.entity_gate("cover.x", "", "Cancello"), Gate::Never);
        assert_eq!(
            c.entity_gate("cover.soggiorno", "shutter", "Tapparella"),
            Gate::Confirm
        );
        assert_eq!(c.entity_gate("cover.x", "", "Tenda"), Gate::Confirm);
        assert_eq!(
            c.entity_gate("valve.acqua", "water", "Acqua"),
            Gate::Confirm
        );
        assert_eq!(c.entity_gate("switch.relay_3", "", "Garage"), Gate::Never);
        assert_eq!(
            c.entity_gate("switch.shelly", "", "Apri portone"),
            Gate::Never
        );
        assert_eq!(
            c.entity_gate("switch.tapparelle", "", "Tapparelle"),
            Gate::Confirm
        );
        assert_eq!(
            c.entity_gate("switch.p1", "", "Pulsante Serranda Switch 1"),
            Gate::Never
        );
        assert_eq!(
            c.entity_gate("switch.portatile", "", "Portatile"),
            Gate::Allow
        );
        assert_eq!(
            c.entity_gate("light.garage", "", "Luce garage"),
            Gate::Allow
        );
        assert_eq!(c.entity_gate("light.cucina", "", "Cucina"), Gate::Allow);
        assert_eq!(c.entity_gate("climate.casa", "", "Termostato"), Gate::Allow);
    }

    #[test]
    fn german_and_dutch_doors_and_gates() {
        let c = SafetyCfg::default();
        for name in [
            "Tor",
            "Garagentor",
            "Garage links",
            "Hoftor",
            "Einfahrtstor",
            "Rolltor",
            "Tür",
            "Haustür",
            "Haustuer",
            "Türöffner",
            "Pforte",
            "Gartenpforte",
            "Schranke",
            "Toröffner",
            "Poort",
            "Tuinpoort",
            "Garagedeur",
            "Deur",
            "Voordeur",
            "Hek",
            "Tuinhek",
            "Slagboom",
        ] {
            assert_eq!(
                c.entity_gate("switch.relay", "", name),
                Gate::Never,
                "{name}"
            );
            assert_eq!(c.entity_gate("cover.x", "", name), Gate::Never, "{name}");
            assert_eq!(c.entity_gate("scene.x", "", name), Gate::Never, "{name}");
        }
        for name in ["Rollladen", "Jalousie", "Markise", "Rolluik", "Zonnescherm"] {
            assert_eq!(
                c.entity_gate("switch.relay", "", name),
                Gate::Confirm,
                "{name}"
            );
        }
        // Words that only look like them.
        for name in [
            "Motor",
            "Ventilator",
            "Monitor",
            "Torte",
            "Kleiderschrank",
            "Deurbel",
            "Heks",
            "Poortwachter lamp",
            "Tortuga",
        ] {
            let g = c.entity_gate("switch.relay", "", name);
            assert_ne!(g, Gate::Never, "{name}");
        }
        assert_eq!(c.entity_gate("switch.motor", "", "Motor"), Gate::Allow);
        // The door or the gate at the end of a compound, and the beginnings of their drives.
        for name in [
            "Tiefgaragentor",
            "Carporttor",
            "Eingangstor",
            "Zufahrtstor",
            "Seitentor",
            "Gartentür",
            "Kellertür",
            "Terrassentür",
            "Schiebetür",
            "Nebentuer",
            "Torantrieb",
            "Torsteuerung",
            "Türsummer",
            "Schuifpoort",
            "Oprijpoort",
            "Buitendeur",
            "Schuurdeur",
            "Tuindeur",
            "Kontor",
        ] {
            assert_eq!(
                c.entity_gate("switch.relay", "", name),
                Gate::Never,
                "{name}"
            );
        }
        for name in [
            "Motor",
            "Ventilator",
            "Monitor",
            "Rotor",
            "Traktor",
            "Projektor",
            "Detector",
            "Transistor",
            "Pastor",
            "Bibliothek",
            "Ambassadeur",
            "Deurbel",
            "Heks",
            "Torte",
        ] {
            assert_ne!(
                c.entity_gate("switch.relay", "", name),
                Gate::Never,
                "{name}"
            );
        }
        // German and Dutch locks.
        for name in [
            "Schloss",
            "Türschloss",
            "Riegel",
            "Verriegelung",
            "Entriegeln",
            "Slot",
            "Deurslot",
            "Nachtslot",
            "Voordeurslot",
        ] {
            assert_eq!(
                c.entity_gate("switch.relay", "", name),
                Gate::Never,
                "{name}"
            );
        }
        for name in ["Schokoriegel", "Schlosser", "Slotting", "Anschluss"] {
            assert_ne!(
                c.entity_gate("switch.relay", "", name),
                Gate::Never,
                "{name}"
            );
        }
        assert_eq!(c.entity_gate("light.tor", "", "Licht Tor"), Gate::Allow);
    }

    #[test]
    fn scenes_and_automations_are_read_by_name() {
        let c = SafetyCfg::default();
        assert_eq!(c.entity_gate("scene.open_gate", "", ""), Gate::Never);
        assert_eq!(
            c.entity_gate("automation.x", "", "Apri cancello"),
            Gate::Never
        );
        assert_eq!(
            c.entity_gate("scene.x", "", "Tapparelle giù"),
            Gate::Confirm
        );
        assert_eq!(c.entity_gate("scene.movie", "", "Movie"), Gate::Allow);
        assert_eq!(
            c.entity_gate("automation.lights", "", "Lights at dusk"),
            Gate::Allow
        );
    }

    #[test]
    fn confirm_domains_by_default() {
        let c = SafetyCfg::default();
        assert!(c.confirm_domain("cover") && c.confirm_domain("valve"));
        assert!(!c.confirm_domain("light"));
    }
}
