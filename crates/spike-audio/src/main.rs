// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! spike-audio: M0 audio spike of home-companion.
//!
//! One process owns the speaker and mixes an answer (resampled) with beeps, while it
//! captures 16 kHz from the microphones, and counts xruns, latency, CPU and memory.
//! It never touches mixer controls: volume is set by a human before a run.

mod apm;
mod backend;
mod measure;
mod mixer;
mod run;

// The software mixer is used as `mix::...` so that the no-mixer-control grep of the README
// (ALSA mixer API names) finds nothing in src/ and keeps its meaning.
use mixer as mix;

use backend::{
    AlsaCapture, AlsaPlayback, Capture, FileCapture, FilePlayback, Playback, StreamSpec,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use mix::{AnswerStream, Source, SourceKind};
use run::{CapCfg, CapMsg, CapReport, Idle, Msg, PlayCfg, PlayReport, Shared};
use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    version,
    about = "M0 audio spike: one speaker owner, mixing, capture, WebRTC cost"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show which rates, channel counts and formats a pcm accepts (hw_params test only).
    Probe {
        #[arg(long, default_value = "null")]
        pcm: String,
        #[arg(long, value_enum, default_value_t = Dir::Playback)]
        direction: Dir,
    },
    /// Capture from the microphones.
    Capture {
        /// Capture pcm (alsa backend).
        #[arg(long, default_value = "null")]
        pcm: String,
        /// Input WAV for the file backend (silence when absent).
        #[arg(long)]
        in_wav: Option<String>,
        /// Write what was captured (processed mono with --webrtc) to this WAV.
        #[arg(long)]
        out: Option<String>,
        #[command(flatten)]
        cap: CapOpts,
        #[command(flatten)]
        common: Common,
    },
    /// Play an answer and beeps through the mixer.
    Play {
        /// Playback pcm (alsa backend).
        #[arg(long, default_value = "null")]
        pcm: String,
        /// Output WAV for the file backend.
        #[arg(long)]
        out: Option<String>,
        #[command(flatten)]
        play: PlayOpts,
        #[command(flatten)]
        common: Common,
    },
    /// Capture and play at the same time (optionally with echo cancellation).
    Duplex {
        #[arg(long, default_value = "null")]
        pcm_in: String,
        #[arg(long, default_value = "null")]
        pcm_out: String,
        /// Input WAV for the file backend (silence when absent).
        #[arg(long)]
        in_wav: Option<String>,
        /// Output WAV of the mix for the file backend.
        #[arg(long)]
        play_out: Option<String>,
        /// Write what was captured to this WAV.
        #[arg(long)]
        capture_out: Option<String>,
        /// Emit a click at 1.0 s and report the speaker -> microphone round trip.
        #[arg(long)]
        echo_click: bool,
        #[command(flatten)]
        cap: CapOpts,
        #[command(flatten)]
        play: PlayOpts,
        #[command(flatten)]
        common: Common,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Dir {
    Playback,
    Capture,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendKind {
    Alsa,
    File,
}

#[derive(Args)]
struct Common {
    #[arg(long, value_enum, default_value_t = BackendKind::Alsa)]
    backend: BackendKind,
    /// Length of the run in seconds.
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,
    /// Period length; the ALSA buffer is 4 periods.
    #[arg(long, default_value_t = 20)]
    period_ms: u32,
    /// Try SCHED_FIFO priority 10 for the audio threads (only a warning when refused).
    #[arg(long)]
    rt: bool,
    /// File backend: run as fast as possible instead of at real-time pace.
    #[arg(long)]
    no_realtime: bool,
    /// Write all counters as JSON to this file.
    #[arg(long)]
    stats: Option<String>,
}

#[derive(Args)]
struct CapOpts {
    /// Capture rate in Hz.
    #[arg(long, default_value_t = 16000)]
    rate: u32,
    /// Capture channels.
    #[arg(long, default_value_t = 2)]
    channels: usize,
    /// WebRTC AGC2 + noise suppression + AEC3 on the capture (needs the `webrtc` feature).
    #[arg(long)]
    webrtc: bool,
}

#[derive(Args)]
struct PlayOpts {
    /// Answer WAV (16-bit, any rate, mono or stereo), resampled to --out-rate.
    #[arg(long)]
    answer: Option<String>,
    /// When the answer starts, in seconds from the start of the run.
    #[arg(long, default_value_t = 0.0)]
    answer_at: f64,
    /// A 200 ms 880 Hz beep at this time in seconds (repeatable).
    #[arg(long)]
    beep_at: Vec<f64>,
    #[arg(long, value_enum, default_value_t = Idle::KeepOpen)]
    idle: Idle,
    /// Output rate in Hz (pick it with `probe`).
    #[arg(long, default_value_t = 48000)]
    out_rate: u32,
    #[arg(long, default_value_t = 2)]
    out_channels: usize,
    /// Silence written after each reopen in close mode, before the sound.
    #[arg(long, default_value_t = 0)]
    lead_silence_ms: u32,
    /// Three beeps 4 s apart (1, 5, 9 s), no answer; the run lasts at least 10 s.
    #[arg(long)]
    gap_test: bool,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Probe { pcm, direction } => probe(&pcm, direction),
        Cmd::Capture {
            pcm,
            in_wav,
            out,
            cap,
            common,
        } => run(
            &common,
            None,
            Some((&pcm, in_wav.as_deref(), &cap)),
            out.as_deref(),
            false,
        ),
        Cmd::Play {
            pcm,
            out,
            play,
            common,
        } => run(
            &common,
            Some((&pcm, out.as_deref(), &play)),
            None,
            None,
            false,
        ),
        Cmd::Duplex {
            pcm_in,
            pcm_out,
            in_wav,
            play_out,
            capture_out,
            echo_click,
            cap,
            play,
            common,
        } => run(
            &common,
            Some((&pcm_out, play_out.as_deref(), &play)),
            Some((&pcm_in, in_wav.as_deref(), &cap)),
            capture_out.as_deref(),
            echo_click,
        ),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn probe(pcm: &str, dir: Dir) -> io::Result<()> {
    let d = if dir == Dir::Playback {
        alsa::Direction::Playback
    } else {
        alsa::Direction::Capture
    };
    let v = backend::probe(pcm, d)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&v).map_err(io::Error::other)?
    );
    Ok(())
}

/// What the main thread keeps from the capture stream.
struct CapSink {
    wav: Option<hound::WavWriter<io::BufWriter<std::fs::File>>>,
    raw: Vec<i16>,
    /// (end_frame, read_at) per period, to put a time on a capture frame.
    times: Vec<(u64, Instant)>,
}

impl CapSink {
    fn take(&mut self, m: CapMsg) -> io::Result<()> {
        if let Some(w) = &mut self.wav {
            for s in &m.data {
                w.write_sample(*s).map_err(io::Error::other)?;
            }
        }
        if let Some(r) = m.raw_mono {
            self.raw.extend_from_slice(&r);
            self.times.push((m.end_frame, m.read_at));
        }
        Ok(())
    }

    /// When capture frame `idx` was available to the program.
    fn time_of(&self, idx: u64, rate: u32) -> Option<Instant> {
        let &(end, at) = self.times.iter().find(|(end, _)| *end > idx)?;
        Some(at - Duration::from_secs_f64((end - idx) as f64 / rate as f64))
    }
}

type PlaySide<'a> = (&'a str, Option<&'a str>, &'a PlayOpts);
type CapSide<'a> = (&'a str, Option<&'a str>, &'a CapOpts);

fn run(
    common: &Common,
    play: Option<PlaySide>,
    cap: Option<CapSide>,
    cap_out: Option<&str>,
    echo_click: bool,
) -> io::Result<()> {
    let realtime = !(common.backend == BackendKind::File && common.no_realtime);
    let gap_test = play.is_some_and(|p| p.2.gap_test);
    let seconds = if gap_test {
        common.seconds.max(10.0)
    } else {
        common.seconds
    };
    let t_start_cpu = measure::cpu_seconds(false);
    let shared = Arc::new(Shared::default());

    // WebRTC processor shared by the capture (near end) and playback (far end) threads.
    let apm = match cap {
        Some((_, _, c)) if c.webrtc => Some(apm::Apm::new(c.rate)?),
        _ => None,
    };

    // Sound events, sorted by time.
    let mut events: Vec<(f64, &'static str)> = Vec::new();
    if let Some((_, _, p)) = play {
        if p.gap_test {
            events.extend([(1.0, "beep"), (5.0, "beep"), (9.0, "beep")]);
        } else {
            if p.answer.is_some() {
                events.push((p.answer_at, "answer"));
            }
            events.extend(p.beep_at.iter().map(|&t| (t, "beep")));
        }
        if echo_click {
            events.push((1.0, "click"));
        }
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let answer = match play {
        Some((_, _, p)) if !p.gap_test => match &p.answer {
            Some(path) => Some(mix::read_wav_mono(path)?),
            None => None,
        },
        _ => None,
    };
    let out_rate = play.map(|(_, _, p)| p.out_rate).unwrap_or(48000);
    let beep: Arc<[i16]> = mix::synth_beep(out_rate, 880.0, 200, 10, 0.3).into();
    let click: Arc<[i16]> = mix::synth_click(out_rate).into();

    let make_source = |t: f64, what: &str| -> Source {
        let kind = match what {
            "answer" => {
                let (clip, rate) = answer.clone().expect("answer loaded");
                SourceKind::Answer(Box::new(AnswerStream::new(clip, rate, out_rate)))
            }
            "click" => SourceKind::Clip(click.clone()),
            _ => SourceKind::Clip(beep.clone()),
        };
        Source {
            label: if what == "click" {
                "click".into()
            } else {
                format!("{what}@{t}s")
            },
            kind,
            start_frame: (t * out_rate as f64) as u64,
            requested: Instant::now(),
        }
    };
    let mut next = 0;
    let mut stop_sent = false;

    // Open devices before t0 so the timeline starts with both streams ready.
    let mut play_side = None;
    if let Some((pcm, out, p)) = play {
        let spec = StreamSpec {
            rate: p.out_rate,
            channels: p.out_channels,
            period_ms: common.period_ms,
        };
        let open_now = p.idle == Idle::KeepOpen;
        let be: Box<dyn Playback> = match common.backend {
            BackendKind::Alsa => {
                let a = AlsaPlayback::new(pcm, spec.clone(), open_now)?;
                if open_now {
                    println!(
                        "playback {pcm}: {} Hz, {} ch, period {} frames, buffer {} frames",
                        spec.rate,
                        spec.channels,
                        a.period_frames(),
                        a.buffer_frames()
                    );
                }
                Box::new(a)
            }
            BackendKind::File => {
                let path =
                    out.ok_or_else(|| io::Error::other("--backend file needs --out / --play-out"))?;
                Box::new(FilePlayback::new(path, &spec, realtime)?)
            }
        };
        play_side = Some((be, p, open_now));
    }
    let mut cap_side = None;
    if let Some((pcm, in_wav, c)) = cap {
        let spec = StreamSpec {
            rate: c.rate,
            channels: c.channels,
            period_ms: common.period_ms,
        };
        let be: Box<dyn Capture> = match common.backend {
            BackendKind::Alsa => Box::new(AlsaCapture::new(pcm, spec.clone())?),
            BackendKind::File => Box::new(FileCapture::new(in_wav, &spec, realtime)?),
        };
        println!(
            "capture {pcm}: {} Hz, {} ch, period {} frames",
            spec.rate,
            spec.channels,
            be.period_frames()
        );
        cap_side = Some((be, c));
    }

    let t0 = Instant::now();
    let (play_tx, play_handle) = match play_side.take() {
        Some((be, p, open_now)) => {
            let (tx, rx) = sync_channel::<Msg>(events.len() + 64);
            if !realtime {
                // Without real time the playback thread runs ahead at once: queue every sound
                // (at its absolute frame) and the stop before it starts.
                for &(t, what) in &events {
                    let _ = tx.send(Msg::Play(make_source(t, what)));
                }
                let _ = tx.send(Msg::Stop);
                (next, stop_sent) = (events.len(), true);
            }
            let cfg = PlayCfg {
                idle: p.idle,
                rate: p.out_rate,
                total_frames: (seconds * p.out_rate as f64) as u64,
                lead_silence_frames: (p.out_rate as u64 * p.lead_silence_ms as u64 / 1000) as usize,
                realtime,
                rt: common.rt,
                t0,
                far_end: apm
                    .clone()
                    .map(|a| (a, cap.map(|c| c.2.rate).unwrap_or(16000))),
            };
            let sh = shared.clone();
            let h = std::thread::Builder::new()
                .name("playback".into())
                .spawn(move || run::playback_thread(be, rx, cfg, sh, open_now))?;
            (Some((tx, p)), Some(h))
        }
        None => (None, None),
    };
    let click_rate = cap.map(|c| c.2.rate).unwrap_or(16000);
    let (cap_rx, cap_handle, mut sink) = match cap_side.take() {
        Some((be, c)) => {
            let (tx, rx) = sync_channel::<CapMsg>(32);
            let cfg = CapCfg {
                total_frames: (seconds * c.rate as f64) as u64,
                rt: common.rt,
                t0,
                apm: apm.clone(),
                keep_raw_frames: if echo_click {
                    (c.rate as f64 * 3.5) as u64
                } else {
                    0
                },
            };
            let sh = shared.clone();
            let h = std::thread::Builder::new()
                .name("capture".into())
                .spawn(move || run::capture_thread(be, tx, cfg, sh))?;
            let wav = match cap_out {
                Some(path) => {
                    let ch = if c.webrtc { 1 } else { c.channels as u16 };
                    let spec = hound::WavSpec {
                        channels: ch,
                        sample_rate: c.rate,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    };
                    Some(hound::WavWriter::create(path, spec).map_err(io::Error::other)?)
                }
                None => None,
            };
            (
                Some(rx),
                Some(h),
                Some(CapSink {
                    wav,
                    raw: Vec::new(),
                    times: Vec::new(),
                }),
            )
        }
        None => (None, None, None),
    };

    // Main loop: send each sound at its time (or all at once without real time), keep the
    // capture queue drained, stop at the end of the run.
    loop {
        let now = t0.elapsed().as_secs_f64();
        while next < events.len() && events[next].0 <= now {
            let (t, what) = events[next];
            let src = make_source(t, what);
            if let Some((tx, _)) = &play_tx {
                let _ = tx.send(Msg::Play(src));
            }
            next += 1;
        }
        if next == events.len()
            && !stop_sent
            && let Some((tx, _)) = &play_tx
            && now >= seconds
        {
            let _ = tx.send(Msg::Stop);
            stop_sent = true;
        }
        if now >= seconds && (realtime || cap_rx.is_none()) {
            break;
        }
        let wake = if next < events.len() {
            events[next].0.min(seconds)
        } else {
            seconds
        };
        let wait = Duration::from_secs_f64((wake - now).clamp(0.001, 0.1));
        match (&cap_rx, sink.as_mut()) {
            (Some(rx), Some(sink)) => match rx.recv_timeout(wait) {
                Ok(m) => sink.take(m)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    if !realtime {
                        break;
                    }
                    std::thread::sleep(wait)
                }
            },
            _ => {
                if !realtime {
                    break;
                }
                std::thread::sleep(wait)
            }
        }
    }
    shared.stop.store(true, Ordering::Relaxed);
    if let Some((tx, _)) = play_tx {
        let _ = tx.try_send(Msg::Stop);
    }
    let play_rep = play_handle.map(|h| h.join().expect("playback thread"));
    let cap_rep = cap_handle.map(|h| h.join().expect("capture thread"));
    if let (Some(rx), Some(sink)) = (&cap_rx, sink.as_mut()) {
        drain(rx, sink)?;
    }
    let wall = t0.elapsed().as_secs_f64();

    // Echo click: when the click entered the pcm vs when the microphone had it.
    let round_trip_ms = if echo_click {
        let handed = *shared.click_handed.lock().unwrap();
        let s = sink.as_ref();
        match (
            handed,
            s.and_then(|s| run::find_click(&s.raw, &mix::synth_click(click_rate)).map(|i| (s, i))),
        ) {
            (Some(h), Some((s, idx))) => s
                .time_of(idx as u64, click_rate)
                .map(|t| if t >= h { ms(t - h) } else { -ms(h - t) }),
            _ => None,
        }
    } else {
        None
    };
    if let Some(s) = sink.as_mut()
        && let Some(w) = s.wav.take()
    {
        w.finalize().map_err(io::Error::other)?;
    }

    report(
        common,
        wall,
        measure::cpu_seconds(false) - t_start_cpu,
        play_rep.as_ref(),
        cap_rep.as_ref(),
        echo_click,
        round_trip_ms,
        apm.is_some(),
    )
}

fn ms(d: Duration) -> f64 {
    measure::ms(d)
}

fn drain(rx: &Receiver<CapMsg>, sink: &mut CapSink) -> io::Result<()> {
    for m in rx.try_iter() {
        sink.take(m)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn report(
    common: &Common,
    wall: f64,
    cpu: f64,
    play: Option<&PlayReport>,
    cap: Option<&CapReport>,
    echo_click: bool,
    round_trip_ms: Option<f64>,
    webrtc: bool,
) -> io::Result<()> {
    let rss = measure::rss_max_mb();
    println!("--- results ({wall:.1} s) ---");
    println!(
        "process CPU: {:.2} % of one core, RSS max {:.1} MB",
        100.0 * cpu / wall,
        rss
    );
    if let Some(p) = play {
        let pct = |c: f64, w: f64| if w > 0.0 { 100.0 * c / w } else { 0.0 };
        println!(
            "playback: underruns {}, recoveries {}, periods {}",
            p.xruns.underruns, p.xruns.recoveries, p.periods
        );
        println!(
            "playback thread CPU: {:.2} % of one core (sounds {:.2} % over {:.1} s, silence {:.2} % over {:.1} s, closed {:.1} s)",
            pct(p.cpu_s, p.wall_s),
            pct(p.busy_cpu_s, p.busy_wall_s),
            p.busy_wall_s,
            pct(p.idle_cpu_s, p.idle_wall_s),
            p.idle_wall_s,
            p.closed_wall_s
        );
        println!(
            "snd_pcm_delay: {}",
            measure::Summary::of(&p.delay_ms).line("ms")
        );
        for (l, h, d) in &p.first_frame {
            println!(
                "request-to-first-frame {l}: {h:.2} ms handed to the pcm, ~{d:.2} ms at the DAC"
            );
        }
        if !p.reopen_ms.is_empty() {
            let list: Vec<String> = p.reopen_ms.iter().map(|x| format!("{x:.1}")).collect();
            println!("reopen times: {} ms", list.join(", "));
        }
        if let Some(rt) = &p.rt {
            println!("{rt}");
        }
        if webrtc {
            println!(
                "WebRTC render (far end): {}",
                measure::Summary::of(&p.webrtc_render_ms).line("ms per 10 ms frame")
            );
        }
    }
    if let Some(c) = cap {
        let pct = if c.wall_s > 0.0 {
            100.0 * c.cpu_s / c.wall_s
        } else {
            0.0
        };
        println!(
            "capture: overruns {}, recoveries {}, periods {}, dropped {}",
            c.xruns.overruns, c.xruns.recoveries, c.periods, c.dropped_msgs
        );
        println!("capture thread CPU: {pct:.2} % of one core");
        if webrtc {
            println!(
                "WebRTC capture (AGC2+NS+AEC3): {}",
                measure::Summary::of(&c.webrtc_capture_ms).line("ms per 10 ms frame")
            );
            let db = |i: usize| measure::dbfs(c.energy[i].0, c.energy[i].1);
            println!(
                "mic RMS while playing: raw {:.1} dBFS, processed {:.1} dBFS (idle: raw {:.1}, processed {:.1})",
                db(0),
                db(2),
                db(1),
                db(3)
            );
        } else {
            let db = |i: usize| measure::dbfs(c.energy[i].0, c.energy[i].1);
            println!(
                "mic RMS: raw while playing {:.1} dBFS, raw idle {:.1} dBFS",
                db(0),
                db(1)
            );
        }
        if let Some(rt) = &c.rt {
            println!("{rt}");
        }
    }
    if echo_click {
        match round_trip_ms {
            Some(x) => println!("echo click round trip (written -> captured): {x:.1} ms"),
            None => println!("echo click round trip: n/a (click not found in the capture)"),
        }
    }
    if let Some(path) = &common.stats {
        let v = serde_json::json!({
            "wall_s": measure::round3(wall),
            "process_cpu_pct_of_core": measure::round3(100.0 * cpu / wall),
            "rss_max_mb": measure::round3(rss),
            "period_ms": common.period_ms,
            "webrtc": webrtc,
            "playback": play.map(|p| p.json()),
            "capture": cap.map(|c| c.json()),
            "echo_click_round_trip_ms": round_trip_ms.map(measure::round3),
        });
        if let Some(dir) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(&v).map_err(io::Error::other)? + "\n",
        )?;
        println!("stats written to {path}");
    }
    let failed = play
        .and_then(|p| p.error.as_ref())
        .or(cap.and_then(|c| c.error.as_ref()));
    match failed {
        Some(e) => Err(io::Error::other(e.clone())),
        None => Ok(()),
    }
}
