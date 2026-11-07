// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The software mix bus of the single speaker owner (this is not ALSA's mixer API: the crate
//! never touches the card's controls).
//!
//! Every source is mono at the mixer's rate. A period is mixed in i32, scaled by the volume,
//! saturated to i16 and duplicated to every output channel. The state lives behind one mutex
//! that the playback thread takes once per period and the `Player` takes briefly to add or
//! stop a source; answer streams keep their data in their own buffer so that resampling
//! (done by the caller of `Stream::push`) never holds the mix lock.

use crate::player::Clip;
use crate::{Level, SourceId, lock};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Data of an answer stream at the mixer's rate, filled by `Stream::push`.
#[derive(Default)]
pub(crate) struct StreamBuf {
    pub data: VecDeque<f32>,
    pub finished: bool,
}

pub(crate) struct StreamShared {
    pub buf: Mutex<StreamBuf>,
    /// Gain of this stream alone (f32 bits, 0..1): music ducked under an answer. A change is
    /// ramped over one period.
    pub gain: AtomicU32,
}

impl Default for StreamShared {
    fn default() -> Self {
        Self {
            buf: Mutex::new(StreamBuf::default()),
            gain: AtomicU32::new(1f32.to_bits()),
        }
    }
}

enum Kind {
    Clip {
        clip: Arc<Clip>,
        pos: usize,
        repeats_left: u32,
        gap: usize,
        gap_left: usize,
    },
    Stream {
        shared: Arc<StreamShared>,
        started: bool,
        /// The gain applied at the end of the last period (the ramp starts there).
        gain_now: f32,
    },
}

enum Done {
    Ended(u64),
    Stopped,
}

struct Voice {
    id: SourceId,
    kind: Kind,
    /// Absolute output frame of its first sample (it starts at once if already past).
    start_at: u64,
    /// (frames left, fade length) once a stop was requested.
    fade: Option<(usize, usize)>,
    /// Absolute frame of the first sample actually mixed.
    first: Option<u64>,
    done: Option<Done>,
}

#[derive(Default)]
struct State {
    voices: Vec<Voice>,
    /// Output frames mixed so far.
    pos: u64,
    /// Sources whose last sample was mixed, with the absolute frame just after it, waiting
    /// for that frame to leave the pcm.
    pending: Vec<(SourceId, u64)>,
    acc: Vec<i32>,
    tmp: Vec<f32>,
}

/// A source whose last sample was mixed in this period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ended {
    pub id: SourceId,
    /// Absolute frame of its first mixed sample (== end when it never produced audio).
    pub start: u64,
    /// Absolute frame just after its last sample.
    pub end: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Rendered {
    pub ended: Vec<Ended>,
    pub stopped: Vec<SourceId>,
    /// A started stream ran out of data before its `finish`.
    pub starved: bool,
}

pub(crate) struct MixBus {
    rate: u32,
    state: Mutex<State>,
    cond: Condvar,
    volume: AtomicU32,
    out_rms: AtomicU32,
    out_peak: AtomicU32,
    next_id: AtomicU64,
}

impl MixBus {
    pub fn new(rate: u32, volume: f32) -> Self {
        Self {
            rate,
            state: Mutex::new(State::default()),
            cond: Condvar::new(),
            volume: AtomicU32::new(volume.clamp(0.0, 1.0).to_bits()),
            out_rms: AtomicU32::new((-120f32).to_bits()),
            out_peak: AtomicU32::new(0f32.to_bits()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Frames a stream must have buffered before it starts (100 ms), unless finished.
    fn stream_min_start(&self) -> usize {
        self.rate as usize / 10
    }

    fn fade_frames(&self) -> usize {
        (self.rate as usize / 100).max(1)
    }

    pub fn next_id(&self) -> SourceId {
        SourceId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        let v = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn output_level(&self) -> Level {
        Level {
            rms_dbfs: f32::from_bits(self.out_rms.load(Ordering::Relaxed)),
            peak: f32::from_bits(self.out_peak.load(Ordering::Relaxed)),
        }
    }

    /// Output frames mixed so far.
    #[cfg(test)]
    pub fn position(&self) -> u64 {
        lock(&self.state).pos
    }

    pub fn add_clip(
        &self,
        id: SourceId,
        clip: Arc<Clip>,
        times: u32,
        gap: Duration,
        at: Option<u64>,
    ) {
        let gap = (gap.as_secs_f64() * self.rate as f64).round() as usize;
        self.add(
            id,
            Kind::Clip {
                clip,
                pos: 0,
                repeats_left: times.max(1),
                gap,
                gap_left: 0,
            },
            at,
        );
    }

    pub fn add_stream(&self, id: SourceId, shared: Arc<StreamShared>) {
        self.add(
            id,
            Kind::Stream {
                shared,
                started: false,
                gain_now: 1.0,
            },
            None,
        );
    }

    fn add(&self, id: SourceId, kind: Kind, at: Option<u64>) {
        let mut st = lock(&self.state);
        let start_at = at.unwrap_or(st.pos);
        st.voices.push(Voice {
            id,
            kind,
            start_at,
            fade: None,
            first: None,
            done: None,
        });
        drop(st);
        self.cond.notify_all();
    }

    /// Starts a 10 ms fade; `Stopped` follows. No effect on a source already fully mixed
    /// (its `Finished` comes when it leaves the pcm).
    pub fn stop(&self, id: SourceId) {
        let fade = self.fade_frames();
        let mut st = lock(&self.state);
        for v in st
            .voices
            .iter_mut()
            .filter(|v| v.id == id && v.fade.is_none())
        {
            v.fade = Some((fade, fade));
        }
        drop(st);
        self.cond.notify_all();
    }

    pub fn stop_all(&self) {
        let fade = self.fade_frames();
        let mut st = lock(&self.state);
        for v in st.voices.iter_mut().filter(|v| v.fade.is_none()) {
            v.fade = Some((fade, fade));
        }
        drop(st);
        self.cond.notify_all();
    }

    /// True while the source is mixed, scheduled, or not yet out of the pcm.
    pub fn is_active(&self, id: SourceId) -> bool {
        let st = lock(&self.state);
        st.voices.iter().any(|v| v.id == id) || st.pending.iter().any(|p| p.0 == id)
    }

    /// Something is playing, scheduled, or waiting to leave the pcm.
    pub fn busy(&self) -> bool {
        let st = lock(&self.state);
        !st.voices.is_empty() || !st.pending.is_empty()
    }

    /// Something can be mixed right now (a clip, scheduled or playing, or a stream with data
    /// or finished). Streams waiting for their data do not count: the non-real-time file
    /// backend paces those periods like a card instead of racing ahead.
    pub fn has_work(&self) -> bool {
        let st = lock(&self.state);
        let min_start = self.stream_min_start();
        st.voices.iter().any(|v| match &v.kind {
            Kind::Clip { .. } => true,
            Kind::Stream {
                shared, started, ..
            } => {
                let b = lock(&shared.buf);
                b.finished
                    || v.fade.is_some()
                    || (*started && !b.data.is_empty())
                    || b.data.len() >= min_start
            }
        })
    }

    /// A started, unfinished stream holds less than `frames`.
    pub fn stream_starving(&self, frames: usize) -> bool {
        let st = lock(&self.state);
        st.voices.iter().any(|v| match &v.kind {
            Kind::Stream {
                shared, started, ..
            } if *started && v.fade.is_none() => {
                let b = lock(&shared.buf);
                !b.finished && b.data.len() < frames
            }
            _ => false,
        })
    }

    /// Waits until something is queued (or `timeout`).
    pub fn wait_for_work(&self, timeout: Duration) {
        let st = lock(&self.state);
        if st.voices.is_empty() && st.pending.is_empty() {
            let _ = self.cond.wait_timeout(st, timeout);
        }
    }

    /// Wakes a playback thread waiting in `wait_for_work`.
    pub fn wake(&self) {
        self.cond.notify_all();
    }

    /// Moves the timeline forward (close policy: time passed while the pcm was closed) and
    /// returns the new position, which the caller's count of written frames must follow.
    pub fn skip_to(&self, frame: u64) -> u64 {
        let mut st = lock(&self.state);
        st.pos = st.pos.max(frame);
        st.pos
    }

    /// Sources whose last sample is now out of the pcm (`played` = frames that left it).
    pub fn retire(&self, played: u64) -> Vec<SourceId> {
        let mut st = lock(&self.state);
        let mut out = Vec::new();
        st.pending.retain(|&(id, end)| {
            if end <= played {
                out.push(id);
                false
            } else {
                true
            }
        });
        out
    }

    /// Mixes `out.len() / channels` interleaved frames.
    pub fn render(&self, out: &mut [i16], channels: usize) -> Rendered {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        let min_start = self.stream_min_start();
        let mut res = Rendered::default();
        let mut st = lock(&self.state);
        let (p0, p1) = (st.pos, st.pos + frames as u64);
        let State {
            voices,
            pending,
            acc,
            tmp,
            ..
        } = &mut *st;
        acc.clear();
        acc.resize(frames, 0);
        tmp.clear();
        tmp.resize(frames, 0.0);

        for v in voices.iter_mut() {
            if v.fade.is_some() && v.first.is_none() {
                // Stopped before it made a sound: nothing to fade.
                v.done = Some(Done::Stopped);
                continue;
            }
            if v.start_at >= p1 {
                continue;
            }
            let first = (v.start_at.saturating_sub(p0) as usize).min(frames);
            let buf = &mut tmp[first..frames];
            buf.fill(0.0);
            let (n, ended, produced) = fill(&mut v.kind, buf, min_start, &mut res.starved);
            if let Kind::Stream {
                shared, gain_now, ..
            } = &mut v.kind
            {
                let target = f32::from_bits(shared.gain.load(Ordering::Relaxed)).clamp(0.0, 1.0);
                if target != 1.0 || *gain_now != 1.0 {
                    ramp(&mut buf[..n], *gain_now, target);
                }
                *gain_now = target;
            }
            if produced && v.first.is_none() {
                v.first = Some(p0 + first as u64);
            }
            let mut stopped = false;
            if let Some((left, total)) = v.fade.as_mut() {
                for s in buf[..n].iter_mut() {
                    *s *= *left as f32 / *total as f32;
                    *left = left.saturating_sub(1);
                }
                if *left == 0 || ended {
                    stopped = true;
                }
            }
            for (a, &s) in acc[first..first + n].iter_mut().zip(&buf[..n]) {
                *a += s.round() as i32;
            }
            if stopped {
                v.done = Some(Done::Stopped);
            } else if ended {
                v.done = Some(Done::Ended(p0 + (first + n) as u64));
            }
        }

        voices.retain_mut(|v| match v.done.take() {
            None => true,
            Some(Done::Stopped) => {
                res.stopped.push(v.id);
                false
            }
            Some(Done::Ended(end)) => {
                res.ended.push(Ended {
                    id: v.id,
                    start: v.first.unwrap_or(end),
                    end,
                });
                pending.push((v.id, end));
                false
            }
        });

        let vol = self.volume();
        let mut sum = 0f64;
        let mut peak = 0i32;
        for (frame, &a) in out.chunks_exact_mut(channels).zip(acc.iter()) {
            let s = saturate((a as f32 * vol).round() as i32);
            sum += (s as f64) * (s as f64);
            peak = peak.max((s as i32).abs());
            frame.fill(s);
        }
        st.pos = p1;
        drop(st);
        self.out_rms.store(
            (crate::stats::dbfs(sum, frames as u64) as f32).to_bits(),
            Ordering::Relaxed,
        );
        self.out_peak.store(
            (peak as f32 / 32768.0).min(1.0).to_bits(),
            Ordering::Relaxed,
        );
        res
    }
}

/// Fills `out` from one source. Returns (frames occupied, ended at that point, produced any
/// real sample).
fn fill(
    kind: &mut Kind,
    out: &mut [f32],
    min_start: usize,
    starved: &mut bool,
) -> (usize, bool, bool) {
    let want = out.len();
    match kind {
        Kind::Clip {
            clip,
            pos,
            repeats_left,
            gap,
            gap_left,
        } => {
            let samples = &clip.samples;
            let len = samples.len();
            let mut i = 0;
            let mut ended = false;
            let produced = true;
            loop {
                if *gap_left > 0 {
                    if i == want {
                        break;
                    }
                    let k = (*gap_left).min(want - i);
                    i += k;
                    *gap_left -= k;
                    continue;
                }
                if *pos >= len {
                    if *repeats_left > 1 {
                        *repeats_left -= 1;
                        *pos = 0;
                        *gap_left = *gap;
                        continue;
                    }
                    ended = true;
                    break;
                }
                if i == want {
                    break;
                }
                let k = (len - *pos).min(want - i);
                for (o, &s) in out[i..i + k].iter_mut().zip(&samples[*pos..*pos + k]) {
                    *o = s as f32;
                }
                *pos += k;
                i += k;
            }
            (i, ended, produced)
        }
        Kind::Stream {
            shared, started, ..
        } => {
            let mut b = lock(&shared.buf);
            if !*started {
                if b.finished || b.data.len() >= min_start {
                    *started = true;
                } else {
                    return (0, false, false);
                }
            }
            let k = want.min(b.data.len());
            for (o, s) in out[..k].iter_mut().zip(b.data.drain(..k)) {
                *o = s;
            }
            if k < want {
                if b.finished {
                    (k, true, true)
                } else {
                    *starved = true;
                    (want, false, true)
                }
            } else {
                (k, b.finished && b.data.is_empty(), true)
            }
        }
    }
}

/// Scales `buf` by a gain going linearly from `from` to `to` (no click when ducking).
fn ramp(buf: &mut [f32], from: f32, to: f32) {
    let n = buf.len().max(1) as f32;
    for (i, s) in buf.iter_mut().enumerate() {
        *s *= from + (to - from) * (i + 1) as f32 / n;
    }
}

/// i32 mix bus to i16 with saturation (never wraps, never panics).
pub(crate) fn saturate(x: i32) -> i16 {
    x.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(samples: Vec<i16>) -> Arc<Clip> {
        Arc::new(Clip { samples })
    }

    /// Renders until idle (or `max` frames), mono; returns (signal, all ended, all stopped).
    fn run(m: &MixBus, max: usize, period: usize) -> (Vec<i16>, Vec<Ended>, Vec<SourceId>) {
        let mut out = Vec::new();
        let (mut ended, mut stopped) = (Vec::new(), Vec::new());
        let mut buf = vec![0i16; period];
        while out.len() < max {
            let r = m.render(&mut buf, 1);
            out.extend_from_slice(&buf);
            ended.extend(r.ended);
            stopped.extend(r.stopped);
            let pos = m.position();
            m.retire(pos);
            if !m.busy() {
                break;
            }
        }
        (out, ended, stopped)
    }

    #[test]
    fn clip_lands_on_requested_frame_and_ends_exactly() {
        let m = MixBus::new(48000, 1.0);
        let id = m.next_id();
        m.add_clip(id, clip(vec![1000; 4800]), 1, Duration::ZERO, Some(30_000));
        let (out, ended, _) = run(&m, 96000, 960);
        assert_eq!(out.iter().position(|&s| s != 0), Some(30_000));
        assert_eq!(out.iter().rposition(|&s| s != 0), Some(34_799));
        assert_eq!(
            ended,
            vec![Ended {
                id,
                start: 30_000,
                end: 34_800
            }]
        );
    }

    #[test]
    fn overlapping_clips_saturate() {
        let m = MixBus::new(48000, 1.0);
        for v in [i16::MAX, i16::MAX, i16::MAX, i16::MIN] {
            let id = m.next_id();
            m.add_clip(id, clip(vec![v; 4800]), 1, Duration::ZERO, None);
        }
        let mut buf = vec![0i16; 480 * 2];
        m.render(&mut buf, 2);
        assert!(buf.iter().all(|&s| s == i16::MAX));
        let m = MixBus::new(48000, 1.0);
        for _ in 0..3 {
            let id = m.next_id();
            m.add_clip(id, clip(vec![i16::MIN; 480]), 1, Duration::ZERO, None);
        }
        m.render(&mut buf, 2);
        assert!(buf.iter().all(|&s| s == i16::MIN));
        assert_eq!(saturate(i32::MIN), i16::MIN);
    }

    #[test]
    fn repeated_clip_with_gaps() {
        let m = MixBus::new(1000, 1.0);
        let id = m.next_id();
        m.add_clip(id, clip(vec![5; 100]), 3, Duration::from_millis(1000), None);
        let (out, ended, _) = run(&m, 100_000, 64);
        let on: Vec<usize> = out
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] == 0 && w[1] != 0)
            .map(|(i, _)| i + 1)
            .collect();
        assert_eq!(out[0], 5);
        assert_eq!(on, vec![1100, 2200]);
        assert_eq!(ended[0].end, 2300);
    }

    #[test]
    fn volume_scales_the_mix() {
        let m = MixBus::new(48000, 0.5);
        let id = m.next_id();
        m.add_clip(id, clip(vec![10000; 960]), 1, Duration::ZERO, None);
        let mut buf = vec![0i16; 960];
        m.render(&mut buf, 1);
        assert!(buf.iter().all(|&s| s == 5000));
        assert!((m.output_level().peak - 5000.0 / 32768.0).abs() < 1e-4);
    }

    #[test]
    fn stop_fades_and_reports_stopped_only() {
        let m = MixBus::new(48000, 1.0);
        let id = m.next_id();
        m.add_clip(id, clip(vec![16000; 48000]), 1, Duration::ZERO, None);
        let mut buf = vec![0i16; 960];
        m.render(&mut buf, 1);
        m.stop(id);
        let (out, ended, stopped) = run(&m, 48000, 960);
        assert!(ended.is_empty());
        assert_eq!(stopped, vec![id]);
        // The fade is 480 frames long, linear, starting from full level.
        let steps = std::iter::once(16000 - out[0] as i32)
            .chain(out.windows(2).map(|w| w[0] as i32 - w[1] as i32))
            .map(i32::abs)
            .max()
            .unwrap();
        assert!(steps < 3277, "step {steps}");
        assert!(out[480..].iter().all(|&s| s == 0));
    }

    #[test]
    fn stream_waits_for_100_ms_then_finishes_once() {
        let m = MixBus::new(48000, 1.0);
        let id = m.next_id();
        let sh = Arc::new(StreamShared::default());
        m.add_stream(id, sh.clone());
        lock(&sh.buf).data.extend(std::iter::repeat_n(100.0, 2000));
        let mut buf = vec![0i16; 960];
        let r = m.render(&mut buf, 1);
        assert!(
            buf.iter().all(|&s| s == 0) && r.ended.is_empty(),
            "under 100 ms"
        );
        lock(&sh.buf).data.extend(std::iter::repeat_n(100.0, 3000));
        let r = m.render(&mut buf, 1);
        assert!(buf.iter().all(|&s| s == 100) && !r.starved);
        lock(&sh.buf).finished = true;
        let (out, ended, stopped) = run(&m, 48000, 960);
        assert_eq!(out.iter().filter(|&&s| s == 100).count(), 5000 - 960);
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].end - ended[0].start, 5000);
        assert!(stopped.is_empty());
    }

    #[test]
    fn finished_stream_without_data_ends_at_once() {
        let m = MixBus::new(48000, 1.0);
        let id = m.next_id();
        let sh = Arc::new(StreamShared::default());
        m.add_stream(id, sh.clone());
        lock(&sh.buf).finished = true;
        let mut buf = vec![0i16; 960];
        let r = m.render(&mut buf, 1);
        assert_eq!(r.ended.len(), 1);
        assert_eq!(r.ended[0].start, r.ended[0].end);
        assert!(m.is_active(id), "waits to leave the pcm");
        assert_eq!(m.retire(960), vec![id]);
        assert!(!m.is_active(id));
    }

    #[test]
    fn starved_stream_plays_silence_and_counts() {
        let m = MixBus::new(48000, 1.0);
        let id = m.next_id();
        let sh = Arc::new(StreamShared::default());
        m.add_stream(id, sh.clone());
        lock(&sh.buf).data.extend(std::iter::repeat_n(7.0, 4800));
        let mut buf = vec![0i16; 960];
        for _ in 0..5 {
            assert!(!m.render(&mut buf, 1).starved);
        }
        assert!(m.stream_starving(960));
        assert!(m.render(&mut buf, 1).starved);
        assert!(buf.iter().all(|&s| s == 0));
        assert!(m.is_active(id));
    }

    #[test]
    fn stream_gain_ramps_over_one_period() {
        let m = MixBus::new(48000, 1.0);
        let id = m.next_id();
        let sh = Arc::new(StreamShared::default());
        m.add_stream(id, sh.clone());
        lock(&sh.buf)
            .data
            .extend(std::iter::repeat_n(1000.0, 48000));
        let mut buf = vec![0i16; 960];
        m.render(&mut buf, 1);
        assert!(buf.iter().all(|&s| s == 1000));
        sh.gain.store(0.2f32.to_bits(), Ordering::Relaxed);
        m.render(&mut buf, 1);
        // Down smoothly: never a jump of more than a few steps, ending at the target.
        assert!(buf.windows(2).all(|w| (w[0] - w[1]).abs() <= 2), "no click");
        assert_eq!(buf[959], 200);
        m.render(&mut buf, 1);
        assert!(buf.iter().all(|&s| s == 200));
        sh.gain.store(1f32.to_bits(), Ordering::Relaxed);
        m.render(&mut buf, 1);
        assert_eq!(buf[959], 1000);
    }
}
