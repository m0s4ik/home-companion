// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The debug utterance recorder.

mod common;

use audio::{ChannelPick, RecorderConfig};
use common::*;
use std::time::Duration;

/// A ramp the recorder output can be matched against: sample i = i mod 20000.
fn ramp_wav(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("ramp.wav");
    let s: Vec<i16> = (0..16000 * 6).map(|i| (i % 20000) as i16).collect();
    write_wav(&p, 16000, 1, &s);
    p
}

fn wavs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    v.sort();
    v
}

#[test]
fn recording_with_pre_roll_and_rotation() {
    let dir = tmpdir("rec");
    let rec_dir = dir.join("utterances");
    let mut cfg = file_cfg(Some(ramp_wav(&dir)), None, false);
    cfg.capture_channels = 1;
    cfg.recorder = Some(RecorderConfig {
        dir: rec_dir.clone(),
        keep: 2,
        pre_roll_ms: 200,
        wake_context_ms: 300,
    });
    let (audio, _ev) = start(&cfg);
    let frames = audio.subscribe(500);
    let rec = audio.recorder().expect("configured");
    // 0.5 s of capture, then a recording of about 1 s.
    for _ in 0..25 {
        frames.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    rec.start("what time is it?");
    assert!(rec.is_recording());
    for _ in 0..50 {
        frames.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let path = rec.stop().expect("a file");
    assert!(!rec.is_recording());
    // What came before the start, apart.
    let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
    let wake = path.with_file_name(format!("{stem}-wake.wav"));
    let w: Vec<i16> = hound::WavReader::open(&wake)
        .unwrap()
        .into_samples::<i16>()
        .map(Result::unwrap)
        .collect();
    assert_eq!(w.len(), 4800, "300 ms");
    assert!(
        w.windows(2)
            .all(|p| p[1] == p[0] + 1 || (p[0] == 19999 && p[1] == 0))
    );
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with("-what_time_is_it_.wav"), "{name}");

    let r = hound::WavReader::open(&path).unwrap();
    let spec = r.spec();
    assert_eq!(
        (spec.sample_rate, spec.channels, spec.bits_per_sample),
        (16000, 1, 16)
    );
    let s: Vec<i16> = r.into_samples::<i16>().map(Result::unwrap).collect();
    // Starts 200 ms (one full pre-roll) before a frame boundary and is a contiguous slice.
    assert!(s.len() >= 3200 + 50 * 320, "{}", s.len());
    assert_eq!(s.len() % 320, 0);
    assert_eq!(s[0] as usize % 320, 0, "starts on a frame boundary");
    assert!(
        s.windows(2)
            .all(|w| w[1] == w[0] + 1 || (w[0] == 19999 && w[1] == 0))
    );

    // Idle: nothing written any more.
    let len = std::fs::metadata(&path).unwrap().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), len);
    assert_eq!(
        wavs(&rec_dir).len(),
        2,
        "the recording and what came before it"
    );

    // keep = 2: the oldest goes.
    for label in ["b", "c"] {
        rec.start(label);
        for _ in 0..3 {
            frames.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        rec.stop().unwrap();
    }
    let left: Vec<_> = wavs(&rec_dir)
        .into_iter()
        .filter(|p| !p.to_string_lossy().ends_with("-wake.wav"))
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(!wake.exists(), "went with its recording");
    assert!(!left.contains(&path), "the first recording was deleted");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&rec_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
    audio.stop();
}

#[test]
fn no_recorder_unless_configured() {
    let (audio, _ev) = start(&file_cfg(None, None, false));
    assert!(audio.recorder().is_none());
    audio.stop();
    let _ = ChannelPick::Left;
}
