// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The last spoken answer, kept for "ripeti" / "repeat that": the audio Home Assistant sent (the
//! character's voice, as received) and its words. Bounded on a 2 GB device: at most
//! [`MAX_AUDIO`] of it (the start of a longer answer), forgotten after [`KEEP`].

use std::time::{Duration, Instant};

use audio::StreamFormat;

/// The most audio kept of one answer (30 s at 22 kHz, 16-bit mono: ~1.3 MB).
pub const MAX_AUDIO: Duration = Duration::from_secs(30);
/// An answer older than this is not repeated (and its audio is freed).
pub const KEEP: Duration = Duration::from_secs(10 * 60);

/// An answer that can be played again.
#[derive(Debug, Clone, PartialEq)]
pub struct LastAnswer {
    pub format: StreamFormat,
    pub pcm: Vec<u8>,
    pub text: String,
    at: Instant,
}

impl LastAnswer {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.pcm.len() as f64 / bytes_per_second(self.format).max(1) as f64)
    }
}

fn bytes_per_second(f: StreamFormat) -> usize {
    f.rate as usize * f.width as usize * f.channels.max(1) as usize
}

/// Records the answer being received, keeps the last one complete.
#[derive(Debug, Default)]
pub struct Tape {
    recording: Option<(StreamFormat, Vec<u8>)>,
    last: Option<LastAnswer>,
}

impl Tape {
    /// A new answer starts (`audio-start`): the one being recorded, if any, is dropped.
    pub fn start(&mut self, format: StreamFormat) {
        self.recording = Some((format, Vec::new()));
    }

    /// Answer audio as received; past [`MAX_AUDIO`] the rest is not kept.
    pub fn push(&mut self, pcm: &[u8]) {
        let Some((f, buf)) = &mut self.recording else {
            return;
        };
        let frame = f.width as usize * f.channels.max(1) as usize;
        let max = bytes_per_second(*f) * MAX_AUDIO.as_secs() as usize / frame.max(1) * frame;
        let room = max.saturating_sub(buf.len());
        buf.extend_from_slice(&pcm[..pcm.len().min(room)]);
    }

    /// The answer was stopped or swallowed: nothing to keep.
    pub fn cancel(&mut self) {
        self.recording = None;
    }

    /// The answer is complete (`audio-stop`): it becomes the one to repeat (an empty one does
    /// not replace the last).
    pub fn commit(&mut self, text: &str, now: Instant) {
        let Some((format, pcm)) = self.recording.take() else {
            return;
        };
        if pcm.is_empty() {
            return;
        }
        self.last = Some(LastAnswer {
            format,
            pcm,
            text: text.to_string(),
            at: now,
        });
    }

    /// The last answer, if it is recent enough to repeat.
    pub fn last(&self, now: Instant) -> Option<&LastAnswer> {
        self.last
            .as_ref()
            .filter(|a| now.saturating_duration_since(a.at) < KEEP)
    }

    /// Frees an answer too old to repeat (the core's tick).
    pub fn expire(&mut self, now: Instant) {
        if self.last(now).is_none() {
            self.last = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: StreamFormat = StreamFormat {
        rate: 1000,
        width: 2,
        channels: 1,
    };

    #[test]
    fn keeps_the_last_complete_answer() {
        let t0 = Instant::now();
        let mut t = Tape::default();
        assert!(t.last(t0).is_none());
        t.start(F);
        t.push(&[1, 0, 2, 0]);
        // Not complete yet.
        assert!(t.last(t0).is_none());
        t.commit("Ciao.", t0);
        let a = t.last(t0).unwrap();
        assert_eq!((a.pcm.len(), a.text.as_str()), (4, "Ciao."));
        assert_eq!(a.duration(), Duration::from_millis(2));
        // A stopped answer does not replace it, nor does an empty one.
        t.start(F);
        t.push(&[9, 9]);
        t.cancel();
        t.commit("x", t0);
        t.start(F);
        t.commit("vuota", t0);
        assert_eq!(t.last(t0).unwrap().text, "Ciao.");
    }

    #[test]
    fn bounded_in_size_and_time() {
        let t0 = Instant::now();
        let mut t = Tape::default();
        t.start(F);
        for _ in 0..40 {
            t.push(&[0u8; 2000]); // one second each
        }
        t.commit("lunga", t0);
        assert_eq!(t.last(t0).unwrap().duration(), MAX_AUDIO);
        assert!(t.last(t0 + KEEP - Duration::from_secs(1)).is_some());
        assert!(t.last(t0 + KEEP).is_none());
        t.expire(t0 + KEEP);
        assert!(t.last.is_none(), "freed");
    }
}
