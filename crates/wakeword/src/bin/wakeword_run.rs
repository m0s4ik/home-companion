// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! wakeword-run: run a microWakeWord v2 model over WAV files with the same detector home-companion uses.
//! Exit code: 0 = every file after --positives detected and none after --negatives (files given
//! before either flag are only reported); 1 = a list did not match; 2 = bad arguments or error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;
use wakeword::{Detector, DetectorConfig, Model, SAMPLE_RATE, wav};

const USAGE: &str = "usage: wakeword-run --manifest M.json [FILE.wav...] [--positives FILE.wav...] \
[--negatives FILE.wav...] [--cutoff X] [--window N] [--arena BYTES] [--cooloff-s S] [--refractory-s S] \
[--channel N] [--frame SAMPLES] [--realtime] [--json]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Any,
    Positive,
    Negative,
}

struct Opts {
    manifest: PathBuf,
    files: Vec<(PathBuf, Expect)>,
    cfg: DetectorConfig,
    channel: Option<usize>,
    frame: usize,
    /// Feed the frames at the audio rate (as home-companion does), not as fast as possible.
    realtime: bool,
    json: bool,
}

fn parse() -> Result<Opts, String> {
    let mut manifest = None;
    let mut files = Vec::new();
    let mut cfg = DetectorConfig::default();
    let mut channel = None;
    let mut frame = 320usize;
    let mut json = false;
    let mut realtime = false;
    let mut list = Expect::Any;
    let mut it = std::env::args().skip(1);
    fn value<T: std::str::FromStr>(key: &str, v: Option<String>) -> Result<T, String> {
        let v = v.ok_or_else(|| format!("{key} needs a value"))?;
        v.parse().map_err(|_| format!("{key}: invalid value '{v}'"))
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(value::<String>(&a, it.next())?)),
            "--positives" => list = Expect::Positive,
            "--negatives" => list = Expect::Negative,
            "--cutoff" => cfg.cutoff = Some(value(&a, it.next())?),
            "--window" => cfg.window = Some(value(&a, it.next())?),
            "--arena" => cfg.arena_bytes = Some(value(&a, it.next())?),
            "--cooloff-s" => {
                cfg.cooloff = Duration::from_secs_f64(value::<f64>(&a, it.next())?.max(0.0))
            }
            "--refractory-s" => {
                cfg.refractory = Duration::from_secs_f64(value::<f64>(&a, it.next())?.max(0.0))
            }
            "--channel" => channel = Some(value(&a, it.next())?),
            "--frame" => frame = value::<usize>(&a, it.next())?.max(1),
            "--json" => json = true,
            "--realtime" => realtime = true,
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => files.push((PathBuf::from(a), list)),
        }
    }
    let manifest = manifest.ok_or("--manifest is required")?;
    if files.is_empty() {
        return Err("no WAV files given".into());
    }
    Ok(Opts {
        manifest,
        files,
        cfg,
        channel,
        frame,
        realtime,
        json,
    })
}

fn main() -> ExitCode {
    let opts = match parse() {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("wakeword-run: {e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&opts) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("wakeword-run: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(o: &Opts) -> Result<bool, wakeword::Error> {
    let model = Model::load(&o.manifest)?;
    let mut det = Detector::new(&model, &o.cfg)?;
    let (mut ok, mut audio_total, mut cpu_total) = (true, 0.0f64, 0.0f64);
    let (mut pos_hit, mut pos_n, mut neg_hit, mut neg_n) = (0, 0, 0, 0);
    for (i, (path, expect)) in o.files.iter().enumerate() {
        let audio = wav::read_16k(path, o.channel)?;
        if i > 0 {
            det.reset();
        }
        let before = det.stats();
        let start = before.samples;
        let mut times = Vec::new();
        let mut max_prob = 0.0f32;
        let t0 = std::time::Instant::now();
        for (n, chunk) in audio.chunks(o.frame).enumerate() {
            if o.realtime {
                // Pace like a capture card: frame n is available n x frame later.
                let due = t0 + Duration::from_secs_f64((n * o.frame) as f64 / SAMPLE_RATE as f64);
                if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
                    std::thread::sleep(wait);
                }
            }
            if let Some(d) = det.feed(chunk)? {
                times.push((d.at_samples - start) as f64 / SAMPLE_RATE as f64);
            }
            max_prob = max_prob.max(det.last_probability());
        }
        let s = det.stats();
        let audio_s = audio.len() as f64 / SAMPLE_RATE as f64;
        let cpu_s = s.cpu_s - before.cpu_s;
        audio_total += audio_s;
        cpu_total += cpu_s;
        let detected = !times.is_empty();
        let verdict = match expect {
            Expect::Any => "",
            Expect::Positive => {
                pos_n += 1;
                pos_hit += detected as usize;
                ok &= detected;
                if detected { " ok" } else { " MISS" }
            }
            Expect::Negative => {
                neg_n += 1;
                neg_hit += detected as usize;
                ok &= !detected;
                if detected { " FALSE ACCEPT" } else { " ok" }
            }
        };
        let cpu_per_s = if audio_s > 0.0 { cpu_s / audio_s } else { 0.0 };
        if o.json {
            println!(
                "{}",
                serde_json::json!({
                    "file": path.display().to_string(),
                    "expect": match expect { Expect::Any => "any", Expect::Positive => "positive", Expect::Negative => "negative" },
                    "detected": detected,
                    "detections_s": times,
                    "max_prob": max_prob,
                    "invocations": s.invocations - before.invocations,
                    "audio_s": audio_s,
                    "cpu_s": cpu_s,
                    "cpu_per_audio_second": cpu_per_s,
                    "arena_used": det.arena_used(),
                    "arena_size": det.arena_size(),
                    "stride": det.stride(),
                })
            );
        } else {
            let at: Vec<String> = times.iter().map(|t| format!("{t:.2}")).collect();
            println!(
                "{}: {}{} max {:.3} invocations {} cpu/s {:.5} arena {}/{} stride {}{}",
                name(path),
                if detected {
                    "detected at "
                } else {
                    "not detected"
                },
                at.join(", "),
                max_prob,
                s.invocations - before.invocations,
                cpu_per_s,
                det.arena_used(),
                det.arena_size(),
                det.stride(),
                verdict
            );
        }
    }
    let cpu_per_s = if audio_total > 0.0 {
        cpu_total / audio_total
    } else {
        0.0
    };
    if o.json {
        println!(
            "{}",
            serde_json::json!({
                "summary": true, "model": model.name, "files": o.files.len(),
                "positives": pos_n, "positives_detected": pos_hit,
                "negatives": neg_n, "negatives_detected": neg_hit,
                "audio_s": audio_total, "cpu_s": cpu_total, "cpu_per_audio_second": cpu_per_s,
                "arena_used": det.arena_used(), "arena_size": det.arena_size(), "pass": ok,
            })
        );
    } else {
        println!(
            "summary {}: positives {pos_hit}/{pos_n} detected, negatives {neg_hit}/{neg_n} detected, \
             audio {audio_total:.1} s, cpu/s {cpu_per_s:.5}, arena {}/{} -> {}",
            model.name,
            det.arena_used(),
            det.arena_size(),
            if ok { "PASS" } else { "FAIL" }
        );
    }
    Ok(ok)
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}
