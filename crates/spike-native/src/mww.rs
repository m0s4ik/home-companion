// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Safe wrapper over src/mww_shim.cc (TensorFlow Lite Micro + microfrontend, built by build.rs).

use std::ptr::NonNull;

pub const FEATURES: usize = 40;

#[repr(C)]
struct MwwHandle {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn mww_new(model: *const u8, len: usize, arena_bytes: usize, step_ms: i32) -> *mut MwwHandle;
    fn mww_process(
        h: *mut MwwHandle,
        samples: *const i16,
        n: usize,
        consumed: *mut usize,
        prob: *mut u8,
        features: *mut i8,
        raw: *mut u16,
    ) -> i32;
    fn mww_arena_used(h: *const MwwHandle) -> usize;
    fn mww_stride(h: *const MwwHandle) -> i32;
    fn mww_free(h: *mut MwwHandle);
    fn mww_scale_feature(value: u16) -> i8;
}

/// What one call to [`WakeWord::process`] produced.
pub enum Step {
    /// All samples consumed, no new output.
    NeedMore,
    /// A new probability (0..=255); `consumed` samples were used.
    Probability { prob: u8, consumed: usize },
    /// Debug mode only: a feature slice without a new probability.
    Features { consumed: usize },
}

pub struct WakeWord {
    h: NonNull<MwwHandle>,
    debug: bool,
    pub features: [i8; FEATURES],
    pub raw: [u16; FEATURES],
}

impl WakeWord {
    pub fn new(model: &[u8], arena_bytes: usize, step_ms: u32, debug: bool) -> Result<Self, String> {
        // SAFETY: the shim copies the model and returns NULL on any failure.
        let h = unsafe { mww_new(model.as_ptr(), model.len(), arena_bytes, step_ms as i32) };
        let h = NonNull::new(h).ok_or("cannot create the wake word model (see stderr)")?;
        Ok(Self { h, debug, features: [0; FEATURES], raw: [0; FEATURES] })
    }

    pub fn process(&mut self, samples: &[i16]) -> Result<Step, String> {
        let mut consumed = 0usize;
        let mut prob = 0u8;
        let (f, r) = if self.debug {
            (self.features.as_mut_ptr(), self.raw.as_mut_ptr())
        } else {
            (std::ptr::null_mut(), std::ptr::null_mut())
        };
        // SAFETY: valid handle, slice pointer/length, out-pointers to live locals and arrays of FEATURES.
        let rc = unsafe {
            mww_process(self.h.as_ptr(), samples.as_ptr(), samples.len(), &mut consumed, &mut prob, f, r)
        };
        match rc {
            0 => Ok(Step::NeedMore),
            1 => Ok(Step::Probability { prob, consumed }),
            2 => Ok(Step::Features { consumed }),
            _ => Err("wake word inference failed".into()),
        }
    }

    pub fn arena_used(&self) -> usize {
        unsafe { mww_arena_used(self.h.as_ptr()) }
    }

    pub fn stride(&self) -> usize {
        unsafe { mww_stride(self.h.as_ptr()) as usize }
    }
}

impl Drop for WakeWord {
    fn drop(&mut self) {
        unsafe { mww_free(self.h.as_ptr()) }
    }
}

/// ESPHome's quantization of one front-end value, as compiled into the shim.
pub fn scale_feature(value: u16) -> i8 {
    unsafe { mww_scale_feature(value) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esphome_feature_scaling() {
        // input = round(feature * 256 / 666) - 128, clamped to int8 (666 ~ 25.6 * 26.0)
        assert_eq!(scale_feature(0), -128);
        assert_eq!(scale_feature(332), 0); // 127.6 -> 128 - 128
        assert_eq!(scale_feature(670), 127); // 257.5 -> 258 - 128 = 130, clamped
        assert_eq!(scale_feature(u16::MAX), 127);
    }

    #[test]
    fn matches_the_float_formula_in_range() {
        // The float formula used in training, (f * 256) / (25.6 * 26.0) - 128, agrees within 1 step.
        for f in (0u16..=665).step_by(7) {
            let float = (f as f64 * 256.0) / (25.6 * 26.0) - 128.0;
            let got = scale_feature(f) as f64;
            assert!((got - float.clamp(-128.0, 127.0)).abs() <= 1.0, "f={f} got={got} float={float}");
        }
    }
}
