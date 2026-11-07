// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Microphone levels per channel, with only the capture device open (the speaker is not
//! touched). `home-companion mic-levels` and `audio-bench levels` use it on the device to settle the M0
//! open point on the microphone level and to choose `capture_channel`.

use crate::backend::open_capture;
use crate::{AudioConfig, Error, Level};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Levels over one interval. With a mono capture all three are the same.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct ChannelLevels {
    /// End of the interval, seconds of audio since the start.
    pub at_s: f64,
    pub left_rms_dbfs: f32,
    pub left_peak: f32,
    pub right_rms_dbfs: f32,
    pub right_peak: f32,
    pub average_rms_dbfs: f32,
    pub average_peak: f32,
}

#[derive(Default)]
struct Acc {
    sum: f64,
    peak: i32,
    n: u64,
}

impl Acc {
    fn add(&mut self, s: i32) {
        self.sum += (s as f64) * (s as f64);
        self.peak = self.peak.max(s.abs());
        self.n += 1;
    }
    fn level(&self) -> Level {
        Level {
            rms_dbfs: crate::stats::dbfs(self.sum, self.n) as f32,
            peak: (self.peak as f32 / 32768.0).min(1.0),
        }
    }
}

/// Captures for `duration` and calls `report` every `every` of audio.
pub fn capture_levels(
    cfg: &AudioConfig,
    duration: Duration,
    every: Duration,
    mut report: impl FnMut(&ChannelLevels),
) -> Result<(), Error> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut be = open_capture(cfg, stop)?;
    let ch = be.channels();
    let mut buf = vec![0i16; be.period_frames() * ch];
    let rate = crate::CAPTURE_RATE as f64;
    let total = (duration.as_secs_f64() * rate) as u64;
    let step = ((every.as_secs_f64() * rate) as u64).max(1);
    let (mut frames, mut next) = (0u64, step);
    let mut acc: [Acc; 3] = Default::default();
    while frames < total {
        be.read(&mut buf)?;
        for f in buf.chunks_exact(ch) {
            let l = f[0] as i32;
            let r = if ch > 1 { f[1] as i32 } else { l };
            acc[0].add(l);
            acc[1].add(r);
            acc[2].add((l + r) >> 1);
            frames += 1;
            if frames >= next || frames == total {
                let [a, b, c] = [acc[0].level(), acc[1].level(), acc[2].level()];
                report(&ChannelLevels {
                    at_s: frames as f64 / rate,
                    left_rms_dbfs: a.rms_dbfs,
                    left_peak: a.peak,
                    right_rms_dbfs: b.rms_dbfs,
                    right_peak: b.peak,
                    average_rms_dbfs: c.rms_dbfs,
                    average_peak: c.peak,
                });
                acc = Default::default();
                next += step;
                if frames == total {
                    break;
                }
            }
        }
    }
    Ok(())
}
