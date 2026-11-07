// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! One conversation (or announcement) and its latencies. Every conversation ends with one
//! `conversation` log line (target `home_companion::conversation`) carrying them (design section 13:
//! "added latency measured").

use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Wake,
    Talk,
    Announcement,
    /// The answer was a question: the microphone opens again by itself (Home Assistant's
    /// `continue_conversation` never reaches a Wyoming satellite).
    FollowUp,
}

impl Trigger {
    pub fn name(self) -> &'static str {
        match self {
            Trigger::Wake => "wake",
            Trigger::Talk => "talk",
            Trigger::Announcement => "announcement",
            Trigger::FollowUp => "follow_up",
        }
    }
}

pub struct Conversation {
    pub id: u64,
    pub trigger: Trigger,
    pub started_at: Instant,
    pub detection_at: Option<Instant>,
    pub run_pipeline_at: Option<Instant>,
    pub first_chunk_at: Option<Instant>,
    pub transcript_at: Option<Instant>,
    pub synth_at: Option<Instant>,
    pub audio_start_at: Option<Instant>,
    pub first_sample_at: Option<Instant>,
    pub audio_stop_at: Option<Instant>,
    pub played_at: Option<Instant>,
    /// Capture frame index of the first chunk sent (chunk timestamps start at 0).
    pub first_frame: Option<u64>,
    pub frames_sent: u64,
    pub heard: String,
    pub answer: String,
    /// Bytes of answer audio received and their format.
    pub answer_bytes: u64,
    pub answer_format: Option<wyoming::AudioFormat>,
    /// Stop pressed while listening or thinking: a late answer is swallowed and acknowledged
    /// with `played` (as the Python satellite's stop.patch did).
    pub discard_answer: bool,
    /// We ended our microphone stream with `audio-stop` (the Stop button).
    pub audio_stop_sent: bool,
    /// The answer's audio stream, while it plays.
    pub stream: Option<audio::Stream>,
}

/// The fields of the `conversation` line, kept for tests.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: u64,
    pub trigger: &'static str,
    pub outcome: String,
    pub detect_to_run_pipeline_ms: Option<f64>,
    pub audio_stop_to_played_ms: Option<f64>,
    pub audio_start_to_first_sample_ms: Option<f64>,
    pub audio_seconds_sent: f64,
    pub answer_seconds: f64,
    pub heard: String,
    pub answer: String,
}

fn ms(a: Option<Instant>, b: Option<Instant>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) if b >= a => Some((b - a).as_secs_f64() * 1e3),
        _ => None,
    }
}

impl Conversation {
    pub fn new(id: u64, trigger: Trigger) -> Conversation {
        Conversation {
            id,
            trigger,
            started_at: Instant::now(),
            detection_at: None,
            run_pipeline_at: None,
            first_chunk_at: None,
            transcript_at: None,
            synth_at: None,
            audio_start_at: None,
            first_sample_at: None,
            audio_stop_at: None,
            played_at: None,
            first_frame: None,
            frames_sent: 0,
            heard: String::new(),
            answer: String::new(),
            answer_bytes: 0,
            answer_format: None,
            discard_answer: false,
            audio_stop_sent: false,
            stream: None,
        }
    }

    pub fn answer_seconds(&self) -> f64 {
        match &self.answer_format {
            Some(f) if f.rate > 0 => {
                self.answer_bytes as f64
                    / (f.rate as f64 * f.width.max(1) as f64 * f.channels.max(1) as f64)
            }
            _ => 0.0,
        }
    }

    pub fn record(&self, outcome: &str) -> Record {
        Record {
            id: self.id,
            trigger: self.trigger.name(),
            outcome: outcome.to_string(),
            detect_to_run_pipeline_ms: ms(self.detection_at, self.run_pipeline_at),
            audio_stop_to_played_ms: ms(self.audio_stop_at, self.played_at),
            audio_start_to_first_sample_ms: ms(self.audio_start_at, self.first_sample_at),
            audio_seconds_sent: self.frames_sent as f64 * audio::FRAME_MS as f64 / 1000.0,
            answer_seconds: self.answer_seconds(),
            heard: self.heard.clone(),
            answer: self.answer.clone(),
        }
    }

    /// Writes the `conversation` line and returns its record.
    pub fn log(&self, outcome: &str) -> Record {
        let r = self.record(outcome);
        let o = |v: Option<f64>| v.map_or(-1.0, |x| (x * 10.0).round() / 10.0);
        log::info!(
            target: "home_companion::conversation",
            event = "conversation",
            id = self.id,
            trigger = r.trigger,
            detect_to_run_pipeline_ms = o(r.detect_to_run_pipeline_ms),
            run_pipeline_to_first_chunk_ms = o(ms(self.run_pipeline_at, self.first_chunk_at)),
            first_chunk_to_transcript_ms = o(ms(self.first_chunk_at, self.transcript_at)),
            transcript_to_audio_start_ms = o(ms(self.transcript_at, self.audio_start_at)),
            audio_start_to_first_sample_ms = o(r.audio_start_to_first_sample_ms),
            audio_stop_to_played_ms = o(r.audio_stop_to_played_ms),
            total_ms = o(ms(Some(self.started_at), Some(Instant::now()))),
            audio_seconds_sent = r.audio_seconds_sent,
            answer_seconds = (r.answer_seconds * 1000.0).round() / 1000.0,
            heard = r.heard.as_str(),
            answer = r.answer.as_str(),
            outcome = outcome;
            "conversation {} ({}) ended: {outcome}", self.id, r.trigger
        );
        r
    }
}
