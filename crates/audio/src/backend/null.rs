// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! No devices: playback is discarded, capture is silence, both paced like a card.

use super::{Capture, Pacer, Playback, StreamSpec, Xruns};
use std::io;
use std::time::Duration;

pub(crate) struct NullPlayback {
    channels: usize,
    period: usize,
    rate: u32,
    pacer: Pacer,
}

impl NullPlayback {
    pub fn new(spec: &StreamSpec) -> Self {
        let period = spec.period_frames();
        Self {
            channels: spec.channels,
            period,
            rate: spec.rate,
            // A 4-period buffer, as the ALSA backend asks for.
            pacer: Pacer::new(spec.rate, period as u64 * 4, true),
        }
    }
}

impl Playback for NullPlayback {
    fn channels(&self) -> usize {
        self.channels
    }
    fn period_frames(&self) -> usize {
        self.period
    }
    fn write(&mut self, buf: &[i16]) -> io::Result<()> {
        self.pacer.advance(buf.len() / self.channels);
        Ok(())
    }
    fn delay_frames(&mut self) -> Option<i64> {
        self.pacer.ahead_frames()
    }
    fn xruns(&self) -> Xruns {
        Xruns::default()
    }
    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn resume(&mut self) -> io::Result<Duration> {
        // A reopened card starts a fresh clock with an empty buffer, as the ALSA backend does.
        self.pacer = Pacer::new(self.rate, self.period as u64 * 4, true);
        Ok(Duration::ZERO)
    }
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) struct NullCapture {
    channels: usize,
    period: usize,
    pacer: Pacer,
}

impl NullCapture {
    pub fn new(spec: &StreamSpec) -> Self {
        Self {
            channels: spec.channels,
            period: spec.period_frames(),
            pacer: Pacer::new(spec.rate, 0, true),
        }
    }
}

impl Capture for NullCapture {
    fn channels(&self) -> usize {
        self.channels
    }
    fn period_frames(&self) -> usize {
        self.period
    }
    fn read(&mut self, buf: &mut [i16]) -> io::Result<()> {
        buf.fill(0);
        self.pacer.advance(buf.len() / self.channels);
        Ok(())
    }
    fn xruns(&self) -> Xruns {
        Xruns::default()
    }
}
