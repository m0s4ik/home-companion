// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the rest of the program plays (`Player`, `Clip`, `Stream`) and the playback thread.

use crate::backend::Playback;
use crate::mixer::{MixBus, StreamShared};
use crate::resample::{Quality, StreamResampler, resample_clip};
use crate::stats::{StatsShared, thread_cpu_seconds, try_realtime};
use crate::{AudioEvent, Error, EventSink, Level, OUTPUT_RATE, SourceId, lock};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A sound held in memory: mono at [`OUTPUT_RATE`].
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub samples: Vec<i16>,
}

impl Clip {
    /// Loads a 16-bit PCM WAV of any rate and channel count (channels averaged), resampled
    /// once to [`OUTPUT_RATE`].
    pub fn from_wav(path: &Path) -> Result<Arc<Clip>, Error> {
        let (mono, rate) = crate::wav::read_mono(path, crate::ChannelPick::Average)?;
        Ok(Arc::new(Clip {
            samples: resample_clip(&mono, rate, OUTPUT_RATE),
        }))
    }

    /// Sine notes one after the other (`gap_ms` of silence between them), each with 10 ms
    /// linear fades, at `amp` of full scale. The cues are in [`crate::cues`].
    pub fn tones(notes: &[(f32, u32)], gap_ms: u32, amp: f32) -> Arc<Clip> {
        let rate = OUTPUT_RATE as f32;
        let amp = amp.clamp(0.0, 1.0);
        let gap = (OUTPUT_RATE as u64 * gap_ms as u64 / 1000) as usize;
        let fade = (OUTPUT_RATE / 100) as usize;
        let mut samples = Vec::new();
        for (k, &(freq, ms)) in notes.iter().enumerate() {
            if k > 0 {
                samples.extend(std::iter::repeat_n(0i16, gap));
            }
            let n = (OUTPUT_RATE as u64 * ms as u64 / 1000) as usize;
            samples.extend((0..n).map(|i| {
                let env = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
                let x = (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin();
                (x * env * amp * 32767.0).round() as i16
            }));
        }
        Arc::new(Clip { samples })
    }

    pub fn silence(ms: u32) -> Arc<Clip> {
        Arc::new(Clip {
            samples: vec![0; (OUTPUT_RATE as u64 * ms as u64 / 1000) as usize],
        })
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / OUTPUT_RATE as f64)
    }
}

/// Format of an answer or announcement stream as received (Wyoming `audio-start`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub rate: u32,
    /// Bytes per sample, signed little endian: 1, 2 (what Home Assistant sends) or 4.
    pub width: u16,
    pub channels: u16,
}

/// Plays sounds through the mixer. Cheap to clone; every method returns at once.
#[derive(Clone)]
pub struct Player(Arc<MixBus>);

impl Player {
    pub(crate) fn new(m: Arc<MixBus>) -> Self {
        Self(m)
    }

    fn at_mixer_rate(&self, clip: &Arc<Clip>) -> Arc<Clip> {
        if self.0.rate() == OUTPUT_RATE {
            clip.clone()
        } else {
            // cpal on a device that is not at 48 kHz (simulator only).
            Arc::new(Clip {
                samples: resample_clip(&clip.samples, OUTPUT_RATE, self.0.rate()),
            })
        }
    }

    /// Starts at once (the next period), mixed over anything else.
    pub fn play(&self, clip: &Arc<Clip>) -> SourceId {
        self.play_repeated(clip, 1, Duration::ZERO)
    }

    /// Plays the clip `times` times with `gap` of silence between (the timer bell).
    pub fn play_repeated(&self, clip: &Arc<Clip>, times: u32, gap: Duration) -> SourceId {
        let id = self.0.next_id();
        self.0
            .add_clip(id, self.at_mixer_rate(clip), times, gap, None);
        id
    }

    /// Plays the clip at an absolute position of the output timeline (time since `start`,
    /// counted in mixed frames; it starts at once if that moment has passed). Meant for tests
    /// and benches with the file backend, where it places a sound on an exact frame.
    pub fn play_at(&self, clip: &Arc<Clip>, at: Duration) -> SourceId {
        let id = self.0.next_id();
        let frame = (at.as_secs_f64() * self.0.rate() as f64).round() as u64;
        self.0
            .add_clip(id, self.at_mixer_rate(clip), 1, Duration::ZERO, Some(frame));
        id
    }

    /// Opens an answer/announcement stream: any rate (8-48 kHz), width 1/2/4, 1-2 channels.
    pub fn open_stream(&self, format: StreamFormat) -> Stream {
        let id = self.0.next_id();
        let shared = Arc::new(StreamShared::default());
        let conv = StreamConv {
            format,
            rs: StreamResampler::new(format.rate.max(1), self.0.rate(), Quality::Cubic),
            leftover: Vec::new(),
            scratch: Vec::new(),
        };
        self.0.add_stream(id, shared.clone());
        Stream {
            id,
            shared,
            conv: Mutex::new(conv),
            mixer: self.0.clone(),
            done: AtomicBool::new(false),
        }
    }

    /// 10 ms fade, then `Stopped`.
    pub fn stop(&self, id: SourceId) {
        self.0.stop(id);
    }

    pub fn stop_all(&self) {
        self.0.stop_all();
    }

    pub fn set_volume(&self, v: f32) {
        self.0.set_volume(v);
    }

    pub fn volume(&self) -> f32 {
        self.0.volume()
    }

    /// True from `play` until its `Finished` or `Stopped`.
    pub fn is_active(&self, id: SourceId) -> bool {
        self.0.is_active(id)
    }

    /// Level of the last mixed period (after the volume), for the UI wave while speaking.
    pub fn output_level(&self) -> Level {
        self.0.output_level()
    }
}

struct StreamConv {
    format: StreamFormat,
    rs: StreamResampler,
    /// Bytes of an incomplete frame left from the previous push.
    leftover: Vec<u8>,
    scratch: Vec<f32>,
}

/// An answer or announcement being received. Dropping it without `finish` or `abort`
/// finishes it (whatever was pushed plays out and `Finished` follows).
pub struct Stream {
    id: SourceId,
    shared: Arc<StreamShared>,
    conv: Mutex<StreamConv>,
    mixer: Arc<MixBus>,
    done: AtomicBool,
}

impl Stream {
    pub fn id(&self) -> SourceId {
        self.id
    }

    /// Interleaved bytes as received: converted, downmixed to mono and resampled into the
    /// source's buffer. Playback starts once 100 ms are buffered or on `finish`. Never blocks.
    pub fn push(&self, pcm: &[u8]) -> Result<(), Error> {
        if self.done.load(Ordering::Relaxed) {
            return Err(Error::Unsupported(
                "push after finish/abort on an audio stream".into(),
            ));
        }
        let mut c = lock(&self.conv);
        let width = c.format.width as usize;
        let channels = c.format.channels.max(1) as usize;
        if !matches!(width, 1 | 2 | 4) {
            return Err(Error::Unsupported(format!("sample width {width} bytes")));
        }
        let frame_bytes = width * channels;
        let StreamConv {
            rs,
            leftover,
            scratch,
            ..
        } = &mut *c;
        let joined: Vec<u8>;
        let data: &[u8] = if leftover.is_empty() {
            pcm
        } else {
            joined = [leftover.as_slice(), pcm].concat();
            leftover.clear();
            &joined
        };
        let whole = data.len() / frame_bytes * frame_bytes;
        leftover.extend_from_slice(&data[whole..]);
        scratch.clear();
        scratch.extend(data[..whole].chunks_exact(frame_bytes).map(|f| {
            let sum: f32 = f.chunks_exact(width).map(sample_i16_scale).sum();
            sum / channels as f32
        }));
        rs.push(scratch);
        let mut b = lock(&self.shared.buf);
        rs.drain_into(&mut b.data);
        Ok(())
    }

    /// No more data: flushes the resampler. `Finished` follows when the last sample has left
    /// the pcm.
    pub fn finish(&self) {
        if self.done.swap(true, Ordering::Relaxed) {
            return;
        }
        let mut c = lock(&self.conv);
        c.rs.finish();
        let mut b = lock(&self.shared.buf);
        c.rs.drain_into(&mut b.data);
        b.finished = true;
        drop(b);
        self.mixer.wake();
    }

    /// = `Player::stop(id)`.
    pub fn abort(&self) {
        self.done.store(true, Ordering::Relaxed);
        self.mixer.stop(self.id);
    }

    /// This stream's own gain, 0..1 (1 = as pushed), ramped over one period: music ducked
    /// under an answer. The player's volume still applies on top.
    pub fn set_gain(&self, gain: f32) {
        let g = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.shared.gain.store(g.to_bits(), Ordering::Relaxed);
    }

    /// Audio pushed but not yet mixed.
    pub fn queued(&self) -> Duration {
        let n = lock(&self.shared.buf).data.len();
        Duration::from_secs_f64(n as f64 / self.mixer.rate() as f64)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.finish();
    }
}

/// One sample of `width` bytes, signed little endian, scaled to the i16 range.
fn sample_i16_scale(b: &[u8]) -> f32 {
    match b.len() {
        1 => (b[0] as i8) as f32 * 256.0,
        2 => i16::from_le_bytes([b[0], b[1]]) as f32,
        _ => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 65536.0,
    }
}

pub(crate) struct PlayThreadCfg {
    pub keep_open: bool,
    pub idle_close: Duration,
    pub realtime_prio: bool,
    /// false only for the file backend in non-real-time mode.
    pub realtime_pacing: bool,
}

/// The single owner of the speaker. Never returns early except on stop or a device error.
pub(crate) fn playback_thread(
    mut be: Box<dyn Playback>,
    mixer: Arc<MixBus>,
    cfg: PlayThreadCfg,
    stats: Arc<StatsShared>,
    stop: Arc<AtomicBool>,
    events: EventSink,
) {
    if cfg.realtime_prio {
        try_realtime("audio-play");
    }
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        playback_loop(&mut *be, &mixer, &cfg, &stats, &stop, &events)
    }));
    let err = match res {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("playback: {e}")),
        Err(_) => Some("playback thread panicked".to_string()),
    };
    if let Err(e) = be.finish() {
        log::warn!("playback: closing: {e}");
    }
    stats.pcm_open.store(false, Ordering::Relaxed);
    stats.playback_cpu_s.set(thread_cpu_seconds());
    if let Some(e) = err {
        log::error!("{e}");
        events(AudioEvent::Error(e));
    }
}

fn playback_loop(
    be: &mut dyn Playback,
    mixer: &MixBus,
    cfg: &PlayThreadCfg,
    stats: &StatsShared,
    stop: &AtomicBool,
    events: &EventSink,
) -> std::io::Result<()> {
    let ch = be.channels();
    let rate = mixer.rate();
    let mut buf = vec![0i16; be.period_frames() * ch];
    let mut open = be.is_open();
    stats.pcm_open.store(open, Ordering::Relaxed);
    let mut written: u64 = 0;
    let mut idle_since = Instant::now();
    let mut last_underruns = 0u64;
    let mut periods = 0u64;
    let t0 = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let busy = mixer.busy();
        if busy {
            idle_since = Instant::now();
        }
        if !cfg.keep_open && !busy {
            if open && idle_since.elapsed() >= cfg.idle_close {
                be.suspend()?;
                open = false;
                stats.pcm_open.store(false, Ordering::Relaxed);
                stats.queued_ms.set(f64::NAN);
                log::debug!("playback: idle for {:?}, pcm closed", cfg.idle_close);
            }
            if !open {
                mixer.wait_for_work(Duration::from_millis(200));
                continue;
            }
        }
        if !open {
            let took = be.resume()?;
            open = true;
            stats.pcm_open.store(true, Ordering::Relaxed);
            log::info!(
                "playback: pcm reopened in {:.1} ms",
                took.as_secs_f64() * 1e3
            );
            // Time passed while closed: sounds placed with play_at stay on the wall clock.
            // `written` follows the mixer's timeline, otherwise every end frame in `pending`
            // would sit ahead of `played` by the time spent closed and `Finished` would come
            // that much late (the reopened card's delay restarts from zero).
            let pos = mixer.skip_to((t0.elapsed().as_secs_f64() * rate as f64) as u64);
            written = written.max(pos);
            let period = be.period_frames();
            if buf.len() != period * ch {
                buf = vec![0i16; period * ch];
            }
        }
        if !cfg.realtime_pacing {
            // Deterministic mode: give a starving answer stream a moment to receive its data
            // instead of mixing silence into it.
            let waited = Instant::now();
            while mixer.stream_starving(buf.len() / ch)
                && waited.elapsed() < Duration::from_millis(50)
                && !stop.load(Ordering::Relaxed)
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            be.hint_busy(mixer.has_work());
        }
        let r = mixer.render(&mut buf, ch);
        if r.starved {
            stats.stream_starved.fetch_add(1, Ordering::Relaxed);
        }
        be.write(&buf)?;
        written += (buf.len() / ch) as u64;
        periods += 1;
        let delay = be.delay_frames();
        let played = written.saturating_sub(delay.unwrap_or(0).max(0) as u64);
        for id in r.stopped {
            events(AudioEvent::Stopped(id));
        }
        for id in mixer.retire(played) {
            events(AudioEvent::Finished(id));
        }
        stats
            .queued_ms
            .set(delay.map_or(f64::NAN, |d| d as f64 * 1000.0 / rate as f64));
        stats.playback_audio_s.set(written as f64 / rate as f64);
        let xr = be.xruns();
        stats.underruns.store(xr.underruns, Ordering::Relaxed);
        stats
            .play_recoveries
            .store(xr.recoveries, Ordering::Relaxed);
        if xr.underruns > last_underruns {
            last_underruns = xr.underruns;
            log::warn!("playback: underrun (total {})", xr.underruns);
            events(AudioEvent::Underrun {
                total: xr.underruns,
            });
        }
        if periods % 25 == 0 {
            stats.playback_cpu_s.set(thread_cpu_seconds());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_conversions() {
        assert_eq!(sample_i16_scale(&[0x80]), -32768.0);
        assert_eq!(sample_i16_scale(&[0x7f]), 127.0 * 256.0);
        assert_eq!(sample_i16_scale(&(-1234i16).to_le_bytes()), -1234.0);
        assert_eq!(sample_i16_scale(&(i32::MIN).to_le_bytes()), -32768.0);
    }

    #[test]
    fn tones_have_the_requested_shape() {
        let c = Clip::tones(&[(880.0, 200), (660.0, 100)], 50, 0.3);
        assert_eq!(c.samples.len(), 48000 * 350 / 1000);
        assert_eq!(c.duration(), Duration::from_millis(350));
        let peak = c.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((9700..=9831).contains(&peak), "{peak}");
        assert!(c.samples[9600..12000].iter().all(|&s| s == 0), "gap");
    }

    #[test]
    fn partial_frames_are_kept_between_pushes() {
        let m = Arc::new(MixBus::new(48000, 1.0));
        let p = Player::new(m.clone());
        let s = p.open_stream(StreamFormat {
            rate: 48000,
            width: 2,
            channels: 2,
        });
        let bytes: Vec<u8> = (0..100i16)
            .flat_map(|i| [i * 10, i * 10 + 2])
            .flat_map(|v| v.to_le_bytes())
            .collect();
        for c in bytes.chunks(3) {
            s.push(c).unwrap();
        }
        s.finish();
        let b = lock(&s.shared.buf);
        assert_eq!(b.data.len(), 100);
        assert_eq!(b.data[7], 71.0);
        assert!(b.finished);
    }
}
