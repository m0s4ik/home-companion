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

/// (1): negatives acted on, allowed.
pub const OBJECTIVE_MAX_FALSE_ACCEPTS: usize = 0;
/// (2): true reflexes rejected, strictly below this share.
pub const OBJECTIVE_MAX_REJECTED_SHARE: f32 = 0.05;
/// The tail slacks of rule C swept by the bench, in seconds.
pub const TAIL_SLACKS_S: [f32; 4] = [0.6, 0.8, 1.0, 1.5];
/// The partial stabilities of the early release swept by the bench, in milliseconds.
pub const STABILITY_MS: [u32; 3] = [0, 200, 400];
