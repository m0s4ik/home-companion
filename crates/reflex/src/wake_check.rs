// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The second check of a wake word detection: the small wake word model also fires on ordinary
//! speech in the room, so the audio that woke the character is decoded once more by Vosk against
//! a grammar made of the wake phrase alone (the whole phrase, the name, anything else as `[unk]`).
//! The detection stands when the name is heard.
//!
//! Measured on the ThinkSmart View's recordings (16/11/2026, "okay alfred", the last 2 s before
//! the detection, vosk-model-small-it): 44 of 44 real wakes kept, 45 of 50 false ones dropped;
//! the English model gave the same.

/// Seconds of audio before the detection that are checked (1.5-2.5 s gave the same results;
/// 1 s lost real wakes, 3.5 s let more false ones through).
pub const AUDIO_S: f32 = 2.0;

/// The phrase as the grammar has it: lower case, single spaces.
pub fn normalize(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The name: the phrase's last word ("alfred" in "okay alfred").
pub fn name(phrase: &str) -> String {
    normalize(phrase)
        .rsplit(' ')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Vosk's grammar for the check: the whole phrase, the name alone, anything else.
pub fn grammar_json(phrase: &str) -> String {
    let (p, n) = (normalize(phrase), name(phrase));
    let mut list = vec![p.clone()];
    if n != p {
        list.push(n);
    }
    list.push("[unk]".into());
    serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())
}

/// Every word of the phrase is in the model's vocabulary (`in_vocabulary`); otherwise Vosk drops
/// it from the grammar and the check would refuse every wake.
pub fn known(phrase: &str, in_vocabulary: impl Fn(&str) -> bool) -> bool {
    let p = normalize(phrase);
    !p.is_empty() && p.split(' ').all(in_vocabulary)
}

/// The check's text has the name in it.
pub fn heard_name(phrase: &str, text: &str) -> bool {
    let n = name(phrase);
    !n.is_empty() && text.split_whitespace().any(|w| w.eq_ignore_ascii_case(&n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_has_the_phrase_the_name_and_the_rest() {
        assert_eq!(
            grammar_json("Okay  Alfred"),
            r#"["okay alfred","alfred","[unk]"]"#
        );
        assert_eq!(grammar_json("alfred"), r#"["alfred","[unk]"]"#);
    }

    #[test]
    fn the_name_is_the_last_word() {
        assert_eq!(name("okay alfred"), "alfred");
        assert_eq!(name("hey  Jarvis "), "jarvis");
        assert_eq!(name(""), "");
    }

    #[test]
    fn heard_only_with_the_name() {
        assert!(heard_name("okay alfred", "okay alfred"));
        assert!(heard_name("okay alfred", "alfred"));
        assert!(heard_name("okay alfred", "alfred okay alfred"));
        assert!(!heard_name("okay alfred", "okay"));
        assert!(!heard_name("okay alfred", ""));
        assert!(!heard_name("okay alfred", "alfredo"));
        assert!(!heard_name("", "alfred"));
    }

    #[test]
    fn known_needs_every_word() {
        let vocab = |w: &str| ["okay", "alfred"].contains(&w);
        assert!(known("Okay Alfred", vocab));
        assert!(!known("hey alfred", vocab));
        assert!(!known("", vocab));
    }
}
