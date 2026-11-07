// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Capture through the file backend: channel pick, levels, re-chunking, fan-out.

mod common;

use audio::{AudioEvent, CaptureFrame, ChannelPick};
use common::*;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// Receives `n` frames.
fn take(rx: &Receiver<CaptureFrame>, n: usize) -> Vec<CaptureFrame> {
    (0..n)
        .map(|_| rx.recv_timeout(Duration::from_secs(5)).expect("a frame"))
        .collect()
}

fn capture(
    path: PathBuf,
    channels: u16,
    pick: ChannelPick,
    period_ms: u32,
    n: usize,
) -> Vec<CaptureFrame> {
    let mut cfg = file_cfg(Some(path), None, false);
    cfg.capture_channels = channels;
    cfg.capture_channel = pick;
    cfg.period_ms = period_ms;
    let (audio, _rx) = start(&cfg);
    let frames = audio.subscribe(64);
    let got = take(&frames, n);
    audio.stop();
    got
}

fn joined(frames: &[CaptureFrame]) -> Vec<i16> {
    frames
        .iter()
        .flat_map(|f| f.samples.iter().copied())
        .collect()
}

#[test]
fn channel_pick_left_right_average() {
    let f = fixture("stereo-left-16k.wav");
    // The fixture: 1 kHz at 0.5 of full scale on the left, silence on the right.
    let left = capture(f.clone(), 2, ChannelPick::Left, 20, 50);
    let right = capture(f.clone(), 2, ChannelPick::Right, 20, 50);
    let avg = capture(f, 2, ChannelPick::Average, 20, 50);
    assert_eq!(left[0].index, 0, "the first frame is delivered");
    let (l, r, a) = (
        rms_dbfs(&joined(&left)),
        rms_dbfs(&joined(&right)),
        rms_dbfs(&joined(&avg)),
    );
    assert!((l - (-9.03)).abs() < 0.1, "left {l}");
    assert_eq!(r, -120.0, "right");
    assert!((a - l + 6.02).abs() < 0.1, "average {a} vs left {l}");
    assert_eq!(crossings(&joined(&left)), 999);
    assert!(
        right
            .iter()
            .all(|f| f.level.peak == 0.0 && f.level.rms_dbfs == -120.0)
    );
}

#[test]
fn level_of_a_minus_20_dbfs_tone() {
    let dir = tmpdir("lvl");
    let p = dir.join("t.wav");
    let amp = 10f64.powf(-20.0 / 20.0) * 2f64.sqrt();
    write_wav(&p, 16000, 1, &sine(16000, 440.0, 32000, amp, 0.0));
    let frames = capture(p, 1, ChannelPick::Left, 20, 110);
    for f in &frames[5..85] {
        assert!((f.level.rms_dbfs - (-20.0)).abs() < 0.5, "{:?}", f.level);
        assert!((f.level.peak - amp as f32).abs() < 0.01, "{:?}", f.level);
    }
    // After the end of the file: silence.
    assert_eq!(frames[105].level.rms_dbfs, -120.0);
}

#[test]
fn frames_are_320_samples_whatever_the_period() {
    let f = fixture("stereo-left-16k.wav");
    let src = audio::wav::read_mono_16k(&f, ChannelPick::Left).unwrap();
    for period in [50, 30, 7] {
        let frames = capture(f.clone(), 2, ChannelPick::Left, period, 40);
        assert!(
            frames.iter().all(|f| f.samples.len() == 320),
            "period {period}"
        );
        assert!(frames.iter().enumerate().all(|(i, f)| f.index == i as u64));
        assert_eq!(joined(&frames), src[..40 * 320], "period {period}");
    }
}

#[test]
fn a_slow_subscriber_loses_frames_and_they_are_counted() {
    let mut cfg = file_cfg(Some(fixture("stereo-left-16k.wav")), None, true);
    cfg.capture_channels = 2;
    let (audio, events) = start(&cfg);
    let slow = audio.subscribe(2);
    let fast = audio.subscribe(100);
    std::thread::sleep(Duration::from_millis(500));
    let got_fast = fast.try_iter().count();
    let st = audio.stats();
    assert!(st.capture_dropped >= 15, "{st:?}");
    assert!(
        got_fast >= 20,
        "the other subscriber is not affected: {got_fast}"
    );
    assert_eq!(slow.try_iter().count(), 2);
    let mut seen = Vec::new();
    assert!(wait_event(
        &events,
        &mut seen,
        Duration::from_secs(2),
        |e| matches!(e, AudioEvent::CaptureDropped { .. })
    ));
    audio.stop();
}
