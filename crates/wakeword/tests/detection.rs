// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Detection tests on the committed fixtures. Run with `cargo test -p wakeword --release`
//! (TFLM in a debug build is slow).

use std::path::{Path, PathBuf};
use std::time::Duration;
use wakeword::{Detector, DetectorConfig, Error, Model, SAMPLE_RATE, wav};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(rel: &str) -> PathBuf {
    crate_dir().join("fixtures").join(rel)
}

fn alfred() -> Model {
    Model::load(&crate_dir().join("../../characters/alfred/wakeword/okay_alfred.json")).unwrap()
}

fn nabu() -> Model {
    Model::load(&fixture("okay_nabu/okay_nabu.json")).unwrap()
}

fn read(p: &Path) -> Vec<i16> {
    wav::read_16k(p, None).unwrap()
}

struct Run {
    times: Vec<f64>,
    max_prob: f32,
}

/// Feeds `audio` in frames of `frame` samples; detection times relative to the start of `audio`.
fn run_on(det: &mut Detector, audio: &[i16], frame: usize) -> Run {
    let start = det.stats().samples;
    let mut times = Vec::new();
    let mut max_prob = 0.0f32;
    for chunk in audio.chunks(frame) {
        if let Some(d) = det.feed(chunk).unwrap() {
            assert_eq!(
                d.at,
                Duration::from_secs_f64(d.at_samples as f64 / SAMPLE_RATE as f64)
            );
            times.push((d.at_samples - start) as f64 / SAMPLE_RATE as f64);
        }
        max_prob = max_prob.max(det.last_probability());
    }
    Run { times, max_prob }
}

fn run_file(model: &Model, path: &Path) -> Run {
    let mut det = Detector::new(model, &DetectorConfig::default()).unwrap();
    run_on(&mut det, &read(path), 320)
}

#[test]
fn model_metadata() {
    let m = alfred();
    assert_eq!(m.name, "okay_alfred");
    assert_eq!(m.phrase(), "okay alfred");
    assert_eq!(m.languages(), vec!["en".to_string()]);
    assert_eq!(m.manifest.author.as_deref(), Some("Tater Totterson"));
    assert_eq!(m.manifest.micro.tensor_arena_size, 30000);
    assert!(m.path.ends_with("okay_alfred.tflite"));
}

#[test]
fn arena_declared_by_okay_alfred_is_too_small() {
    let cfg = DetectorConfig {
        arena_bytes: Some(30000),
        ..Default::default()
    };
    match Detector::new(&alfred(), &cfg) {
        Err(Error::Arena(30000)) => {}
        other => panic!("expected Arena(30000), got {other:?}"),
    }
}

#[test]
fn arena_is_grown_automatically() {
    let det = Detector::new(&alfred(), &DetectorConfig::default()).unwrap();
    assert_eq!(det.arena_used(), 42912);
    assert!(
        (42912..=67500).contains(&det.arena_size()),
        "arena {}",
        det.arena_size()
    );
    assert_eq!(det.stride(), 2); // one invocation every 20 ms
    let det = Detector::new(&nabu(), &DetectorConfig::default()).unwrap();
    assert_eq!(det.arena_used(), 22448);
}

#[test]
fn arena_max_is_respected() {
    let cfg = DetectorConfig {
        arena_max: 40000,
        ..Default::default()
    };
    match Detector::new(&alfred(), &cfg) {
        Err(Error::Arena(40000)) => {}
        other => panic!("expected Arena(40000), got {other:?}"),
    }
}

#[test]
fn garbage_model_is_a_model_error() {
    let mut m = alfred();
    let n = m.bytes.len();
    m.bytes.truncate(n / 2);
    assert!(matches!(
        Detector::new(&m, &DetectorConfig::default()),
        Err(Error::Model(..))
    ));
}

/// (file stem, expected detection time in s or None, max probability when not detected)
const ALFRED_TABLE: &[(&str, Option<f64>)] = &[
    ("amy_okay_alfred", Some(2.60)),
    ("amy_hey_alfred", Some(2.30)),
    ("amy_ehi_alfred", Some(2.52)),
    ("amy_alfred", Some(2.26)),
    ("lessac_okay_alfred", Some(2.48)),
    ("lessac_hey_alfred", Some(2.30)),
    ("lessac_ehi_alfred", Some(2.40)),
    ("lessac_alfred", Some(2.16)),
    ("paola_okay_alfred", Some(2.32)),
    ("paola_hey_alfred", Some(2.34)),
    ("paola_ehi_alfred", Some(2.28)),
    ("paola_alfred", None),
];

#[test]
fn okay_alfred_detection_table() {
    let m = alfred();
    let mut report = String::new();
    for (stem, want) in ALFRED_TABLE {
        let r = run_file(&m, &fixture(&format!("alfred/{stem}.wav")));
        report += &format!("{stem}: {:?} max {:.3}\n", r.times, r.max_prob);
        match want {
            Some(t) => {
                assert_eq!(r.times.len(), 1, "{stem}: {:?}", r.times);
                assert!(
                    (r.times[0] - t).abs() <= 0.1,
                    "{stem}: at {:.2}, want {t:.2}",
                    r.times[0]
                );
                assert!(r.max_prob > 0.99, "{stem}: max {}", r.max_prob);
            }
            None => {
                assert!(
                    r.times.is_empty(),
                    "{stem} must NOT be detected (model changed?): {:?}",
                    r.times
                );
                assert!(
                    (r.max_prob - 0.137).abs() < 0.02,
                    "{stem}: max {}",
                    r.max_prob
                );
            }
        }
    }
    println!("{report}");
}

#[test]
fn okay_alfred_negatives() {
    let m = alfred();
    for (stem, max) in [
        ("fuori1", 0.502),
        ("fuori2", 0.024),
        ("fuori3", 0.024),
        ("ora", 0.914),
    ] {
        let r = run_file(&m, &fixture(&format!("negatives/{stem}.wav")));
        println!("{stem}: {:?} max {:.3}", r.times, r.max_prob);
        assert!(r.times.is_empty(), "{stem} detected at {:?}", r.times);
        assert!(
            (r.max_prob - max).abs() < 0.02,
            "{stem}: max {} (M0: {max})",
            r.max_prob
        );
    }
}

#[test]
fn okay_nabu_regression_unchanged_from_m0() {
    let m = nabu();
    let mut missed = Vec::new();
    for e in std::fs::read_dir(fixture("nabu")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "wav") {
            let r = run_file(&m, &p);
            if r.times.is_empty() {
                missed.push(p.file_stem().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    missed.sort();
    assert_eq!(missed, ["okay_nabu_amy_comma", "okay_nabu_amy_slow"]);
    for stem in ["fuori1", "fuori2", "fuori3", "ora"] {
        assert!(
            run_file(&m, &fixture(&format!("negatives/{stem}.wav")))
                .times
                .is_empty(),
            "{stem}"
        );
    }
    let mut det = Detector::new(&m, &DetectorConfig::default()).unwrap();
    assert!(
        run_on(&mut det, &vec![0i16; 5 * SAMPLE_RATE as usize], 320)
            .times
            .is_empty(),
        "silence"
    );
}

#[test]
fn streaming_invariance() {
    let m = alfred();
    let audio = read(&fixture("alfred/lessac_ehi_alfred.wav"));
    let mut results = Vec::new();
    for frame in [320, 1000, audio.len()] {
        let mut det = Detector::new(&m, &DetectorConfig::default()).unwrap();
        let r = run_on(&mut det, &audio, frame);
        assert_eq!(r.times.len(), 1, "frame {frame}: {:?}", r.times);
        results.push((r.times[0], det.stats().invocations, frame));
    }
    // One stride = 2 x 10 ms for okay_alfred.
    for (t, inv, frame) in &results {
        assert!(
            (t - results[0].0).abs() <= 0.02 + 1e-9,
            "frame {frame}: {t} vs {}",
            results[0].0
        );
        assert_eq!(*inv, results[0].1);
    }
    // Max probability per frame size: the whole-file call only sees the last probability, so
    // compare the detection probabilities instead.
    let probs: Vec<f32> = [320usize, 1000, audio.len()]
        .iter()
        .map(|&f| {
            let mut det = Detector::new(&m, &DetectorConfig::default()).unwrap();
            audio
                .chunks(f)
                .find_map(|c| det.feed(c).unwrap())
                .unwrap()
                .probability
        })
        .collect();
    assert!(
        probs.iter().all(|p| (p - probs[0]).abs() < 1e-6),
        "{probs:?}"
    );
}

#[test]
fn reset_is_like_a_fresh_detector() {
    let m = alfred();
    let audio = read(&fixture("alfred/lessac_ehi_alfred.wav"));
    let mut fresh = Detector::new(&m, &DetectorConfig::default()).unwrap();
    let first = run_on(&mut fresh, &audio, 320);
    // Default refractory (2 s) is shorter than the file (4 s): reset + same file again.
    fresh.reset();
    let second = run_on(&mut fresh, &audio, 320);
    assert_eq!(first.times.len(), 1);
    assert_eq!(second.times.len(), 1);
    assert!(
        (first.times[0] - second.times[0]).abs() < 1e-9,
        "{:?} vs {:?}",
        first.times,
        second.times
    );
    assert_eq!(fresh.stats().detections, 2);
}

#[test]
fn reset_clears_the_refractory() {
    // The conversation starts right at the detection: the detector is not fed while streaming,
    // then `reset()` and the same word at once must be detected (the refractory must not carry over).
    let m = alfred();
    let audio = read(&fixture("alfred/lessac_ehi_alfred.wav"));
    let mut det = Detector::new(&m, &DetectorConfig::default()).unwrap();
    let mut stopped = None;
    for chunk in audio.chunks(320) {
        if let Some(d) = det.feed(chunk).unwrap() {
            stopped = Some(d.at_samples);
            break;
        }
    }
    let stopped = stopped.expect("first detection");
    assert!((stopped as f64 / SAMPLE_RATE as f64 - 2.40).abs() < 0.1);
    det.reset();
    // Same word without its first 0.8 s of silence: detected ~1.6 s after the reset, past the
    // 1 s cool-off but inside the 2 s refractory of the first detection.
    let trimmed = &audio[(SAMPLE_RATE as usize * 8 / 10)..];
    let again = run_on(&mut det, trimmed, 320);
    assert_eq!(again.times.len(), 1, "{:?}", again.times);
    assert!(again.times[0] < 2.0, "{:?}", again.times);
    assert_eq!(det.stats().detections, 2);
}

#[test]
fn without_reset_and_refractory_the_cooloff_alone_allows_two() {
    let m = alfred();
    let audio = read(&fixture("alfred/lessac_ehi_alfred.wav"));
    let mut twice = audio.clone();
    twice.extend_from_slice(&audio);
    let cfg = DetectorConfig {
        refractory: Duration::ZERO,
        ..Default::default()
    };
    let mut det = Detector::new(&m, &cfg).unwrap();
    let r = run_on(&mut det, &twice, 320);
    assert_eq!(r.times.len(), 2, "{:?}", r.times);
    // A refractory longer than the gap leaves one.
    let cfg = DetectorConfig {
        refractory: Duration::from_secs(10),
        ..Default::default()
    };
    let mut det = Detector::new(&m, &cfg).unwrap();
    let r = run_on(&mut det, &twice, 320);
    assert_eq!(r.times.len(), 1, "{:?}", r.times);
}

#[test]
fn sixty_seconds_mixed() {
    let m = alfred();
    let mut audio = Vec::new();
    let mut expected = 0;
    for (stem, want) in ALFRED_TABLE {
        audio.extend(read(&fixture(&format!("alfred/{stem}.wav"))));
        expected += want.is_some() as usize;
    }
    for stem in ["fuori1", "fuori2", "fuori3", "ora"] {
        audio.extend(read(&fixture(&format!("negatives/{stem}.wav"))));
    }
    audio.extend(std::iter::repeat_n(0i16, 5 * SAMPLE_RATE as usize));
    let audio_s = audio.len() as f64 / SAMPLE_RATE as f64;
    assert!(audio_s >= 58.0, "{audio_s}");
    let mut det = Detector::new(&m, &DetectorConfig::default()).unwrap();
    let r = run_on(&mut det, &audio, 320);
    let s = det.stats();
    println!(
        "{audio_s:.1} s: {} detections, cpu {:.4} s, cpu per audio second {:.5}, {} invocations",
        r.times.len(),
        s.cpu_s,
        s.cpu_s / audio_s,
        s.invocations
    );
    assert_eq!(r.times.len(), expected, "{:?}", r.times);
    assert_eq!(s.detections as usize, expected);
    assert_eq!(s.samples, audio.len() as u64);
    assert!(
        s.cpu_s / audio_s < 0.05,
        "cpu per audio second {}",
        s.cpu_s / audio_s
    );
}

#[test]
fn silence_never_triggers() {
    let mut det = Detector::new(&alfred(), &DetectorConfig::default()).unwrap();
    let r = run_on(&mut det, &vec![0i16; 60 * SAMPLE_RATE as usize], 320);
    assert!(r.times.is_empty());
    // One invocation every stride x 10 ms; the 30 ms analysis window delays the first slice.
    let per_s = 100 / det.stride() as u64;
    assert_eq!(det.stats().invocations, 60 * per_s - 1);
}
