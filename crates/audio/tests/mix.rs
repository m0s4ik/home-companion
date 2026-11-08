// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The mixer through the whole audio subsystem, file backend.
//!
//! `AUDIO_MIX_WAV=path cargo test -p audio --test mix` also checks a mix written by
//! `audio-bench play --backend file --no-realtime --cues` (beeps at 1.0 s and 2.5 s).

mod common;

use audio::{AudioEvent, Clip, StreamFormat, cues};
use common::*;
use std::time::Duration;

const LEVEL: f64 = -10.46; // 0.3 of full scale

fn check_beeps(left: &[i16], rate: u32, times: &[f64]) {
    for &t in times {
        let (onset, level) = find_beep(left, rate, t, 0.05);
        let err_ms = (onset as f64 / rate as f64 - t) * 1000.0;
        assert!(
            err_ms.abs() <= 1.0,
            "beep at {t} s found at {err_ms:+.2} ms"
        );
        assert!(
            (level - LEVEL).abs() < 0.3,
            "beep at {t} s: {level:.2} dBFS"
        );
    }
}

#[test]
fn cues_land_on_their_frames_over_an_answer() {
    let dir = tmpdir("mix");
    let out = dir.join("mix.wav");
    let (audio, rx) = start(&file_cfg(None, Some(out.clone()), false));
    let p = audio.player();
    // Everything placed on the timeline first: in non-real-time mode the mix races ahead as
    // soon as there is something to play, so a `play_at` issued later could come too late.
    let beep = cues::beep();
    p.play_at(&beep, Duration::from_millis(1000));
    p.play_at(&beep, Duration::from_millis(2500));
    let end = p.play_at(&Clip::silence(0), Duration::from_secs(3));
    // An answer underneath, as Home Assistant sends it.
    let (pcm, _) = common::read_left(&fixture("answer-22050.wav"));
    let s = p.open_stream(StreamFormat {
        rate: 22050,
        width: 2,
        channels: 1,
    });
    let bytes: Vec<u8> = pcm.iter().flat_map(|x| x.to_le_bytes()).collect();
    for c in bytes.chunks(2048) {
        s.push(c).unwrap();
    }
    s.finish();
    let mut seen = Vec::new();
    assert!(wait_event(
        &rx,
        &mut seen,
        Duration::from_secs(20),
        is_finished(end)
    ));
    // The answer can end after the last cue (it plays out at its own pace): wait for its
    // `Finished` too before stopping, or a slow machine stops the mix with it still queued.
    let answer = is_finished(s.id());
    if !seen.iter().any(&answer) {
        assert!(wait_event(&rx, &mut seen, Duration::from_secs(20), answer));
    }
    audio.stop();
    let (left, rate) = read_left(&out);
    assert_eq!(rate, 48000);
    check_beeps(&left, rate, &[1.0, 2.5]);
    let finished = seen
        .iter()
        .filter(|e| matches!(e, AudioEvent::Finished(_)))
        .count();
    assert_eq!(finished, 4, "{seen:?}");
}

#[test]
fn bench_mix_if_given() {
    if let Ok(p) = std::env::var("AUDIO_MIX_WAV") {
        let (left, rate) = read_left(std::path::Path::new(&p));
        assert_eq!(rate, 48000);
        check_beeps(&left, rate, &[1.0, 2.5]);
    }
}

#[test]
fn repeated_bell_has_its_gaps() {
    let dir = tmpdir("bell");
    let out = dir.join("bell.wav");
    let (audio, rx) = start(&file_cfg(None, Some(out.clone()), false));
    let p = audio.player();
    let id = p.play_repeated(&cues::beep(), 3, Duration::from_secs(1));
    let mut seen = Vec::new();
    assert!(wait_event(
        &rx,
        &mut seen,
        Duration::from_secs(20),
        is_finished(id)
    ));
    audio.stop();
    let (left, rate) = read_left(&out);
    // Onsets from the samples themselves (the 200 ms beep starts with a zero sample, so the
    // first non-zero one is onset + 1): 200 ms beep + 1 s gap = 1.2 s apart, exactly.
    let next_onset = |from: usize| from + left[from..].iter().position(|&s| s != 0).unwrap();
    let a = next_onset(0);
    let b = next_onset(a + 9600);
    let c = next_onset(b + 9600);
    assert_eq!(b - a, 57600);
    assert_eq!(c - b, 57600);
    let (mc, lc) = find_beep(&left, rate, c as f64 / rate as f64, 0.01);
    assert!(mc.abs_diff(c) <= 2, "matched filter {mc} vs {c}");
    assert!((lc - LEVEL).abs() < 0.3);
    // Silence in the gaps and after the third.
    assert!(left[a + 9600..b].iter().all(|&s| s == 0));
    // (The file may end right where the third beep does.)
    assert!(left[(c + 9600).min(left.len())..].iter().all(|&s| s == 0));
}

#[test]
fn overlapping_clips_saturate_in_the_output() {
    let dir = tmpdir("sat");
    let out = dir.join("sat.wav");
    let (audio, rx) = start(&file_cfg(None, Some(out.clone()), false));
    let p = audio.player();
    let loud = std::sync::Arc::new(Clip {
        samples: vec![30000; 4800],
    });
    p.play_at(&loud, Duration::from_millis(100));
    let id = p.play_at(&loud, Duration::from_millis(100));
    let mut seen = Vec::new();
    assert!(wait_event(
        &rx,
        &mut seen,
        Duration::from_secs(20),
        is_finished(id)
    ));
    audio.stop();
    let (left, _) = read_left(&out);
    assert!(left[4800..9600].iter().all(|&s| s == i16::MAX));
    assert!(left[..4800].iter().all(|&s| s == 0));
    assert!(left[9600..].iter().all(|&s| s == 0));
}

#[test]
fn stop_fades_out_without_a_click() {
    let dir = tmpdir("stop");
    let out = dir.join("stop.wav");
    // Real-time pacing so the stop lands in the middle of the tone.
    let (audio, rx) = start(&file_cfg(None, Some(out.clone()), true));
    let p = audio.player();
    let tone = std::sync::Arc::new(Clip {
        samples: sine(48000, 1000.0, 48000 * 3, 0.5, 0.0),
    });
    let id = p.play(&tone);
    std::thread::sleep(Duration::from_millis(400));
    p.stop(id);
    let mut seen = Vec::new();
    assert!(wait_event(
        &rx,
        &mut seen,
        Duration::from_secs(5),
        |e| matches!(
            e,
            AudioEvent::Stopped(x) if *x == id
        )
    ));
    // Let the fade leave the "card", then make sure Finished never comes.
    std::thread::sleep(Duration::from_millis(200));
    while let Ok(e) = rx.try_recv() {
        seen.push(e);
    }
    assert!(!p.is_active(id));
    audio.stop();
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AudioEvent::Finished(x) if *x == id)),
        "{seen:?}"
    );
    let (left, _) = read_left(&out);
    let max_step = left
        .windows(2)
        .map(|w| (w[1] as i32 - w[0] as i32).abs())
        .max()
        .unwrap();
    assert!(
        max_step < 3277,
        "max step {max_step} (0.1 full scale = 3277)"
    );
    let last = left.iter().rposition(|&s| s != 0).unwrap();
    let last_loud = left.iter().rposition(|&s| s.abs() > 15000).unwrap();
    assert!(last < 48000 * 3 - 4800, "the tone was cut, not played out");
    assert!(
        last - last_loud <= 480 + 48,
        "fade longer than 10 ms: {}",
        last - last_loud
    );
}
