// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `fixtures/python-frames.bin` was written by the real wyoming 1.11.0 package
//! (`tools/make-python-frames.py`). We parse it, and our writer must produce the very same header
//! lines (keys, order, separators, lengths) for the same messages.

use std::io::Cursor;

use wyoming::info::tests_support::sample;
use wyoming::{
    AudioFormat, Detection, Info, Message, RunPipeline, Synthesize, TimerStarted, TimerUpdated,
    Transcript, Voice, read_event, write_event,
};

const FRAMES: &[u8] = include_bytes!("../fixtures/python-frames.bin");

fn expected() -> Vec<Message> {
    let f22 = AudioFormat {
        rate: 22050,
        width: 2,
        channels: 1,
    };
    vec![
        Message::Describe,
        Message::Info(Info::for_satellite(sample())),
        Message::Ping {
            text: Some("x".into()),
        },
        Message::Pong { text: None },
        Message::RunSatellite,
        Message::Detection(Detection {
            name: Some("okay_alfred".into()),
            timestamp: Some(1234),
            speaker: None,
        }),
        Message::RunPipeline(RunPipeline::asr_to_tts()),
        Message::AudioStart {
            format: AudioFormat::MIC,
            timestamp: Some(0),
        },
        Message::AudioChunk {
            format: AudioFormat::MIC,
            timestamp: Some(20),
            audio: (0..320).map(|i| (i % 256) as u8).collect(),
        },
        Message::AudioStop {
            timestamp: Some(40),
        },
        Message::Transcribe {
            language: Some("en".into()),
        },
        Message::VoiceStarted {
            timestamp: Some(300),
        },
        Message::VoiceStopped {
            timestamp: Some(1500),
        },
        Message::Transcript(Transcript {
            text: "Che ore sono? È l'una".into(),
            language: None,
        }),
        Message::Synthesize(Synthesize {
            text: "It is ten past two.".into(),
            voice: Some(Voice {
                name: Some("en_US-lessac-medium".into()),
                language: Some("en".into()),
                speaker: None,
            }),
        }),
        Message::AudioStart {
            format: f22,
            timestamp: Some(0),
        },
        Message::AudioChunk {
            format: f22,
            timestamp: Some(0),
            audio: vec![0; 2048],
        },
        Message::AudioStop {
            timestamp: Some(46),
        },
        Message::Played,
        Message::Error {
            text: "No text recognized".into(),
            code: Some("stt-no-text-recognized".into()),
        },
        Message::TimerStarted(TimerStarted {
            id: "t1".into(),
            total_seconds: 90,
            name: Some("Pasta".into()),
            start_hours: None,
            start_minutes: Some(1),
            start_seconds: Some(30),
        }),
        Message::TimerUpdated(TimerUpdated {
            id: "t1".into(),
            is_active: true,
            total_seconds: 88,
        }),
        Message::TimerCancelled { id: "t1".into() },
        Message::TimerFinished { id: "t1".into() },
        Message::PauseSatellite,
    ]
}

/// Splits the file into raw frames using the header lengths.
fn raw_frames(mut b: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let nl = b.iter().position(|c| *c == b'\n').unwrap();
        let header: serde_json::Value = serde_json::from_slice(&b[..nl]).unwrap();
        let n = header
            .get("data_length")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + header
                .get("payload_length")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        let end = nl + 1 + n as usize;
        out.push((&b[..nl + 1], &b[nl + 1..end]));
        b = &b[end..];
    }
    out
}

#[test]
fn parse_python_frames() {
    let mut r = Cursor::new(FRAMES);
    let exp = expected();
    for m in &exp {
        let e = read_event(&mut r).unwrap().unwrap();
        assert_eq!(&Message::from_event(e), m);
    }
    // the last frame is a type we do not handle: kept verbatim
    let e = read_event(&mut r).unwrap().unwrap();
    let Message::Unknown(u) = Message::from_event(e) else {
        panic!("expected Unknown")
    };
    assert_eq!(
        (u.kind.as_str(), u.data["text"].as_str()),
        ("transcript-chunk", Some("che"))
    );
    assert!(read_event(&mut r).unwrap().is_none());
}

#[test]
fn our_headers_are_byte_identical() {
    let frames = raw_frames(FRAMES);
    for (m, (py_header, py_rest)) in expected().iter().zip(&frames) {
        let mut ours = Vec::new();
        write_event(&mut ours, &m.to_event()).unwrap();
        let nl = ours.iter().position(|c| *c == b'\n').unwrap();
        assert_eq!(
            String::from_utf8_lossy(&ours[..nl + 1]),
            String::from_utf8_lossy(py_header),
            "header of {}",
            m.kind()
        );
        // data blocks: same JSON (key order may differ), same payload bytes
        assert_eq!(ours.len() - nl - 1, py_rest.len());
        let back = read_event(&mut Cursor::new(&ours)).unwrap().unwrap();
        let py = read_event(&mut Cursor::new([*py_header, *py_rest].concat()))
            .unwrap()
            .unwrap();
        assert_eq!(back, py, "{}", m.kind());
    }
}
