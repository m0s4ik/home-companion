// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The gate's verdict on an utterance held back for the reflexes.
//!
//! # Objective (written before any measurement)
//!
//! On the labelled recordings of the reflex bench:
//!
//! 1. **Zero false accepts**: no negative (an utterance that was for Home Assistant) is acted on
//!    as a reflex.
//! 2. **Fewer than 5 % of the true reflexes rejected** (released to Home Assistant, by a partial
//!    or by the final rule).
//!
//! A rule that fails (1) is out whatever its (2). Among the rules that meet both, the one that
//! releases negatives earliest wins; ties go to the simpler rule.
//!
//! The acceptance rules compared:
//!
//! - **A** (today, [`crate::Grammar::decide_covered`]): the whole text is one phrase, no `[unk]`,
//!   every confidence at least the threshold, and the speech detector's speech covered by the
//!   words at both ends.
//! - **B** (Vosk only): the same without the detector's coverage; the drop of leading words
//!   inside the wake tail is kept.
//! - **C(S)** (B + tail coverage): speech the detector heard after the last word, up to the
//!   moment of the decision, longer than `S` rejects; `S` in [`TAIL_SLACKS_S`].
//!
//! The early release compared with them: a partial that, after the wake tail, contains `[unk]` or
//! is not a word-prefix of any phrase of the grammar releases the utterance live, once it has
//! stayed so for `K` ([`STABILITY_MS`]).

//!
//! # The verdict
//!
//! [`verdict`] decides everything the body's gate decides, from events, with no I/O and no clock
//! of its own (the body passes the elapsed time and its [`Vad`] in [`Ctx`]). "Over" below = the
//! detector heard the end of speech or nobody speaking ([`Vad::ended`], [`Vad::nobody_spoke`]);
//! every event notices it, `SpeechEnd` only says so explicitly. After any verdict but `Hold` and
//! `Flush` the utterance is decided and later events give `Hold`.
//!
//! | Event | Condition | Verdict |
//! |---|---|---|
//! | `Partial` | before [`WAKE_TAIL_S`] of audio | `Hold` |
//! | `Partial` | the beginning of a phrase, after up to [`PARTIAL_LEAD_WORDS`] leading words, no `[unk]` ([`fits`]) | `Hold` (the stability clock stops) |
//! | `Partial` | anything else, for `stability_s` already (0: at once) | `Release { live: true }` |
//! | `Final` | rule A, Vosk's endpoint, not over | `Hold` (kept for the end of speech) |
//! | `Final` (or the kept one when over) | accepted by the rule, not `Yes` | `Act(c)` |
//! | | accepted, `Yes`, no question open | `Release { live: !over }` |
//! | | empty, not over | `Hold` (the helper goes on) |
//! | | empty, over, less speech than [`Vad::MIN_SPEECH_MS`] | `Quiet` |
//! | | empty, over, before `no_words_wait_s` | `Hold` (the helper goes on: no restart) |
//! | | empty, over, later | `Release { live: true }` |
//! | | rejected, ends on an open word ([`unfinished`]) | `Release { live: true }` |
//! | | rejected otherwise (not one phrase, `[unk]`, unsure, speech outside the words) | `Release { live: !over }` |
//! | any | over (first time since speech), no result yet, no `E` pending | `Flush`: the body sends `E` |
//! | any | `E` sent `helper_wait_s` ago, no result | `Release { live: false }` |
//! | any | the partial has been offending for `stability_s` | `Release { live: true }` |
//! | any | after sound without words, `no_words_silence_s` of silence | `Quiet` |
//! | any | `max_listen_s` reached, no `E` pending | `Release { live: true }` |
//! | `HelperLost` | | `Release { live: no E pending }` |
//! | otherwise | | `Hold` |

/// (1): negatives acted on, allowed.
pub const OBJECTIVE_MAX_FALSE_ACCEPTS: usize = 0;
/// (2): true reflexes rejected, strictly below this share.
pub const OBJECTIVE_MAX_REJECTED_SHARE: f32 = 0.05;
/// The tail slacks of rule C swept by the bench, in seconds.
pub const TAIL_SLACKS_S: [f32; 4] = [0.6, 0.8, 1.0, 1.5];
/// The partial stabilities of the early release swept by the bench, in milliseconds.
pub const STABILITY_MS: [u32; 3] = [0, 200, 400];

use crate::grammar::{Command, Decision, Grammar, Word};
use crate::vad::{Vad, WAKE_TAIL_S};

/// What the body does with the utterance now.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Keep buffering and feeding the helper.
    Hold,
    /// Keep buffering and feeding the helper, and send it `E` now (once): the detector heard the
    /// end of speech and the helper has not answered yet.
    Flush,
    /// Not a reflex: Home Assistant gets the utterance. `live`: the user may still be speaking:
    /// send the buffer and keep streaming; false: the speech is over: buffer, then audio-stop.
    Release { live: bool },
    /// A reflex: the body does it.
    Act(Command),
    /// Nobody spoke: the conversation ends quietly (neither a reflex nor for Home Assistant).
    Quiet,
}

/// The acceptance rule of a final result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// A: [`Grammar::decide_covered`]; the decision waits for the detector's end of speech.
    Covered,
    /// B: Vosk only; decided as soon as Vosk's result comes.
    VoskOnly,
    /// C: B, and no more than `slack_s` of speech after the last word, up to the decision.
    TailCovered { slack_s: f32 },
}

/// What happens to the utterance, as the body reports it.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// The helper's partial result changed (`audio_s`: audio of the utterance it had decoded).
    Partial {
        text: &'a str,
        audio_s: f32,
    },
    /// The detector heard the end of speech or nobody speaking. The body may send it once or on
    /// every frame while it lasts; every other event notices it too.
    SpeechEnd,
    /// The helper's result: at Vosk's own end of speech (`endpoint`) or after `E`.
    Final {
        text: &'a str,
        words: &'a [Word],
        endpoint: bool,
    },
    /// Time passing (every microphone frame).
    Tick,
    HelperLost,
}

/// The gate's limits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Still speaking after this long: Home Assistant takes it live.
    pub max_listen_s: f32,
    /// After `E`, how long the helper may take.
    pub helper_wait_s: f32,
    /// Sound without words before this: keep listening (a breath, a pause after the wake word).
    pub no_words_wait_s: f32,
    /// After sound without words, this much silence: nobody is going to speak.
    pub no_words_silence_s: f32,
    /// The early release: how long a partial that cannot become a phrase must stay so (None: no
    /// early release).
    pub stability_s: Option<f32>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_listen_s: 8.0,
            helper_wait_s: 1.5,
            no_words_wait_s: 3.0,
            no_words_silence_s: Vad::NO_SPEECH_MS as f32 / 1000.0,
            stability_s: Some(DEFAULT_STABILITY_S),
        }
    }
}

/// The early release's stability: the bench's winner. 29 labelled utterances (16 true reflexes,
/// 13 negatives), rule A: K = 0 / 200 / 400 ms released the negatives at a median 1.36 / 1.58 /
/// 1.78 s of audio (3.42 s without the early release) and rejected 1 / 1 / 0 true reflexes that
/// the final rule accepts (Vosk's first partial, ~1.1 s into the audio, can hold a wake-tail
/// word that the next one, 0.24 s later, has dropped: K must outlast one partial step).
pub const DEFAULT_STABILITY_S: f32 = 0.4;
/// The acceptance rule: the bench's winner. Every configuration rejected at least 5 of the 16
/// true reflexes (5 under every rule; A: 7, B and C with S >= 1.0 s: 5), so none met the
/// objective and A stays. Without the early release, or with K = 400 ms, B and every C acted on
/// 1 of the 13 negatives (0 with K = 0 or 200 ms); the "hard" class had 2 examples, too few to
/// drop the coverage.
pub const DEFAULT_RULE: Rule = Rule::Covered;

impl Default for Rule {
    fn default() -> Self {
        DEFAULT_RULE
    }
}

/// What the verdict reads, owned by the body.
pub struct Ctx<'a> {
    pub grammar: &'a Grammar,
    pub threshold: f32,
    pub rule: Rule,
    pub limits: Limits,
    /// The detector, fed with every frame of the utterance.
    pub vad: &'a Vad,
    /// A Yes/No question is open for this utterance ("sì" is a reflex only then).
    pub answering: bool,
    /// Seconds since the utterance started.
    pub elapsed_s: f32,
}

/// What the verdict remembers between events; one per utterance.
#[derive(Debug, Clone, Default)]
pub struct GateState {
    /// When `E` was sent (elapsed seconds); cleared by an empty result (the helper goes on).
    end_sent_s: Option<f32>,
    /// The detector's current end of speech was handled (reset when speech comes back).
    end_handled: bool,
    /// A result kept for the detector's end of speech (rule A).
    result: Option<(String, Vec<Word>, bool)>,
    /// Since when (elapsed) the partials cannot become a phrase.
    offending_since: Option<f32>,
    /// Sound without words: speech heard by then, waiting for more.
    no_words: Option<u32>,
    /// The verdict was given (anything but Hold/Flush).
    done: bool,
    why: &'static str,
}

impl GateState {
    pub fn new() -> GateState {
        GateState::default()
    }

    /// `E` was sent (and no empty result came since).
    pub fn end_sent(&self) -> bool {
        self.end_sent_s.is_some()
    }

    /// Why the last verdict was given, for the logs.
    pub fn why(&self) -> &'static str {
        self.why
    }

    /// Whether the last release came from a partial (the early release).
    pub fn released_by_partial(&self) -> bool {
        self.why == WHY_PARTIAL
    }
}

const WHY_PARTIAL: &str = "the partial is not a reflex";

fn over(vad: &Vad) -> bool {
    vad.ended() || vad.nobody_spoke()
}

/// The verdict on one event. After anything but `Hold`/`Flush` the utterance is decided and
/// later events return `Hold`.
pub fn verdict(st: &mut GateState, ctx: &Ctx, ev: Event) -> Verdict {
    if st.done {
        return Verdict::Hold;
    }
    let v = step(st, ctx, ev);
    if !matches!(v, Verdict::Hold | Verdict::Flush) {
        st.done = true;
    }
    v
}

fn step(st: &mut GateState, ctx: &Ctx, ev: Event) -> Verdict {
    let now = ctx.elapsed_s;
    if !over(ctx.vad) {
        st.end_handled = false;
    }
    match ev {
        Event::HelperLost => {
            st.why = "the helper stopped";
            return Verdict::Release {
                live: st.end_sent_s.is_none(),
            };
        }
        Event::Partial { text, audio_s } => {
            if let Some(v) = partial(st, ctx, text, audio_s) {
                return v;
            }
        }
        Event::Final {
            text,
            words,
            endpoint,
        } => {
            if !endpoint {
                st.end_sent_s = None;
            }
            let decide_now = !endpoint || !matches!(ctx.rule, Rule::Covered) || over(ctx.vad);
            if decide_now {
                return decide(st, ctx, text, words);
            }
            st.result = Some((text.to_string(), words.to_vec(), endpoint));
        }
        Event::SpeechEnd | Event::Tick => {}
    }
    // The detector's end of speech, whichever event brought it.
    if over(ctx.vad) && !st.end_handled {
        st.end_handled = true;
        if let Some((text, words, _)) = st.result.take() {
            return decide(st, ctx, &text, &words);
        }
        if st.end_sent_s.is_none() {
            st.end_sent_s = Some(now);
            st.why = "end of speech";
            return Verdict::Flush;
        }
    }
    if let Some(t) = st.end_sent_s {
        if now - t >= ctx.limits.helper_wait_s {
            st.why = "no answer from the helper";
            return Verdict::Release { live: false };
        }
    }
    if let (Some(since), Some(k)) = (st.offending_since, ctx.limits.stability_s) {
        if now - since >= k {
            st.why = WHY_PARTIAL;
            return Verdict::Release { live: true };
        }
    }
    if st.no_words.is_some()
        && st.end_sent_s.is_none()
        && ctx.vad.silence_ms() as f32 / 1000.0 >= ctx.limits.no_words_silence_s
    {
        st.why = "no speech after sound without words";
        return Verdict::Quiet;
    }
    if st.end_sent_s.is_none() && now >= ctx.limits.max_listen_s {
        st.why = "still speaking";
        return Verdict::Release { live: true };
    }
    Verdict::Hold
}

/// The early release: None to go on with the common checks.
fn partial(st: &mut GateState, ctx: &Ctx, text: &str, audio_s: f32) -> Option<Verdict> {
    if audio_s < WAKE_TAIL_S {
        return None;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if !words.is_empty() {
        st.no_words = None;
    }
    if fits(ctx.grammar, &words) {
        st.offending_since = None;
        return None;
    }
    let since = *st.offending_since.get_or_insert(ctx.elapsed_s);
    match ctx.limits.stability_s {
        Some(k) if ctx.elapsed_s - since >= k => {
            st.why = WHY_PARTIAL;
            Some(Verdict::Release { live: true })
        }
        _ => None,
    }
}

/// The partial can still become a phrase: it is the beginning of one, after up to
/// [`PARTIAL_LEAD_WORDS`] leading words. A partial has no word times, and Vosk shows its first
/// partial about a second into the audio, so the words of the wake tail (the end of "Okay
/// Alfred" heard as a word, `[unk]`) cannot be told by their time as the final rule does: any
/// leading word may be one. Not dropping them all: a partial of only such words must go on.
pub fn fits(g: &Grammar, words: &[&str]) -> bool {
    (0..=PARTIAL_LEAD_WORDS.min(words.len())).any(|k| {
        let rest = &words[k..];
        (k == 0 || !rest.is_empty())
            && !rest.contains(&"[unk]")
            && is_word_prefix(g, &rest.join(" "))
    })
}

/// Leading words a partial may hold before its phrase (the final rule drops up to two).
pub const PARTIAL_LEAD_WORDS: usize = 2;

/// `text` is a phrase of the grammar or the first words of one.
pub fn is_word_prefix(g: &Grammar, text: &str) -> bool {
    if text.is_empty() || g.phrases.contains_key(text) {
        return true;
    }
    let key = format!("{text} ");
    g.phrases
        .range(key.clone()..)
        .next()
        .is_some_and(|(p, _)| p.starts_with(&key))
}

/// The acceptance rule on a final result.
pub fn accept(ctx: &Ctx, text: &str, words: &[Word]) -> Decision {
    match ctx.rule {
        Rule::Covered => ctx
            .grammar
            .decide_covered(text, words, ctx.threshold, ctx.vad),
        Rule::VoskOnly => vosk_only(ctx.grammar, text, words, ctx.threshold).0,
        Rule::TailCovered { slack_s } => match vosk_only(ctx.grammar, text, words, ctx.threshold) {
            (Decision::Accept(c), last_end) => match ctx.vad.span() {
                Some((_, b)) if b > last_end + slack_s => {
                    Decision::Reject("speech after the words")
                }
                _ => Decision::Accept(c),
            },
            (d, _) => d,
        },
    }
}

/// [`Grammar::decide`] with the drop of up to two leading words inside the wake tail (as
/// [`Grammar::decide_covered`], without the detector); also the end of the last word kept.
fn vosk_only(g: &Grammar, text: &str, words: &[Word], threshold: f32) -> (Decision, f32) {
    let last = words.last().map_or(0.0, |w| w.end);
    let d = g.decide(text, words, threshold);
    if matches!(d, Decision::Accept(_)) {
        return (d, last);
    }
    let lead = words.iter().take_while(|w| w.end <= WAKE_TAIL_S).count();
    for n in 1..=lead.min(2) {
        let rest = &words[n..];
        let rest_text = rest
            .iter()
            .map(|w| w.word.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let r = g.decide(&rest_text, rest, threshold);
        if matches!(r, Decision::Accept(_)) {
            return (r, last);
        }
    }
    (d, last)
}

fn decide(st: &mut GateState, ctx: &Ctx, text: &str, words: &[Word]) -> Verdict {
    let speaking = !over(ctx.vad);
    match accept(ctx, text, words) {
        Decision::Accept(Command::Yes) if !ctx.answering => {
            st.why = "yes without a question";
            Verdict::Release { live: speaking }
        }
        Decision::Accept(c) => {
            st.why = "a reflex";
            Verdict::Act(c)
        }
        Decision::Empty => {
            // The helper goes on decoding the audio that follows: no `E` pending.
            st.end_sent_s = None;
            if speaking {
                st.no_words.get_or_insert(ctx.vad.speech_ms());
                Verdict::Hold
            } else if ctx.vad.speech_ms() < Vad::MIN_SPEECH_MS {
                st.why = "no speech";
                Verdict::Quiet
            } else if ctx.elapsed_s < ctx.limits.no_words_wait_s {
                st.no_words.get_or_insert(ctx.vad.speech_ms());
                st.why = "no words yet, listening on";
                Verdict::Hold
            } else {
                st.why = "no words yet";
                Verdict::Release { live: true }
            }
        }
        Decision::Reject(why) if unfinished(text) => {
            let _ = why;
            st.why = "sentence not finished";
            Verdict::Release { live: true }
        }
        Decision::Reject(why) => {
            st.why = why;
            Verdict::Release { live: speaking }
        }
    }
}

/// A sentence ending on a word that cannot end it (a preposition, an article, "e"): the speaker
/// paused in the middle ("metti una sveglia alle …").
pub fn unfinished(text: &str) -> bool {
    const OPEN: &[&str] = &[
        "alle", "alla", "allo", "al", "all", "a", "ad", "di", "del", "della", "da", "dal", "dalle",
        "tra", "fra", "per", "in", "con", "su", "e", "il", "lo", "la", "le", "gli", "un", "una",
        "uno", "at", "to", "for", "in", "of", "the", "a", "an", "and",
    ];
    text.split_whitespace()
        .last()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .is_some_and(|w| OPEN.contains(&w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    fn grammar() -> &'static Grammar {
        static G: OnceLock<Grammar> = OnceLock::new();
        G.get_or_init(|| {
            Grammar::parse(include_str!("../../../characters/alfred/grammar/it.toml")).unwrap()
        })
    }

    fn w(word: &str, conf: f32, start: f32, end: f32) -> Word {
        Word {
            word: word.into(),
            conf,
            start,
            end,
        }
    }

    /// Words evenly spread over `start..end`, all at `conf`.
    fn spread(text: &str, conf: f32, start: f32, end: f32) -> Vec<Word> {
        let t: Vec<&str> = text.split(' ').collect();
        let d = (end - start) / t.len() as f32;
        t.iter()
            .enumerate()
            .map(|(i, x)| w(x, conf, start + i as f32 * d, start + (i + 1) as f32 * d))
            .collect()
    }

    /// An utterance as the body runs it: 20 ms frames of tone (speech) or faint noise into the
    /// detector, a Tick per frame, the helper's events when the test says.
    struct Sim {
        vad: Vad,
        st: GateState,
        t: f32,
        rule: Rule,
        answering: bool,
        limits: Limits,
        speech: Vec<(f32, f32)>,
        seed: u32,
    }

    impl Sim {
        fn new(rule: Rule, speech: &[(f32, f32)]) -> Sim {
            Sim {
                vad: Vad::new(),
                st: GateState::new(),
                t: 0.0,
                rule,
                answering: false,
                limits: Limits::default(),
                speech: speech.to_vec(),
                seed: 12345,
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                grammar: grammar(),
                threshold: 0.8,
                rule: self.rule,
                limits: self.limits,
                vad: &self.vad,
                answering: self.answering,
                elapsed_s: self.t,
            }
        }

        fn frame(&mut self) {
            let mid = self.t + 0.01;
            let speaking = self.speech.iter().any(|&(a, b)| mid >= a && mid < b);
            let f: Vec<i16> = (0..crate::vad::FRAME)
                .map(|i| {
                    if speaking {
                        ((i as f32 * 0.2).sin() * 0.05 * 32767.0) as i16
                    } else {
                        self.seed = self.seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        (((self.seed >> 16) as f32 / 32768.0 - 1.0) * 0.001 * 32767.0) as i16
                    }
                })
                .collect();
            self.vad.push(&f);
            self.t += 0.02;
        }

        /// Frames and ticks until `until`; the first verdict that is not Hold, with its time.
        fn run_to(&mut self, until: f32) -> Option<(f32, Verdict)> {
            while self.t + 0.01 < until {
                self.frame();
                let v = self.ev(Event::Tick);
                if v != Verdict::Hold {
                    return Some((self.t, v));
                }
            }
            None
        }

        fn ev(&mut self, ev: Event) -> Verdict {
            let mut st = std::mem::take(&mut self.st);
            let v = verdict(&mut st, &self.ctx(), ev);
            self.st = st;
            v
        }

        fn fin(&mut self, text: &str, words: &[Word], endpoint: bool) -> Verdict {
            self.ev(Event::Final {
                text,
                words,
                endpoint,
            })
        }
    }

    const C1: Rule = Rule::TailCovered { slack_s: 1.0 };

    /// R1: a phrase understood at 1.00 with voices behind (speech going on 2 s after the last
    /// word): today's rule rejects it (the bench kept it: see DEFAULT_RULE); the tail rule acts.
    #[test]
    fn r1_voices_behind() {
        let words = spread("attiva non disturbare", 1.0, 0.6, 2.0);
        for rule in [Rule::Covered, C1, Rule::VoskOnly] {
            let mut s = Sim::new(rule, &[(0.5, 4.0)]);
            assert_eq!(s.run_to(2.6), None);
            let v = s.fin("attiva non disturbare", &words, true);
            match rule {
                Rule::Covered => {
                    // Waits for the detector's end of speech, then rejects.
                    assert_eq!(v, Verdict::Hold);
                    let (at, v) = s.run_to(6.0).unwrap();
                    assert_eq!(v, Verdict::Release { live: false });
                    assert_eq!(s.st.why(), "speech outside the words");
                    assert!(at > 4.6, "{at}");
                }
                _ => assert_eq!(v, Verdict::Act(Command::DoNotDisturb { seconds: None })),
            }
        }
        assert_eq!(DEFAULT_RULE, Rule::Covered);
    }

    /// R2: the answer after a pause: the cue's echo, an empty result, then "no" at 1.95 s.
    #[test]
    fn r2_no_after_a_pause() {
        for rule in [DEFAULT_RULE, C1] {
            let mut s = Sim::new(rule, &[(0.0, 0.4), (1.95, 2.3)]);
            s.answering = true;
            let (at, v) = s.run_to(2.0).unwrap();
            assert_eq!(v, Verdict::Flush, "{at}");
            assert!(s.st.end_sent());
            // Sound without words, early: listening on (no `E` pending, no restart).
            assert_eq!(s.fin("", &[], false), Verdict::Hold);
            assert!(!s.st.end_sent());
            let (_, v) = s.run_to(4.0).unwrap();
            assert_eq!(v, Verdict::Flush);
            let words = [w("no", 1.0, 1.95, 2.25)];
            assert_eq!(s.fin("no", &words, false), Verdict::Act(Command::Silence));
        }
    }

    /// R3: "che ore sono" in one breath after the wake word, a junk word in the wake tail.
    #[test]
    fn r3_one_breath() {
        let mut words = vec![w("e", 1.0, 0.1, 0.4)];
        words.extend(spread("che ore sono", 1.0, 0.5, 1.2));
        for rule in [DEFAULT_RULE, C1, Rule::VoskOnly] {
            let mut s = Sim::new(rule, &[(0.0, 1.2)]);
            let (_, v) = s.run_to(3.0).unwrap();
            assert_eq!(v, Verdict::Flush);
            assert_eq!(
                s.fin("e che ore sono", &words, false),
                Verdict::Act(Command::Time)
            );
        }
    }

    /// R4: more words after a phrase: the partial releases it; the final alone never acts under
    /// a rule with coverage.
    #[test]
    fn r4_more_after_a_phrase() {
        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 3.0)]);
        s.run_to(1.2);
        let p = |text| Event::Partial { text, audio_s: 1.2 };
        assert_eq!(s.ev(p("che ore sono")), Verdict::Hold);
        assert_eq!(s.ev(p("che ore sono [unk]")), Verdict::Hold);
        let (at, v) = s.run_to(3.0).unwrap();
        assert_eq!(v, Verdict::Release { live: true });
        assert!(s.st.released_by_partial());
        assert!((at - 1.2 - DEFAULT_STABILITY_S).abs() < 0.03, "{at}");

        let words = spread("che ore sono", 1.0, 0.3, 1.2);
        for rule in [Rule::Covered, Rule::TailCovered { slack_s: 0.6 }] {
            let mut s = Sim::new(rule, &[(0.3, 2.0)]);
            s.limits.stability_s = None;
            let (_, v) = s.run_to(4.0).unwrap();
            assert_eq!(v, Verdict::Flush);
            assert_eq!(
                s.fin("che ore sono", &words, false),
                Verdict::Release { live: false }
            );
        }
    }

    /// R5: a phrase followed by more words, seen in the partial.
    #[test]
    fn r5_timer_and_more() {
        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 3.0)]);
        s.run_to(1.4);
        let p = |text| Event::Partial { text, audio_s: 1.4 };
        assert_eq!(s.ev(p("timer cinque minuti")), Verdict::Hold);
        assert_eq!(s.ev(p("timer cinque minuti [unk]")), Verdict::Hold);
        assert_eq!(s.run_to(3.0).unwrap().1, Verdict::Release { live: true });
        // With no stability, at once.
        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 3.0)]);
        s.limits.stability_s = Some(0.0);
        s.run_to(1.4);
        assert_eq!(
            s.ev(p("timer cinque minuti [unk]")),
            Verdict::Release { live: true }
        );
    }

    /// R6: a sentence cut on an open word.
    #[test]
    fn r6_unfinished() {
        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 1.8)]);
        s.limits.stability_s = None;
        let (_, v) = s.run_to(3.0).unwrap();
        assert_eq!(v, Verdict::Flush);
        let words = spread("metti una sveglia alle", 1.0, 0.3, 1.8);
        assert_eq!(
            s.fin("metti una sveglia alle", &words, false),
            Verdict::Release { live: true }
        );
        assert!(unfinished("metti una sveglia alle"));
        assert!(!unfinished("che ore sono"));
    }

    /// R7: noise only.
    #[test]
    fn r7_noise_only() {
        // Nobody spoke: the detector gives up after 5 s, the empty result ends it quietly.
        let mut s = Sim::new(DEFAULT_RULE, &[(0.0, 0.1)]);
        let (at, v) = s.run_to(6.0).unwrap();
        assert_eq!(v, Verdict::Flush);
        assert!(at >= 4.99, "{at}");
        assert_eq!(s.fin("", &[], false), Verdict::Quiet);

        // Sound without words at 1.5 s: listening on; at 4 s: Home Assistant, live.
        let mut s = Sim::new(DEFAULT_RULE, &[(0.0, 0.8)]);
        assert_eq!(s.run_to(1.5).unwrap().1, Verdict::Flush);
        assert_eq!(s.fin("", &[], false), Verdict::Hold);
        let mut s = Sim::new(DEFAULT_RULE, &[(0.0, 0.8), (2.0, 3.4)]);
        s.run_to(1.6);
        assert_eq!(s.fin("", &[], false), Verdict::Hold);
        assert_eq!(s.run_to(4.5).unwrap().1, Verdict::Flush);
        assert!(s.t > 3.9);
        assert_eq!(s.fin("", &[], false), Verdict::Release { live: true });

        // Sound without words and then silence: nobody is going to speak.
        let mut s = Sim::new(DEFAULT_RULE, &[(0.0, 0.8)]);
        s.run_to(1.6);
        assert_eq!(s.fin("", &[], false), Verdict::Hold);
        let (at, v) = s.run_to(8.0).unwrap();
        assert_eq!(v, Verdict::Quiet);
        assert!(at >= 5.7, "{at}");
    }

    /// R8: a first partial that is only `[unk]`: after the wake tail it releases; inside it,
    /// not.
    #[test]
    fn r8_unk_alone() {
        let mut s = Sim::new(DEFAULT_RULE, &[(0.0, 3.0)]);
        s.run_to(0.6);
        assert_eq!(
            s.ev(Event::Partial {
                text: "[unk]",
                audio_s: 0.6
            }),
            Verdict::Hold
        );
        let (at, v) = s.run_to(3.0).unwrap();
        assert_eq!(v, Verdict::Release { live: true });
        assert!((at - 0.6 - DEFAULT_STABILITY_S).abs() < 0.03, "{at}");

        let mut s = Sim::new(DEFAULT_RULE, &[(0.0, 3.0)]);
        s.run_to(0.3);
        assert_eq!(
            s.ev(Event::Partial {
                text: "[unk]",
                audio_s: 0.3
            }),
            Verdict::Hold
        );
        assert_eq!(s.run_to(2.5), None);
    }

    /// R9: "sì" acts only with a question open.
    #[test]
    fn r9_yes() {
        for answering in [false, true] {
            let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 0.8)]);
            s.answering = answering;
            assert_eq!(s.run_to(2.0).unwrap().1, Verdict::Flush);
            let v = s.fin("sì", &[w("sì", 1.0, 0.3, 0.7)], false);
            if answering {
                assert_eq!(v, Verdict::Act(Command::Yes));
            } else {
                assert_eq!(v, Verdict::Release { live: false });
            }
        }
    }

    /// R10: one word under the threshold.
    #[test]
    fn r10_unsure() {
        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 1.2)]);
        assert_eq!(s.run_to(3.0).unwrap().1, Verdict::Flush);
        let mut words = spread("che ore sono", 1.0, 0.3, 1.2);
        words[1].conf = 0.6;
        assert_eq!(
            s.fin("che ore sono", &words, false),
            Verdict::Release { live: false }
        );
        assert_eq!(s.st.why(), "unsure");
    }

    /// R11: the helper's silence, the longest listen, the helper lost.
    #[test]
    fn r11_limits() {
        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 1.2)]);
        let (at, v) = s.run_to(3.0).unwrap();
        assert_eq!(v, Verdict::Flush);
        let (at2, v) = s.run_to(6.0).unwrap();
        assert_eq!(v, Verdict::Release { live: false });
        assert!((at2 - at - 1.5).abs() < 0.03, "{at} {at2}");

        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 20.0)]);
        let (at, v) = s.run_to(20.0).unwrap();
        assert_eq!(v, Verdict::Release { live: true });
        assert!((at - 8.0).abs() < 0.03, "{at}");

        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 1.2)]);
        s.run_to(1.0);
        assert_eq!(s.ev(Event::HelperLost), Verdict::Release { live: true });
        let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 1.2)]);
        assert_eq!(s.run_to(3.0).unwrap().1, Verdict::Flush);
        assert_eq!(s.ev(Event::HelperLost), Verdict::Release { live: false });
        // Decided: later events change nothing.
        assert_eq!(s.ev(Event::Tick), Verdict::Hold);
    }

    /// R12: several grammar words that are not one phrase, unsure.
    #[test]
    fn r12_not_one_phrase() {
        for (text, live) in [("tre per di sera", false), ("sera tre di la", true)] {
            let mut s = Sim::new(DEFAULT_RULE, &[(0.3, 1.8)]);
            s.limits.stability_s = None;
            assert_eq!(s.run_to(3.0).unwrap().1, Verdict::Flush);
            let words = spread(text, 0.5, 0.3, 1.8);
            assert_eq!(s.fin(text, &words, false), Verdict::Release { live });
        }
    }

    /// Vosk's own end of speech under rule A waits for the detector; under B and C it decides.
    #[test]
    fn endpoint_before_the_detector() {
        let words = spread("che ore sono", 1.0, 0.3, 1.0);
        let mut s = Sim::new(Rule::Covered, &[(0.3, 1.0)]);
        s.run_to(1.4);
        assert_eq!(s.fin("che ore sono", &words, true), Verdict::Hold);
        let (_, v) = s.run_to(3.0).unwrap();
        assert_eq!(v, Verdict::Act(Command::Time));
        let mut s = Sim::new(C1, &[(0.3, 1.0)]);
        s.run_to(1.4);
        assert_eq!(
            s.fin("che ore sono", &words, true),
            Verdict::Act(Command::Time)
        );
    }

    #[test]
    fn prefixes() {
        let g = grammar();
        let fit = |t: &str| fits(g, &t.split_whitespace().collect::<Vec<_>>());
        assert!(fit(""));
        assert!(fit("che ore"));
        assert!(fit("che ore sono"));
        assert!(fit("metti un timer di"));
        assert!(!fit("che ore sono a"));
        assert!(!fit("[unk]"));
        assert!(!fit("che ore [unk]"));
        // Up to two leading words (the wake tail) may come before a phrase.
        assert!(fit("riso che ore"));
        assert!(fit("[unk] e che ore sono"));
        assert!(!fit("a b c che ore"));
        assert!(!fit("riso"));
        assert!(is_word_prefix(g, "attiva non"));
        assert!(!is_word_prefix(g, "attiva il"));
    }
}
