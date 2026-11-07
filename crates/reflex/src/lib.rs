// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Local reflexes: a closed grammar per language that Vosk decodes
//! into exactly one of its phrases, the rule that accepts a result, and the protocol between the
//! body and the `home-companion-reflex` helper (the process that runs Vosk's C++ code, so a crash there
//! never takes the screen down).
//!
//! A character's `grammar/<lang>.toml` lists the phrases of each command with slots
//! (`{duration}`, `{time}`, `{percent}`); the slot vocabularies (numbers, durations, times of
//! day) are per language in [`slots`]. The grammar expands to an explicit phrase list (for Vosk)
//! and a phrase → command map (for the body), so parsing a result is a lookup.

pub mod grammar;
pub mod proto;
pub mod slots;
pub mod vad;

pub use grammar::{Command, Decision, Grammar, Word};
