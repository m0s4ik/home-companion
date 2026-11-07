// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! One owner of the speaker and the microphone.
//!
//! [`Audio::start`] opens the playback and capture devices and runs two threads:
//!
//! - `audio-play` owns the playback pcm (kept open, writing silence when idle, as decided in
//!   M0: the device's amplifier loses the start of a sound after a reopen). Every period it
//!   mixes every active source (cues, the timer bell, answer streams resampled with rubato)
//!   into one bus, applies the volume and writes it. [`AudioEvent::Finished`] is sent when a
//!   source's last sample has left the pcm.
//! - `audio-capture` reads the microphones, picks a channel, optionally runs WebRTC AGC2,
//!   measures the level, feeds the debug recorder and hands 20 ms frames of 16 kHz mono to
//!   every subscriber. It never blocks on a slow subscriber: a full queue drops the frame.
//!
//! Backends: ALSA (the device), cpal (the simulator), null (silence, real-time paced) and file
//! (WAV in, WAV out; tests and CI). There is no mixer-control code in this crate: the volume
//! is a software gain on the mix, the card's controls are never touched.

mod apm;
mod backend;
mod capture;
pub mod cues;
pub mod meter;
mod mixer;
mod player;
pub mod probe;
mod recorder;
mod resample;
mod stats;
pub mod wav;

pub use player::{Clip, Player, Stream, StreamFormat};
pub use recorder::Recorder;
pub use stats::{Stats, process_cpu_seconds};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Capture rate of every [`CaptureFrame`].
pub const CAPTURE_RATE: u32 = 16_000;
/// Playback rate of the device (the cpal backend may run at another rate: [`Audio::output_rate`]).
pub const OUTPUT_RATE: u32 = 48_000;
/// Playback channels of the device.
pub const OUTPUT_CHANNELS: u16 = 2;
/// Length of a capture frame: 320 samples at 16 kHz.
pub const FRAME_MS: u32 = 20;
/// Samples in a capture frame.
pub const FRAME_SAMPLES: usize = (CAPTURE_RATE * FRAME_MS / 1000) as usize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Device(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("built without feature {0}")]
    Feature(&'static str),
}

#[derive(Debug, Clone)]
pub enum Backend {
    /// Linux, feature `alsa`.
    Alsa,
    /// Feature `cpal` (the simulator).
    Cpal,
    /// No devices, real-time paced; capture is silence.
    Null,
    /// Tests and CI: capture from a 16 kHz WAV (mono or 2 ch; silence after its end), playback
    /// to a stereo WAV at [`OUTPUT_RATE`]. `realtime = false` runs as fast as there is
    /// something to play and paces idle periods like a card; the capture never runs ahead of
    /// the playback, so the two timelines stay aligned (see the README).
    File {
        capture: Option<PathBuf>,
        playback: Option<PathBuf>,
        realtime: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelPick {
    Left,
    Right,
    Average,
}

/// The debug utterance recorder: what the brain heard, for a speech-to-text bench.
#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub dir: PathBuf,
    /// Files kept; the oldest beyond this are deleted after every recording.
    pub keep: usize,
    /// Audio from before [`Recorder::start`] included at the start of each file.
    pub pre_roll_ms: u32,
    /// The last this many ms before [`Recorder::start`] also go to `<name>-wake.wav` next to
    /// the recording (0 = none): what made the wake word fire, the false ones above all.
    pub wake_context_ms: u32,
}

#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub backend: Backend,
    /// ALSA "hw:0,0" / cpal device name, "" = default.
    pub playback_device: String,
    /// ALSA "hw:0,1" / cpal device name, "" = default.
    pub capture_device: String,
    /// 2 on the device, 1 on a laptop.
    pub capture_channels: u16,
    pub capture_channel: ChannelPick,
    /// ALSA period (and file/null period); capture frames are always 20 ms.
    pub period_ms: u32,
    /// true: write silence when idle (decided in M0); false: close after `idle_close`.
    pub keep_open: bool,
    pub idle_close: Duration,
    /// WebRTC AGC2 (features `webrtc` / `webrtc-system`); `Error::Feature` without them.
    pub agc: bool,
    /// Try SCHED_FIFO 10 on the playback thread; a refusal is logged, not fatal.
    pub realtime: bool,
    /// Software gain on the mix, 0.0..=1.0 (1.0 = 0 dB).
    pub volume: f32,
    pub recorder: Option<RecorderConfig>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            backend: if cfg!(target_os = "linux") {
                Backend::Alsa
            } else {
                Backend::Cpal
            },
            playback_device: "hw:0,0".into(),
            capture_device: "hw:0,1".into(),
            capture_channels: 2,
            capture_channel: ChannelPick::Left,
            period_ms: 20,
            keep_open: true,
            idle_close: Duration::from_secs(30),
            agc: false,
            realtime: true,
            volume: 0.8,
            recorder: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Level {
    /// RMS in dBFS (-120 for digital silence).
    pub rms_dbfs: f32,
    /// Peak, 0..1 of full scale.
    pub peak: f32,
}

impl Level {
    /// Level of i16 samples.
    pub fn of(samples: &[i16]) -> Level {
        if samples.is_empty() {
            return Level {
                rms_dbfs: -120.0,
                peak: 0.0,
            };
        }
        let mut sum = 0f64;
        let mut peak = 0i32;
        for &s in samples {
            sum += (s as f64) * (s as f64);
            peak = peak.max((s as i32).abs());
        }
        Level {
            rms_dbfs: stats::dbfs(sum, samples.len() as u64) as f32,
            peak: (peak as f32 / 32768.0).min(1.0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CaptureFrame {
    /// 320 mono samples at 16 kHz, after channel pick and AGC.
    pub samples: Arc<[i16]>,
    /// Frame counter since start.
    pub index: u64,
    pub level: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(pub u64);

#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// The source's last sample has left the pcm (Wyoming `played` goes to HA after this).
    /// Sent exactly once per source, never after `Stopped`.
    Finished(SourceId),
    /// The source was stopped ([`Player::stop`]) and its fade is over.
    Stopped(SourceId),
    Underrun {
        total: u64,
    },
    Overrun {
        total: u64,
    },
    /// A subscriber's queue was full (rate-limited to one event per second).
    CaptureDropped {
        total: u64,
    },
    /// An audio thread died: the core logs it and exits 2 so systemd restarts it.
    Error(String),
}

pub(crate) type EventSink = Arc<dyn Fn(AudioEvent) + Send + Sync + 'static>;

/// The running audio subsystem.
pub struct Audio {
    mixer: Arc<mixer::MixBus>,
    capture: Arc<capture::CaptureShared>,
    recorder: Option<Recorder>,
    stats: Arc<stats::StatsShared>,
    stop: Arc<AtomicBool>,
    threads: Vec<(std::thread::JoinHandle<()>, Receiver<()>)>,
    output_rate: u32,
    // Keeps the cpal streams alive (dropped on stop).
    _keep: Option<Box<dyn std::any::Any + Send>>,
}

impl Audio {
    /// Opens the devices and starts the threads.
    pub fn start(
        cfg: &AudioConfig,
        on_event: Box<dyn Fn(AudioEvent) + Send + Sync + 'static>,
    ) -> Result<Audio, Error> {
        let events: EventSink = Arc::from(on_event);
        if !(1..=2).contains(&cfg.capture_channels) {
            return Err(Error::Unsupported(format!(
                "capture_channels = {} (1 or 2)",
                cfg.capture_channels
            )));
        }
        let agc = if cfg.agc {
            Some(apm::Agc::new(CAPTURE_RATE)?)
        } else {
            None
        };
        let stop = Arc::new(AtomicBool::new(false));
        let opened = backend::open(cfg, stop.clone())?;
        let output_rate = opened.output_rate;
        let stats = Arc::new(stats::StatsShared::new());
        let volume = cfg.volume.clamp(0.0, 1.0);
        let mixer = Arc::new(mixer::MixBus::new(output_rate, volume));
        let recorder = cfg.recorder.clone().map(Recorder::new);
        let capture = Arc::new(capture::CaptureShared::new(!opened.realtime));

        let mut threads = Vec::new();
        {
            let (done_tx, done_rx) = sync_channel(1);
            let pcfg = player::PlayThreadCfg {
                keep_open: cfg.keep_open,
                idle_close: cfg.idle_close,
                realtime_prio: cfg.realtime,
                realtime_pacing: opened.realtime,
            };
            let (m, s, st, ev) = (mixer.clone(), stats.clone(), stop.clone(), events.clone());
            let be = opened.playback;
            let h = std::thread::Builder::new()
                .name("audio-play".into())
                .spawn(move || {
                    player::playback_thread(be, m, pcfg, s, st, ev);
                    let _ = done_tx.send(());
                })?;
            threads.push((h, done_rx));
        }
        {
            let (done_tx, done_rx) = sync_channel(1);
            let ccfg = capture::CapThreadCfg {
                pick: cfg.capture_channel,
                agc,
            };
            let (c, s, st, ev, rec) = (
                capture.clone(),
                stats.clone(),
                stop.clone(),
                events.clone(),
                recorder.clone(),
            );
            let be = opened.capture;
            let h = std::thread::Builder::new()
                .name("audio-capture".into())
                .spawn(move || {
                    capture::capture_thread(be, c, ccfg, rec, s, st, ev);
                    let _ = done_tx.send(());
                })?;
            threads.push((h, done_rx));
        }
        log::info!(
            "audio: started ({}), output {} Hz, capture {} ch -> {:?}, agc {}, keep_open {}",
            opened.description,
            output_rate,
            cfg.capture_channels,
            cfg.capture_channel,
            cfg.agc,
            cfg.keep_open
        );
        Ok(Audio {
            mixer,
            capture,
            recorder,
            stats,
            stop,
            threads,
            output_rate,
            _keep: opened.keep,
        })
    }

    pub fn player(&self) -> Player {
        Player::new(self.mixer.clone())
    }

    /// A bounded queue of capture frames. A full queue drops the frame and counts
    /// `CaptureDropped` (the audio thread never blocks; with the file backend in
    /// non-real-time mode it waits up to 1 s for room instead, to stay deterministic).
    pub fn subscribe(&self, capacity_frames: usize) -> Receiver<CaptureFrame> {
        self.capture.subscribe(capacity_frames.max(1))
    }

    pub fn recorder(&self) -> Option<Recorder> {
        self.recorder.clone()
    }

    pub fn stats(&self) -> Stats {
        self.stats.snapshot()
    }

    /// The mixer's rate: [`OUTPUT_RATE`] except with cpal on a device that runs at another rate.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Stops the threads and closes the devices; blocks at most 500 ms. Sources still playing
    /// get no `Finished`.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.mixer.wake();
        drop(self._keep.take());
        let deadline = Instant::now() + Duration::from_millis(500);
        for (h, done) in self.threads.drain(..) {
            let left = deadline.saturating_duration_since(Instant::now());
            if done.recv_timeout(left).is_ok() {
                let _ = h.join();
            } else {
                log::warn!(
                    "audio: thread {} did not stop within 500 ms, detached",
                    h.thread().name().unwrap_or("?")
                );
            }
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        if !self.threads.is_empty() {
            self.shutdown();
        }
    }
}

/// Locks a mutex, recovering the data of a poisoned one (an audio thread must not die
/// because another one panicked while holding a lock).
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
