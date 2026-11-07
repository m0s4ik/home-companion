// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! M0 native-library spike: microWakeWord through TensorFlow Lite Micro, and a Vosk grammar helper.
//! Everything here is small on purpose; it exists to produce measurements.

pub mod args;
pub mod detect;
pub mod grammar;
pub mod manifest;
pub mod measure;
pub mod mww;
pub mod wav;
