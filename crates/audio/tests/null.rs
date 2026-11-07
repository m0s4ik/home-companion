// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The null backend: real-time paced duplex with no device.

mod common;

use audio::{AudioConfig, Backend, cues};
use common::*;
use std::time::{Duration, Instant};

#[test]
fn one_second_duplex_without_xruns() {
    let cfg = AudioConfig {
        backend: Backend::Null,
        realtime: false,
        ..AudioConfig::default()
    };
    let (audio, events) = start(&cfg);
    let frames = audio.subscribe(10);
    let t = Instant::now();
    let id = audio.player().play(&cues::wake());
    let mut n = 0;
    while n < 50 {
        frames
            .recv_timeout(Duration::from_secs(2))
            .expect("frames keep coming");
        n += 1;
    }
    let elapsed = t.elapsed();
    assert!(
        elapsed > Duration::from_millis(850) && elapsed < Duration::from_millis(1500),
        "paced like a card: {elapsed:?}"
    );
    let mut seen = Vec::new();
    assert!(wait_event(
        &events,
        &mut seen,
        Duration::from_secs(2),
        is_finished(id)
    ));
    let st = audio.stats();
    assert_eq!(
        (st.underruns, st.overruns, st.capture_dropped),
        (0, 0, 0),
        "{st:?}"
    );
    assert!(st.pcm_open);
    assert!(st.queued_ms.is_some());
    let level = audio.player().output_level();
    assert!(level.rms_dbfs <= 0.0);
    let t = Instant::now();
    audio.stop();
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "stop took {:?}",
        t.elapsed()
    );
}

#[test]
fn volume_is_clamped_and_kept() {
    let (audio, _ev) = start(&AudioConfig {
        backend: Backend::Null,
        volume: 3.0,
        ..AudioConfig::default()
    });
    let p = audio.player();
    assert_eq!(p.volume(), 1.0);
    p.set_volume(0.25);
    assert_eq!(p.volume(), 0.25);
    p.set_volume(f32::NAN);
    assert_eq!(p.volume(), 0.0);
    audio.stop();
}

/// Close policy (`keep_open = false`): the reopened pcm's clock restarts from zero (the null
/// backend restarts its pacer on resume, as ALSA does), and `Finished` must still follow the
/// clip's real length instead of arriving late by the time the pcm spent closed.
#[test]
fn finished_on_time_after_reopen() {
    let cfg = AudioConfig {
        backend: Backend::Null,
        realtime: false,
        keep_open: false,
        idle_close: Duration::from_millis(100),
        ..AudioConfig::default()
    };
    let (audio, events) = start(&cfg);
    let mut seen = Vec::new();
    let id = audio.player().play(&cues::wake());
    assert!(wait_event(
        &events,
        &mut seen,
        Duration::from_secs(2),
        is_finished(id)
    ));
    let t = Instant::now();
    while audio.stats().pcm_open && t.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!audio.stats().pcm_open, "pcm closed when idle");
    std::thread::sleep(Duration::from_millis(1500));
    for round in 0..2 {
        // The wake cue lasts 220 ms; allow the 80 ms buffer and some scheduling slack.
        let t = Instant::now();
        let id = audio.player().play(&cues::wake());
        assert!(wait_event(
            &events,
            &mut seen,
            Duration::from_secs(3),
            is_finished(id)
        ));
        let took = t.elapsed();
        assert!(
            took >= Duration::from_millis(200) && took < Duration::from_millis(600),
            "round {round}: Finished after {took:?}"
        );
        // Let it close again before the second round.
        std::thread::sleep(Duration::from_millis(700));
    }
    audio.stop();
}
