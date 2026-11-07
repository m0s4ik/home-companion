// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! audio-bench: drives the `audio` crate for the device runbook and CI.
//!
//! Every run ends with one `RESULT {...}` JSON line (counters, CPU, memory). The default
//! backend is `null`, so a mistyped command never makes a sound. Volume is never changed on
//! the card (there is no mixer-control code in the crate).

use audio::{
    Audio, AudioConfig, AudioEvent, Backend, ChannelPick, Clip, RecorderConfig, SourceId,
    StreamFormat, cues,
};
use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: audio-bench <command> [options]

commands:
  probe     [--device hw:0,0] [--capture-device hw:0,1]
            what the pcms accept (hw_params tests only, no sound)
  play      --seconds N [--keep-open | --close] [--cues] [--answer FILE.wav] [--volume V]
            mix for N seconds; --cues: an answer stream (22050 Hz, pushed in 2048-byte chunks as
            Home Assistant does) with an 880 Hz beep on top at 1.0 s and 2.5 s, then the wake,
            done and bell cues at 3.5, 4.2 and 4.9 s
  duplex    [--seconds N] [--agc] [--record DIR] [--cues]
            capture (and play silence, or the cues) for N seconds of audio; prints the capture
            thread's CPU per second of audio (with --agc: WebRTC AGC2)
  levels    --seconds N [--every S]
            microphone RMS/peak per channel (left, right, average) every S seconds (default 1)

backend options (all commands but probe):
  --backend null|file|alsa|cpal       (default null)
  --playback-device D --capture-device D    (alsa: hw:0,0 / hw:0,1)
  --capture-channels N                (default 2) --channel left|right|average (default left)
  --period-ms N                       (default 20)
  --playback-file F --capture-file F  (file backend)
  --no-realtime                       file backend: as fast as possible (deterministic)
  --rt                                ask SCHED_FIFO 10 for the playback thread
  -v                                  debug log
";

struct Args {
    cmd: String,
    backend: String,
    playback_device: String,
    capture_device: String,
    capture_channels: u16,
    channel: ChannelPick,
    period_ms: u32,
    playback_file: Option<PathBuf>,
    capture_file: Option<PathBuf>,
    no_realtime: bool,
    rt: bool,
    seconds: Option<f64>,
    keep_open: bool,
    cues: bool,
    answer: Option<PathBuf>,
    volume: f32,
    agc: bool,
    record: Option<PathBuf>,
    every: f64,
    device: String,
    verbose: bool,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().ok_or("missing command")?;
    if cmd == "-h" || cmd == "--help" {
        print!("{USAGE}");
        std::process::exit(0);
    }
    let mut a = Args {
        cmd,
        backend: "null".into(),
        playback_device: "hw:0,0".into(),
        capture_device: "hw:0,1".into(),
        capture_channels: 2,
        channel: ChannelPick::Left,
        period_ms: 20,
        playback_file: None,
        capture_file: None,
        no_realtime: false,
        rt: false,
        seconds: None,
        keep_open: true,
        cues: false,
        answer: None,
        volume: 1.0,
        agc: false,
        record: None,
        every: 1.0,
        device: "hw:0,0".into(),
        verbose: false,
    };
    let mut capture_device_set = false;
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or(format!("{k} needs a value"));
        let num = |s: String| {
            s.parse::<f64>()
                .map_err(|_| format!("{k}: not a number: {s}"))
        };
        match k.as_str() {
            "--backend" => a.backend = val()?,
            "--playback-device" => a.playback_device = val()?,
            "--capture-device" => {
                a.capture_device = val()?;
                capture_device_set = true;
            }
            "--device" => a.device = val()?,
            "--capture-channels" => a.capture_channels = num(val()?)? as u16,
            "--channel" => {
                a.channel = match val()?.as_str() {
                    "left" => ChannelPick::Left,
                    "right" => ChannelPick::Right,
                    "average" => ChannelPick::Average,
                    o => return Err(format!("--channel: {o}?")),
                }
            }
            "--period-ms" => a.period_ms = num(val()?)? as u32,
            "--playback-file" => a.playback_file = Some(val()?.into()),
            "--capture-file" => a.capture_file = Some(val()?.into()),
            "--no-realtime" => a.no_realtime = true,
            "--rt" => a.rt = true,
            "--seconds" => a.seconds = Some(num(val()?)?),
            "--keep-open" => a.keep_open = true,
            "--close" => a.keep_open = false,
            "--cues" => a.cues = true,
            "--answer" => a.answer = Some(val()?.into()),
            "--volume" => a.volume = num(val()?)? as f32,
            "--agc" => a.agc = true,
            "--record" => a.record = Some(val()?.into()),
            "--every" => a.every = num(val()?)?,
            "-v" | "--verbose" => a.verbose = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            o => return Err(format!("unknown option {o}")),
        }
    }
    if a.cmd == "probe" && !capture_device_set {
        a.capture_device = "hw:0,1".into();
    }
    Ok(a)
}

/// Minimal stderr logger (the crate logs through `log`).
struct Logger(log::LevelFilter);

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.0
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("{} {}: {}", r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}

fn config(a: &Args) -> Result<AudioConfig, String> {
    let backend = match a.backend.as_str() {
        "null" => Backend::Null,
        "alsa" => Backend::Alsa,
        "cpal" => Backend::Cpal,
        "file" => Backend::File {
            capture: a.capture_file.clone(),
            playback: a.playback_file.clone(),
            realtime: !a.no_realtime,
        },
        o => return Err(format!("--backend {o}?")),
    };
    Ok(AudioConfig {
        backend,
        playback_device: a.playback_device.clone(),
        capture_device: a.capture_device.clone(),
        capture_channels: a.capture_channels,
        capture_channel: a.channel,
        period_ms: a.period_ms,
        keep_open: a.keep_open,
        idle_close: Duration::from_secs(1),
        agc: a.agc,
        realtime: a.rt,
        volume: a.volume,
        recorder: a.record.clone().map(|dir| RecorderConfig {
            dir,
            keep: 20,
            pre_roll_ms: 0,
            wake_context_ms: 0,
        }),
    })
}

fn rss_max_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[derive(Default)]
struct Seen {
    finished: Vec<SourceId>,
    stopped: Vec<SourceId>,
    errors: Vec<String>,
}

type Started = (Audio, Arc<Mutex<Seen>>, std::sync::mpsc::Receiver<SourceId>);

fn start(cfg: &AudioConfig) -> Result<Started, String> {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (tx, rx) = channel();
    let s2 = seen.clone();
    let tx = Mutex::new(tx);
    let audio = Audio::start(
        cfg,
        Box::new(move |e| {
            let mut s = s2.lock().unwrap_or_else(|e| e.into_inner());
            match e {
                AudioEvent::Finished(id) => {
                    s.finished.push(id);
                    let _ = tx.lock().map(|t| t.send(id));
                }
                AudioEvent::Stopped(id) => s.stopped.push(id),
                AudioEvent::Error(m) => s.errors.push(m),
                _ => {}
            }
        }),
    )
    .map_err(|e| e.to_string())?;
    Ok((audio, seen, rx))
}

/// The answer to play: a WAV or, when there is none, 1.5 s of tones at 22050 Hz.
fn answer_pcm(a: &Args) -> Result<(Vec<u8>, StreamFormat), String> {
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/answer-22050.wav");
    let path = a
        .answer
        .clone()
        .or_else(|| default.exists().then_some(default));
    if let Some(p) = path {
        let mut r = hound::WavReader::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let spec = r.spec();
        if spec.bits_per_sample != 16 {
            return Err(format!("{}: 16-bit WAV expected", p.display()));
        }
        let bytes: Vec<u8> = r
            .samples::<i16>()
            .filter_map(Result::ok)
            .flat_map(|s| s.to_le_bytes())
            .collect();
        return Ok((
            bytes,
            StreamFormat {
                rate: spec.sample_rate,
                width: 2,
                channels: spec.channels,
            },
        ));
    }
    let rate = 22050u32;
    let bytes = (0..rate as usize * 3 / 2)
        .flat_map(|i| {
            let t = i as f32 / rate as f32;
            let f = if t < 0.75 { 440.0 } else { 554.0 };
            (((2.0 * std::f32::consts::PI * f * t).sin() * 6000.0) as i16).to_le_bytes()
        })
        .collect();
    Ok((
        bytes,
        StreamFormat {
            rate,
            width: 2,
            channels: 1,
        },
    ))
}

fn result(mut v: serde_json::Value, audio: &Audio, t0: Instant, cpu0: f64) {
    let st = audio.stats();
    let wall = t0.elapsed().as_secs_f64();
    let cpu = audio::process_cpu_seconds() - cpu0;
    let per = |c: f64, s: f64| if s > 0.0 { r3(c / s) } else { 0.0 };
    v["wall_s"] = r3(wall).into();
    v["process_cpu_s"] = r3(cpu).into();
    v["process_cpu_pct_of_core"] = per(100.0 * cpu, wall).into();
    v["capture_cpu_s_per_audio_s"] =
        (per(st.capture_cpu_s * 1000.0, st.capture_audio_s) / 1000.0).into();
    v["playback_cpu_s_per_audio_s"] =
        (per(st.playback_cpu_s * 1000.0, st.playback_audio_s) / 1000.0).into();
    v["output_rate"] = audio.output_rate().into();
    v["rss_max_mb"] = r3(rss_max_mb()).into();
    v["stats"] = serde_json::to_value(&st).unwrap_or_default();
    println!("RESULT {v}");
}

fn wait_for(rx: &std::sync::mpsc::Receiver<SourceId>, id: SourceId, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(x) if x == id => return true,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

fn cmd_probe(a: &Args) -> Result<(), String> {
    let mut v = serde_json::json!({"cmd": "probe"});
    for (dev, playback) in [(&a.device, true), (&a.capture_device, false)] {
        let key = if playback { "playback" } else { "capture" };
        match audio::probe::alsa_formats(dev, playback) {
            Ok(s) => {
                println!("{s}");
                v[key] = serde_json::from_str(&s).unwrap_or_default();
            }
            Err(e) => {
                eprintln!("{dev}: {e}");
                v[key] = serde_json::json!({"error": e.to_string()});
            }
        }
    }
    println!("RESULT {v}");
    Ok(())
}

fn cmd_play(a: &Args) -> Result<(), String> {
    let seconds = a.seconds.unwrap_or(6.0);
    let cfg = config(a)?;
    let (t0, cpu0) = (Instant::now(), audio::process_cpu_seconds());
    let (audio, seen, rx) = start(&cfg)?;
    let player = audio.player();
    let mut placed = Vec::new();
    // Everything is placed on the timeline before the answer starts: with --no-realtime the
    // mix races ahead as soon as there is something to play.
    if a.cues {
        let beep = cues::beep();
        for at in [1.0, 2.5] {
            player.play_at(&beep, Duration::from_secs_f64(at));
            placed.push(("beep", at));
        }
        for (name, clip, at) in [
            ("wake", cues::wake(), 3.5),
            ("done", cues::done(), 4.2),
            ("bell", cues::bell(), 4.9),
        ] {
            player.play_at(&clip, Duration::from_secs_f64(at));
            placed.push((name, at));
        }
    }
    // A silent marker at the end of the timeline: its `Finished` ends the run.
    let end = player.play_at(&Clip::silence(0), Duration::from_secs_f64(seconds));
    if a.cues {
        let (pcm, fmt) = answer_pcm(a)?;
        let s = player.open_stream(fmt);
        for chunk in pcm.chunks(2048) {
            s.push(chunk).map_err(|e| e.to_string())?;
        }
        s.finish();
        placed.push(("answer", 0.0));
    }
    let ok = wait_for(&rx, end, Duration::from_secs_f64(seconds * 2.0 + 5.0));
    let s = seen.lock().unwrap_or_else(|e| e.into_inner());
    let v = serde_json::json!({
        "cmd": "play", "backend": a.backend, "seconds": seconds, "keep_open": a.keep_open,
        "placed": placed.iter().map(|(n, t)| serde_json::json!({"sound": n, "at_s": t})).collect::<Vec<_>>(),
        "finished_events": s.finished.len(), "errors": s.errors, "timeline_complete": ok,
    });
    drop(s);
    result(v, &audio, t0, cpu0);
    audio.stop();
    if ok {
        Ok(())
    } else {
        Err("the run did not reach its end".into())
    }
}

fn cmd_duplex(a: &Args) -> Result<(), String> {
    let cfg = config(a)?;
    let seconds = match (a.seconds, &a.capture_file) {
        (Some(s), _) => s,
        (None, Some(f)) => {
            let r = hound::WavReader::open(f).map_err(|e| format!("{}: {e}", f.display()))?;
            r.duration() as f64 / r.spec().sample_rate as f64
        }
        (None, None) => 10.0,
    };
    let (t0, cpu0) = (Instant::now(), audio::process_cpu_seconds());
    let (audio, seen, _rx) = start(&cfg)?;
    let frames_rx = audio.subscribe(50);
    if let Some(r) = audio.recorder() {
        r.start("bench");
    }
    if a.cues {
        let p = audio.player();
        p.play(&cues::wake());
        p.play_repeated(&cues::bell(), 2, Duration::from_millis(500));
    }
    let want = (seconds * 1000.0 / audio::FRAME_MS as f64).round() as u64;
    let (mut got, mut sum_db, mut peak) = (0u64, 0f64, 0f32);
    let deadline = Instant::now() + Duration::from_secs_f64(seconds * 2.0 + 5.0);
    while got < want && Instant::now() < deadline {
        match frames_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(f) => {
                got += 1;
                sum_db += f.level.rms_dbfs as f64;
                peak = peak.max(f.level.peak);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let recorded = audio.recorder().and_then(|r| r.stop());
    let s = seen.lock().unwrap_or_else(|e| e.into_inner());
    let v = serde_json::json!({
        "cmd": "duplex", "backend": a.backend, "seconds": seconds, "agc": a.agc,
        "frames": got, "mean_frame_rms_dbfs": if got > 0 { r3(sum_db / got as f64) } else { -120.0 },
        "peak": r3(peak as f64), "recorded": recorded, "errors": s.errors,
    });
    drop(s);
    result(v, &audio, t0, cpu0);
    audio.stop();
    if got < want {
        return Err(format!("only {got} of {want} frames captured"));
    }
    Ok(())
}

fn cmd_levels(a: &Args) -> Result<(), String> {
    let cfg = config(a)?;
    let seconds = a.seconds.unwrap_or(10.0);
    let (t0, cpu0) = (Instant::now(), audio::process_cpu_seconds());
    let mut all = Vec::new();
    println!("     t   left dBFS (peak)   right dBFS (peak)   average dBFS (peak)");
    audio::meter::capture_levels(
        &cfg,
        Duration::from_secs_f64(seconds),
        Duration::from_secs_f64(a.every.max(0.02)),
        |l| {
            println!(
                "{:6.1}   {:7.1} ({:.3})    {:7.1} ({:.3})     {:7.1} ({:.3})",
                l.at_s,
                l.left_rms_dbfs,
                l.left_peak,
                l.right_rms_dbfs,
                l.right_peak,
                l.average_rms_dbfs,
                l.average_peak
            );
            all.push(*l);
        },
    )
    .map_err(|e| e.to_string())?;
    let mean = |f: fn(&audio::meter::ChannelLevels) -> f32| {
        if all.is_empty() {
            -120.0
        } else {
            r3(all.iter().map(|l| f(l) as f64).sum::<f64>() / all.len() as f64)
        }
    };
    let wall = t0.elapsed().as_secs_f64();
    let v = serde_json::json!({
        "cmd": "levels", "backend": a.backend, "seconds": seconds,
        "mean_rms_dbfs": {"left": mean(|l| l.left_rms_dbfs), "right": mean(|l| l.right_rms_dbfs), "average": mean(|l| l.average_rms_dbfs)},
        "intervals": all,
        "wall_s": r3(wall), "process_cpu_s": r3(audio::process_cpu_seconds() - cpu0), "rss_max_mb": r3(rss_max_mb()),
    });
    println!("RESULT {v}");
    Ok(())
}

fn main() {
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("audio-bench: {e}\n\n{USAGE}");
            std::process::exit(1);
        }
    };
    let level = if a.verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    static LOGGER: std::sync::OnceLock<Logger> = std::sync::OnceLock::new();
    let _ =
        log::set_logger(LOGGER.get_or_init(|| Logger(level))).map(|()| log::set_max_level(level));
    let r = match a.cmd.as_str() {
        "probe" => cmd_probe(&a),
        "play" => cmd_play(&a),
        "duplex" => cmd_duplex(&a),
        "levels" => cmd_levels(&a),
        o => Err(format!("unknown command {o}\n\n{USAGE}")),
    };
    if let Err(e) = r {
        eprintln!("audio-bench: {e}");
        std::process::exit(1);
    }
}
