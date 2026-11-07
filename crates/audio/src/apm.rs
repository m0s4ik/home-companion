// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! WebRTC AGC2 (adaptive digital gain) on the capture, features `webrtc` / `webrtc-system`.
//!
//! Only the gain controller is enabled: M0 measured the full chain (AEC3 + NS + AGC2) at 28 %
//! of an A53 core, a no-go; AGC alone is the candidate (its cost is in the README). Frames are
//! 10 ms of mono f32 in [-1, 1].

#[cfg(any(feature = "webrtc", feature = "webrtc-system"))]
mod imp {
    use crate::Error;
    use webrtc_audio_processing::Processor;
    use webrtc_audio_processing::config::{
        AdaptiveDigital, Config, GainController, GainController2,
    };

    pub(crate) struct Agc {
        p: Processor,
        frame: Vec<f32>,
    }

    impl Agc {
        pub fn new(rate: u32) -> Result<Self, Error> {
            let p = Processor::new(rate).map_err(|e| Error::Device(format!("webrtc: {e}")))?;
            p.set_config(Config {
                gain_controller: Some(GainController::GainController2(GainController2 {
                    adaptive_digital: Some(AdaptiveDigital::default()),
                    ..Default::default()
                })),
                ..Default::default()
            });
            let n = p.num_samples_per_frame();
            Ok(Self {
                p,
                frame: vec![0.0; n],
            })
        }

        /// Processes a capture frame in place (its length must be a multiple of 10 ms).
        pub fn process(&mut self, samples: &mut [i16]) -> Result<(), Error> {
            let n = self.frame.len();
            for chunk in samples.chunks_exact_mut(n) {
                for (f, &s) in self.frame.iter_mut().zip(chunk.iter()) {
                    *f = s as f32 / 32768.0;
                }
                self.p
                    .process_capture_frame([self.frame.as_mut_slice()])
                    .map_err(|e| Error::Device(format!("webrtc capture: {e}")))?;
                for (s, &f) in chunk.iter_mut().zip(self.frame.iter()) {
                    *s = (f * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                }
            }
            Ok(())
        }
    }
}

#[cfg(not(any(feature = "webrtc", feature = "webrtc-system")))]
mod imp {
    use crate::Error;

    /// Stand-in without the webrtc features: creating it fails with `Error::Feature`.
    pub(crate) enum Agc {}

    impl Agc {
        pub fn new(_rate: u32) -> Result<Self, Error> {
            Err(Error::Feature("webrtc"))
        }

        pub fn process(&mut self, _samples: &mut [i16]) -> Result<(), Error> {
            match *self {}
        }
    }
}

pub(crate) use imp::Agc;
