// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A minimal satellite built on [`Server`], for the tests and for the `stub-satellite` binary
//! (kept for home-companion's engineers as a reference of the expected behaviour).
//!
//! After `run-satellite` it waits `detect_after`, sends `detection` (`okay_alfred`),
//! `run-pipeline` asr -> tts and `audio-start`, then streams a quiet tone in real time (20 ms
//! chunks) until `transcript`, `error`, `pause-satellite` or `stream` elapsed, and ends with
//! `audio-stop`. Answer and announcement audio from Home Assistant is "played" in real time: the
//! `played` goes out when the audio's length has elapsed since its `audio-start`, and never before
//! its `audio-stop`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::Error;
use crate::info::{Attribution, Info, SatelliteInfo, WakeModelInfo};
use crate::message::{AudioFormat, Detection, Message, RunPipeline};
use crate::net::Net;
use crate::server::{Sender, Server, ServerConfig, ServerEvent};

#[derive(Debug, Clone)]
pub struct StubConfig {
    pub bind: SocketAddr,
    pub allowed: Vec<Net>,
    /// Answer Home Assistant's pings.
    pub answer_pings: bool,
    /// Ping Home Assistant (2 s after each pong).
    pub ping: bool,
    /// Misbehave: send `played` as soon as the transcript arrives, before any answer audio.
    pub played_early: bool,
    pub detect_after: Duration,
    pub stream: Duration,
}

impl Default for StubConfig {
    fn default() -> Self {
        StubConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            allowed: vec![
                Net::parse("127.0.0.0/8").unwrap_or_else(|_| unreachable!()),
                Net::parse("::1").unwrap_or_else(|_| unreachable!()),
            ],
            answer_pings: true,
            ping: true,
            played_early: false,
            detect_after: Duration::from_secs(1),
            stream: Duration::from_secs(2),
        }
    }
}

/// The `info` of the stub (same shape as home-companion's).
pub fn stub_info() -> Info {
    let attribution = Attribution {
        name: "home-companion".into(),
        url: "https://github.com/m0s4ik/home-companion".into(),
    };
    Info::for_satellite(SatelliteInfo {
        name: "Stub satellite".into(),
        area: None,
        description: None,
        version: env!("CARGO_PKG_VERSION").into(),
        attribution,
        has_vad: false,
        wake_program_name: "home-companion-wake".into(),
        wake_models: vec![WakeModelInfo {
            name: "okay_alfred".into(),
            phrase: "okay alfred".into(),
            languages: vec!["en".into()],
            attribution: Attribution {
                name: "Tater Totterson".into(),
                url: "https://github.com/TaterTotterson/Tater-Wake-Words".into(),
            },
            description: None,
            version: None,
        }],
    })
}

pub struct Stub {
    server: Option<Server>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stub {
    pub fn start(cfg: StubConfig) -> Result<Stub, Error> {
        let (tx, rx) = mpsc::channel();
        let server = Server::start(
            ServerConfig {
                bind: cfg.bind,
                allowed: cfg.allowed.clone(),
                info: stub_info(),
                ping_interval: cfg.ping.then_some(Duration::from_secs(2)),
                answer_pings: cfg.answer_pings,
                ..Default::default()
            },
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
        )?;
        let sender = server.sender();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("stub-satellite".into())
            .spawn(move || run(cfg, sender, rx, s2))?;
        Ok(Stub {
            server: Some(server),
            stop,
            thread: Some(thread),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.server
            .as_ref()
            .map(|s| s.local_addr())
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)))
    }

    pub fn shutdown(self) {}
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = self.server.take() {
            s.shutdown();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
struct Conversation {
    detect_at: Option<Instant>,
    streaming_since: Option<Instant>,
    sent_ms: u64,
    /// Answer being received: when its audio-start came, bytes per second, bytes so far.
    answer: Option<(Instant, u64, u64)>,
    /// `played` due at this time.
    played_at: Option<Instant>,
}

const CHUNK_MS: u64 = 20;

fn run(cfg: StubConfig, sender: Sender, rx: mpsc::Receiver<ServerEvent>, stop: Arc<AtomicBool>) {
    let mut c = Conversation::default();
    let tone: Vec<u8> = (0..(16_000 * CHUNK_MS / 1000))
        .flat_map(|i| {
            let s =
                (2000.0 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16_000.0).sin()) as i16;
            s.to_le_bytes()
        })
        .collect();
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok(ServerEvent::Message(m)) => match m {
                Message::RunSatellite => {
                    c = Conversation {
                        detect_at: Some(Instant::now() + cfg.detect_after),
                        ..Default::default()
                    };
                }
                Message::PauseSatellite => {
                    end_stream(&sender, &mut c);
                    c.detect_at = None;
                }
                Message::Transcript(_) | Message::Error { .. } => {
                    end_stream(&sender, &mut c);
                    if cfg.played_early {
                        let _ = sender.send(&Message::Played);
                    }
                }
                Message::AudioStart { format, .. } => {
                    c.answer = Some((Instant::now(), format.bytes_per_second().max(1), 0));
                }
                Message::AudioChunk { audio, .. } => {
                    if let Some((_, _, bytes)) = &mut c.answer {
                        *bytes += audio.len() as u64;
                    }
                }
                Message::AudioStop { .. } => {
                    if let Some((start, bps, bytes)) = c.answer.take() {
                        let length = Duration::from_secs_f64(bytes as f64 / bps as f64);
                        c.played_at = Some((start + length).max(Instant::now()));
                    }
                }
                _ => {}
            },
            Ok(ServerEvent::Disconnected(..)) => c = Conversation::default(),
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        if c.detect_at.is_some_and(|t| now >= t) {
            c.detect_at = None;
            let ok = sender
                .send(&Message::Detection(Detection {
                    name: Some("okay_alfred".into()),
                    timestamp: None,
                    speaker: None,
                }))
                .is_ok()
                && sender
                    .send(&Message::RunPipeline(RunPipeline::asr_to_tts()))
                    .is_ok()
                && sender
                    .send(&Message::AudioStart {
                        format: AudioFormat::MIC,
                        timestamp: Some(0),
                    })
                    .is_ok();
            if ok {
                c.streaming_since = Some(now);
                c.sent_ms = 0;
            }
        }
        if let Some(since) = c.streaming_since {
            let due = now.duration_since(since).as_millis() as u64;
            while c.sent_ms + CHUNK_MS <= due && c.sent_ms < cfg.stream.as_millis() as u64 {
                if sender
                    .send_chunk(&AudioFormat::MIC, Some(c.sent_ms as i64), &tone)
                    .is_err()
                {
                    break;
                }
                c.sent_ms += CHUNK_MS;
            }
            if c.sent_ms >= cfg.stream.as_millis() as u64 {
                end_stream(&sender, &mut c);
            }
        }
        if c.played_at.is_some_and(|t| now >= t) {
            c.played_at = None;
            let _ = sender.send(&Message::Played);
        }
    }
}

fn end_stream(sender: &Sender, c: &mut Conversation) {
    if c.streaming_since.take().is_some() {
        let _ = sender.send(&Message::AudioStop {
            timestamp: Some(c.sent_ms as i64),
        });
    }
}

/// A minimal stderr logger for the two binaries (home-companion installs its own).
pub fn init_stderr_logger(level: log::LevelFilter) {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata<'_>) -> bool {
            m.level() <= log::max_level()
        }
        fn log(&self, r: &log::Record<'_>) {
            if self.enabled(r.metadata()) {
                eprintln!("{:5} {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: L = L;
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level);
    }
}
