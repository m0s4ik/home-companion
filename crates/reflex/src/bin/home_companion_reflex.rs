// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! home-companion-reflex: the body's reflex helper. Loads a Vosk model and a
//! character's grammar, then decodes each utterance the body streams on standard input against
//! the closed grammar and answers on standard output (reflex::proto). Runs at a low priority;
//! the body starts it, restarts it when it dies, and never waits for it.
//!
//!   home-companion-reflex --model DIR --grammar FILE
//!   home-companion-reflex --model DIR --grammar FILE --wav F.wav...   (decode files, print one result each)
//!   home-companion-reflex --model DIR --grammar FILE --eval LIST.tsv [--thresholds 0.8,0.9,...]
//!
//! `--eval` tunes the threshold: LIST.tsv has one `file.wav<TAB>expected phrase` per line, `-` for
//! an utterance that must go to the brain (a negative). It prints each file's result and, per
//! threshold, the commands accepted right, the commands missed, the wrong commands and the
//! negatives accepted (false accepts: the number to keep at zero).

#[path = "../vosk_ffi.rs"]
mod vosk;

use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use reflex::proto::{Reply, Request};
use reflex::vad::Vad;
use reflex::{Grammar, Word};

#[derive(serde::Deserialize, Default)]
struct VoskResult {
    #[serde(default)]
    text: String,
    #[serde(default)]
    result: Vec<Word>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let (Some(model), Some(grammar)) = (get("--model"), get("--grammar")) else {
        eprintln!("usage: home-companion-reflex --model DIR --grammar FILE [--wav F.wav...]");
        return ExitCode::from(64);
    };
    let eval = get("--eval");
    let thresholds: Vec<f32> = get("--thresholds")
        .unwrap_or_else(|| "0.5,0.6,0.7,0.8,0.85,0.9,0.95,0.97,0.99".into())
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    let wavs: Vec<String> = args
        .iter()
        .skip_while(|a| *a != "--wav")
        .skip(1)
        .cloned()
        .collect();
    // Below the body and the audio (nice 10); a failure is harmless.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    if let Some(list) = eval {
        return match evaluate(&model, &grammar, &list, &thresholds) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("home-companion-reflex: {e}");
                ExitCode::from(65)
            }
        };
    }
    match run(&model, &grammar, &wavs) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = Reply::Error { message: e.clone() }.write(&mut std::io::stdout());
            eprintln!("home-companion-reflex: {e}");
            ExitCode::from(65)
        }
    }
}

fn run(model_dir: &str, grammar_path: &str, wavs: &[String]) -> Result<(), String> {
    let t0 = Instant::now();
    let grammar = Grammar::load(Path::new(grammar_path))?;
    vosk::set_log_level(-1);
    let model = vosk::Model::new(model_dir)?;
    let mut rec = vosk::Recognizer::with_grammar(&model, 16_000.0, &grammar.vosk_json())?;
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let out = std::io::stdout();
    let mut out = BufWriter::new(out.lock());
    let ready = Reply::Ready {
        language: grammar.language.clone(),
        phrases: grammar.len(),
        load_ms,
    };
    if !wavs.is_empty() {
        eprintln!(
            "home-companion-reflex: {} phrases, loaded in {load_ms:.0} ms",
            grammar.len()
        );
        let mut fed = 0usize;
        for (i, w) in wavs.iter().enumerate() {
            let samples = read_wav(w)?;
            let (r, end, ms, _, _) = decode(&mut rec, &mut fed, &samples);
            Reply::Result {
                id: i as u64,
                endpoint: end == "vosk",
                text: r.text,
                words: r.result,
                ms,
                audio_s: samples.len() as f64 / 16_000.0,
            }
            .write(&mut out)
            .map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    ready.write(&mut out).map_err(|e| e.to_string())?;

    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut current: Option<u64> = None;
    let mut done = true;
    let mut samples = 0usize;
    // Samples given to the recognizer since its creation (its word times count from there).
    let mut fed = 0usize;
    let mut offset = 0f32;
    loop {
        let req = match Request::read(&mut input) {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("reading the body: {e}")),
        };
        match req {
            Request::Start(id) => {
                rec.reset();
                offset = fed as f32 / 16_000.0;
                current = Some(id);
                done = false;
                samples = 0;
            }
            Request::Audio(s) => {
                if done {
                    continue;
                }
                samples += s.len();
                fed += s.len();
                let t = Instant::now();
                if rec.accept(&s) {
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    done = true;
                    let json = rec.result();
                    send(
                        &mut out,
                        current.unwrap_or(0),
                        true,
                        &json,
                        ms,
                        samples,
                        offset,
                    )?;
                }
            }
            Request::End(id) => {
                if !done && current == Some(id) {
                    done = true;
                    let t = Instant::now();
                    let json = rec.final_result();
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    send(&mut out, id, false, &json, ms, samples, offset)?;
                }
            }
        }
    }
}

/// One file as a live utterance: 20 ms blocks until Vosk's own end of speech or the body's
/// (reflex::vad), whichever comes first, as the body does. Returns the result, which end it was
/// ("vosk", "vad" or "file"), the last block's decoding time, the detector, and the audio
/// position of the decision in seconds.
fn decode(
    rec: &mut vosk::Recognizer,
    fed: &mut usize,
    samples: &[i16],
) -> (VoskResult, &'static str, f64, Vad, f64) {
    let (r, end, ms, vad, at, to_vosk) = decode_inner(rec, samples);
    let offset = *fed as f32 / 16_000.0;
    *fed += to_vosk;
    (shifted(r, offset), end, ms, vad, at)
}

/// Vosk's word times count from the recognizer's creation, not from `reset`: make them relative
/// to the utterance.
fn shifted(mut r: VoskResult, offset: f32) -> VoskResult {
    for w in &mut r.result {
        w.start = (w.start - offset).max(0.0);
        w.end = (w.end - offset).max(0.0);
    }
    r
}

/// Also returns how many samples Vosk was given (its clock).
fn decode_inner(
    rec: &mut vosk::Recognizer,
    samples: &[i16],
) -> (VoskResult, &'static str, f64, Vad, f64, usize) {
    rec.reset();
    let mut vad = Vad::new();
    let mut pos = 0usize;
    let mut chunks = samples.chunks(reflex::vad::FRAME);
    while let Some(chunk) = chunks.next() {
        pos += chunk.len();
        vad.push(chunk);
        let t = Instant::now();
        if rec.accept(chunk) {
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let r = serde_json::from_str(&rec.result()).unwrap_or_default();
            let to_vosk = pos;
            // The body decides at the detector's end of speech, with all the speech it heard.
            for rest in chunks.by_ref() {
                if vad.ended() || vad.nobody_spoke() {
                    break;
                }
                pos += rest.len();
                vad.push(rest);
            }
            return (r, "vosk", ms, vad, pos as f64 / 16_000.0, to_vosk);
        }
        if vad.ended() || vad.nobody_spoke() {
            let t = Instant::now();
            let json = rec.final_result();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let r = serde_json::from_str(&json).unwrap_or_default();
            return (r, "vad", ms, vad, pos as f64 / 16_000.0, pos);
        }
    }
    let t = Instant::now();
    let json = rec.final_result();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let r = serde_json::from_str(&json).unwrap_or_default();
    (r, "file", ms, vad, pos as f64 / 16_000.0, pos)
}

fn evaluate(
    model_dir: &str,
    grammar_path: &str,
    list: &str,
    thresholds: &[f32],
) -> Result<(), String> {
    let grammar = Grammar::load(Path::new(grammar_path))?;
    vosk::set_log_level(-1);
    let model = vosk::Model::new(model_dir)?;
    let mut rec = vosk::Recognizer::with_grammar(&model, 16_000.0, &grammar.vosk_json())?;
    let text = std::fs::read_to_string(list).map_err(|e| format!("{list}: {e}"))?;
    let base = Path::new(list).parent().unwrap_or(Path::new("."));
    // (expected command or None, decoded text, words)
    let mut rows: Vec<(Option<reflex::Command>, String, Vec<Word>, Vad)> = Vec::new();
    let (mut endpoints, mut worst_ms) = (0, 0f64);
    let mut after_speech: Vec<f64> = Vec::new();
    let mut fed = 0usize;
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let (file, expected) = line
            .split_once('\t')
            .ok_or_else(|| format!("{list}: {line:?}"))?;
        let expected = expected.trim();
        let want =
            if expected == "-" {
                None
            } else {
                Some(grammar.command(expected).ok_or_else(|| {
                    format!("{list}: {expected:?} is not a phrase of the grammar")
                })?)
            };
        let samples = read_wav(&base.join(file).to_string_lossy())?;
        let (r, end, ms, vad, at) = decode(&mut rec, &mut fed, &samples);
        endpoints += (end == "vosk") as usize;
        worst_ms = worst_ms.max(ms);
        let after = vad.span().map(|(_, b)| at - b as f64);
        if let Some(a) = after {
            after_speech.push(a);
        }
        println!(
            "{file}\t{}\t{:?}\tmin {:.2}\t{end}\t{}",
            if want.is_some() {
                "command"
            } else {
                "negative"
            },
            r.text,
            reflex::grammar::min_conf(&r.result),
            after.map_or("-".into(), |a| format!("+{:.2} s", a)),
        );
        rows.push((want, r.text, r.result, vad));
    }
    let commands = rows.iter().filter(|r| r.0.is_some()).count();
    let negatives = rows.len() - commands;
    after_speech.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        after_speech
            .get(((after_speech.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    println!(
        "\n{} files: {commands} commands, {negatives} negatives; Vosk's end of speech on {endpoints}, the detector's on the rest; \
         decision after the end of speech: median {:.2} s, max {:.2} s; slowest last block {worst_ms:.1} ms",
        rows.len(),
        pct(0.5),
        pct(1.0)
    );
    println!("threshold\tright\tmissed\twrong\tfalse accepts");
    for &th in thresholds {
        let (mut right, mut missed, mut wrong, mut fa) = (0, 0, 0, 0);
        for (want, text, words, vad) in &rows {
            let got = match grammar.decide_covered(text, words, th, vad) {
                reflex::Decision::Accept(c) => Some(c),
                _ => None,
            };
            match (want, got) {
                (Some(w), Some(g)) if *w == g => right += 1,
                (Some(_), Some(_)) => wrong += 1,
                (Some(_), None) => missed += 1,
                (None, Some(_)) => fa += 1,
                (None, None) => {}
            }
        }
        println!(
            "{th:.2}\t{right}/{commands} ({:.0} %)\t{missed}\t{wrong}\t{fa}/{negatives}",
            100.0 * right as f64 / commands.max(1) as f64
        );
    }
    Ok(())
}

fn send(
    out: &mut impl Write,
    id: u64,
    endpoint: bool,
    json: &str,
    ms: f64,
    samples: usize,
    offset: f32,
) -> Result<(), String> {
    let r = shifted(serde_json::from_str(json).unwrap_or_default(), offset);
    Reply::Result {
        id,
        endpoint,
        text: r.text,
        words: r.result,
        ms,
        audio_s: samples as f64 / 16_000.0,
    }
    .write(out)
    .map_err(|e| format!("writing to the body: {e}"))
}

/// A 16 kHz mono (or first channel) S16 WAV, for the --wav mode.
fn read_wav(path: &str) -> Result<Vec<i16>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut i = 12;
    let (mut channels, mut rate, mut bits) = (1u16, 0u32, 0u16);
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        let body = &b[i + 8..(i + 8 + len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            if rate != 16_000 || bits != 16 {
                return Err(format!("{path}: need 16 kHz S16, got {rate} Hz {bits} bit"));
            }
            let ch = channels.max(1) as usize;
            return Ok(body
                .chunks_exact(2 * ch)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect());
        }
        i += 8 + len + (len & 1);
    }
    Err(format!("{path}: no data chunk"))
}
