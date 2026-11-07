// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! microWakeWord v2 models on TensorFlow Lite Micro: a streaming detector with ESPHome's rules.
//!
//! TFLM, the microfrontend and kissfft are compiled into the crate by `build.rs` (run
//! `tools/fetch-tflm.sh` first). Feed 16 kHz mono samples to a [`Detector`]; it returns a
//! [`Detection`] when the sliding-window rule fires. See README.md for the semantics.

mod cpu;
mod detector;
mod ffi;
mod manifest;
pub mod wav;
mod window;

pub use detector::{Detection, Detector, DetectorConfig, Stats};
pub use manifest::{Manifest, Micro, Model};

/// The only sample rate the front end takes.
pub const SAMPLE_RATE: u32 = 16_000;
/// Filterbank channels per feature slice.
pub const FEATURES: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}: {1}")]
    Manifest(std::path::PathBuf, String),
    #[error("{0}: {1}")]
    Model(std::path::PathBuf, String),
    #[error("tensor arena too small even at {0} bytes")]
    Arena(usize),
    #[error("inference failed")]
    Invoke,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// ESPHome's quantization of one front-end value to the model's int8 input:
/// `round(value * 256 / 666) - 128`, clamped (666 ~ 25.6 * 26.0, the scale used in training).
pub fn scale_feature(value: u16) -> i8 {
    ffi::scale_feature(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esphome_feature_scaling() {
        assert_eq!(scale_feature(0), -128);
        assert_eq!(scale_feature(332), 0); // 127.6 -> 128 - 128
        assert_eq!(scale_feature(670), 127); // 257.5 -> 258 - 128 = 130, clamped
        assert_eq!(scale_feature(u16::MAX), 127);
    }

    #[test]
    fn matches_the_float_formula_in_range() {
        for f in (0u16..=665).step_by(7) {
            let float = (f as f64 * 256.0) / (25.6 * 26.0) - 128.0;
            let got = scale_feature(f) as f64;
            assert!(
                (got - float.clamp(-128.0, 127.0)).abs() <= 1.0,
                "f={f} got={got} float={float}"
            );
        }
    }

    #[test]
    fn detector_is_send() {
        fn send<T: Send>() {}
        send::<Detector>();
    }
}
