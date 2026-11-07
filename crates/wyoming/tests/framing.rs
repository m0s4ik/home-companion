// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

use std::io::Cursor;

use serde_json::json;
use wyoming::event::read_event_limited;
use wyoming::info::tests_support;
use wyoming::{
    AudioFormat, Detection, Error, Event, Info, Message, RunPipeline, Stage, Synthesize,
    TimerStarted, TimerUpdated, Transcript, Voice, read_event, write_event,
};

fn all_messages() -> Vec<Message> {
    let f = AudioFormat {
        rate: 22050,
        width: 2,
        channels: 1,
    };
    vec![
        Message::Describe,
        Message::Info(Info::for_satellite(tests_support::sample())),
        Message::Ping {
            text: Some("x".into()),
        },
        Message::Ping { text: None },
        Message::Pong {
            text: Some("x".into()),
        },
        Message::Pong { text: None },
        Message::RunSatellite,
        Message::PauseSatellite,
        Message::RunPipeline(RunPipeline::asr_to_tts()),
        Message::RunPipeline(RunPipeline {
            start_stage: Stage::Wake,
            end_stage: Stage::Handle,
            restart_on_end: true,
            wake_word_name: Some("okay_alfred".into()),
        }),
        Message::RunPipeline(RunPipeline {
            start_stage: Stage::Intent,
            end_stage: Stage::Intent,
            restart_on_end: false,
            wake_word_name: None,
        }),
        Message::AudioStart {
            format: AudioFormat::MIC,
            timestamp: Some(0),
        },
        Message::AudioStart {
            format: f,
            timestamp: None,
        },
        Message::AudioChunk {
            format: AudioFormat::MIC,
            timestamp: Some(20),
            audio: vec![1; 640],
        },
        Message::AudioChunk {
            format: f,
            timestamp: None,
            audio: vec![],
        },
        Message::AudioChunk {
            format: f,
            timestamp: Some(1),
            audio: (0..65536).map(|i| i as u8).collect(),
        },
        Message::AudioStop {
            timestamp: Some(40),
        },
        Message::AudioStop { timestamp: None },
        Message::Detect { names: None },
        Message::Detect {
            names: Some(vec!["okay_alfred".into()]),
        },
        Message::Detection(Detection {
            name: Some("okay_alfred".into()),
            timestamp: Some(5),
            speaker: None,
        }),
        Message::Detection(Detection {
            name: None,
            timestamp: None,
            speaker: Some("a".into()),
        }),
        Message::NotDetected,
        Message::Transcribe {
            language: Some("it".into()),
        },
        Message::Transcribe { language: None },
        Message::Transcript(Transcript {
            text: "Che ore sono? È l'una \"quotes\"\n".into(),
            language: Some("it".into()),
        }),
        Message::Transcript(Transcript {
            text: String::new(),
            language: None,
        }),
        Message::Synthesize(Synthesize {
            text: "Ciao".into(),
            voice: None,
        }),
        Message::Synthesize(Synthesize {
            text: "Ciao".into(),
            voice: Some(Voice {
                name: Some("v".into()),
                language: None,
                speaker: Some("s".into()),
            }),
        }),
        Message::VoiceStarted {
            timestamp: Some(300),
        },
        Message::VoiceStopped { timestamp: None },
        Message::Error {
            text: "No text recognized".into(),
            code: Some("stt-no-text-recognized".into()),
        },
        Message::Error {
            text: "x".into(),
            code: None,
        },
        Message::Played,
        Message::TimerStarted(TimerStarted {
            id: "t1".into(),
            total_seconds: 90,
            name: Some("Pasta".into()),
            start_hours: Some(0),
            start_minutes: Some(1),
            start_seconds: Some(30),
        }),
        Message::TimerStarted(TimerStarted {
            id: "t2".into(),
            total_seconds: 5,
            name: None,
            start_hours: None,
            start_minutes: None,
            start_seconds: None,
        }),
        Message::TimerUpdated(TimerUpdated {
            id: "t1".into(),
            is_active: false,
            total_seconds: 88,
        }),
        Message::TimerCancelled { id: "t1".into() },
        Message::TimerFinished { id: "t1".into() },
        Message::StreamingStarted,
        Message::StreamingStopped,
        Message::SatelliteConnected,
        Message::SatelliteDisconnected,
        Message::Unknown(Event::with_data(
            "transcript-chunk",
            json!({"text": "che"}).as_object().unwrap().clone(),
        )),
        Message::Unknown(Event {
            kind: "user-event".into(),
            data: Default::default(),
            payload: Some(vec![9, 9]),
        }),
    ]
}

#[test]
fn round_trip_every_variant() {
    let msgs = all_messages();
    let mut buf = Vec::new();
    for m in &msgs {
        write_event(&mut buf, &m.to_event()).unwrap();
    }
    let mut r = Cursor::new(buf);
    for m in &msgs {
        let e = read_event(&mut r).unwrap().expect("event");
        assert_eq!(e.kind, m.event_type());
        assert_eq!(&Message::from_event(e), m);
    }
    assert!(
        read_event(&mut r).unwrap().is_none(),
        "EOF at a frame boundary"
    );
}

fn parse(bytes: &[u8]) -> Result<Option<Event>, Error> {
    read_event(&mut Cursor::new(bytes.to_vec()))
}

#[test]
fn data_forms() {
    // inline data (what older writers and hand-written clients do)
    let e = parse(b"{\"type\": \"ping\", \"data\": {\"text\": \"a\"}}\n")
        .unwrap()
        .unwrap();
    assert_eq!(
        Message::from_event(e),
        Message::Ping {
            text: Some("a".into())
        }
    );
    // data as block
    let e = parse(b"{\"type\": \"ping\", \"data_length\": 13}\n{\"text\": \"b\"}")
        .unwrap()
        .unwrap();
    assert_eq!(
        Message::from_event(e),
        Message::Ping {
            text: Some("b".into())
        }
    );
    // data: null and missing data
    for h in [
        &b"{\"type\": \"played\", \"data\": null}\n"[..],
        b"{\"type\": \"played\"}\n",
        b"{\"type\":\"played\",\"version\":\"1.0.0\"}\n",
    ] {
        let e = parse(h).unwrap().unwrap();
        assert!(e.data.is_empty());
        assert_eq!(Message::from_event(e), Message::Played);
    }
    // merge: block wins per key, inline-only keys stay
    let e = parse(b"{\"type\": \"transcript\", \"data\": {\"text\": \"old\", \"language\": \"en\"}, \"data_length\": 15}\n{\"text\": \"new\"}")
        .unwrap()
        .unwrap();
    assert_eq!(
        Message::from_event(e),
        Message::Transcript(Transcript {
            text: "new".into(),
            language: Some("en".into())
        })
    );
    // zero lengths
    let e = parse(b"{\"type\": \"audio-stop\", \"data_length\": 0, \"payload_length\": 0}\n")
        .unwrap()
        .unwrap();
    assert_eq!(e.payload, None);
}

#[test]
fn payload_sizes() {
    for n in [0usize, 1, 65536] {
        let m = Message::AudioChunk {
            format: AudioFormat::MIC,
            timestamp: Some(0),
            audio: vec![7; n],
        };
        let mut buf = Vec::new();
        write_event(&mut buf, &m.to_event()).unwrap();
        let header = buf.split(|b| *b == b'\n').next().unwrap();
        let header = std::str::from_utf8(header).unwrap();
        assert_eq!(header.contains("payload_length"), n > 0, "{header}");
        let back = Message::from_event(parse(&buf).unwrap().unwrap());
        assert_eq!(back, m);
    }
}

#[test]
fn limits() {
    let big = 17 * 1024 * 1024;
    for h in [
        format!("{{\"type\": \"x\", \"data_length\": {big}}}\n"),
        format!("{{\"type\": \"x\", \"payload_length\": {big}}}\n"),
        "{\"type\": \"x\", \"payload_length\": -1}\n".into(),
        "{\"type\": \"x\", \"data_length\": -5}\n".into(),
        "{\"type\": \"x\", \"data_length\": 1.5}\n".into(),
        "{\"type\": \"x\", \"data_length\": \"4\"}\n".into(),
    ] {
        assert!(matches!(parse(h.as_bytes()), Err(Error::Frame(_))), "{h}");
    }
    // a custom limit
    let h = b"{\"type\": \"x\", \"payload_length\": 11}\n01234567890";
    assert!(matches!(
        read_event_limited(&mut Cursor::new(&h[..]), 10),
        Err(Error::Frame(_))
    ));
    assert!(
        read_event_limited(&mut Cursor::new(&h[..]), 11)
            .unwrap()
            .is_some()
    );
    // header line over 1 MiB
    let mut long = b"{\"type\": \"x\", \"pad\": \"".to_vec();
    long.extend(std::iter::repeat_n(b'a', 1024 * 1024 + 10));
    long.extend_from_slice(b"\"}\n");
    assert!(matches!(parse(&long), Err(Error::Frame(_))));
}

#[test]
fn truncated_and_malformed() {
    for bad in [
        &b"{\"type\": \"ping\""[..],                             // no newline
        b"{\"type\": \"ping\", \"data_length\": 13}\n{\"text\"", // short data
        b"{\"type\": \"x\", \"payload_length\": 4}\n12",         // short payload
        b"{\"version\": \"1.11.0\"}\n",                          // no type
        b"{\"type\": 3}\n",                                      // type not a string
        b"not json\n",
        b"[1, 2]\n",
        b"\n",
        b"{\"type\": \"x\", \"data\": [1]}\n",
        b"{\"type\": \"x\", \"data_length\": 3}\n[1]",
    ] {
        assert!(
            matches!(parse(bad), Err(Error::Frame(_))),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    assert!(parse(b"").unwrap().is_none());
}

#[test]
fn malformed_known_types_become_unknown() {
    for (kind, data) in [
        ("transcript", json!({})),
        ("timer-started", json!({"total_seconds": 3})),
        ("timer-updated", json!({"id": "a", "total_seconds": 3})),
        ("audio-chunk", json!({"rate": 16000})),
        (
            "run-pipeline",
            json!({"start_stage": "nope", "end_stage": "tts"}),
        ),
        ("error", json!({"code": "x"})),
        ("synthesize", json!({"voice": {}})),
    ] {
        let e = Event::with_data(kind, data.as_object().unwrap().clone());
        assert!(
            matches!(Message::from_event(e), Message::Unknown(_)),
            "{kind}"
        );
    }
    // tolerant numbers and extra keys
    let e = Event::with_data(
        "timer-started",
        json!({"id": "t", "total_seconds": 90.0, "start_minutes": 1, "extra": [1]})
            .as_object()
            .unwrap()
            .clone(),
    );
    let Message::TimerStarted(t) = Message::from_event(e) else {
        panic!()
    };
    assert_eq!((t.total_seconds, t.start_minutes), (90, Some(1)));
    // synthesize with null voice keys (as Home Assistant may send) still has a voice
    let e = Event::with_data(
        "synthesize",
        json!({"text": "a", "voice": {"name": null, "language": "it"}})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(
        Message::from_event(e),
        Message::Synthesize(Synthesize {
            text: "a".into(),
            voice: Some(Voice {
                name: None,
                language: Some("it".into()),
                speaker: None
            })
        })
    );
}

#[test]
fn is_audio_and_kind() {
    assert!(
        Message::AudioChunk {
            format: AudioFormat::MIC,
            timestamp: None,
            audio: vec![]
        }
        .is_audio()
    );
    assert!(!Message::Played.is_audio());
    let u = Message::Unknown(Event::new("handled"));
    assert_eq!((u.kind(), u.event_type()), ("unknown", "handled"));
}
