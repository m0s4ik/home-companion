// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The streaming detector: the shim's interpreter + front end, ESPHome's window rule, the
//! refractory period, the arena sizing loop and the counters.

use crate::cpu::thread_cpu_seconds;
use crate::ffi;
use crate::window::{Rules, SlidingWindow};
use crate::{Error, Model, SAMPLE_RATE};
use std::ptr::NonNull;
use std::time::Duration;

/// Smallest arena tried when the size is chosen automatically.
const MIN_AUTO_ARENA: usize = 32 * 1024;

#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// Probability cutoff 0..=1; `None` = the manifest's.
    pub cutoff: Option<f32>,
    /// Sliding window in invocations; `None` = the manifest's.
    pub window: Option<usize>,
    /// No detection is checked for this long at start, after a detection and after `reset()`
    /// (ESPHome's MIN_SLICES_BEFORE_DETECTION, 1 s).
    pub cooloff: Duration,
    /// No second detection is reported within this time of the previous one, in audio time;
    /// `reset()` clears it.
    pub refractory: Duration,
    /// `None`: start from max(manifest, 32 KiB) and grow x1.5 until the model fits (up to
    /// `arena_max`). `Some(n)`: exactly n bytes, no growth (for measurements).
    pub arena_bytes: Option<usize>,
    pub arena_max: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            cutoff: None,
            window: None,
            cooloff: Duration::from_secs(1),
            refractory: Duration::from_secs(2),
            arena_bytes: None,
            arena_max: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// The model's name (the manifest's file stem).
    pub name: String,
    /// 0..1: the probability of the invocation that completed the window.
    pub probability: f32,
    /// Samples fed to the detector (since `new`, across resets) when it fired.
    pub at_samples: u64,
    /// `at_samples` as audio time.
    pub at: Duration,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Stats {
    pub samples: u64,
    pub invocations: u64,
    pub detections: u64,
    /// CPU seconds spent in `feed` by the calling thread(s).
    pub cpu_s: f64,
}

/// A microWakeWord model running on a stream of 16 kHz mono samples.
pub struct Detector {
    h: NonNull<ffi::MwwHandle>,
    name: String,
    rules: Rules,
    arena_size: usize,
    arena_used: usize,
    stride: usize,
    last_prob: u8,
    stats: Stats,
}

// SAFETY: the handle is owned exclusively by the Detector and the shim keeps no thread-affine
// state in it (the per-thread error buffer is only read inside `new`). Not Sync: `feed` mutates.
unsafe impl Send for Detector {}

impl std::fmt::Debug for Detector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Detector")
            .field("name", &self.name)
            .field("arena_size", &self.arena_size)
            .field("arena_used", &self.arena_used)
            .field("stride", &self.stride)
            .field("stats", &self.stats)
            .finish()
    }
}

fn duration_to_samples(d: Duration) -> u64 {
    (d.as_secs_f64() * SAMPLE_RATE as f64).round() as u64
}

impl Detector {
    pub fn new(model: &Model, cfg: &DetectorConfig) -> Result<Detector, Error> {
        let micro = &model.manifest.micro;
        let step_ms = i32::try_from(micro.feature_step_size).map_err(|_| {
            Error::Manifest(model.path.clone(), "feature_step_size out of range".into())
        })?;
        let (mut size, grow) = match cfg.arena_bytes {
            Some(n) => (n, false),
            None => (
                micro
                    .tensor_arena_size
                    .max(MIN_AUTO_ARENA)
                    .min(cfg.arena_max.max(1)),
                true,
            ),
        };
        let h = loop {
            // SAFETY: valid slice; the shim copies the model and returns NULL on any failure.
            let p = unsafe { ffi::mww_new(model.bytes.as_ptr(), model.bytes.len(), size, step_ms) };
            if let Some(h) = NonNull::new(p) {
                break h;
            }
            // SAFETY: plain thread-local reads, on the thread that called mww_new.
            let code = unsafe { ffi::mww_last_error() };
            let detail = ffi::last_message();
            match code {
                ffi::MWW_ERR_ARENA => {
                    let next = (size * 3 / 2).min(cfg.arena_max);
                    if !grow || next <= size {
                        log::debug!(
                            "wake word {}: arena of {size} bytes too small: {detail}",
                            model.name
                        );
                        return Err(Error::Arena(size));
                    }
                    log::debug!(
                        "wake word {}: arena of {size} bytes too small, trying {next}",
                        model.name
                    );
                    size = next;
                }
                ffi::MWW_ERR_OPS => {
                    return Err(Error::Model(model.path.clone(), format!("ops: {detail}")));
                }
                ffi::MWW_ERR_FRONTEND => {
                    return Err(Error::Model(
                        model.path.clone(),
                        format!("audio front end: {detail}"),
                    ));
                }
                ffi::MWW_ERR_MEMORY => {
                    return Err(Error::Model(model.path.clone(), "out of memory".into()));
                }
                ffi::MWW_ERR_MODEL => return Err(Error::Model(model.path.clone(), detail)),
                other => {
                    return Err(Error::Model(
                        model.path.clone(),
                        format!("error {other}: {detail}"),
                    ));
                }
            }
        };
        // SAFETY: valid handle.
        let (arena_used, stride) = unsafe {
            (
                ffi::mww_arena_used(h.as_ptr()),
                ffi::mww_stride(h.as_ptr()).max(1) as usize,
            )
        };
        let cutoff = cfg
            .cutoff
            .unwrap_or(micro.probability_cutoff)
            .clamp(0.0, 1.0);
        let window = cfg.window.unwrap_or(micro.sliding_window_size).max(1);
        let cooloff = SlidingWindow::cooloff_invocations(
            cfg.cooloff.as_secs_f64(),
            micro.feature_step_size,
            stride,
        );
        log::info!(
            "wake word {}: arena {size} bytes (manifest {}), {arena_used} used, stride {stride}, cutoff {cutoff}, window {window}, cool-off {cooloff} invocations",
            model.name,
            micro.tensor_arena_size
        );
        Ok(Detector {
            h,
            name: model.name.clone(),
            rules: Rules::new(
                SlidingWindow::new(cutoff, window, cooloff),
                duration_to_samples(cfg.refractory),
            ),
            arena_size: size,
            arena_used,
            stride,
            last_prob: 0,
            stats: Stats::default(),
        })
    }

    /// Consumes every sample (any length). Returns the detection, if any; at most one per call:
    /// should the window fire twice inside one call (only possible with a cool-off shorter than
    /// the slice), the first is returned and the others are counted in `stats` and logged.
    pub fn feed(&mut self, samples: &[i16]) -> Result<Option<Detection>, Error> {
        let cpu0 = thread_cpu_seconds();
        let mut found: Option<Detection> = None;
        let mut off = 0usize;
        let mut result = Ok(());
        while off < samples.len() {
            let mut consumed = 0usize;
            let mut prob = 0u8;
            let rest = &samples[off..];
            // SAFETY: valid handle, slice pointer/length, out-pointers to live locals.
            let rc = unsafe {
                ffi::mww_process(
                    self.h.as_ptr(),
                    rest.as_ptr(),
                    rest.len(),
                    &mut consumed,
                    &mut prob,
                )
            };
            off += consumed.min(rest.len());
            match rc {
                0 => break,
                1 => {
                    self.stats.invocations += 1;
                    self.last_prob = prob;
                    let at_samples = self.stats.samples + off as u64;
                    if self.rules.push(prob, at_samples) {
                        self.stats.detections += 1;
                        let d = Detection {
                            name: self.name.clone(),
                            probability: prob as f32 / 255.0,
                            at_samples,
                            at: Duration::from_secs_f64(at_samples as f64 / SAMPLE_RATE as f64),
                        };
                        if found.is_none() {
                            found = Some(d);
                        } else {
                            log::warn!(
                                "wake word {}: second detection in one feed at {:?} dropped",
                                self.name,
                                d.at
                            );
                        }
                    }
                }
                _ => {
                    result = Err(Error::Invoke);
                    break;
                }
            }
        }
        // Samples count as fed even after an error, so time keeps moving.
        self.stats.samples += samples.len() as u64;
        self.stats.cpu_s += thread_cpu_seconds() - cpu0;
        result.map(|()| found)
    }

    /// Back to the state after `new`: front end, model streaming state, window, cool-off, and the
    /// refractory period is cleared (it only guards one continuous listening period). Call it after
    /// streaming to the brain, before listening for the wake word again: the cool-off keeps the tail
    /// of the answer or of the user's speech from triggering. `at_samples` keeps increasing.
    pub fn reset(&mut self) {
        // SAFETY: valid handle.
        let rc = unsafe { ffi::mww_reset(self.h.as_ptr()) };
        if rc != 0 {
            log::warn!("wake word {}: model reset failed", self.name);
        }
        self.rules.reset();
        self.last_prob = 0;
    }

    pub fn arena_used(&self) -> usize {
        self.arena_used
    }

    pub fn arena_size(&self) -> usize {
        self.arena_size
    }

    /// Feature slices per invocation (one invocation every `stride` x feature step).
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// 0..1, the last invocation's probability (for a debug meter).
    pub fn last_probability(&self) -> f32 {
        self.last_prob as f32 / 255.0
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// The model's name (the manifest's file stem).
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Detector {
    fn drop(&mut self) {
        // SAFETY: the handle came from mww_new and is freed once.
        unsafe { ffi::mww_free(self.h.as_ptr()) }
    }
}
