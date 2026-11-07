// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Microphone processing: WebRTC AGC2 + noise suppression + AEC3 (feature `webrtc` or
//! `webrtc-system`).
//!
//! One `Processor` is shared by two threads, as WebRTC intends: the playback thread feeds
//! the far-end (what the speaker plays, downsampled to the capture rate) with
//! `analyze_render_frame`, the capture thread cleans the microphone with
//! `process_capture_frame`. Frames are 10 ms, mono, f32 in [-1, 1].

use std::io;
use std::sync::Arc;

#[cfg(any(feature = "webrtc", feature = "webrtc-system"))]
mod imp {
    use super::*;
    use webrtc_audio_processing::Processor;
    use webrtc_audio_processing::config::{
        AdaptiveDigital, Config, EchoCanceller, GainController, GainController2, HighPassFilter,
        NoiseSuppression, NoiseSuppressionLevel,
    };

    pub struct Apm {
        p: Processor,
    }

    impl Apm {
        pub fn new(rate: u32) -> io::Result<Arc<Self>> {
            let p = Processor::new(rate).map_err(|e| io::Error::other(format!("webrtc: {e}")))?;
            p.set_config(Config {
                high_pass_filter: Some(HighPassFilter::default()),
                echo_canceller: Some(EchoCanceller::Full {
                    stream_delay_ms: None,
                }),
                noise_suppression: Some(NoiseSuppression {
                    level: NoiseSuppressionLevel::High,
                    ..Default::default()
                }),
                gain_controller: Some(GainController::GainController2(GainController2 {
                    adaptive_digital: Some(AdaptiveDigital::default()),
                    ..Default::default()
                })),
                ..Default::default()
            });
            Ok(Arc::new(Self { p }))
        }

        pub fn frame_len(&self) -> usize {
            self.p.num_samples_per_frame()
        }

        pub fn render(&self, frame: &mut [f32]) -> io::Result<()> {
            self.p
                .analyze_render_frame([&*frame])
                .map_err(|e| io::Error::other(format!("webrtc render: {e}")))
        }

        pub fn capture(&self, frame: &mut [f32]) -> io::Result<()> {
            self.p
                .process_capture_frame([frame])
                .map_err(|e| io::Error::other(format!("webrtc capture: {e}")))
        }
    }
}

#[cfg(not(any(feature = "webrtc", feature = "webrtc-system")))]
mod imp {
    use super::*;

    /// Stand-in when built without the `webrtc` feature: creating it fails with a clear message.
    pub struct Apm;

    impl Apm {
        pub fn new(_rate: u32) -> io::Result<Arc<Self>> {
            Err(io::Error::other(
                "built without the `webrtc` feature: cargo build --features webrtc",
            ))
        }

        pub fn frame_len(&self) -> usize {
            unreachable!()
        }

        pub fn render(&self, _frame: &mut [f32]) -> io::Result<()> {
            unreachable!()
        }

        pub fn capture(&self, _frame: &mut [f32]) -> io::Result<()> {
            unreachable!()
        }
    }
}

pub use imp::Apm;
