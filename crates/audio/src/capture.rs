// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The capture thread: channel pick, re-chunking to 20 ms, optional AGC, level, recorder tap,
//! fan-out to the subscribers.

use crate::apm::Agc;
use crate::backend::Capture;
use crate::stats::{StatsShared, thread_cpu_seconds};
use crate::wav::pick_frame;
use crate::{
    AudioEvent, CaptureFrame, ChannelPick, EventSink, FRAME_SAMPLES, Level, Recorder, lock,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) struct CaptureShared {
    subscribers: Mutex<Vec<SyncSender<CaptureFrame>>>,
    /// File backend in non-real-time mode: wait (up to 1 s) for room instead of dropping.
    patient: bool,
}

impl CaptureShared {
    pub fn new(patient: bool) -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
            patient,
        }
    }

    pub fn subscribe(&self, capacity: usize) -> Receiver<CaptureFrame> {
        let (tx, rx) = sync_channel(capacity);
        lock(&self.subscribers).push(tx);
        rx
    }

    /// Sends to every subscriber; returns how many frames were dropped.
    fn fan_out(&self, frame: &CaptureFrame, stop: &AtomicBool) -> u64 {
        if self.patient && frame.index == 0 {
            // Deterministic mode: the first frame waits (up to 1 s) for a subscriber, so a
            // test that subscribes right after `start` sees the capture from its first sample.
            let deadline = Instant::now() + Duration::from_secs(1);
            while lock(&self.subscribers).is_empty()
                && Instant::now() < deadline
                && !stop.load(Ordering::Relaxed)
            {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let mut subs = lock(&self.subscribers);
        let mut dropped = 0;
        subs.retain(|tx| {
            let mut item = frame.clone();
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match tx.try_send(item) {
                    Ok(()) => return true,
                    Err(TrySendError::Disconnected(_)) => return false,
                    Err(TrySendError::Full(back)) => {
                        if !self.patient
                            || Instant::now() >= deadline
                            || stop.load(Ordering::Relaxed)
                        {
                            dropped += 1;
                            return true;
                        }
                        item = back;
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        });
        dropped
    }
}

/// Development only: with `HOME_COMPANION_INJECT_DIR` set, a 16 kHz WAV dropped there as
/// `inject.wav` replaces the microphone, frame by frame at the card's pace, then the file is
/// removed and the microphone comes back. It lets recorded voices drive the running body (the
/// wake word, the reflexes, Home Assistant) for trials without a person in the room.
struct Injector {
    path: std::path::PathBuf,
    pending: std::collections::VecDeque<i16>,
    checked: Instant,
}

impl Injector {
    fn from_env() -> Option<Injector> {
        let dir = std::env::var_os("HOME_COMPANION_INJECT_DIR")?;
        let path = std::path::PathBuf::from(dir).join("inject.wav");
        log::warn!("capture: test injection on, from {}", path.display());
        Some(Injector {
            path,
            pending: Default::default(),
            checked: Instant::now(),
        })
    }

    fn replace(&mut self, frame: &mut [i16]) {
        if self.pending.is_empty() && self.checked.elapsed() >= Duration::from_millis(100) {
            self.checked = Instant::now();
            if self.path.exists() {
                match crate::wav::read_mono_16k(&self.path, ChannelPick::Left) {
                    Ok(v) => {
                        log::info!(
                            "capture: injecting {:.2} s from {}",
                            v.len() as f64 / 16_000.0,
                            self.path.display()
                        );
                        self.pending.extend(v);
                    }
                    Err(e) => log::warn!("capture: {}: {e}", self.path.display()),
                }
                let _ = std::fs::remove_file(&self.path);
            }
        }
        if self.pending.is_empty() {
            return;
        }
        for x in frame.iter_mut() {
            *x = self.pending.pop_front().unwrap_or(0);
        }
        if self.pending.is_empty() {
            log::info!("capture: injection over, the microphone again");
        }
    }
}

pub(crate) struct CapThreadCfg {
    pub pick: ChannelPick,
    pub agc: Option<Agc>,
}

pub(crate) fn capture_thread(
    mut be: Box<dyn Capture>,
    shared: Arc<CaptureShared>,
    mut cfg: CapThreadCfg,
    recorder: Option<Recorder>,
    stats: Arc<StatsShared>,
    stop: Arc<AtomicBool>,
    events: EventSink,
) {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        capture_loop(
            &mut *be,
            &shared,
            &mut cfg,
            recorder.as_ref(),
            &stats,
            &stop,
            &events,
        )
    }));
    stats.capture_cpu_s.set(thread_cpu_seconds());
    let err = match res {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("capture: {e}")),
        Err(_) => Some("capture thread panicked".to_string()),
    };
    if let Some(e) = err {
        log::error!("{e}");
        events(AudioEvent::Error(e));
    }
}

/// Turns periods of interleaved capture into 20 ms mono frames.
pub(crate) struct Rechunker {
    pick: ChannelPick,
    acc: Vec<i16>,
}

impl Rechunker {
    pub fn new(pick: ChannelPick) -> Self {
        Self {
            pick,
            acc: Vec::with_capacity(FRAME_SAMPLES * 4),
        }
    }

    pub fn push(&mut self, interleaved: &[i16], channels: usize) {
        let channels = channels.max(1);
        self.acc.extend(
            interleaved
                .chunks_exact(channels)
                .map(|f| pick_frame(f, self.pick)),
        );
    }

    /// The next complete frame, if any.
    pub fn next_frame(&mut self) -> Option<Vec<i16>> {
        (self.acc.len() >= FRAME_SAMPLES).then(|| self.acc.drain(..FRAME_SAMPLES).collect())
    }
}

fn capture_loop(
    be: &mut dyn Capture,
    shared: &CaptureShared,
    cfg: &mut CapThreadCfg,
    recorder: Option<&Recorder>,
    stats: &StatsShared,
    stop: &AtomicBool,
    events: &EventSink,
) -> Result<(), crate::Error> {
    let ch = be.channels();
    let mut buf = vec![0i16; be.period_frames() * ch];
    let mut chunks = Rechunker::new(cfg.pick);
    let mut index = 0u64;
    let mut dropped_total = 0u64;
    let mut reported_dropped = 0u64;
    let mut last_drop_event = Instant::now() - Duration::from_secs(10);
    let mut last_overruns = 0u64;
    let mut inject = Injector::from_env();

    while !stop.load(Ordering::Relaxed) {
        be.read(&mut buf)?;
        chunks.push(&buf, ch);
        while let Some(mut samples) = chunks.next_frame() {
            if let Some(i) = inject.as_mut() {
                i.replace(&mut samples);
            }
            if let Some(agc) = cfg.agc.as_mut() {
                agc.process(&mut samples)?;
            }
            let level = Level::of(&samples);
            if let Some(r) = recorder {
                r.tap(&samples);
            }
            let frame = CaptureFrame {
                samples: Arc::from(samples),
                index,
                level,
            };
            index += 1;
            dropped_total += shared.fan_out(&frame, stop);
            stats.capture_frames.store(index, Ordering::Relaxed);
            stats
                .capture_dropped
                .store(dropped_total, Ordering::Relaxed);
            if dropped_total > reported_dropped
                && last_drop_event.elapsed() >= Duration::from_secs(1)
            {
                reported_dropped = dropped_total;
                last_drop_event = Instant::now();
                log::warn!("capture: subscriber queue full, {dropped_total} frames dropped so far");
                events(AudioEvent::CaptureDropped {
                    total: dropped_total,
                });
            }
            if index % 25 == 0 {
                stats.capture_cpu_s.set(thread_cpu_seconds());
            }
        }
        let xr = be.xruns();
        stats.overruns.store(xr.overruns, Ordering::Relaxed);
        stats.cap_recoveries.store(xr.recoveries, Ordering::Relaxed);
        if xr.overruns > last_overruns {
            last_overruns = xr.overruns;
            log::warn!("capture: overrun (total {})", xr.overruns);
            events(AudioEvent::Overrun { total: xr.overruns });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechunks_any_period_to_320() {
        let mut r = Rechunker::new(ChannelPick::Right);
        let mut got = Vec::new();
        for p in 0..10 {
            let period: Vec<i16> = (0..347 * 2).map(|i| (p * 1000 + i) as i16).collect();
            r.push(&period, 2);
            while let Some(f) = r.next_frame() {
                got.push(f);
            }
        }
        assert_eq!(got.len(), 3470 / 320);
        assert!(got.iter().all(|f| f.len() == 320));
        assert_eq!(got[0][0], 1);
        // Frame 1 starts at sample 320; sample 347 is the first of period 1 (right = odd index).
        assert_eq!(got[1][27], 1001);
    }
}
