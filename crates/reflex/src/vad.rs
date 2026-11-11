// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A small energy-based speech detector for the utterance after the wake word, in 20 ms frames.
//! Two jobs:
//!
//! - **End of speech** for the reflexes: Vosk only reports its own end of speech after something
//!   it could decode, so an utterance outside the grammar ("ciao, come stai?") would wait for its
//!   5 s rule. The body ends the utterance when this detector has heard speech and then
//!   [`Vad::END_SILENCE_MS`] of silence, whichever comes first.
//! - **Coverage**: Vosk drops out-of-grammar words at the end ("che ore sono a New York" comes out
//!   as "che ore sono", every word at 1.00). A reflex fires only if its words cover the speech
//!   this detector heard ([`Vad::covered_by`]).
//!
//! The level is the frame's RMS in dBFS against a noise floor that falls at once and rises slowly,
//! so it follows the room without a calibration.

/// Samples per frame (20 ms at 16 kHz).
pub const FRAME: usize = 320;
pub const FRAME_MS: u32 = 20;

/// The start of an utterance that may hold the end of "Okay Alfred" (the audio from just before
/// the detection is included, so a phrase said without a pause keeps its first word) and the wake
/// cue: words and sound there are not held against a reflex.
pub const WAKE_TAIL_S: f32 = 0.45;

#[derive(Debug, Clone)]
pub struct Vad {
    floor_db: f32,
    frames: u32,
    speech_frames: u32,
    silence_run: u32,
    first_speech: Option<u32>,
    last_speech: Option<u32>,
    pending: Vec<i16>,
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

impl Vad {
    /// Above the noise floor by this much = speech.
    pub const MARGIN_DB: f32 = 9.0;
    /// Never speech below this level.
    pub const MIN_DB: f32 = -62.0;
    /// Speech needed before an end of speech counts.
    pub const MIN_SPEECH_MS: u32 = 160;
    /// Silence after speech that ends the utterance.
    pub const END_SILENCE_MS: u32 = 700;
    /// No speech at all for this long: nobody spoke.
    pub const NO_SPEECH_MS: u32 = 5000;

    pub fn new() -> Vad {
        Vad {
            floor_db: -55.0,
            frames: 0,
            speech_frames: 0,
            silence_run: 0,
            first_speech: None,
            last_speech: None,
            pending: Vec::with_capacity(FRAME),
        }
    }

    /// Feeds samples of any length (cut into 20 ms frames).
    pub fn push(&mut self, samples: &[i16]) {
        for &s in samples {
            self.pending.push(s);
            if self.pending.len() == FRAME {
                let f = std::mem::take(&mut self.pending);
                self.frame(&f);
                self.pending = f;
                self.pending.clear();
            }
        }
    }

    fn frame(&mut self, f: &[i16]) {
        let e = f.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / f.len() as f64;
        let db = if e > 0.0 {
            (10.0 * (e / (32768.0 * 32768.0)).log10()) as f32
        } else {
            -120.0
        };
        let speech = db > Self::MIN_DB && db > self.floor_db + Self::MARGIN_DB;
        if db < self.floor_db {
            self.floor_db = db.max(-90.0);
        } else {
            // Up slowly: 1 dB/s in silence, 2.5 dB/s during "speech" (a steady fan that started
            // loud stops counting as speech after a few seconds; a sentence loses ~3 dB).
            self.floor_db += if speech { 0.05 } else { 0.02 };
        }
        if speech {
            self.speech_frames += 1;
            self.silence_run = 0;
            self.first_speech.get_or_insert(self.frames);
            self.last_speech = Some(self.frames);
        } else {
            self.silence_run += 1;
        }
        self.frames += 1;
    }

    pub fn heard_ms(&self) -> u32 {
        self.frames * FRAME_MS
    }

    pub fn speech_ms(&self) -> u32 {
        self.speech_frames * FRAME_MS
    }

    /// Speech, then enough silence.
    pub fn ended(&self) -> bool {
        self.speech_ms() >= Self::MIN_SPEECH_MS
            && self.silence_run * FRAME_MS >= Self::END_SILENCE_MS
    }

    /// Nothing said for a while.
    pub fn nobody_spoke(&self) -> bool {
        self.speech_ms() < Self::MIN_SPEECH_MS && self.heard_ms() >= Self::NO_SPEECH_MS
    }

    /// Seconds from the start: the first and the last speech frame.
    pub fn span(&self) -> Option<(f32, f32)> {
        let a = self.first_speech?;
        let b = self.last_speech?;
        Some((
            a as f32 * FRAME_MS as f32 / 1000.0,
            (b + 1) as f32 * FRAME_MS as f32 / 1000.0,
        ))
    }

    /// The words (start and end in seconds from the start) cover the speech: nothing was said
    /// more than `slack` seconds after the last word, nor well before the first one.
    pub fn covered_by(&self, first_word_start: f32, last_word_end: f32, slack: f32) -> bool {
        match self.span() {
            None => true,
            Some((a, b)) => {
                // Sound at the very start is the wake word's tail and the wake (or follow-up) cue
                // coming back from the speaker, not something else said: tolerated before a
                // phrase starting within a couple of seconds (a "no" after a pause, 1.95 s).
                let wake_tail = a <= WAKE_TAIL_S && first_word_start <= WAKE_TAIL_S + 2.0;
                b <= last_word_end + slack && (wake_tail || a + 2.0 * slack >= first_word_start)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(ms: u32, amp: f32) -> Vec<i16> {
        (0..ms * 16)
            .map(|i| ((i as f32 * 0.2).sin() * amp * 32767.0) as i16)
            .collect()
    }

    fn noise(ms: u32, amp: f32) -> Vec<i16> {
        let mut x: u32 = 12345;
        (0..ms * 16)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (((x >> 16) as f32 / 32768.0 - 1.0) * amp * 32767.0) as i16
            })
            .collect()
    }

    #[test]
    fn speech_then_silence_ends() {
        let mut v = Vad::new();
        v.push(&noise(300, 0.001));
        v.push(&tone(800, 0.05));
        assert!(!v.ended());
        v.push(&noise(400, 0.001));
        assert!(!v.ended());
        v.push(&noise(400, 0.001));
        assert!(v.ended());
        let (a, b) = v.span().unwrap();
        assert!((a - 0.3).abs() < 0.05 && (b - 1.1).abs() < 0.05, "{a} {b}");
        assert!(v.covered_by(0.35, 1.0, 0.3));
        assert!(!v.covered_by(0.35, 0.6, 0.3));
    }

    #[test]
    fn quiet_room_is_nobody() {
        let mut v = Vad::new();
        v.push(&noise(5200, 0.001));
        assert!(!v.ended());
        assert!(v.nobody_spoke());
        assert_eq!(v.span(), None);
    }

    #[test]
    fn steady_noise_stops_being_speech() {
        // A fan at about -40 dBFS from the start: speech at first, then the floor catches up and
        // the utterance ends.
        let mut v = Vad::new();
        v.push(&noise(8000, 0.017));
        assert!(v.ended(), "speech {} ms", v.speech_ms());
    }
}
