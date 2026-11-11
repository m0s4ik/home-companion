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
//!   home-companion-reflex --model DIR --grammar FILE --bench LIST.tsv [--threshold 0.8] [--out DIR]
//!
//! `--eval` tunes the threshold: LIST.tsv has one `file.wav<TAB>expected phrase` per line, `-` for
//! an utterance that must go to the brain (a negative). It prints each file's result and, per
//! threshold, the commands accepted right, the commands missed, the wrong commands and the
//! negatives accepted (false accepts: the number to keep at zero).
//!
//! `--bench` replays each file through the gate (reflex::verdict) as the body and the helper run
//! it, under every acceptance rule and early release compared, and prints per file the trace and
//! the verdicts, then per configuration the false accepts, the rejected reflexes and when the
//! negatives were released. LIST.tsv: `file<TAB>expected or -<TAB>source<TAB>class`.

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

#[derive(serde::Deserialize, Default)]
struct Partial {
    #[serde(default)]
    partial: String,
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
    if let Some(list) = get("--bench") {
        let threshold = get("--threshold")
            .and_then(|t| t.parse().ok())
            .unwrap_or(0.8);
        return match bench(&model, &grammar, &list, threshold, get("--out").as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("home-companion-reflex: {e}");
                ExitCode::from(65)
            }
        };
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
    // The last partial sent for this utterance.
    let mut partial = String::new();
    loop {
        let req = match Request::read(&mut input) {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("reading the body: {e}")),
        };
        match req {
            Request::Start(id) => {
                rec.reset();
                partial.clear();
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
                    let json = rec.result();
                    done = send(
                        &mut out,
                        current.unwrap_or(0),
                        true,
                        &json,
                        ms,
                        samples,
                        offset,
                    )?;
                    partial.clear();
                } else {
                    // ~11 us against ~450 us for the block's decoding (x86_64 bench): every block.
                    let p: Partial = serde_json::from_str(&rec.partial()).unwrap_or_default();
                    if p.partial != partial {
                        partial = p.partial;
                        Reply::Partial {
                            id: current.unwrap_or(0),
                            text: partial.clone(),
                            audio_s: samples as f64 / 16_000.0,
                        }
                        .write(&mut out)
                        .map_err(|e| format!("writing to the body: {e}"))?;
                    }
                }
            }
            Request::End(id) => {
                if !done && current == Some(id) {
                    let t = Instant::now();
                    let json = rec.final_result();
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    done = send(&mut out, id, false, &json, ms, samples, offset)?;
                    partial.clear();
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

/// Sends a result; true when it ends the utterance (it has words: after an empty one the helper
/// goes on decoding what follows, and another `E` gets another result).
fn send(
    out: &mut impl Write,
    id: u64,
    endpoint: bool,
    json: &str,
    ms: f64,
    samples: usize,
    offset: f32,
) -> Result<bool, String> {
    let r = shifted(serde_json::from_str(json).unwrap_or_default(), offset);
    let over = !r.text.trim().is_empty();
    Reply::Result {
        id,
        endpoint,
        text: r.text,
        words: r.result,
        ms,
        audio_s: samples as f64 / 16_000.0,
    }
    .write(out)
    .map_err(|e| format!("writing to the body: {e}"))?;
    Ok(over)
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

// ---- --bench: the gate's rules replayed on labelled recordings -----------------------------

/// One configuration of the gate: the acceptance rule and the early release.
#[derive(Clone, Copy)]
struct Config {
    name: &'static str,
    rule: reflex::verdict::Rule,
    stability_s: Option<f32>,
}

fn configs() -> Vec<Config> {
    use reflex::verdict::Rule;
    let rules: [(&str, Rule); 6] = [
        ("A", Rule::Covered),
        ("B", Rule::VoskOnly),
        ("C0.6", Rule::TailCovered { slack_s: 0.6 }),
        ("C0.8", Rule::TailCovered { slack_s: 0.8 }),
        ("C1.0", Rule::TailCovered { slack_s: 1.0 }),
        ("C1.5", Rule::TailCovered { slack_s: 1.5 }),
    ];
    let early: [(&str, Option<f32>); 4] = [
        ("off", None),
        ("K0", Some(0.0)),
        ("K200", Some(0.2)),
        ("K400", Some(0.4)),
    ];
    let mut v = Vec::new();
    for (rn, rule) in rules {
        for (en, stability_s) in early {
            let name: &'static str = Box::leak(format!("{rn}/{en}").into_boxed_str());
            v.push(Config {
                name,
                rule,
                stability_s,
            });
        }
    }
    v
}

/// What one replay gave.
struct Outcome {
    verdict: reflex::verdict::Verdict,
    at_s: f32,
    why: &'static str,
    by_partial: bool,
    /// The trace: (audio s, what) for every partial change, Vosk's endpoint, the detector's end, `E`.
    trace: Vec<(f32, String)>,
    partials: usize,
    blocks: usize,
    accept_us: f64,
    partial_us: f64,
}

/// One file through the gate as the body and the helper run it: 20 ms blocks to the detector and
/// to Vosk, partials read after each block, `E` when the verdict asks, the helper going on after
/// an empty result; silence appended if the file ends first.
fn replay(
    rec: &mut vosk::Recognizer,
    fed: &mut usize,
    grammar: &Grammar,
    threshold: f32,
    cfg: Config,
    samples: &[i16],
) -> Outcome {
    use reflex::verdict::{Ctx, Event, GateState, Limits, Verdict, verdict};
    rec.reset();
    let offset = *fed as f32 / 16_000.0;
    let mut vad = Vad::new();
    let mut st = GateState::new();
    let limits = Limits {
        stability_s: cfg.stability_s,
        ..Limits::default()
    };
    let (mut helper_done, mut last_partial, mut vad_end_seen) = (false, String::new(), false);
    let mut out = Outcome {
        verdict: Verdict::Hold,
        at_s: 0.0,
        why: "",
        by_partial: false,
        trace: Vec::new(),
        partials: 0,
        blocks: 0,
        accept_us: 0.0,
        partial_us: 0.0,
    };
    let silence = [0i16; reflex::vad::FRAME];
    let max_blocks = 12 * 16_000 / reflex::vad::FRAME;
    let mut blocks = samples.chunks(reflex::vad::FRAME);
    for i in 0..max_blocks {
        let block = blocks.next().unwrap_or(&silence);
        let pos = ((i + 1) * reflex::vad::FRAME) as f32 / 16_000.0;
        vad.push(block);
        let mut events: Vec<(String, Vec<Word>, Option<bool>)> = Vec::new(); // text, words, Some(endpoint) or None = partial
        if !helper_done {
            *fed += block.len();
            out.blocks += 1;
            let t = Instant::now();
            let end = rec.accept(block);
            out.accept_us += t.elapsed().as_secs_f64() * 1e6;
            if end {
                let r = shifted(
                    serde_json::from_str(&rec.result()).unwrap_or_default(),
                    offset,
                );
                out.trace.push((pos, format!("endpoint {:?}", r.text)));
                helper_done = !r.text.is_empty();
                last_partial.clear();
                events.push((r.text, r.result, Some(true)));
            } else {
                let t = Instant::now();
                let p: serde_json::Value = serde_json::from_str(&rec.partial()).unwrap_or_default();
                out.partial_us += t.elapsed().as_secs_f64() * 1e6;
                let p = p["partial"].as_str().unwrap_or("").to_string();
                if p != last_partial {
                    out.partials += 1;
                    out.trace.push((pos, format!("partial {p:?}")));
                    last_partial = p.clone();
                    events.push((p, Vec::new(), None));
                }
            }
        }
        if (vad.ended() || vad.nobody_spoke()) && !vad_end_seen {
            vad_end_seen = true;
            out.trace.push((pos, "detector end".into()));
        } else if !(vad.ended() || vad.nobody_spoke()) {
            vad_end_seen = false;
        }
        let ctx = Ctx {
            grammar,
            threshold,
            rule: cfg.rule,
            limits,
            vad: &vad,
            answering: false,
            elapsed_s: pos,
        };
        let mut v = Verdict::Hold;
        for (text, words, kind) in &events {
            v = match kind {
                None => verdict(&mut st, &ctx, Event::Partial { text, audio_s: pos }),
                Some(endpoint) => verdict(
                    &mut st,
                    &ctx,
                    Event::Final {
                        text,
                        words,
                        endpoint: *endpoint,
                    },
                ),
            };
            if !matches!(v, Verdict::Hold) {
                break;
            }
        }
        if matches!(v, Verdict::Hold) {
            v = verdict(&mut st, &ctx, Event::Tick);
        }
        if matches!(v, Verdict::Flush) {
            out.trace.push((pos, "E".into()));
            v = if helper_done {
                Verdict::Hold
            } else {
                let r = shifted(
                    serde_json::from_str(&rec.final_result()).unwrap_or_default(),
                    offset,
                );
                out.trace.push((pos, format!("final {:?}", r.text)));
                helper_done = !r.text.is_empty();
                last_partial.clear();
                verdict(
                    &mut st,
                    &ctx,
                    Event::Final {
                        text: &r.text,
                        words: &r.result,
                        endpoint: false,
                    },
                )
            };
        }
        if !matches!(v, Verdict::Hold | Verdict::Flush) {
            out.verdict = v;
            out.at_s = pos;
            out.why = st.why();
            out.by_partial = st.released_by_partial();
            return out;
        }
    }
    out.at_s = max_blocks as f32 * 0.02;
    out.why = "undecided";
    out
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f32::NAN)
}

/// `--bench LIST.tsv [--out DIR]`: LIST has `file<TAB>expected<TAB>source<TAB>class` (expected `-`
/// for a negative; class `reflex`, `negative`, `hard` or `noise`).
fn bench(
    model_dir: &str,
    grammar_path: &str,
    list: &str,
    threshold: f32,
    out_dir: Option<&str>,
) -> Result<(), String> {
    use reflex::verdict::Verdict;
    let grammar = Grammar::load(Path::new(grammar_path))?;
    vosk::set_log_level(-1);
    let model = vosk::Model::new(model_dir)?;
    let json = grammar.vosk_json();
    let text = std::fs::read_to_string(list).map_err(|e| format!("{list}: {e}"))?;
    struct Row {
        file: String,
        want: Option<reflex::Command>,
        source: String,
        class: String,
        res: Vec<Outcome>,
    }
    let cfgs = configs();
    let mut rows = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 {
            return Err(format!("{list}: {line:?}"));
        }
        let want = match f[1] {
            "-" => None,
            e => Some(
                grammar
                    .command(e)
                    .ok_or_else(|| format!("{e:?} is not a phrase of the grammar"))?,
            ),
        };
        let samples = read_wav(f[0])?;
        // A new recognizer for every replay: Vosk keeps adapting its features from one
        // utterance to the next, so a shared one would make the configurations differ by the
        // order they ran in.
        let mut res = Vec::new();
        for c in &cfgs {
            let mut rec = vosk::Recognizer::with_grammar(&model, 16_000.0, &json)?;
            let mut fed = 0usize;
            res.push(replay(
                &mut rec, &mut fed, &grammar, threshold, *c, &samples,
            ));
        }
        let name = Path::new(f[0])
            .file_name()
            .map_or(f[0].into(), |n| n.to_string_lossy().into_owned());
        let a = &res[0];
        println!(
            "{name}\t{}\t{}\taudio {:.2} s\t{} partial lines\ttrace: {}",
            f[3],
            f[2],
            samples.len() as f32 / 16_000.0,
            a.partials,
            a.trace
                .iter()
                .map(|(t, w)| format!("{t:.2} {w}"))
                .collect::<Vec<_>>()
                .join("; ")
        );
        for (c, o) in cfgs.iter().zip(&res) {
            println!(
                "    {:<10} {:>5.2} s  {:?} ({})",
                c.name, o.at_s, o.verdict, o.why
            );
        }
        rows.push(Row {
            file: name,
            want,
            source: f[2].into(),
            class: f[3].into(),
            res,
        });
    }
    if let Some(dir) = out_dir {
        let mut tsv = String::from("file\tclass\tsource\tconfig\tverdict\tat_s\twhy\n");
        for r in &rows {
            for (c, o) in cfgs.iter().zip(&r.res) {
                tsv += &format!(
                    "{}\t{}\t{}\t{}\t{:?}\t{:.2}\t{}\n",
                    r.file, r.class, r.source, c.name, o.verdict, o.at_s, o.why
                );
            }
        }
        std::fs::write(Path::new(dir).join("bench-files.tsv"), tsv).map_err(|e| e.to_string())?;
    }
    let neg = rows.iter().filter(|r| r.want.is_none()).count();
    let hard = rows.iter().filter(|r| r.class == "hard").count();
    let noise = rows.iter().filter(|r| r.class == "noise").count();
    let ha = rows
        .iter()
        .filter(|r| r.want.is_some() && r.source == "ha")
        .count();
    let dev = rows
        .iter()
        .filter(|r| r.want.is_some() && r.source == "device")
        .count();
    println!(
        "\n{} files: {} true reflexes ({ha} ha, {dev} device), {neg} negatives ({hard} hard, {noise} noise only); threshold {threshold}",
        rows.len(),
        ha + dev
    );
    println!(
        "config\tFA/neg\tFA hard\tFA noise\trej ha\trej dev\trej by partial\twrong\tneg release med\tneg release max\tact med"
    );
    for (k, c) in cfgs.iter().enumerate() {
        let (mut fa, mut fa_h, mut fa_n, mut rj_h, mut rj_d, mut rj_p, mut wrong) =
            (0, 0, 0, 0, 0, 0, 0);
        let (mut rel, mut act) = (Vec::new(), Vec::new());
        for r in &rows {
            let o = &r.res[k];
            match (&r.want, &o.verdict) {
                (None, Verdict::Act(_)) => {
                    fa += 1;
                    fa_h += (r.class == "hard") as usize;
                    fa_n += (r.class == "noise") as usize;
                }
                (None, _) => rel.push(o.at_s),
                (Some(w), Verdict::Act(g)) if w == g => act.push(o.at_s),
                (Some(_), Verdict::Act(_)) => wrong += 1,
                (Some(_), _) => {
                    if r.source == "ha" {
                        rj_h += 1
                    } else {
                        rj_d += 1
                    }
                    rj_p += o.by_partial as usize;
                }
            }
        }
        println!(
            "{}\t{fa}/{neg}\t{fa_h}/{hard}\t{fa_n}/{noise}\t{rj_h}/{ha}\t{rj_d}/{dev}\t{rj_p}\t{wrong}\t{:.2}\t{:.2}\t{:.2}",
            c.name,
            median(&mut rel.clone()),
            rel.iter().copied().fold(f32::NAN, f32::max),
            median(&mut act),
        );
    }
    let (mut lines, mut blocks, mut acc, mut par) = (0usize, 0usize, 0f64, 0f64);
    for r in &rows {
        // The early release at K = 0 runs longest before deciding; the costs of rule A/off are
        // the plain helper's.
        let o = &r.res[0];
        lines += o.partials;
        blocks += o.blocks;
        acc += o.accept_us;
        par += o.partial_us;
    }
    println!(
        "\npartial lines per utterance: {:.1} (rule A, no early release); per 20 ms block: accept {:.0} us, partial read {:.0} us",
        lines as f64 / rows.len().max(1) as f64,
        acc / blocks.max(1) as f64,
        par / blocks.max(1) as f64
    );
    Ok(())
}
