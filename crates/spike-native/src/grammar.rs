// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The reflex rule of design section 5: a Vosk grammar result is accepted only when the whole text
//! is exactly one grammar phrase (never "[unk]") and every word's confidence reaches the threshold.

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Word {
    pub word: String,
    pub conf: f32,
    pub start: f32,
    pub end: f32,
}

pub fn accepted(text: &str, words: &[Word], phrases: &[String], min_conf: f32) -> bool {
    !text.is_empty()
        && text != "[unk]"
        && phrases.iter().any(|p| p == text)
        && !words.is_empty()
        && words.iter().all(|w| w.conf >= min_conf)
}

/// The lowest word confidence (1.0 when there are no words).
pub fn min_conf(words: &[Word]) -> f32 {
    words.iter().map(|w| w.conf).fold(1.0, f32::min)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(word: &str, conf: f32) -> Word {
        Word { word: word.into(), conf, start: 0.0, end: 0.0 }
    }

    fn phrases() -> Vec<String> {
        ["stop", "alza il volume", "che ore sono", "[unk]"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_phrase_with_confident_words() {
        let ws = [w("alza", 0.95), w("il", 1.0), w("volume", 0.9)];
        assert!(accepted("alza il volume", &ws, &phrases(), 0.8));
    }

    #[test]
    fn one_unsure_word_rejects() {
        let ws = [w("che", 0.95), w("ore", 0.5), w("sono", 0.99)];
        assert!(!accepted("che ore sono", &ws, &phrases(), 0.8));
        assert!((min_conf(&ws) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn partial_or_extra_words_reject() {
        assert!(!accepted("alza il", &[w("alza", 1.0), w("il", 1.0)], &phrases(), 0.8));
        let ws = [w("stop", 1.0), w("stop", 1.0)];
        assert!(!accepted("stop stop", &ws, &phrases(), 0.8));
    }

    #[test]
    fn unk_and_empty_reject() {
        assert!(!accepted("[unk]", &[w("[unk]", 1.0)], &phrases(), 0.8));
        assert!(!accepted("", &[], &phrases(), 0.8));
    }
}
