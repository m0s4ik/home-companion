// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The fake Home Assistant against the stub satellite (`wyoming::stub`), well-behaved and not.

use std::time::Duration;

use wyoming::fake_ha::{self, Record, Scenario};
use wyoming::stub::{Stub, StubConfig};

fn run(cfg: StubConfig, scenario: &Scenario) -> Record {
    let stub = Stub::start(cfg).unwrap();
    let record = fake_ha::run(stub.local_addr(), scenario).unwrap();
    stub.shutdown();
    record
}

fn kinds(r: &Record) -> Vec<&str> {
    r.received
        .iter()
        .map(|(_, k, _)| k.as_str())
        .filter(|k| *k != "audio-chunk")
        .collect()
}

#[test]
fn what_time_passes() {
    let r = run(
        StubConfig::default(),
        &fake_ha::builtin("what-time").unwrap(),
    );
    assert!(r.failures.is_empty(), "{:?}", r.failures);
    let info = r.info.as_ref().unwrap();
    assert_eq!(info["satellite"]["installed"], true);
    assert_eq!(info["wake"][0]["models"][0]["name"], "okay_alfred");
    assert!(r.audio_seconds >= 1.5, "{}", r.audio_seconds);
    let d = r.detection_to_first_chunk_ms.unwrap();
    assert!(d < 200.0, "{d}");
    // the stub plays in real time: 1.5 s from audio-start, audio-stop comes right after it
    let p = r.audio_stop_to_played_ms.unwrap();
    assert!((1000.0..3500.0).contains(&p), "{p}");
    let k = kinds(&r);
    for want in [
        "info",
        "detection",
        "run-pipeline",
        "audio-start",
        "audio-stop",
        "played",
        "ping",
        "pong",
    ] {
        assert!(k.contains(&want), "{want} missing in {k:?}");
    }
    assert_eq!(
        k.iter().filter(|k| **k == "played").count(),
        2,
        "answer + announcement"
    );
    // the record serializes
    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains("\"failures\":[]"));
}

#[test]
fn tts_before_synthesize_passes() {
    let r = run(
        StubConfig::default(),
        &fake_ha::builtin("tts-before-synthesize").unwrap(),
    );
    assert!(r.failures.is_empty(), "{:?}", r.failures);
}

#[test]
fn stop_button_and_timers_and_probes_pass() {
    for name in [
        "stop-button",
        "timers",
        "announce",
        "probe-only",
        "probe-while-connected",
    ] {
        let r = run(StubConfig::default(), &fake_ha::builtin(name).unwrap());
        assert!(r.failures.is_empty(), "{name}: {:?}", r.failures);
    }
}

#[test]
fn silent_satellite_fails_the_5_s_rule() {
    // never detects, never pings, never answers pings: silent after run-satellite
    let cfg = StubConfig {
        answer_pings: false,
        ping: false,
        detect_after: Duration::from_secs(3600),
        ..Default::default()
    };
    let r = run(cfg.clone(), &fake_ha::builtin("timers").unwrap());
    assert_eq!(r.failures.len(), 1, "{:?}", r.failures);
    assert!(
        r.failures[0].contains("no event within 5 s"),
        "{:?}",
        r.failures
    );
    // the same satellite answering pings passes
    let r = run(
        StubConfig {
            answer_pings: true,
            ..cfg
        },
        &fake_ha::builtin("timers").unwrap(),
    );
    assert!(r.failures.is_empty(), "{:?}", r.failures);
}

#[test]
fn played_before_audio_stop_is_a_failure() {
    let cfg = StubConfig {
        played_early: true,
        ..Default::default()
    };
    let r = run(cfg, &fake_ha::builtin("what-time").unwrap());
    assert!(
        r.failures
            .iter()
            .any(|f| f.contains("played before audio-stop")),
        "{:?}",
        r.failures
    );
}

#[test]
fn connection_refused_is_an_error() {
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let s = Scenario {
        ping: true,
        steps: vec![fake_ha::Step::Connect],
    };
    assert!(fake_ha::run(addr, &s).is_err());
}

#[test]
fn rejected_by_the_allow_list_fails() {
    let cfg = StubConfig {
        allowed: vec![wyoming::Net::parse("192.0.2.0/24").unwrap()],
        ..Default::default()
    };
    let s = Scenario {
        ping: true,
        steps: vec![fake_ha::Step::Probe],
    };
    let stub = Stub::start(cfg).unwrap();
    let r = fake_ha::run(stub.local_addr(), &s).unwrap();
    assert!(!r.failures.is_empty());
}

#[test]
fn scenarios_round_trip_through_toml() {
    for name in fake_ha::BUILTINS {
        let s = fake_ha::builtin(name).unwrap();
        let text = toml::to_string_pretty(&s).unwrap();
        let back: Scenario = toml::from_str(&text).unwrap();
        assert_eq!(
            format!("{:?}", back.steps),
            format!("{:?}", s.steps),
            "{name}"
        );
    }
    let dir = std::env::temp_dir().join(format!("fake-ha-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("s.toml");
    std::fs::write(
        &f,
        "ping = false\n[[steps]]\nstep = \"connect\"\n[[steps]]\nstep = \"announce\"\npath = \"a.wav\"\n\
         [[steps]]\nstep = \"timer-started\"\nid = \"t\"\ntotal_seconds = 5\n[[steps]]\nstep = \"sleep\"\nms = 10\n",
    )
    .unwrap();
    let s = fake_ha::load_scenario(&f).unwrap();
    assert!(!s.ping);
    match &s.steps[1] {
        fake_ha::Step::Announce { path } => assert_eq!(path, &dir.join("a.wav")),
        o => panic!("{o:?}"),
    }
    std::fs::write(&f, "[[steps]]\nstep = \"sleep\"\nmss = 10\n").unwrap();
    assert!(fake_ha::load_scenario(&f).is_err(), "typos are errors");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fixtures_have_the_documented_shape() {
    let (f, pcm) = fake_ha::read_wav(&fake_ha::answer_wav()).unwrap();
    assert_eq!((f.rate, f.width, f.channels), (22050, 2, 1));
    assert_eq!(pcm.len(), 22050 * 2 * 3 / 2);
    let (f, pcm) = fake_ha::read_wav(&fake_ha::announce_wav()).unwrap();
    assert_eq!((f.rate, pcm.len()), (22050, 22050 * 2 * 8 / 10));
}
