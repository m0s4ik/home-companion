// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Answer streams: conversion, resampling, start rule, `Finished` / `Stopped`.

mod common;

use audio::{AudioEvent, Clip, SourceId, StreamFormat};
use common::*;
use std::time::Duration;

/// Plays one stream (pushed in 2048-byte chunks) through the file backend; returns the left
/// channel of the output and the events.
fn play_stream(
    name: &str,
    fmt: StreamFormat,
    bytes: &[u8],
) -> (Vec<i16>, Vec<AudioEvent>, SourceId) {
    let dir = tmpdir(name);
    let out = dir.join("out.wav");
    let (audio, rx) = start(&file_cfg(None, Some(out.clone()), false));
    let p = audio.player();
    let s = p.open_stream(fmt);
    for c in bytes.chunks(2048) {
        s.push(c).unwrap();
    }
    s.finish();
    let id = s.id();
    let mut seen = Vec::new();
    assert!(wait_event(
        &rx,
        &mut seen,
        Duration::from_secs(20),
        is_finished(id)
    ));
    // A little more timeline so a late extra event would show up.
    let tail = p.play(&Clip::silence(100));
    assert!(wait_event(
        &rx,
        &mut seen,
        Duration::from_secs(20),
        is_finished(tail)
    ));
    audio.stop();
    (read_left(&out).0, seen, id)
}

fn bytes16(s: &[i16]) -> Vec<u8> {
    s.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn nonzero_span(s: &[i16]) -> (usize, usize) {
    let a = s.iter().position(|&x| x != 0).unwrap();
    let b = s.iter().rposition(|&x| x != 0).unwrap();
    (a, b + 1)
}

fn finished_count(seen: &[AudioEvent], id: SourceId) -> usize {
    seen.iter()
        .filter(|e| matches!(e, AudioEvent::Finished(x) if *x == id))
        .count()
}

#[test]
fn length_22050_mono_in_2048_byte_chunks() {
    for n in [22050usize, 33075, 10_001] {
        // A constant signal: every output sample is non-zero, so the span is the length.
        let input = vec![8000i16; n];
        let fmt = StreamFormat {
            rate: 22050,
            width: 2,
            channels: 1,
        };
        let (out, seen, id) = play_stream(&format!("len{n}"), fmt, &bytes16(&input));
        let (a, b) = nonzero_span(&out);
        let expected = (n as f64 * 48000.0 / 22050.0).round() as i64;
        assert!(
            ((b - a) as i64 - expected).abs() <= 1,
            "n={n}: {} vs {expected}",
            b - a
        );
        assert_eq!(finished_count(&seen, id), 1);
    }
}

#[test]
fn tone_keeps_frequency_and_level() {
    let input = sine(22050, 1000.0, 22050 * 2, 0.5, 0.3);
    let fmt = StreamFormat {
        rate: 22050,
        width: 2,
        channels: 1,
    };
    let (out, _, _) = play_stream("tone", fmt, &bytes16(&input));
    let (a, _) = nonzero_span(&out);
    let mid = &out[a + 4800..a + 4800 + 48000];
    assert_eq!(crossings(mid), 1000);
    let want = rms_dbfs(&input[2205..2205 + 22050]);
    let got = rms_dbfs(mid);
    assert!((got - want).abs() < 0.1, "{got:.3} vs {want:.3} dBFS");
}

#[test]
fn stereo_44100_and_8000_width_1() {
    // 44.1 kHz stereo, 16-bit: left and right different, downmixed to their mean.
    let l = sine(44100, 500.0, 44100, 0.4, 0.3);
    let r = sine(44100, 500.0, 44100, 0.2, 0.3);
    let inter: Vec<i16> = l.iter().zip(&r).flat_map(|(&a, &b)| [a, b]).collect();
    let fmt = StreamFormat {
        rate: 44100,
        width: 2,
        channels: 2,
    };
    let (out, _, _) = play_stream("st44", fmt, &bytes16(&inter));
    let (a, b) = nonzero_span(&out);
    assert!(((b - a) as i64 - 48000).abs() <= 1, "{}", b - a);
    let mid = &out[a + 4800..a + 4800 + 24000];
    assert_eq!(crossings(mid), 250);
    let want = 20.0 * (0.3f64 / 2f64.sqrt()).log10();
    assert!((rms_dbfs(mid) - want).abs() < 0.2, "{}", rms_dbfs(mid));

    // 8 kHz, 8-bit signed.
    let s8: Vec<u8> = (0..8000)
        .map(|i| {
            let v = (2.0 * std::f64::consts::PI * 300.0 * i as f64 / 8000.0 + 0.3).sin() * 100.0;
            (v.round() as i8) as u8
        })
        .collect();
    let fmt = StreamFormat {
        rate: 8000,
        width: 1,
        channels: 1,
    };
    let (out, _, _) = play_stream("s8", fmt, &s8);
    let (a, b) = nonzero_span(&out);
    assert!(((b - a) as i64 - 48000).abs() <= 1, "{}", b - a);
    assert_eq!(crossings(&out[a + 4800..a + 4800 + 24000]), 150);
}

#[test]
fn width_4_is_scaled() {
    let s: Vec<u8> = sine(16000, 400.0, 16000, 0.5, 0.3)
        .iter()
        .flat_map(|&x| ((x as i32) << 16).to_le_bytes())
        .collect();
    let fmt = StreamFormat {
        rate: 16000,
        width: 4,
        channels: 1,
    };
    let (out, _, _) = play_stream("w4", fmt, &s);
    let (a, _) = nonzero_span(&out);
    let level = rms_dbfs(&out[a + 4800..a + 4800 + 24000]);
    assert!((level - (-9.03)).abs() < 0.1, "{level}");
}

#[test]
fn finish_without_push_gives_finished_and_no_audio() {
    let fmt = StreamFormat {
        rate: 22050,
        width: 2,
        channels: 1,
    };
    let (out, seen, id) = play_stream("empty", fmt, &[]);
    assert!(out.iter().all(|&s| s == 0));
    assert_eq!(finished_count(&seen, id), 1);
}

#[test]
fn abort_gives_stopped_and_never_finished() {
    let dir = tmpdir("abort");
    let (audio, rx) = start(&file_cfg(None, Some(dir.join("o.wav")), true));
    let p = audio.player();
    let s = p.open_stream(StreamFormat {
        rate: 16000,
        width: 2,
        channels: 1,
    });
    s.push(&bytes16(&vec![1000i16; 16000 * 2])).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(p.is_active(s.id()));
    s.abort();
    let id = s.id();
    drop(s);
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
    std::thread::sleep(Duration::from_millis(300));
    while let Ok(e) = rx.try_recv() {
        seen.push(e);
    }
    audio.stop();
    assert_eq!(finished_count(&seen, id), 0, "{seen:?}");
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, AudioEvent::Stopped(x) if *x == id))
            .count(),
        1
    );
}

#[test]
fn dropping_a_stream_finishes_it() {
    let dir = tmpdir("drop");
    let (audio, rx) = start(&file_cfg(None, Some(dir.join("o.wav")), false));
    let p = audio.player();
    let s = p.open_stream(StreamFormat {
        rate: 48000,
        width: 2,
        channels: 1,
    });
    s.push(&bytes16(&[500i16; 960])).unwrap();
    let id = s.id();
    drop(s);
    let mut seen = Vec::new();
    assert!(wait_event(
        &rx,
        &mut seen,
        Duration::from_secs(10),
        is_finished(id)
    ));
    audio.stop();
}

#[test]
fn bad_width_is_an_error() {
    let dir = tmpdir("badw");
    let (audio, _rx) = start(&file_cfg(None, Some(dir.join("o.wav")), false));
    let s = audio.player().open_stream(StreamFormat {
        rate: 16000,
        width: 3,
        channels: 1,
    });
    assert!(s.push(&[0u8; 6]).is_err());
    s.abort();
    audio.stop();
}
