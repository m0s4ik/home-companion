// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! microWakeWord v2 model manifests (the JSON next to each .tflite, as in
//! esphome/micro-wake-word-models) and the model they name.

use crate::Error;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// A microWakeWord v2 manifest. Unknown fields (such as `"type": "micro"`) are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// The phrase, e.g. "okay alfred" (what Home Assistant shows for the model).
    pub wake_word: String,
    /// The .tflite file, relative to the manifest.
    pub model: String,
    /// Must be 2.
    pub version: u32,
    pub author: Option<String>,
    pub website: Option<String>,
    pub trained_languages: Option<Vec<String>>,
    pub micro: Micro,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Micro {
    pub probability_cutoff: f32,
    pub feature_step_size: u32,
    pub sliding_window_size: usize,
    /// As declared by the model's author; the detector grows it when it is too small.
    pub tensor_arena_size: usize,
    pub minimum_esphome_version: Option<String>,
}

impl Manifest {
    fn parse(path: &Path, json: &str) -> Result<Manifest, Error> {
        let bad = |why: String| Error::Manifest(path.to_path_buf(), why);
        let m: Manifest = serde_json::from_str(json).map_err(|e| bad(e.to_string()))?;
        if m.version != 2 {
            return Err(bad(format!(
                "version {} (only microWakeWord v2 manifests are supported)",
                m.version
            )));
        }
        if !(0.0..=1.0).contains(&m.micro.probability_cutoff) {
            return Err(bad(format!(
                "probability_cutoff {} is not in 0..=1",
                m.micro.probability_cutoff
            )));
        }
        if m.micro.feature_step_size == 0 || m.micro.feature_step_size > 1000 {
            return Err(bad(format!(
                "feature_step_size {} ms is not usable",
                m.micro.feature_step_size
            )));
        }
        if m.micro.sliding_window_size == 0 {
            return Err(bad("sliding_window_size is 0".into()));
        }
        if m.model.is_empty() {
            return Err(bad("model is empty".into()));
        }
        Ok(m)
    }
}

/// A loaded model: its manifest and the .tflite bytes.
pub struct Model {
    /// The manifest's file stem, e.g. "okay_alfred": the model id announced over Wyoming.
    pub name: String,
    pub manifest: Manifest,
    pub bytes: Vec<u8>,
    /// The .tflite file.
    pub path: PathBuf,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("name", &self.name)
            .field("manifest", &self.manifest)
            .field("bytes", &self.bytes.len())
            .field("path", &self.path)
            .finish()
    }
}

impl Model {
    /// Loads a v2 manifest and the .tflite it names, resolved next to the json.
    pub fn load(manifest_json: &Path) -> Result<Model, Error> {
        let text = std::fs::read_to_string(manifest_json)
            .map_err(|e| Error::Manifest(manifest_json.to_path_buf(), e.to_string()))?;
        let manifest = Manifest::parse(manifest_json, &text)?;
        let name = manifest_json
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Manifest(manifest_json.to_path_buf(), "no file name".into()))?;
        let dir = manifest_json.parent().unwrap_or(Path::new(""));
        let path = dir.join(&manifest.model);
        let bytes = std::fs::read(&path).map_err(|e| Error::Model(path.clone(), e.to_string()))?;
        if bytes.len() < 8 || &bytes[4..8] != b"TFL3" {
            return Err(Error::Model(
                path,
                "not a TensorFlow Lite model (no TFL3 identifier)".into(),
            ));
        }
        Ok(Model {
            name,
            manifest,
            bytes,
            path,
        })
    }

    /// The manifest's `wake_word`: what Home Assistant shows. The character's `wake_word` is what
    /// the screen shows.
    pub fn phrase(&self) -> &str {
        &self.manifest.wake_word
    }

    /// The manifest's trained languages (empty when not declared).
    pub fn languages(&self) -> Vec<String> {
        self.manifest.trained_languages.clone().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NABU: &str = r#"{"type":"micro","wake_word":"Okay Nabu","author":"x","website":"x",
        "model":"okay_nabu.tflite","trained_languages":["en"],"version":2,
        "micro":{"probability_cutoff":0.97,"feature_step_size":10,"sliding_window_size":5,
        "tensor_arena_size":26080,"minimum_esphome_version":"2024.7.0"}}"#;

    #[test]
    fn parses_v2() {
        let m = Manifest::parse(Path::new("x.json"), NABU).unwrap();
        assert_eq!(m.wake_word, "Okay Nabu");
        assert_eq!(m.model, "okay_nabu.tflite");
        assert!((m.micro.probability_cutoff - 0.97).abs() < 1e-6);
        assert_eq!(m.micro.feature_step_size, 10);
        assert_eq!(m.micro.sliding_window_size, 5);
        assert_eq!(m.micro.tensor_arena_size, 26080);
        assert_eq!(
            m.trained_languages.as_deref(),
            Some(&["en".to_string()][..])
        );
        assert_eq!(m.micro.minimum_esphome_version.as_deref(), Some("2024.7.0"));
    }

    #[test]
    fn rejects_v1() {
        let e = Manifest::parse(
            Path::new("v1.json"),
            r#"{"wake_word":"a","model":"a","version":1,"micro":{"probability_cutoff":0.5,
            "feature_step_size":20,"sliding_window_size":10,"tensor_arena_size":1}}"#,
        )
        .unwrap_err();
        assert!(
            matches!(&e, Error::Manifest(p, why) if p == Path::new("v1.json") && why.contains("version 1")),
            "{e}"
        );
    }

    #[test]
    fn rejects_garbage_and_zero_window() {
        assert!(Manifest::parse(Path::new("x"), "not json").is_err());
        let zero = NABU.replace("\"sliding_window_size\":5", "\"sliding_window_size\":0");
        assert!(Manifest::parse(Path::new("x"), &zero).is_err());
    }

    #[test]
    fn model_resolved_next_to_the_json_and_named_after_it() {
        let d = std::env::temp_dir().join(format!("wakeword-manifest-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let json = d.join("sub/my_word.json");
        std::fs::write(&json, NABU).unwrap();
        // Missing .tflite: a Model error naming the resolved path.
        match Model::load(&json) {
            Err(Error::Model(p, _)) => assert_eq!(p, d.join("sub/okay_nabu.tflite")),
            other => panic!("{other:?}"),
        }
        let mut fake = vec![0u8; 16];
        fake[4..8].copy_from_slice(b"TFL3");
        std::fs::write(d.join("sub/okay_nabu.tflite"), &fake).unwrap();
        let m = Model::load(&json).unwrap();
        assert_eq!(m.name, "my_word");
        assert_eq!(m.path, d.join("sub/okay_nabu.tflite"));
        assert_eq!(m.phrase(), "Okay Nabu");
        assert_eq!(m.languages(), vec!["en".to_string()]);
        std::fs::write(d.join("sub/okay_nabu.tflite"), b"garbage!garbage!").unwrap();
        assert!(matches!(Model::load(&json), Err(Error::Model(..))));
    }
}
