// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The synthesized cues (no files to ship): rising for the wake word, falling when done,
//! a bright triple ring for the timer bell. A character may replace them with its own WAVs
//! later ([`crate::Clip::from_wav`]).

use crate::Clip;
use std::sync::Arc;

/// Level of the cues: 0.3 of full scale (-10.5 dBFS peak before the volume).
pub const CUE_AMP: f32 = 0.3;

/// Wake word heard: 660 Hz then 880 Hz.
pub fn wake() -> Arc<Clip> {
    Clip::tones(&[(660.0, 90), (880.0, 110)], 20, CUE_AMP)
}

/// Conversation over (or Home Assistant unreachable): 880 Hz then 660 Hz.
pub fn done() -> Arc<Clip> {
    Clip::tones(&[(880.0, 90), (660.0, 110)], 20, CUE_AMP)
}

/// One ring of the timer bell: 1319 Hz (E6) three times; repeat it with `play_repeated`.
pub fn bell() -> Arc<Clip> {
    Clip::tones(&[(1319.0, 120), (1319.0, 120), (1319.0, 120)], 80, CUE_AMP)
}

/// The test beep of spike-audio: 880 Hz, 200 ms, 10 ms fades (what `tests/mix.rs` and the
/// bench place at 1.0 s and 2.5 s).
pub fn beep() -> Arc<Clip> {
    Clip::tones(&[(880.0, 200)], 0, CUE_AMP)
}
