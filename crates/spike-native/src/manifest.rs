// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! microWakeWord v2 model manifest (the JSON next to each .tflite in esphome/micro-wake-word-models).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub wake_word: String,
    pub model: String,
    pub version: u32,
    pub micro: Micro,
}

#[derive(Debug, Deserialize)]
pub struct Micro {
    pub probability_cutoff: f32,
    pub feature_step_size: u32,
    pub sliding_window_size: usize,
    pub tensor_arena_size: usize,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Self, String> {
        let m: Manifest = serde_json::from_str(json).map_err(|e| format!("manifest: {e}"))?;
        if m.version != 2 {
            return Err(format!("manifest: version {} (only v2 is supported)", m.version));
        }
        Ok(m)
    }

    pub fn load(path: &str) -> Result<Self, String> {
        Self::parse(&std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_okay_nabu_v2() {
        let m = Manifest::parse(
            r#"{"type":"micro","wake_word":"Okay Nabu","author":"x","website":"x",
            "model":"okay_nabu.tflite","trained_languages":["en"],"version":2,
            "micro":{"probability_cutoff":0.97,"feature_step_size":10,"sliding_window_size":5,
            "tensor_arena_size":26080,"minimum_esphome_version":"2024.7.0"}}"#,
        )
        .unwrap();
        assert_eq!(m.wake_word, "Okay Nabu");
        assert_eq!(m.model, "okay_nabu.tflite");
        assert!((m.micro.probability_cutoff - 0.97).abs() < 1e-6);
        assert_eq!(m.micro.feature_step_size, 10);
        assert_eq!(m.micro.sliding_window_size, 5);
        assert_eq!(m.micro.tensor_arena_size, 26080);
    }

    #[test]
    fn rejects_v1() {
        let e = Manifest::parse(
            r#"{"wake_word":"a","model":"a","version":1,"micro":{"probability_cutoff":0.5,
            "feature_step_size":20,"sliding_window_size":10,"tensor_arena_size":1}}"#,
        );
        assert!(e.is_err());
    }
}
