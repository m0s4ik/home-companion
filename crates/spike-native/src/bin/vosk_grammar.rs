// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! vosk-grammar: decode one 16 kHz mono WAV with libvosk limited to a closed grammar, streaming it
//! in chunks like a live microphone, and apply the reflex rule of design section 5.
//! Exit code: 0 = accepted, 1 = not accepted, 2 = error.

#[path = "../vosk_ffi.rs"]
mod vosk_ffi;

use spike_native::{args::Args, grammar, measure, wav};
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "usage: vosk-grammar --model DIR --grammar grammar.json --wav F.wav \
[--chunk-ms 100] [--min-conf 0.8] [--channel N]";

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::from(0),
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("vosk-grammar: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[derive(serde::Deserialize, Default)]
struct VoskResult {
    #[serde(default)]
    text: String,
    #[serde(default)]
    result: Vec<grammar::Word>,
}

fn run() -> Result<bool, String> {
    let a = Args::parse(&[])?;
    let model_dir = a.required("model")?;
    let grammar_path = a.required("grammar")?;
    let wav_path = a.required("wav")?;
    let chunk_ms: usize = a.get("chunk-ms")?.unwrap_or(100);
    let min_conf: f32 = a.get("min-conf")?.unwrap_or(0.8);
    if chunk_ms == 0 {
        return Err("--chunk-ms must be > 0".into());
    }

    let grammar_json = std::fs::read_to_string(grammar_path).map_err(|e| format!("{grammar_path}: {e}"))?;
    let phrases: Vec<String> =
        serde_json::from_str(&grammar_json).map_err(|e| format!("{grammar_path}: {e}"))?;
    let audio = wav::read_16k(wav_path, a.get("channel")?)?;
    let audio_s = audio.len() as f64 / wav::SAMPLE_RATE as f64;

    vosk_ffi::set_log_level(-1);
    let t = Instant::now();
    let model = vosk_ffi::Model::new(model_dir)?;
    let model_load_s = t.elapsed().as_secs_f64();

    let mut rec = vosk_ffi::Recognizer::with_grammar(&model, wav::SAMPLE_RATE as f32, &grammar_json)?;
    let mut segments: Vec<VoskResult> = Vec::new();
    let parse = |s: &str| serde_json::from_str::<VoskResult>(s).map_err(|e| format!("vosk result: {e}"));

    let cpu0 = measure::cpu_seconds();
    let t0 = Instant::now();
    for chunk in audio.chunks(wav::SAMPLE_RATE as usize * chunk_ms / 1000) {
        if rec.accept(chunk) {
            segments.push(parse(&rec.result())?);
        }
    }
    let tf = Instant::now();
    segments.push(parse(&rec.final_result())?);
    let final_ms = tf.elapsed().as_secs_f64() * 1000.0;
    let decode_s = t0.elapsed().as_secs_f64();
    let cpu_s = measure::cpu_seconds() - cpu0;

    // Join the utterances Vosk may have split the audio into.
    let text = segments.iter().map(|s| s.text.as_str()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
    let words: Vec<grammar::Word> = segments.into_iter().flat_map(|s| s.result).collect();
    let ok = grammar::accepted(&text, &words, &phrases, min_conf);
    let min_word_conf = grammar::min_conf(&words);

    let out = serde_json::json!({
        "wav": wav_path,
        "text": text,
        "words": words,
        "accepted": ok,
        "min_conf": min_conf,
        "min_word_conf": min_word_conf,
        "audio_s": audio_s,
        "model_load_s": model_load_s,
        "decode_s": decode_s,
        "final_ms": final_ms,
        "rtf": if audio_s > 0.0 { decode_s / audio_s } else { 0.0 },
        "cpu_s": cpu_s,
        "rss_mb": measure::rss_mb(),
        "peak_rss_mb": measure::peak_rss_mb(),
    });
    println!("{out}");
    println!(
        "{wav_path}: '{text}' {} (min word conf {min_word_conf:.2}), audio {audio_s:.2} s, load {model_load_s:.2} s, \
         decode {decode_s:.3} s, RTF {:.3}, final {final_ms:.0} ms, cpu {cpu_s:.3} s, RSS {:.0} MB",
        if ok { "ACCEPTED" } else { "rejected" },
        out["rtf"].as_f64().unwrap_or(0.0),
        measure::rss_mb()
    );
    Ok(ok)
}
