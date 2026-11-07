// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A character's reflex grammar (`grammar/<lang>.toml`), expanded to its phrases, and the
//! acceptance rule of design section 5: a Vosk result fires a reflex only when the whole text is
//! exactly one phrase of the grammar and every word's confidence reaches the threshold.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::slots;

/// What a reflex does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// "Che ore sono?"
    Time,
    /// A local timer of this many seconds.
    TimerStart {
        seconds: u64,
    },
    /// Cancel the timers.
    TimerCancel,
    /// How long until the next timer rings.
    TimerLeft,
    /// A one-time alarm at this time of day.
    AlarmSet {
        hour: u8,
        minute: u8,
    },
    /// Cancel the next alarm.
    AlarmCancel,
    VolumeUp,
    VolumeDown,
    VolumeSet {
        percent: u8,
    },
    /// Nothing to do: the conversation ends quietly ("silenzio", "lascia stare").
    Silence,
    /// The music playing through AirPlay: the sender pauses, plays, changes track.
    MusicPause,
    MusicPlay,
    MusicNext,
    MusicPrevious,
}

/// One word of a Vosk result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub word: String,
    pub conf: f32,
    #[serde(default)]
    pub start: f32,
    #[serde(default)]
    pub end: f32,
}

/// The rule's verdict on a result.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Accept(Command),
    /// Not a reflex: the utterance goes to the brain. The reason is for the logs.
    Reject(&'static str),
    /// Nothing was said.
    Empty,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    format: u32,
    language: String,
    commands: BTreeMap<String, Vec<String>>,
}

/// A language's reflexes: phrase → command.
#[derive(Debug, Clone)]
pub struct Grammar {
    pub language: String,
    phrases: BTreeMap<String, Command>,
}

impl Grammar {
    pub fn load(path: &Path) -> Result<Grammar, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Grammar::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Grammar, String> {
        let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
        if f.format != 1 {
            return Err(format!("format {} (expected 1)", f.format));
        }
        let slots = slots::for_language(&f.language)
            .ok_or_else(|| format!("no reflex vocabulary for language {:?}", f.language))?;
        let mut phrases = BTreeMap::new();
        let mut add = |phrase: String, c: Command| -> Result<(), String> {
            let phrase = normalize(&phrase);
            match phrases.insert(phrase.clone(), c) {
                Some(old) if old != c => Err(format!("{phrase:?} means both {old:?} and {c:?}")),
                _ => Ok(()),
            }
        };
        for (name, templates) in &f.commands {
            for t in templates {
                let needs = |slot: &str| -> Result<(), String> {
                    if t.contains(slot) {
                        Ok(())
                    } else {
                        Err(format!("{name}: {t:?} has no {slot}"))
                    }
                };
                let plain = |c: Command| -> Result<Vec<(String, Command)>, String> {
                    if t.contains('{') {
                        return Err(format!("{name}: {t:?} takes no slot"));
                    }
                    Ok(vec![(t.clone(), c)])
                };
                let expanded: Vec<(String, Command)> = match name.as_str() {
                    "time" => plain(Command::Time)?,
                    "timer_cancel" => plain(Command::TimerCancel)?,
                    "timer_left" => plain(Command::TimerLeft)?,
                    "alarm_cancel" => plain(Command::AlarmCancel)?,
                    "volume_up" => plain(Command::VolumeUp)?,
                    "volume_down" => plain(Command::VolumeDown)?,
                    "volume_max" => plain(Command::VolumeSet { percent: 100 })?,
                    "volume_min" => plain(Command::VolumeSet { percent: 5 })?,
                    "silence" => plain(Command::Silence)?,
                    "music_pause" => plain(Command::MusicPause)?,
                    "music_play" => plain(Command::MusicPlay)?,
                    "music_next" => plain(Command::MusicNext)?,
                    "music_previous" => plain(Command::MusicPrevious)?,
                    "timer_start" => {
                        needs("{duration}")?;
                        slots
                            .durations
                            .iter()
                            .map(|(s, v)| {
                                (
                                    t.replace("{duration}", s),
                                    Command::TimerStart { seconds: *v },
                                )
                            })
                            .collect()
                    }
                    "alarm_set" => {
                        needs("{time}")?;
                        slots
                            .times
                            .iter()
                            .map(|(s, (h, m))| {
                                (
                                    t.replace("{time}", s),
                                    Command::AlarmSet {
                                        hour: *h,
                                        minute: *m,
                                    },
                                )
                            })
                            .collect()
                    }
                    "volume_set" => {
                        needs("{percent}")?;
                        slots
                            .percents
                            .iter()
                            .map(|(s, p)| {
                                (
                                    t.replace("{percent}", s),
                                    Command::VolumeSet { percent: *p },
                                )
                            })
                            .collect()
                    }
                    other => return Err(format!("unknown command {other:?}")),
                };
                for (p, c) in expanded {
                    add(p, c)?;
                }
            }
        }
        if phrases.is_empty() {
            return Err("no phrases".into());
        }
        Ok(Grammar {
            language: f.language,
            phrases,
        })
    }

    pub fn len(&self) -> usize {
        self.phrases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.phrases.is_empty()
    }

    /// The command of a phrase exactly as Vosk writes it.
    pub fn command(&self, text: &str) -> Option<Command> {
        self.phrases.get(text).copied()
    }

    pub fn phrases(&self) -> impl Iterator<Item = &str> {
        self.phrases.keys().map(String::as_str)
    }

    /// Vosk's closed grammar: every phrase plus "[unk]" (what does not fit goes there instead of
    /// being forced into the nearest phrase).
    pub fn vosk_json(&self) -> String {
        let mut list: Vec<&str> = self.phrases().collect();
        list.push("[unk]");
        serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())
    }

    /// The rule: the whole text is one phrase, no `[unk]`, every word at least `threshold`.
    pub fn decide(&self, text: &str, words: &[Word], threshold: f32) -> Decision {
        let text = text.trim();
        if text.is_empty() && words.is_empty() {
            return Decision::Empty;
        }
        if text.contains("[unk]") || words.iter().any(|w| w.word == "[unk]") {
            return Decision::Reject("not in the grammar");
        }
        let Some(c) = self.command(text) else {
            return Decision::Reject("not one phrase");
        };
        if words.is_empty() {
            return Decision::Reject("no words");
        }
        if words.iter().any(|w| w.conf < threshold) {
            return Decision::Reject("unsure");
        }
        Decision::Accept(c)
    }
}

impl Grammar {
    /// [`Grammar::decide`], and the words must cover the speech the detector heard: Vosk drops
    /// out-of-grammar words around a phrase ("che ore sono a New York" → "che ore sono").
    pub fn decide_covered(
        &self,
        text: &str,
        words: &[Word],
        threshold: f32,
        vad: &crate::vad::Vad,
    ) -> Decision {
        let mut d = self.decide(text, words, threshold);
        let mut words = words;
        // Words that end inside the wake tail (the end of "Okay Alfred" heard as "e", "fred",
        // [unk]…) may be dropped when the rest is a phrase on its own.
        if !matches!(d, Decision::Accept(_)) {
            let lead = words
                .iter()
                .take_while(|w| w.end <= crate::vad::WAKE_TAIL_S)
                .count();
            for n in 1..=lead.min(2) {
                let rest = &words[n..];
                let rest_text = rest
                    .iter()
                    .map(|w| w.word.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let r = self.decide(&rest_text, rest, threshold);
                if matches!(r, Decision::Accept(_)) {
                    d = r;
                    words = rest;
                    break;
                }
            }
        }
        match d {
            Decision::Accept(c) => {
                let first = words.first().map_or(0.0, |w| w.start);
                let last = words.last().map_or(0.0, |w| w.end);
                if vad.covered_by(first, last, COVER_SLACK_S) {
                    Decision::Accept(c)
                } else {
                    Decision::Reject("speech outside the words")
                }
            }
            d => d,
        }
    }
}

/// How much speech may stick out of the words (breath, a trailing vowel) before it counts as
/// something else said.
pub const COVER_SLACK_S: f32 = 0.35;

/// The lowest word confidence (1.0 with no words).
pub fn min_conf(words: &[Word]) -> f32 {
    words.iter().map(|w| w.conf).fold(1.0, f32::min)
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IT: &str = r#"
format = 1
language = "it"
[commands]
time = ["che ore sono"]
timer_start = ["metti un timer di {duration}"]
timer_cancel = ["annulla il timer"]
alarm_set = ["metti la sveglia {time}"]
volume_set = ["volume al {percent}"]
volume_up = ["alza il volume"]
silence = ["silenzio"]
"#;

    fn w(word: &str, conf: f32) -> Word {
        Word {
            word: word.into(),
            conf,
            start: 0.0,
            end: 0.0,
        }
    }

    fn words(s: &str, conf: f32) -> Vec<Word> {
        s.split(' ').map(|x| w(x, conf)).collect()
    }

    #[test]
    fn expands_and_parses() {
        let g = Grammar::parse(IT).unwrap();
        assert_eq!(g.command("che ore sono"), Some(Command::Time));
        assert_eq!(
            g.command("metti un timer di dodici minuti e mezzo"),
            Some(Command::TimerStart { seconds: 750 })
        );
        assert_eq!(
            g.command("metti la sveglia alle sette e mezza"),
            Some(Command::AlarmSet {
                hour: 7,
                minute: 30
            })
        );
        assert_eq!(
            g.command("volume al cinquanta per cento"),
            Some(Command::VolumeSet { percent: 50 })
        );
        assert!(g.len() > 1000);
        assert!(g.vosk_json().ends_with("\"[unk]\"]"));
        assert!(g.vosk_json().contains("ventitré"));
    }

    #[test]
    fn the_rule() {
        let g = Grammar::parse(IT).unwrap();
        let t = "metti un timer di un minuto";
        assert_eq!(
            g.decide(t, &words(t, 1.0), 0.97),
            Decision::Accept(Command::TimerStart { seconds: 60 })
        );
        // One unsure word.
        let mut ws = words(t, 1.0);
        ws[3].conf = 0.9;
        assert_eq!(g.decide(t, &ws, 0.97), Decision::Reject("unsure"));
        // Part of a phrase, or [unk] in it.
        assert_eq!(
            g.decide("metti un timer", &words("metti un timer", 1.0), 0.97),
            Decision::Reject("not one phrase")
        );
        assert_eq!(
            g.decide(
                "[unk] che ore sono",
                &words("[unk] che ore sono", 1.0),
                0.97
            ),
            Decision::Reject("not in the grammar")
        );
        assert_eq!(g.decide("", &[], 0.97), Decision::Empty);
    }

    #[test]
    fn errors() {
        assert!(
            Grammar::parse("format = 1\nlanguage = \"xx\"\n[commands]\ntime = [\"x\"]").is_err()
        );
        assert!(
            Grammar::parse("format = 1\nlanguage = \"it\"\n[commands]\ntimer_start = [\"timer\"]")
                .is_err()
        );
        assert!(
            Grammar::parse("format = 1\nlanguage = \"it\"\n[commands]\ntime = [\"a {time}\"]")
                .is_err()
        );
        assert!(
            Grammar::parse("format = 1\nlanguage = \"it\"\n[commands]\nfly = [\"vola\"]").is_err()
        );
        let both =
            "format = 1\nlanguage = \"it\"\n[commands]\ntime = [\"ora\"]\nsilence = [\"ora\"]";
        assert!(Grammar::parse(both).unwrap_err().contains("both"));
    }

    #[test]
    fn min_confidence() {
        assert_eq!(min_conf(&[w("a", 0.5), w("b", 0.9)]), 0.5);
        assert_eq!(min_conf(&[]), 1.0);
    }

    #[test]
    fn the_wake_tail_before_a_phrase_is_dropped() {
        let g = Grammar::parse(include_str!("../../../characters/alfred/grammar/it.toml")).unwrap();
        let w = |word: &str, start: f32, end: f32| Word {
            word: word.into(),
            conf: 1.0,
            start,
            end,
        };
        let words = [
            w("e", 0.0, 0.12),
            w("che", 0.13, 0.3),
            w("ore", 0.3, 0.5),
            w("sono", 0.5, 0.9),
        ];
        let vad = crate::vad::Vad::new();
        assert!(matches!(
            g.decide_covered("e che ore sono", &words, 0.8, &vad),
            Decision::Accept(_)
        ));
        // A word after the wake tail is not dropped.
        let words = [
            w("e", 0.6, 0.7),
            w("che", 0.7, 0.9),
            w("ore", 0.9, 1.1),
            w("sono", 1.1, 1.4),
        ];
        assert!(!matches!(
            g.decide_covered("e che ore sono", &words, 0.8, &vad),
            Decision::Accept(_)
        ));
    }
}
