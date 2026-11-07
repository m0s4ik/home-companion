// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! mww: run a microWakeWord v2 model over a 16 kHz mono WAV, the way ESPHome does.
//! Exit code: 0 = detected, 1 = not detected, 2 = error.

use spike_native::{args::Args, detect::SlidingWindow, manifest::Manifest, measure, mww, wav};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "usage: mww --model M.tflite --manifest M.json --wav F.wav \
[--cutoff 0.97] [--window 5] [--arena 26080] [--cooloff-s 1.0] [--channel N] [--dump-features] [--quiet]";

const CHUNK: usize = 160; // 10 ms at 16 kHz

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::from(0),
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("mww: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let a = Args::parse(&["dump-features", "quiet"])?;
    let man = Manifest::load(a.required("manifest")?)?;
    let model_path = a.required("model")?;
    let wav_path = a.required("wav")?;
    let cutoff = a.get("cutoff")?.unwrap_or(man.micro.probability_cutoff);
    let window = a.get("window")?.unwrap_or(man.micro.sliding_window_size);
    let arena = a.get("arena")?.unwrap_or(man.micro.tensor_arena_size);
    let cooloff_s: f64 = a.get("cooloff-s")?.unwrap_or(1.0);
    let dump = a.flag("dump-features");
    let quiet = a.flag("quiet");

    let model = std::fs::read(model_path).map_err(|e| format!("{model_path}: {e}"))?;
    let audio = wav::read_16k(wav_path, a.get("channel")?)?;

    let cpu0 = measure::cpu_seconds();
    let mut ww = mww::WakeWord::new(&model, arena, man.micro.feature_step_size, dump)?;
    let cooloff = SlidingWindow::cooloff_invocations(cooloff_s, man.micro.feature_step_size, ww.stride());
    let mut win = SlidingWindow::new(cutoff, window, cooloff);
    let mut detections = Vec::new();
    let (mut max_prob, mut invocations, mut pos, mut slice) = (0u8, 0u64, 0usize, 0u64);
    let step_s = man.micro.feature_step_size as f64 / 1000.0;

    let cpu1 = measure::cpu_seconds();
    let wall = Instant::now();
    for chunk in audio.chunks(CHUNK) {
        let mut off = 0;
        while off < chunk.len() {
            match ww.process(&chunk[off..])? {
                mww::Step::NeedMore => break,
                mww::Step::Features { consumed } => {
                    off += consumed;
                    dump_slice(&ww, slice, step_s);
                    slice += 1;
                }
                mww::Step::Probability { prob, consumed } => {
                    off += consumed;
                    if dump {
                        dump_slice(&ww, slice, step_s);
                        slice += 1;
                    }
                    invocations += 1;
                    max_prob = max_prob.max(prob);
                    let t = (pos + off) as f64 / wav::SAMPLE_RATE as f64;
                    if win.push(prob) {
                        detections.push(t);
                        if !quiet {
                            println!("detected at {t:.2} s (prob {prob})");
                        }
                    }
                }
            }
        }
        pos += chunk.len();
    }
    let cpu_s = measure::cpu_seconds() - cpu1;
    let wall_s = wall.elapsed().as_secs_f64();
    let audio_s = audio.len() as f64 / wav::SAMPLE_RATE as f64;

    let result = serde_json::json!({
        "wav": wav_path,
        "detected": !detections.is_empty(),
        "detections_s": detections,
        "max_prob": max_prob,
        "max_prob_f": max_prob as f64 / 255.0,
        "invocations": invocations,
        "stride": ww.stride(),
        "audio_s": audio_s,
        "cpu_s": cpu_s,
        "cpu_init_s": cpu1 - cpu0,
        "wall_s": wall_s,
        "cpu_per_audio_second": if audio_s > 0.0 { cpu_s / audio_s } else { 0.0 },
        "rss_mb": measure::rss_mb(),
        "peak_rss_mb": measure::peak_rss_mb(),
        "arena_used": ww.arena_used(),
        "arena_size": arena,
        "arena_manifest": man.micro.tensor_arena_size,
        "cutoff": cutoff,
        "window": window,
        "cooloff_invocations": cooloff,
    });
    if !quiet {
        println!(
            "{}: {} detection(s), max prob {:.3}, audio {:.2} s, cpu {:.4} s, cpu/s audio {:.5}, \
             RSS {:.1} MB (peak {:.1}), arena {}/{} bytes",
            wav_path,
            detections.len(),
            max_prob as f64 / 255.0,
            audio_s,
            cpu_s,
            result["cpu_per_audio_second"].as_f64().unwrap_or(0.0),
            measure::rss_mb(),
            measure::peak_rss_mb(),
            ww.arena_used(),
            arena
        );
    }
    println!("RESULT {result}");
    Ok(!detections.is_empty())
}

fn dump_slice(ww: &mww::WakeWord, slice: u64, step_s: f64) {
    let raw: Vec<String> = ww.raw.iter().map(|v| v.to_string()).collect();
    let q: Vec<String> = ww.features.iter().map(|v| v.to_string()).collect();
    println!("features {:.2} raw [{}] int8 [{}]", slice as f64 * step_s, raw.join(" "), q.join(" "));
}
