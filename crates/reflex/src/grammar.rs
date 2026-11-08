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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Command {
    /// "Che ore sono?"
    Time,
    /// A local timer of this many seconds, named after one of the grammar's `timer_names`
    /// ("timer pasta dieci minuti": "Pasta").
    TimerStart {
        seconds: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Cancel the timers (None) or the one with this name.
    TimerCancel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// How long until the next timer rings (None) or the one with this name.
    TimerLeft {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Every running timer with its time left ("che timer ho?").
    TimerList,
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
    /// The character's last spoken answer again ("ripeti", "cosa hai detto?").
    Repeat,
    /// The backlight off until a touch, a key or a wake word.
    ScreenOff,
    /// Do not disturb for this long; None: the default (the body decides by the hour).
    DoNotDisturb {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<u64>,
    },
    /// Do not disturb until the morning (07:00).
    DoNotDisturbMorning,
    DoNotDisturbOff,
    /// The screen off and do not disturb until the morning.
    GoodNight,
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
    /// Timer names: the label shown on the screen → its spoken forms, by the key a template
    /// names (`{name:the}` → `the = "la pasta"`).
    #[serde(default)]
    timer_names: BTreeMap<String, BTreeMap<String, String>>,
}

/// What the slots of one expanded phrase are worth.
#[derive(Debug, Clone, Default, PartialEq)]
struct Bound {
    duration: Option<u64>,
    time: Option<(u8, u8)>,
    percent: Option<u8>,
    name: Option<String>,
}

/// A template with every slot replaced by each of its spoken forms. Slots: `{duration}`,
/// `{whole_duration}`, `{time}`, `{percent}`, `{name:<key>}` (the timer names that have that
/// form; one name per template).
fn expand(
    t: &str,
    slots: &slots::Slots,
    names: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<Vec<(String, Bound)>, String> {
    let mut out = vec![(t.to_string(), Bound::default())];
    let mut cross = |slot: &str, values: Vec<(String, Bound)>| {
        if !t.contains(slot) {
            return;
        }
        out = out
            .iter()
            .flat_map(|(p, b)| {
                values.iter().map(move |(s, v)| {
                    let mut b = b.clone();
                    b.duration = v.duration.or(b.duration);
                    b.time = v.time.or(b.time);
                    b.percent = v.percent.or(b.percent);
                    b.name = v.name.clone().or(b.name);
                    (p.replace(slot, s), b)
                })
            })
            .collect();
    };
    let bind = |d: Option<u64>, t: Option<(u8, u8)>, p: Option<u8>| Bound {
        duration: d,
        time: t,
        percent: p,
        name: None,
    };
    if t.contains("{duration}") && t.contains("{whole_duration}") {
        return Err(format!("{t:?}: two durations"));
    }
    cross(
        "{duration}",
        slots
            .durations
            .iter()
            .map(|(s, v)| (s.clone(), bind(Some(*v), None, None)))
            .collect(),
    );
    cross(
        "{whole_duration}",
        slots
            .whole_durations
            .iter()
            .map(|(s, v)| (s.clone(), bind(Some(*v), None, None)))
            .collect(),
    );
    cross(
        "{time}",
        slots
            .times
            .iter()
            .map(|(s, v)| (s.clone(), bind(None, Some(*v), None)))
            .collect(),
    );
    cross(
        "{percent}",
        slots
            .percents
            .iter()
            .map(|(s, v)| (s.clone(), bind(None, None, Some(*v))))
            .collect(),
    );
    if let Some(i) = t.find("{name:") {
        let key = &t[i + 6..];
        let key = &key[..key
            .find('}')
            .ok_or_else(|| format!("{t:?}: unclosed {{name:"))?];
        let slot = format!("{{name:{key}}}");
        let values: Vec<(String, Bound)> = names
            .iter()
            .filter_map(|(label, forms)| {
                forms.get(key).map(|f| {
                    (
                        f.clone(),
                        Bound {
                            name: Some(label.clone()),
                            ..Bound::default()
                        },
                    )
                })
            })
            .collect();
        if values.is_empty() {
            return Err(format!("{t:?}: no timer name has the form {key:?}"));
        }
        cross(&slot, values);
    }
    if let Some((p, _)) = out.iter().find(|(p, _)| p.contains('{')) {
        return Err(format!("{t:?}: unknown slot in {p:?}"));
    }
    Ok(out)
}

/// The command a phrase of `name` means, from its slots; an error for a slot the command does
/// not take or a slot it needs and has not.
fn command(name: &str, b: Bound) -> Result<Command, String> {
    let named = |b: &Bound| b.duration.is_none() && b.time.is_none() && b.percent.is_none();
    let plain = |b: &Bound, c: Command| {
        if *b == Bound::default() {
            Ok(c)
        } else {
            Err("takes no slot".to_string())
        }
    };
    Ok(match name {
        "time" => plain(&b, Command::Time)?,
        "timer_list" => plain(&b, Command::TimerList)?,
        "alarm_cancel" => plain(&b, Command::AlarmCancel)?,
        "volume_up" => plain(&b, Command::VolumeUp)?,
        "volume_down" => plain(&b, Command::VolumeDown)?,
        "volume_max" => plain(&b, Command::VolumeSet { percent: 100 })?,
        "volume_min" => plain(&b, Command::VolumeSet { percent: 5 })?,
        "silence" => plain(&b, Command::Silence)?,
        "music_pause" => plain(&b, Command::MusicPause)?,
        "music_play" => plain(&b, Command::MusicPlay)?,
        "music_next" => plain(&b, Command::MusicNext)?,
        "music_previous" => plain(&b, Command::MusicPrevious)?,
        "repeat" => plain(&b, Command::Repeat)?,
        "screen_off" => plain(&b, Command::ScreenOff)?,
        "dnd_morning" => plain(&b, Command::DoNotDisturbMorning)?,
        "dnd_off" => plain(&b, Command::DoNotDisturbOff)?,
        "good_night" => plain(&b, Command::GoodNight)?,
        "timer_cancel" if named(&b) => Command::TimerCancel { name: b.name },
        "timer_left" if named(&b) => Command::TimerLeft { name: b.name },
        "timer_start" => match b {
            Bound {
                duration: Some(seconds),
                time: None,
                percent: None,
                name,
            } => Command::TimerStart { seconds, name },
            _ => return Err("needs a {duration} and nothing else but a name".into()),
        },
        "dnd_on" => match b {
            Bound {
                duration,
                time: None,
                percent: None,
                name: None,
            } => Command::DoNotDisturb { seconds: duration },
            _ => return Err("takes a {whole_duration} only".into()),
        },
        "alarm_set" => match b {
            Bound {
                time: Some((hour, minute)),
                duration: None,
                percent: None,
                name: None,
            } => Command::AlarmSet { hour, minute },
            _ => return Err("needs a {time} only".into()),
        },
        "volume_set" => match b {
            Bound {
                percent: Some(percent),
                duration: None,
                time: None,
                name: None,
            } => Command::VolumeSet { percent },
            _ => return Err("needs a {percent} only".into()),
        },
        "timer_cancel" | "timer_left" => return Err("takes a {name:...} only".into()),
        other => return Err(format!("unknown command {other:?}")),
    })
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
        let mut phrases: BTreeMap<String, Command> = BTreeMap::new();
        for (name, templates) in &f.commands {
            for t in templates {
                for (p, b) in
                    expand(t, &slots, &f.timer_names).map_err(|e| format!("{name}: {e}"))?
                {
                    let c = command(name, b).map_err(|e| format!("{name}: {t:?} {e}"))?;
                    let p = normalize(&p);
                    match phrases.get(&p) {
                        Some(old) if *old != c => {
                            return Err(format!("{p:?} means both {old:?} and {c:?}"));
                        }
                        Some(_) => {}
                        None => {
                            phrases.insert(p, c);
                        }
                    }
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
        self.phrases.get(text).cloned()
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
            Some(Command::TimerStart {
                seconds: 750,
                name: None
            })
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
            Decision::Accept(Command::TimerStart {
                seconds: 60,
                name: None
            })
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

    fn alfred(lang: &str) -> Grammar {
        let path = format!(
            "{}/../../characters/alfred/grammar/{lang}.toml",
            env!("CARGO_MANIFEST_DIR")
        );
        Grammar::load(Path::new(&path)).unwrap()
    }

    fn named(seconds: u64, name: &str) -> Option<Command> {
        Some(Command::TimerStart {
            seconds,
            name: Some(name.into()),
        })
    }

    #[test]
    fn alfred_italian_v02_reflexes() {
        let g = alfred("it");
        let c = |p: &str| g.command(p);
        assert_eq!(c("ripeti"), Some(Command::Repeat));
        assert_eq!(c("cosa hai detto"), Some(Command::Repeat));
        assert_eq!(c("spegni lo schermo"), Some(Command::ScreenOff));
        assert_eq!(
            c("non disturbare"),
            Some(Command::DoNotDisturb { seconds: None })
        );
        assert_eq!(
            c("non disturbare per un'ora"),
            Some(Command::DoNotDisturb {
                seconds: Some(3600)
            })
        );
        assert_eq!(
            c("non disturbarmi per due ore"),
            Some(Command::DoNotDisturb {
                seconds: Some(7200)
            })
        );
        assert_eq!(
            c("non disturbare per mezz'ora"),
            Some(Command::DoNotDisturb {
                seconds: Some(1800)
            })
        );
        assert_eq!(
            c("non disturbare fino a domattina"),
            Some(Command::DoNotDisturbMorning)
        );
        assert_eq!(
            c("disattiva non disturbare"),
            Some(Command::DoNotDisturbOff)
        );
        assert_eq!(c("buonanotte"), Some(Command::GoodNight));
        assert_eq!(c("buona notte"), Some(Command::GoodNight));
        assert_eq!(c("timer pasta dieci minuti"), named(600, "Pasta"));
        assert_eq!(
            c("metti un timer per la pasta di dieci minuti"),
            named(600, "Pasta")
        );
        assert_eq!(c("timer delle uova nove minuti"), named(540, "Uova"));
        assert_eq!(
            c("metti un timer per il forno di un'ora e mezza"),
            named(5400, "Forno")
        );
        assert_eq!(c("timer tè tre minuti"), named(180, "Tè"));
        assert_eq!(c("metti un timer pasta di un minuto"), named(60, "Pasta"));
        assert_eq!(c("metti timer un minuto pasta"), named(60, "Pasta"));
        assert_eq!(
            c("metti un timer di dieci minuti per le uova"),
            named(600, "Uova")
        );
        assert_eq!(
            c("metti un timer del forno di venti minuti"),
            named(1200, "Forno")
        );
        assert_eq!(
            c("annulla il timer della pasta"),
            Some(Command::TimerCancel {
                name: Some("Pasta".into())
            })
        );
        assert_eq!(
            c("annulla il timer"),
            Some(Command::TimerCancel { name: None })
        );
        assert_eq!(
            c("quanto manca alla pasta"),
            Some(Command::TimerLeft {
                name: Some("Pasta".into())
            })
        );
        assert_eq!(
            c("quanto manca per le uova"),
            Some(Command::TimerLeft {
                name: Some("Uova".into())
            })
        );
        assert_eq!(c("quanto manca"), Some(Command::TimerLeft { name: None }));
        assert_eq!(c("che timer ho"), Some(Command::TimerList));
        assert_eq!(c("quali timer ci sono"), Some(Command::TimerList));
        // Named timers take whole minutes only: the grammar stays small.
        assert_eq!(c("timer pasta dieci secondi"), None);
        assert_eq!(c("timer pasta dieci minuti e mezzo"), None);
        // The plain ones still take every duration.
        assert_eq!(
            c("timer dieci minuti e mezzo"),
            Some(Command::TimerStart {
                seconds: 630,
                name: None
            })
        );
    }

    #[test]
    fn alfred_english_v02_reflexes() {
        let g = alfred("en");
        let c = |p: &str| g.command(p);
        assert_eq!(c("repeat that"), Some(Command::Repeat));
        assert_eq!(c("what did you say"), Some(Command::Repeat));
        assert_eq!(c("turn off the screen"), Some(Command::ScreenOff));
        assert_eq!(
            c("do not disturb"),
            Some(Command::DoNotDisturb { seconds: None })
        );
        assert_eq!(
            c("do not disturb for an hour"),
            Some(Command::DoNotDisturb {
                seconds: Some(3600)
            })
        );
        assert_eq!(
            c("do not disturb for two hours"),
            Some(Command::DoNotDisturb {
                seconds: Some(7200)
            })
        );
        assert_eq!(
            c("do not disturb until tomorrow morning"),
            Some(Command::DoNotDisturbMorning)
        );
        assert_eq!(c("turn off do not disturb"), Some(Command::DoNotDisturbOff));
        assert_eq!(c("good night"), Some(Command::GoodNight));
        assert_eq!(c("set a pasta timer for ten minutes"), named(600, "Pasta"));
        assert_eq!(c("set an egg timer for nine minutes"), named(540, "Eggs"));
        assert_eq!(
            c("set a timer for the pasta for ten minutes"),
            named(600, "Pasta")
        );
        assert_eq!(c("rice timer for twenty minutes"), named(1200, "Rice"));
        assert_eq!(
            c("cancel the pasta timer"),
            Some(Command::TimerCancel {
                name: Some("Pasta".into())
            })
        );
        assert_eq!(
            c("how long for the eggs"),
            Some(Command::TimerLeft {
                name: Some("Eggs".into())
            })
        );
        assert_eq!(c("what timers do i have"), Some(Command::TimerList));
        assert_eq!(c("set a pasta timer for ten seconds"), None);
    }

    #[test]
    fn name_slots_and_their_errors() {
        let ok = r#"
format = 1
language = "it"
[commands]
timer_start = ["timer {name:bare} {whole_duration}"]
timer_cancel = ["annulla il timer {name:of}"]
[timer_names]
Pasta = { bare = "pasta", of = "della pasta" }
Uova = { bare = "uova" }
"#;
        let g = Grammar::parse(ok).unwrap();
        assert_eq!(g.command("timer uova un minuto"), named(60, "Uova"));
        // A name without the form is left out of that template.
        assert_eq!(g.command("annulla il timer delle uova"), None);
        let bad = |commands: &str| {
            Grammar::parse(&format!(
                "format = 1\nlanguage = \"it\"\n[commands]\n{commands}\n[timer_names]\nPasta = {{ bare = \"pasta\" }}\n"
            ))
            .unwrap_err()
        };
        assert!(bad(r#"timer_start = ["timer {name:of} {duration}"]"#).contains("no timer name"));
        assert!(bad(r#"time = ["ora {name:bare}"]"#).contains("takes no slot"));
        assert!(bad(r#"timer_cancel = ["annulla {duration}"]"#).contains("name"));
        assert!(bad(r#"dnd_on = ["zitto {name:bare}"]"#).contains("whole_duration"));
        assert!(bad(r#"timer_start = ["timer {minutes}"]"#).contains("unknown slot"));
        assert!(bad(r#"timer_start = ["timer {duration} {whole_duration}"]"#).contains("two"));
    }

    #[test]
    fn alfred_grammars_stay_small() {
        // Vosk compiles the phrase list at start: keep it within reach of the device.
        for lang in ["it", "en"] {
            let n = alfred(lang).len();
            eprintln!("{lang}: {n} phrases");
            assert!(n < 30_000, "{lang}: {n} phrases");
        }
    }
}
