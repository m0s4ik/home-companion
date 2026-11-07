// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The `info` answer to `describe`.
//!
//! Home Assistant creates a satellite device only when `info.satellite` is present and
//! `installed` (`homeassistant/components/wyoming/data.py`, `__init__.py`), and resolves the
//! `detection` name to a phrase through `info.wake[*].models[*]` (`_run_pipeline_loop`).
//! Every `Artifact` field (`name`, `attribution`, `installed`, `description`, `version`) is always
//! written, `null` when unknown, because wyoming's `DataClassJsonMixin.from_dict` does not
//! default missing fields.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Who made an artifact and where it comes from.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Attribution {
    pub name: String,
    pub url: String,
}

/// One wake word model.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeModelInfo {
    /// The model id, sent as `detection.name` (e.g. `okay_alfred`).
    pub name: String,
    /// The phrase Home Assistant shows (e.g. `okay alfred`).
    pub phrase: String,
    pub languages: Vec<String>,
    pub attribution: Attribution,
    pub description: Option<String>,
    pub version: Option<String>,
}

/// What the satellite declares about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct SatelliteInfo {
    /// Shown as the device name in Home Assistant.
    pub name: String,
    /// Suggested area.
    pub area: Option<String>,
    /// `None` = the name.
    pub description: Option<String>,
    /// The program's version, filled by the caller.
    pub version: String,
    /// Attribution of the satellite and of its wake word program.
    pub attribution: Attribution,
    pub has_vad: bool,
    /// Name of the wake word program in `info.wake`.
    pub wake_program_name: String,
    pub wake_models: Vec<WakeModelInfo>,
}

/// The `info` event's data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Info {
    /// Parsed `satellite` section (and wake models), when present.
    pub satellite: Option<SatelliteInfo>,
    /// The full `data` as received or sent.
    pub raw: Map<String, Value>,
}

const WAKE_DESCRIPTION: &str = "microWakeWord on the device";

impl Info {
    /// The exact JSON Home Assistant expects from a satellite with a local wake word.
    /// `s.description` is filled with the name when `None`, as written.
    pub fn for_satellite(mut s: SatelliteInfo) -> Info {
        if s.description.is_none() {
            s.description = Some(s.name.clone());
        }
        let models: Vec<Value> = s
            .wake_models
            .iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "attribution": m.attribution,
                    "installed": true,
                    "description": m.description,
                    "version": m.version,
                    "languages": m.languages,
                    "phrase": m.phrase,
                })
            })
            .collect();
        let wake: Vec<Value> = if models.is_empty() {
            Vec::new()
        } else {
            vec![json!({
                "name": s.wake_program_name,
                "attribution": s.attribution,
                "installed": true,
                "description": WAKE_DESCRIPTION,
                "version": s.version,
                "models": models,
            })]
        };
        let active: Vec<&str> = s.wake_models.iter().map(|m| m.name.as_str()).collect();
        let data = json!({
            "asr": [], "tts": [], "handle": [], "intent": [],
            "wake": wake,
            "mic": [], "snd": [],
            "satellite": {
                "name": s.name,
                "attribution": s.attribution,
                "installed": true,
                "description": s.description,
                "version": s.version,
                "area": s.area,
                "has_vad": s.has_vad,
                "active_wake_words": active,
                "max_active_wake_words": 1,
                "supports_trigger": null,
            },
        });
        let Value::Object(raw) = data else {
            unreachable!("json! object")
        };
        Info {
            satellite: Some(s),
            raw,
        }
    }

    /// The `data` of the `info` event.
    pub fn to_json(&self) -> Value {
        Value::Object(self.raw.clone())
    }

    /// Tolerant parser: missing fields take defaults, unknown fields stay in `raw`.
    pub fn from_json(v: &Value) -> Info {
        let raw = v.as_object().cloned().unwrap_or_default();
        let s =
            |o: &Map<String, Value>, k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
        let attribution = |o: &Map<String, Value>| {
            o.get("attribution")
                .and_then(|a| serde_json::from_value(a.clone()).ok())
                .unwrap_or_default()
        };
        let satellite = raw.get("satellite").and_then(Value::as_object).map(|sat| {
            let wake = raw.get("wake").and_then(Value::as_array);
            let first_program = wake.and_then(|w| w.first()).and_then(Value::as_object);
            let wake_models = wake
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
                .flat_map(|p| {
                    p.get("models")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter_map(Value::as_object)
                .map(|m| WakeModelInfo {
                    name: s(m, "name").unwrap_or_default(),
                    phrase: s(m, "phrase").unwrap_or_default(),
                    languages: m
                        .get("languages")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    attribution: attribution(m),
                    description: s(m, "description"),
                    version: s(m, "version"),
                })
                .collect();
            SatelliteInfo {
                name: s(sat, "name").unwrap_or_default(),
                area: s(sat, "area"),
                description: s(sat, "description"),
                version: s(sat, "version").unwrap_or_default(),
                attribution: attribution(sat),
                has_vad: sat.get("has_vad").and_then(Value::as_bool).unwrap_or(false),
                wake_program_name: first_program.and_then(|p| s(p, "name")).unwrap_or_default(),
                wake_models,
            }
        });
        Info { satellite, raw }
    }

    /// True when Home Assistant would create a satellite device from this info.
    pub fn satellite_installed(&self) -> bool {
        self.raw
            .get("satellite")
            .and_then(|s| s.get("installed"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Names of every wake model declared.
    pub fn wake_model_names(&self) -> Vec<String> {
        self.raw
            .get("wake")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|p| {
                p.get("models")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }
}

/// The satellite of `fixtures/info-expected.json` (tests).
#[doc(hidden)]
pub mod tests_support {
    use super::*;

    pub fn sample() -> SatelliteInfo {
        SatelliteInfo {
            name: "ThinkSmart View".into(),
            area: None,
            description: None,
            version: "0.1.0".into(),
            attribution: Attribution {
                name: "home-companion".into(),
                url: "https://github.com/m0s4ik/home-companion".into(),
            },
            has_vad: false,
            wake_program_name: "home-companion-wake".into(),
            wake_models: vec![WakeModelInfo {
                name: "okay_alfred".into(),
                phrase: "okay alfred".into(),
                languages: vec!["en".into()],
                attribution: Attribution {
                    name: "Tater Totterson".into(),
                    url: "https://github.com/TaterTotterson/Tater-Wake-Words".into(),
                },
                description: None,
                version: None,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::sample;
    use super::*;

    #[test]
    fn matches_fixture() {
        let info = Info::for_satellite(sample());
        let expected: Value =
            serde_json::from_str(include_str!("../fixtures/info-expected.json")).unwrap();
        let got = info.to_json();
        let (e, g) = (expected.as_object().unwrap(), got.as_object().unwrap());
        assert_eq!(e.keys().collect::<Vec<_>>(), g.keys().collect::<Vec<_>>());
        for (k, v) in e {
            assert_eq!(Some(v), g.get(k), "key {k}");
        }
        // every artifact field present
        let sat = g["satellite"].as_object().unwrap();
        let wake = g["wake"][0].as_object().unwrap();
        let model = g["wake"][0]["models"][0].as_object().unwrap();
        for o in [sat, wake, model] {
            for k in ["name", "attribution", "installed", "description", "version"] {
                assert!(o.contains_key(k), "{k} missing");
            }
        }
    }

    #[test]
    fn round_trip() {
        let info = Info::for_satellite(sample());
        let back = Info::from_json(&info.to_json());
        let mut s = sample();
        s.description = Some(s.name.clone());
        assert_eq!(back.satellite, Some(s));
        assert_eq!(back.raw, info.raw);
        assert!(back.satellite_installed());
        assert_eq!(back.wake_model_names(), vec!["okay_alfred".to_string()]);
    }

    #[test]
    fn tolerant() {
        let v = json!({"satellite": {"name": "x", "extra": 1}, "wake": [{"models": [{"name": "m"}]}], "foo": [1]});
        let info = Info::from_json(&v);
        let s = info.satellite.unwrap();
        assert_eq!(s.name, "x");
        assert_eq!(s.wake_models[0].name, "m");
        assert!(info.raw.contains_key("foo"));
        assert!(!Info::from_json(&json!(null)).satellite_installed());
    }
}
