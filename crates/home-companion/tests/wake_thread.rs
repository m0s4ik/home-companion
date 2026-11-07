// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The wake word thread runs the detector exactly once over each capture frame: the same work as
//! `wakeword-run` on the same audio (same samples, same invocations), so the stats line's
//! wake_cpu_s / wake_audio_s compares with `wakeword-run --realtime`.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use audio::{CaptureFrame, FRAME_SAMPLES, Level};
use home_companion::core::{CoreEvent, MicControl, MicMode};

#[test]
fn every_frame_is_fed_once() {
    let model = wakeword::Model::load(
        &character::repo_characters().join("alfred/wakeword/okay_alfred.json"),
    )
    .unwrap();
    let cfg = wakeword::DetectorConfig::default();
    let detector = wakeword::Detector::new(&model, &cfg).unwrap();
    let stride = detector.stride();

    // 10 s of a quiet tone, as 320-sample capture frames.
    let frames_n = 500u64;
    let (tx, rx) = mpsc::sync_channel(frames_n as usize);
    for i in 0..frames_n {
        let samples: Vec<i16> = (0..FRAME_SAMPLES)
            .map(|k| {
                let t = (i as usize * FRAME_SAMPLES + k) as f32 / 16000.0;
                (300.0 * (t * 440.0 * std::f32::consts::TAU).sin()) as i16
            })
            .collect();
        let level = Level::of(&samples);
        tx.send(CaptureFrame {
            samples: samples.into(),
            index: i,
            level,
        })
        .unwrap();
    }
    drop(tx);

    let mic = Arc::new(MicControl::default());
    mic.set(MicMode::Wake);
    let (core_tx, _core_rx) = mpsc::channel::<CoreEvent>();
    let stats = Arc::new(Mutex::new(None));
    let h = home_companion::wake_thread::spawn(
        rx,
        Some(detector),
        Vec::new(),
        mic,
        core_tx,
        stats.clone(),
    )
    .unwrap();
    h.join().unwrap();

    let s = stats.lock().unwrap().expect("stats");
    let samples = frames_n * FRAME_SAMPLES as u64;
    assert_eq!(s.samples, samples, "every sample fed exactly once");
    // One invocation every `stride` feature steps of 10 ms (160 samples); the front end's
    // 30 ms window delays the first slices.
    let expected = samples / (160 * stride as u64);
    assert!(
        s.invocations <= expected && s.invocations + 3 >= expected,
        "{} invocations, expected about {expected}",
        s.invocations
    );

    // The same audio through a fresh detector, as wakeword-run feeds it.
    let mut d = wakeword::Detector::new(&model, &cfg).unwrap();
    for i in 0..frames_n {
        let samples: Vec<i16> = (0..FRAME_SAMPLES)
            .map(|k| {
                let t = (i as usize * FRAME_SAMPLES + k) as f32 / 16000.0;
                (300.0 * (t * 440.0 * std::f32::consts::TAU).sin()) as i16
            })
            .collect();
        d.feed(&samples).unwrap();
    }
    assert_eq!(d.stats().invocations, s.invocations);
}

/// The stop word detector runs only while the core asks for it (the character speaks or rings):
/// the okay_alfred model stands in for a stop word on a recording that contains it.
#[test]
fn the_stop_word_is_heard_only_when_asked() {
    let model = wakeword::Model::load(
        &character::repo_characters().join("alfred/wakeword/okay_alfred.json"),
    )
    .unwrap();
    let cfg = wakeword::DetectorConfig::default();
    let wav = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/e2e-okay-alfred-what-time.wav");
    let mut r = hound::WavReader::open(&wav).unwrap();
    let samples: Vec<i16> = r.samples::<i16>().map(|s| s.unwrap()).collect();
    let run = |stop_on: bool| -> (usize, usize) {
        let (tx, rx) = mpsc::sync_channel(samples.len() / FRAME_SAMPLES + 1);
        for (i, c) in samples.chunks_exact(FRAME_SAMPLES).enumerate() {
            tx.send(CaptureFrame {
                samples: c.to_vec().into(),
                index: i as u64,
                level: Level::of(c),
            })
            .unwrap();
        }
        drop(tx);
        let mic = Arc::new(MicControl::default());
        mic.set(MicMode::Wake);
        mic.set_stop_listening(stop_on);
        let (core_tx, core_rx) = mpsc::channel::<CoreEvent>();
        let stop = wakeword::Detector::new(&model, &cfg).unwrap();
        let h = home_companion::wake_thread::spawn(
            rx,
            None,
            vec![stop],
            mic,
            core_tx,
            Arc::new(Mutex::new(None)),
        )
        .unwrap();
        h.join().unwrap();
        let ev: Vec<CoreEvent> = core_rx.try_iter().collect();
        let stops = ev
            .iter()
            .filter(|e| matches!(e, CoreEvent::StopWord(_)))
            .count();
        let wakes = ev
            .iter()
            .filter(|e| matches!(e, CoreEvent::WakeWord(_)))
            .count();
        (stops, wakes)
    };
    assert_eq!(run(false), (0, 0), "no stop word while nothing plays");
    let (stops, wakes) = run(true);
    assert!(stops >= 1, "the stop word is heard while asked for");
    assert_eq!(wakes, 0);
}
