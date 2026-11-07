// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The playback and capture threads, and what they report.

use crate::apm::Apm;
use crate::backend::{Capture, Playback, Xruns};
use crate::measure::{self, Summary, cpu_seconds, ms};
use crate::mix::{MixBus, Source, StreamResampler};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the playback thread does when nothing is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Idle {
    /// Keep the pcm open and write silence.
    KeepOpen,
    /// Drain and close the pcm; reopen on the next sound.
    Close,
}

pub enum Msg {
    Play(Source),
    /// No more sounds: stop when the run length is reached (keep-open) or when idle (close).
    Stop,
}

/// State shared by the threads of one run.
#[derive(Default)]
pub struct Shared {
    /// True while the mixer has something to play (to split capture RMS by playback state).
    pub playing: AtomicBool,
    /// Set by the main thread at the end of the run.
    pub stop: AtomicBool,
    /// When the first frame of the echo click was handed to the pcm.
    pub click_handed: Mutex<Option<Instant>>,
}

pub struct PlayCfg {
    pub idle: Idle,
    pub rate: u32,
    pub total_frames: u64,
    pub lead_silence_frames: usize,
    /// The timeline follows the wall clock (false only for the file backend with --no-realtime).
    pub realtime: bool,
    pub rt: bool,
    pub t0: Instant,
    /// Far-end reference for echo cancellation: (processor, capture rate).
    pub far_end: Option<(Arc<Apm>, u32)>,
}

#[derive(Default)]
pub struct PlayReport {
    pub periods: u64,
    pub period_frames: usize,
    pub xruns: Xruns,
    pub delay_ms: Vec<f64>,
    /// (label, request -> handed to the pcm ms, request -> estimated at the DAC ms)
    pub first_frame: Vec<(String, f64, f64)>,
    pub reopen_ms: Vec<f64>,
    pub cpu_s: f64,
    pub wall_s: f64,
    pub busy_cpu_s: f64,
    pub busy_wall_s: f64,
    pub idle_cpu_s: f64,
    pub idle_wall_s: f64,
    pub closed_wall_s: f64,
    pub rt: Option<String>,
    pub webrtc_render_ms: Vec<f64>,
    pub error: Option<String>,
}

impl PlayReport {
    pub fn json(&self) -> serde_json::Value {
        let pct = |c: f64, w: f64| {
            if w > 0.0 {
                measure::round3(100.0 * c / w)
            } else {
                0.0
            }
        };
        serde_json::json!({
            "periods": self.periods,
            "period_frames": self.period_frames,
            "underruns": self.xruns.underruns,
            "recoveries": self.xruns.recoveries,
            "delay_ms": Summary::of(&self.delay_ms).json(),
            "first_frame": self.first_frame.iter().map(|(l, h, d)| serde_json::json!({
                "source": l, "request_to_handed_ms": measure::round3(*h), "request_to_dac_est_ms": measure::round3(*d)
            })).collect::<Vec<_>>(),
            "reopen_ms": self.reopen_ms.iter().map(|&x| measure::round3(x)).collect::<Vec<_>>(),
            "thread_cpu_pct_of_core": pct(self.cpu_s, self.wall_s),
            "busy_cpu_pct_of_core": pct(self.busy_cpu_s, self.busy_wall_s),
            "busy_wall_s": measure::round3(self.busy_wall_s),
            "idle_silence_cpu_pct_of_core": pct(self.idle_cpu_s, self.idle_wall_s),
            "idle_silence_wall_s": measure::round3(self.idle_wall_s),
            "closed_wall_s": measure::round3(self.closed_wall_s),
            "rt": self.rt,
            "webrtc_render_ms_per_10ms": Summary::of(&self.webrtc_render_ms).json(),
            "error": self.error,
        })
    }
}

/// The single owner of the speaker: mixes queued sources, writes periods, measures.
pub fn playback_thread(
    mut be: Box<dyn Playback>,
    rx: Receiver<Msg>,
    cfg: PlayCfg,
    shared: Arc<Shared>,
    initially_open: bool,
) -> PlayReport {
    let mut rep = PlayReport::default();
    if cfg.rt {
        rep.rt = Some(measure::try_realtime("playback"));
    }
    if let Err(e) = playback_loop(&mut *be, &rx, &cfg, &shared, initially_open, &mut rep) {
        eprintln!("playback: {e}");
        rep.error = Some(e.to_string());
    }
    if let Err(e) = be.finish() {
        eprintln!("playback: {e}");
    }
    rep.xruns = be.xruns();
    rep.period_frames = be.period_frames();
    rep.cpu_s = cpu_seconds(true);
    rep.wall_s = cfg.t0.elapsed().as_secs_f64();
    shared.playing.store(false, Ordering::Relaxed);
    rep
}

fn playback_loop(
    be: &mut dyn Playback,
    rx: &Receiver<Msg>,
    cfg: &PlayCfg,
    shared: &Shared,
    mut open: bool,
    rep: &mut PlayReport,
) -> std::io::Result<()> {
    let ch = be.channels();
    let mut mixer = MixBus::new(ch);
    let mut buf = vec![0i16; be.period_frames() * ch];
    let mut stop = false;
    let mut since_open = 0u32;
    let (mut far_rs, mut far_buf, mut far_frame) = match &cfg.far_end {
        Some((apm, cap_rate)) => (
            Some(StreamResampler::new(cfg.rate, *cap_rate)),
            Vec::new(),
            vec![0f32; apm.frame_len()],
        ),
        None => (None, Vec::new(), Vec::new()),
    };
    let mut last_cpu = cpu_seconds(true);
    let mut last_wall = Instant::now();
    let mut last_busy = false;

    loop {
        for msg in rx.try_iter() {
            match msg {
                Msg::Play(src) => mixer.add(src),
                Msg::Stop => stop = true,
            }
        }
        // Account the CPU of the previous period as busy (sounds) or idle (silence).
        let (cpu, now) = (cpu_seconds(true), Instant::now());
        let (dc, dw) = (cpu - last_cpu, now.duration_since(last_wall).as_secs_f64());
        if last_busy {
            rep.busy_cpu_s += dc;
            rep.busy_wall_s += dw;
        } else if open {
            rep.idle_cpu_s += dc;
            rep.idle_wall_s += dw;
        }
        (last_cpu, last_wall) = (cpu, now);

        let busy = mixer.busy();
        shared.playing.store(busy, Ordering::Relaxed);
        if cfg.idle == Idle::Close && !busy {
            if stop {
                return Ok(());
            }
            if open {
                be.suspend()?;
                open = false;
            }
            // Closed: wait for the next sound without spending CPU.
            let t = Instant::now();
            let msg = rx.recv_timeout(Duration::from_millis(200));
            rep.closed_wall_s += t.elapsed().as_secs_f64();
            (last_cpu, last_wall) = (cpu_seconds(true), Instant::now());
            match msg {
                Ok(Msg::Play(src)) => mixer.add(src),
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => stop = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
            continue;
        }
        if cfg.idle == Idle::KeepOpen && mixer.position() >= cfg.total_frames {
            return Ok(());
        }
        if !open {
            let took = be.resume()?;
            rep.reopen_ms.push(ms(took));
            println!("reopen: {:.1} ms (open + hw_params + prepare)", ms(took));
            open = true;
            since_open = 0;
            if cfg.realtime {
                mixer.skip_to((cfg.t0.elapsed().as_secs_f64() * cfg.rate as f64) as u64);
            }
            let silence = vec![0i16; be.period_frames() * ch];
            let mut left = cfg.lead_silence_frames;
            while left > 0 {
                let n = left.min(be.period_frames());
                be.write(&silence[..n * ch])?;
                left -= n;
            }
        }

        let started = mixer.render(&mut buf);
        last_busy = busy;
        let delay = be.delay_frames();
        if let Some(d) = delay
            && since_open >= 4
        {
            rep.delay_ms.push(d as f64 * 1000.0 / cfg.rate as f64);
        }
        let handed = Instant::now();
        for s in started {
            let at_dac = handed
                + Duration::from_secs_f64(
                    (delay.unwrap_or(0) as f64 + s.offset_frames as f64) / cfg.rate as f64,
                );
            if s.label == "click" {
                *shared.click_handed.lock().unwrap() = Some(
                    handed + Duration::from_secs_f64(s.offset_frames as f64 / cfg.rate as f64),
                );
            }
            rep.first_frame
                .push((s.label, ms(handed - s.requested), ms(at_dac - s.requested)));
        }
        be.write(&buf)?;
        rep.periods += 1;
        since_open += 1;

        if let (Some((apm, _)), Some(rs)) = (&cfg.far_end, far_rs.as_mut()) {
            far_buf.clear();
            far_buf.extend(buf.iter().step_by(ch).map(|&s| s as f32 / 32768.0));
            rs.push(&far_buf);
            while rs.available() >= far_frame.len() {
                rs.pop(&mut far_frame);
                let t = Instant::now();
                apm.render(&mut far_frame)?;
                rep.webrtc_render_ms.push(ms(t.elapsed()));
            }
        }
    }
}

pub struct CapCfg {
    pub total_frames: u64,
    pub rt: bool,
    pub t0: Instant,
    pub apm: Option<Arc<Apm>>,
    /// Also send the raw mono signal for frames below this index (echo click analysis).
    pub keep_raw_frames: u64,
}

/// One period from the capture thread to the main thread.
pub struct CapMsg {
    /// Interleaved raw capture, or processed mono when WebRTC is on.
    pub data: Vec<i16>,
    pub raw_mono: Option<Vec<i16>>,
    /// Capture frame index just after this period, and when the read returned.
    pub end_frame: u64,
    pub read_at: Instant,
}

#[derive(Default)]
pub struct CapReport {
    pub periods: u64,
    pub period_frames: usize,
    pub xruns: Xruns,
    pub dropped_msgs: u64,
    pub cpu_s: f64,
    pub wall_s: f64,
    pub rt: Option<String>,
    pub webrtc_capture_ms: Vec<f64>,
    /// Sum of squares and counts, mono: [raw while playing, raw idle, processed playing, processed idle].
    pub energy: [(f64, u64); 4],
    pub error: Option<String>,
}

impl CapReport {
    pub fn json(&self) -> serde_json::Value {
        let db = |i: usize| measure::round3(measure::dbfs(self.energy[i].0, self.energy[i].1));
        serde_json::json!({
            "periods": self.periods,
            "period_frames": self.period_frames,
            "overruns": self.xruns.overruns,
            "recoveries": self.xruns.recoveries,
            "dropped_msgs": self.dropped_msgs,
            "thread_cpu_pct_of_core": if self.wall_s > 0.0 { measure::round3(100.0 * self.cpu_s / self.wall_s) } else { 0.0 },
            "rt": self.rt,
            "webrtc_capture_ms_per_10ms": Summary::of(&self.webrtc_capture_ms).json(),
            "rms_dbfs": {
                "raw_while_playing": db(0), "raw_idle": db(1),
                "processed_while_playing": db(2), "processed_idle": db(3),
            },
            "error": self.error,
        })
    }
}

pub fn capture_thread(
    mut be: Box<dyn Capture>,
    tx: SyncSender<CapMsg>,
    cfg: CapCfg,
    shared: Arc<Shared>,
) -> CapReport {
    let mut rep = CapReport {
        period_frames: be.period_frames(),
        ..Default::default()
    };
    if cfg.rt {
        rep.rt = Some(measure::try_realtime("capture"));
    }
    if let Err(e) = capture_loop(&mut *be, &tx, &cfg, &shared, &mut rep) {
        eprintln!("capture: {e}");
        rep.error = Some(e.to_string());
    }
    rep.xruns = be.xruns();
    rep.cpu_s = cpu_seconds(true);
    rep.wall_s = cfg.t0.elapsed().as_secs_f64();
    rep
}

fn capture_loop(
    be: &mut dyn Capture,
    tx: &SyncSender<CapMsg>,
    cfg: &CapCfg,
    shared: &Shared,
    rep: &mut CapReport,
) -> std::io::Result<()> {
    let ch = be.channels();
    let period = be.period_frames();
    let mut buf = vec![0i16; period * ch];
    let mut mono = vec![0f32; period];
    let mut frames = 0u64;
    if let Some(apm) = &cfg.apm
        && period % apm.frame_len() != 0
    {
        return Err(std::io::Error::other(format!(
            "capture period {period} frames is not a multiple of WebRTC's 10 ms frame ({}); use --period-ms 10/20/30",
            apm.frame_len()
        )));
    }
    while frames < cfg.total_frames && !shared.stop.load(Ordering::Relaxed) {
        be.read(&mut buf)?;
        let read_at = Instant::now();
        frames += period as u64;
        rep.periods += 1;
        let playing = shared.playing.load(Ordering::Relaxed);
        let slot = if playing { 0 } else { 1 };

        for (m, f) in mono.iter_mut().zip(buf.chunks_exact(ch)) {
            *m = f.iter().map(|&s| s as f32).sum::<f32>() / ch as f32;
        }
        add_energy(&mut rep.energy[slot], &mono);
        let raw_mono =
            (frames <= cfg.keep_raw_frames).then(|| mono.iter().map(|&s| s as i16).collect());

        let data = if let Some(apm) = &cfg.apm {
            let n = apm.frame_len();
            for frame in mono.chunks_exact_mut(n) {
                frame.iter_mut().for_each(|s| *s /= 32768.0);
                let t = Instant::now();
                apm.capture(frame)?;
                rep.webrtc_capture_ms.push(ms(t.elapsed()));
                frame.iter_mut().for_each(|s| *s *= 32768.0);
            }
            add_energy(&mut rep.energy[slot + 2], &mono);
            mono.iter()
                .map(|&s| s.clamp(-32768.0, 32767.0) as i16)
                .collect()
        } else {
            buf.clone()
        };
        match tx.try_send(CapMsg {
            data,
            raw_mono,
            end_frame: frames,
            read_at,
        }) {
            Ok(()) => {}
            // The audio thread never waits for the main thread: a full queue drops the period.
            Err(TrySendError::Full(_)) => rep.dropped_msgs += 1,
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    Ok(())
}

fn add_energy(acc: &mut (f64, u64), mono: &[f32]) {
    acc.0 += mono.iter().map(|&s| (s as f64).powi(2)).sum::<f64>();
    acc.1 += mono.len() as u64;
}

/// Finds the echo click in the raw capture. Returns the capture frame index of its start,
/// or None when there is no clear peak (muted speaker, null pcm).
pub fn find_click(capture: &[i16], click: &[i16]) -> Option<usize> {
    if capture.len() < click.len() * 4 {
        return None;
    }
    let c: Vec<f64> = click.iter().map(|&s| s as f64).collect();
    let c_norm = c.iter().map(|x| x * x).sum::<f64>().sqrt();
    let x: Vec<f64> = capture.iter().map(|&s| s as f64).collect();
    let n = x.len() - c.len();
    let mut corr = Vec::with_capacity(n);
    for i in 0..n {
        let w = &x[i..i + c.len()];
        let dot: f64 = w.iter().zip(&c).map(|(a, b)| a * b).sum();
        let e = w.iter().map(|a| a * a).sum::<f64>().sqrt();
        corr.push(if e > 0.0 {
            (dot / (e * c_norm)).abs()
        } else {
            0.0
        });
    }
    let (best, &peak) = corr.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let mut sorted = corr.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    // A clear, well-shaped peak well above the typical correlation of noise.
    (peak > 0.5 && peak > 3.0 * median).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mix::synth_click;

    #[test]
    fn click_found_in_noise() {
        let click = synth_click(16000);
        let mut cap: Vec<i16> = (0..16000)
            .map(|i| ((i * 7919 % 401) as i16 - 200) / 4)
            .collect();
        for (k, &s) in click.iter().enumerate() {
            cap[5000 + k] = cap[5000 + k].saturating_add(s / 3);
        }
        assert_eq!(find_click(&cap, &click), Some(5000));
        assert_eq!(find_click(&vec![0; 16000], &click), None);
    }
}
