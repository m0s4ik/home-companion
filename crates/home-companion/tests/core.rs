// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The core in-process against the fake Home Assistant: null audio (no device, no sound), the
//! real Wyoming server on a free port, the Talk and Stop buttons pressed through the core's
//! channel when the screen shows the right state. The wake word itself is covered by the
//! end-to-end test (tools/run-e2e.sh).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use home_companion::core::{
    Core, CoreConfig, CoreEvent, Deps, MicControl, Phase, Summary, UiCommand, UiEvent,
};
use home_companion::timers::Timers;
use wyoming::fake_ha::{self, Record, Scenario, Step};

/// When to press a button: on the first `View(phase)` after `after` such views, then wait.
#[derive(Clone, Copy)]
struct Press {
    on: Phase,
    wait: Duration,
    event: fn() -> UiEvent,
}

fn talk() -> UiEvent {
    UiEvent::Talk
}
fn stop() -> UiEvent {
    UiEvent::Stop
}

struct Run {
    record: Record,
    summary: Summary,
    views: Vec<Phase>,
}

fn run(scenario: Scenario, presses: &[Press]) -> Run {
    run_with(scenario, presses, None)
}

/// `reflex`: a fake helper that answers this text at the end of every utterance.
fn run_with(scenario: Scenario, presses: &[Press], reflex: Option<&'static str>) -> Run {
    let model = wakeword::Model::load(
        &character::repo_characters().join("alfred/wakeword/okay_alfred.json"),
    )
    .unwrap();
    let cfg = home_companion::config::Config::default();
    let info = home_companion::satellite_info(&cfg, &model);

    let (core_tx, core_rx) = mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = mpsc::channel();
    let mic = Arc::new(MicControl::default());
    let tx = core_tx.clone();
    let acfg = audio::AudioConfig {
        backend: audio::Backend::Null,
        realtime: false,
        ..Default::default()
    };
    let audio = audio::Audio::start(
        &acfg,
        Box::new(move |e| {
            let _ = tx.send(CoreEvent::Audio(e));
        }),
    )
    .unwrap();
    let player = audio.player();
    let frames = audio.subscribe(16);
    let wake = home_companion::wake_thread::spawn(
        frames,
        None,
        Vec::new(),
        mic.clone(),
        core_tx.clone(),
        Arc::new(Mutex::new(None)),
    )
    .unwrap();
    let tx = core_tx.clone();
    let server = wyoming::Server::start(
        wyoming::ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            allowed: vec![wyoming::Net::parse("127.0.0.1").unwrap()],
            info,
            ping_interval: Some(Duration::from_secs(2)),
            ..Default::default()
        },
        Box::new(move |e| {
            let _ = tx.send(CoreEvent::Server(e));
        }),
    )
    .unwrap();
    let addr = server.local_addr();
    let reflexes = reflex.map(|text| {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        home_companion::reflex::Reflexes::new(
            Box::new(AnsweringReflex {
                tx: Mutex::new(core_tx.clone()),
                text,
            }),
            &home_companion::config::ReflexesCfg::default(),
            repo.join("characters/alfred/grammar"),
            repo.join("locales"),
            "it",
        )
    });
    let deps = Deps {
        sender: server.sender(),
        player,
        recorder: None,
        ui: ui_tx,
        mic,
        audio: Some(audio),
        server: Some(server),
        timers: Timers::in_memory(),
        alarms: home_companion::alarms::Alarms::in_memory(),
        settings: None,
        volume: None,
        wake_stats: Arc::new(Mutex::new(None)),
        stop_flag: None,
        reflexes,
        music: None,
    };
    let core_cfg = CoreConfig {
        bell_repeats: 2,
        bell_gap: Duration::from_millis(200),
        ..Default::default()
    };
    let core = std::thread::spawn(move || Core::new(core_cfg, deps).run(core_rx));
    let ha = std::thread::spawn(move || fake_ha::run(addr, &scenario).unwrap());
    let views = drive(&ui_rx, &core_tx, presses, &ha);
    let record = ha.join().unwrap();
    // Let the last events settle (a bell ending, a phase change).
    std::thread::sleep(Duration::from_millis(300));
    core_tx.send(CoreEvent::Quit).unwrap();
    let summary = core.join().unwrap();
    wake.join().unwrap();
    let mut views = views;
    views.extend(ui_rx.try_iter().filter_map(|c| {
        if let UiCommand::View(p) = c {
            Some(p)
        } else {
            None
        }
    }));
    Run {
        record,
        summary,
        views,
    }
}

/// Watches the screen commands and presses the buttons; returns the views seen.
fn drive(
    ui: &Receiver<UiCommand>,
    core: &Sender<CoreEvent>,
    presses: &[Press],
    ha: &std::thread::JoinHandle<Record>,
) -> Vec<Phase> {
    let mut views = Vec::new();
    let mut next = 0;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !ha.is_finished() {
        if let Ok(UiCommand::View(p)) = ui.recv_timeout(Duration::from_millis(50)) {
            views.push(p);
            if next < presses.len() && presses[next].on == p {
                let pr = presses[next];
                std::thread::sleep(pr.wait);
                core.send(CoreEvent::Ui((pr.event)())).unwrap();
                next += 1;
            }
        }
    }
    assert_eq!(
        next,
        presses.len(),
        "not every button was pressed; views {views:?}"
    );
    views
}

fn outcomes(s: &Summary) -> Vec<(String, String)> {
    s.conversations
        .iter()
        .map(|c| (c.trigger.to_string(), c.outcome.clone()))
        .collect()
}

const TALK_AT_HOME: Press = Press {
    on: Phase::Home,
    wait: Duration::from_millis(100),
    event: talk,
};

#[test]
fn talk_button_conversation_timers_announcement_pause() {
    let r = run(fake_ha::builtin("what-time").unwrap(), &[TALK_AT_HOME]);
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert!(r.record.audio_seconds >= 1.5);
    assert_eq!(
        outcomes(&r.summary),
        vec![
            ("talk".into(), "played".into()),
            ("announcement".into(), "played".into())
        ]
    );
    let c = &r.summary.conversations[0];
    assert_eq!(c.heard, "what time is it");
    assert_eq!(c.answer, "It is ten past two.");
    assert!(c.detect_to_run_pipeline_ms.unwrap() < 50.0);
    assert!((c.answer_seconds - 1.5).abs() < 0.01);
    for p in [
        Phase::Listening,
        Phase::Thinking,
        Phase::Speaking,
        Phase::Home,
    ] {
        assert!(
            r.summary.phases.contains(&p),
            "{p:?} in {:?}",
            r.summary.phases
        );
    }
    assert_eq!(
        *r.summary.phases.last().unwrap(),
        Phase::Offline,
        "pause-satellite then disconnect: offline"
    );
    assert!(r.summary.rung.is_empty(), "the cancelled timer never rings");
    assert_eq!(r.summary.detections_sent, 1);
}

#[test]
fn answer_audio_before_synthesize() {
    let r = run(
        fake_ha::builtin("tts-before-synthesize").unwrap(),
        &[TALK_AT_HOME],
    );
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert_eq!(r.summary.conversations[0].outcome, "played");
    assert_eq!(r.summary.conversations[0].answer, "It is ten past two.");
}

#[test]
fn stop_before_the_transcript_ends_the_stream_and_swallows_the_error() {
    let presses = [
        TALK_AT_HOME,
        Press {
            on: Phase::Listening,
            wait: Duration::from_millis(800),
            event: stop,
        },
    ];
    let r = run(fake_ha::builtin("stop-button").unwrap(), &presses);
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert_eq!(
        outcomes(&r.summary),
        vec![("talk".into(), "stopped".into())]
    );
    assert!(
        !r.summary.phases.contains(&Phase::Error),
        "a stopped conversation shows no error: {:?}",
        r.summary.phases
    );
}

#[test]
fn stop_while_speaking_sends_played_at_once() {
    let mut steps = fake_ha::builtin("what-time").unwrap().steps;
    // Up to the answer and its `played`, then disconnect.
    let cut = steps
        .iter()
        .position(|s| matches!(s, Step::ExpectPlayed { .. }))
        .unwrap();
    steps.truncate(cut + 1);
    steps.retain(|s| !matches!(s, Step::Probe));
    steps.push(Step::Disconnect);
    let presses = [
        TALK_AT_HOME,
        Press {
            on: Phase::Speaking,
            wait: Duration::from_millis(300),
            event: stop,
        },
    ];
    let r = run(Scenario { ping: true, steps }, &presses);
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert_eq!(
        outcomes(&r.summary),
        vec![("talk".into(), "stopped".into())]
    );
    let ms = r.record.audio_stop_to_played_ms.unwrap();
    assert!(
        ms < 1000.0,
        "played {ms} ms after audio-stop: the 1.5 s answer was cut"
    );
}

#[test]
fn timer_rings_once_even_with_a_late_finish() {
    let r = run(fake_ha::builtin("timers").unwrap(), &[]);
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert_eq!(r.summary.rung, vec!["t2".to_string()]);
    assert!(r.summary.phases.contains(&Phase::Ringing));
    assert!(r.views.contains(&Phase::Ringing));
}

#[test]
fn probes_while_connected_keep_the_satellite_online() {
    // Home Assistant's describe-only connections while the satellite link is up must not
    // replace it: no disconnect, no offline phase until pause-satellite.
    let r = run(fake_ha::builtin("probe-while-connected").unwrap(), &[]);
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    let home = r
        .summary
        .phases
        .iter()
        .position(|p| *p == Phase::Home)
        .expect("home");
    let offline: Vec<_> = r.summary.phases[home..]
        .iter()
        .filter(|p| **p == Phase::Offline)
        .collect();
    // only the final pause-satellite
    assert_eq!(offline.len(), 1, "phases {:?}", r.summary.phases);
    assert_eq!(r.summary.phases.last(), Some(&Phase::Offline));
}

#[test]
fn timer_rings_after_home_assistant_left() {
    let steps = vec![
        Step::Connect,
        Step::RunSatellite,
        Step::TimerStarted(wyoming::TimerStarted {
            id: "away".into(),
            total_seconds: 2,
            name: Some("Tea".into()),
            start_hours: None,
            start_minutes: None,
            start_seconds: Some(2),
        }),
        Step::Sleep { ms: 200 },
        Step::Disconnect,
        // The fake Home Assistant is gone from the satellite's point of view; this only keeps
        // the test running until the timer is due.
        Step::Sleep { ms: 3500 },
    ];
    let r = run(Scenario { ping: false, steps }, &[]);
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert_eq!(r.summary.rung, vec!["away".to_string()]);
}

#[test]
fn talk_with_no_home_assistant_shows_offline_and_starts_nothing() {
    // Connected but never ready (no run-satellite): Talk plays the done cue, no detection.
    let steps = vec![Step::Connect, Step::Sleep { ms: 1500 }, Step::Disconnect];
    let presses = [Press {
        on: Phase::Offline,
        wait: Duration::from_millis(300),
        event: talk,
    }];
    let r = run(Scenario { ping: false, steps }, &presses);
    assert_eq!(r.summary.detections_sent, 0);
    assert!(r.summary.conversations.is_empty());
}

#[test]
fn volume_keys_step_show_the_bar_and_save_at_exit() {
    use home_companion::volume::{Curve, Output, Volume, VolumeRequest};
    let (core_tx, core_rx) = mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = mpsc::channel();
    let tx = core_tx.clone();
    let acfg = audio::AudioConfig {
        backend: audio::Backend::Null,
        realtime: false,
        ..Default::default()
    };
    let audio = audio::Audio::start(
        &acfg,
        Box::new(move |e| {
            let _ = tx.send(CoreEvent::Audio(e));
        }),
    )
    .unwrap();
    let server = wyoming::Server::start(
        wyoming::ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        },
        Box::new(|_| {}),
    )
    .unwrap();
    let dir = std::env::temp_dir().join(format!("home-companion-volume-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let settings = home_companion::settings::Settings::in_dir(&dir);
    let curve = Curve {
        min_amp: 47,
        max_amp: 85,
        step: 5,
    };
    // The amplifier at 61 (40 %), nothing saved yet.
    let volume = Volume::with_output(curve, Output::Memory(61), settings.volume());
    let deps = Deps {
        sender: server.sender(),
        player: audio.player(),
        recorder: None,
        ui: ui_tx,
        mic: Arc::new(MicControl::default()),
        audio: Some(audio),
        server: Some(server),
        timers: Timers::in_memory(),
        alarms: home_companion::alarms::Alarms::in_memory(),
        settings: Some(settings.clone()),
        volume: Some(volume),
        wake_stats: Arc::new(Mutex::new(None)),
        stop_flag: None,
        reflexes: None,
        music: None,
    };
    let core = std::thread::spawn(move || Core::new(CoreConfig::default(), deps).run(core_rx));
    let step = |dir, repeat| CoreEvent::Volume(VolumeRequest::Step { dir, repeat });
    for e in [
        step(1, false),
        step(1, true),
        step(-1, false),
        step(-1, false),
        step(-1, true),
    ] {
        core_tx.send(e).unwrap();
    }
    std::thread::sleep(Duration::from_millis(200));
    core_tx.send(CoreEvent::Quit).unwrap();
    core.join().unwrap();
    let shown: Vec<u8> = ui_rx
        .try_iter()
        .filter_map(|c| match c {
            UiCommand::Volume(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![45, 50, 45, 40, 35]);
    assert_eq!(settings.volume(), Some(35));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A helper that is always ready and records what the body sends.
struct FakeReflex(Arc<Mutex<Vec<reflex::proto::Request>>>);

impl home_companion::reflex::ReflexLink for FakeReflex {
    fn ready(&self) -> bool {
        true
    }
    fn send(&self, r: reflex::proto::Request) -> bool {
        self.0.lock().unwrap().push(r);
        true
    }
    fn set_language(&self, _: &str) {}
}

fn frame(index: u64, amp: f32) -> CoreEvent {
    let samples: Vec<i16> = (0..320)
        .map(|i| (((index * 320 + i) as f32 * 0.2).sin() * amp * 32767.0) as i16)
        .collect();
    let level = audio::Level::of(&samples);
    CoreEvent::MicFrame(audio::CaptureFrame {
        samples: samples.into(),
        index,
        level,
    })
}

/// Runs the core with no Home Assistant and the fake helper: Talk, a spoken second (a tone),
/// silence, then the helper's `result`. Returns the UI commands and what the helper got.
fn reflex_run(text: &str, conf: f32) -> (Vec<UiCommand>, Vec<reflex::proto::Request>, Summary) {
    reflex_run_then(text, conf, |_| {})
}

/// [`reflex_run`], then `then` sends more events before the core quits.
fn reflex_run_then(
    text: &str,
    conf: f32,
    then: impl FnOnce(&Sender<CoreEvent>),
) -> (Vec<UiCommand>, Vec<reflex::proto::Request>, Summary) {
    let (core_tx, core_rx) = mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = mpsc::channel();
    let tx = core_tx.clone();
    let acfg = audio::AudioConfig {
        backend: audio::Backend::Null,
        realtime: false,
        ..Default::default()
    };
    let audio = audio::Audio::start(
        &acfg,
        Box::new(move |e| {
            let _ = tx.send(CoreEvent::Audio(e));
        }),
    )
    .unwrap();
    let server = wyoming::Server::start(
        wyoming::ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        },
        Box::new(|_| {}),
    )
    .unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let reflexes = home_companion::reflex::Reflexes::new(
        Box::new(FakeReflex(sent.clone())),
        &home_companion::config::ReflexesCfg::default(),
        repo.join("characters/alfred/grammar"),
        repo.join("locales"),
        "it",
    );
    let deps = Deps {
        sender: server.sender(),
        player: audio.player(),
        recorder: None,
        ui: ui_tx,
        mic: Arc::new(MicControl::default()),
        audio: Some(audio),
        server: Some(server),
        timers: Timers::in_memory(),
        alarms: home_companion::alarms::Alarms::in_memory(),
        settings: None,
        volume: None,
        wake_stats: Arc::new(Mutex::new(None)),
        stop_flag: None,
        reflexes: Some(reflexes),
        music: None,
    };
    let core = std::thread::spawn(move || Core::new(CoreConfig::default(), deps).run(core_rx));
    core_tx.send(CoreEvent::Ui(UiEvent::Talk)).unwrap();
    // 0.3 s quiet, 1 s of "speech", 1 s quiet (the detector ends the utterance after 0.7 s).
    for i in 0..115u64 {
        let amp = if (15..65).contains(&i) { 0.05 } else { 0.0005 };
        core_tx.send(frame(i, amp)).unwrap();
    }
    std::thread::sleep(Duration::from_millis(200));
    let id = sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|r| match r {
            reflex::proto::Request::Start(id) => Some(*id),
            _ => None,
        })
        .expect("held back for the reflexes");
    let words: Vec<reflex::Word> = text
        .split(' ')
        .enumerate()
        .map(|(i, w)| reflex::Word {
            word: w.into(),
            conf,
            start: 0.3 + 0.2 * i as f32,
            end: 0.5 + 0.2 * i as f32,
        })
        .map(|mut w| {
            w.end = w.end.min(1.3);
            w
        })
        .collect();
    let mut words = words;
    if let Some(l) = words.last_mut() {
        l.end = 1.3;
    }
    core_tx
        .send(CoreEvent::Reflex(
            home_companion::reflex::ReflexEvent::Result {
                id,
                endpoint: false,
                text: text.into(),
                words,
                ms: 5.0,
            },
        ))
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    then(&core_tx);
    core_tx.send(CoreEvent::Quit).unwrap();
    let summary = core.join().unwrap();
    let ui: Vec<UiCommand> = ui_rx.try_iter().collect();
    let sent = sent.lock().unwrap().clone();
    (ui, sent, summary)
}

#[test]
fn a_reflex_sets_a_timer_without_home_assistant() {
    let (ui, sent, summary) = reflex_run("metti un timer di un minuto", 1.0);
    assert!(
        sent.iter()
            .any(|r| matches!(r, reflex::proto::Request::End(_)))
    );
    let timers: Vec<_> = ui
        .iter()
        .filter_map(|c| match c {
            UiCommand::Timers(rows) if !rows.is_empty() => Some(rows.clone()),
            _ => None,
        })
        .collect();
    assert!(!timers.is_empty(), "no timer shown: {ui:?}");
    assert!(
        ui.contains(&UiCommand::Answer("Timer di 01:00 avviato.".into())),
        "{ui:?}"
    );
    assert!(ui.contains(&UiCommand::View(Phase::Speaking)));
    // Nothing went to Home Assistant.
    assert_eq!(summary.detections_sent, 0);
}

#[test]
fn not_a_reflex_without_home_assistant_ends_offline() {
    // Unsure words: not a reflex; with no Home Assistant the conversation ends.
    let (ui, _, summary) = reflex_run("che ore sono", 0.5);
    assert!(
        !ui.iter()
            .any(|c| matches!(c, UiCommand::Answer(a) if a.starts_with("Sono le")))
    );
    assert_eq!(summary.detections_sent, 0);
    assert_eq!(
        summary.conversations.last().map(|r| r.outcome.as_str()),
        Some("no_home_assistant")
    );
}

/// A helper that answers `text` (every word sure, covering 0-6 s) to each `E`.
struct AnsweringReflex {
    tx: Mutex<Sender<CoreEvent>>,
    text: &'static str,
}

impl home_companion::reflex::ReflexLink for AnsweringReflex {
    fn ready(&self) -> bool {
        true
    }
    fn send(&self, r: reflex::proto::Request) -> bool {
        if let reflex::proto::Request::End(id) = r {
            let _ = self.tx.lock().unwrap().send(CoreEvent::Reflex(
                home_companion::reflex::ReflexEvent::Result {
                    id,
                    endpoint: false,
                    text: self.text.into(),
                    words: vec![reflex::Word {
                        word: self.text.into(),
                        conf: 1.0,
                        start: 0.0,
                        end: 6.0,
                    }],
                    ms: 3.0,
                },
            ));
        }
        true
    }
    fn set_language(&self, _: &str) {}
}

#[test]
fn not_a_reflex_goes_to_home_assistant_in_one_burst() {
    // The null microphone is silent: the detector gives up after 5 s ("nobody spoke"), the fake
    // helper says "[unk]": not a reflex, so Home Assistant gets the pipeline, the 5 s held back
    // in one burst, and audio-stop.
    let steps = vec![
        Step::Connect,
        Step::RunSatellite,
        Step::Sleep { ms: 300 },
        Step::WaitDetection { timeout_ms: 9000 },
        Step::CollectAudio {
            min_ms: 4500,
            max_ms: 9000,
        },
        Step::SendTranscript {
            text: "che tempo fa".into(),
        },
        Step::Sleep { ms: 300 },
        Step::Disconnect,
    ];
    let presses = [Press {
        on: Phase::Home,
        wait: Duration::from_millis(200),
        event: talk,
    }];
    let r = run_with(Scenario { ping: false, steps }, &presses, Some("[unk]"));
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert_eq!(r.summary.detections_sent, 1);
    assert!(
        r.record.audio_seconds >= 4.5,
        "{} s",
        r.record.audio_seconds
    );
    assert!(
        r.record.received.iter().any(|(_, t, _)| t == "audio-stop"),
        "{:?}",
        r.record.received.iter().map(|x| &x.1).collect::<Vec<_>>()
    );
    assert!(r.views.contains(&Phase::Thinking), "{:?}", r.views);
}

#[test]
fn good_night_turns_the_screen_off_and_does_not_disturb_until_the_morning() {
    let (ui, _, summary) = reflex_run("buonanotte", 1.0);
    let dnd = ui.iter().find_map(|c| match c {
        UiCommand::DoNotDisturb(u) => Some(*u),
        _ => None,
    });
    let until = dnd.flatten().expect("do not disturb on") as f64;
    let now = home_companion::timers::now_unix();
    assert!(until > now && until <= now + 86_400.0, "{until}");
    assert_eq!(home_companion::alarms::Now::at(until as i64).hour, 7);
    // The screen off comes last, after the view went back home.
    let off = ui
        .iter()
        .rposition(|c| *c == UiCommand::ScreenOff)
        .expect("screen off");
    let home = ui
        .iter()
        .rposition(|c| matches!(c, UiCommand::View(_)))
        .unwrap();
    assert!(off > home, "{ui:?}");
    assert!(
        !ui.iter()
            .any(|c| matches!(c, UiCommand::Answer(a) if !a.is_empty()))
    );
    assert_eq!(summary.detections_sent, 0);
}

#[test]
fn screen_off_by_voice() {
    let (ui, _, _) = reflex_run("spegni lo schermo", 1.0);
    assert!(ui.contains(&UiCommand::ScreenOff), "{ui:?}");
    assert!(!ui.iter().any(|c| matches!(c, UiCommand::DoNotDisturb(_))));
}

#[test]
fn do_not_disturb_for_two_hours_and_timers_still_ring() {
    let (ui, _, summary) = reflex_run_then("non disturbare per due ore", 1.0, |tx| {
        // The answer off the screen first (a timer does not ring over a conversation).
        tx.send(CoreEvent::Ui(UiEvent::Stop)).unwrap();
        tx.send(CoreEvent::Ui(UiEvent::TimerStart(1))).unwrap();
        std::thread::sleep(Duration::from_millis(2500));
    });
    let until = ui
        .iter()
        .find_map(|c| match c {
            UiCommand::DoNotDisturb(Some(u)) => Some(*u as f64),
            _ => None,
        })
        .expect("do not disturb on");
    let now = home_companion::timers::now_unix();
    assert!((until - (now + 7200.0)).abs() < 10.0, "{until} vs {now}");
    assert!(
        ui.iter()
            .any(|c| matches!(c, UiCommand::Answer(a) if a.starts_with("Non disturbo fino alle"))),
        "{ui:?}"
    );
    assert_eq!(
        summary.rung.len(),
        1,
        "a timer rings even with do not disturb"
    );
}

#[test]
fn do_not_disturb_off() {
    let (ui, _, _) = reflex_run("disattiva non disturbare", 1.0);
    assert!(ui.contains(&UiCommand::DoNotDisturb(None)), "{ui:?}");
    assert!(ui.contains(&UiCommand::Answer("Non disturbare disattivato.".into())));
}

#[test]
fn a_named_timer_by_voice() {
    let (ui, _, _) = reflex_run("timer pasta dieci minuti", 1.0);
    let rows = ui
        .iter()
        .find_map(|c| match c {
            UiCommand::Timers(rows) if !rows.is_empty() => Some(rows.clone()),
            _ => None,
        })
        .expect("a timer shown");
    assert_eq!(rows[0].name, "Pasta");
    assert!(
        ui.contains(&UiCommand::Answer("Timer «Pasta» di 10:00 avviato.".into())),
        "{ui:?}"
    );
}

#[test]
fn nothing_to_repeat_and_no_timers_to_list() {
    let (ui, _, _) = reflex_run("ripeti", 1.0);
    assert!(
        ui.contains(&UiCommand::Answer("Non ho niente da ripetere.".into())),
        "{ui:?}"
    );
    let (ui, _, _) = reflex_run("che timer ho", 1.0);
    assert!(
        ui.contains(&UiCommand::Answer("Non c'è nessun timer.".into())),
        "{ui:?}"
    );
    let (ui, _, _) = reflex_run("quanto manca alla pasta", 1.0);
    assert!(
        ui.contains(&UiCommand::Answer("Non c'è un timer «Pasta».".into())),
        "{ui:?}"
    );
}

fn tap() -> UiEvent {
    UiEvent::Tap
}

#[test]
fn repeat_plays_the_last_answer_again() {
    // An announcement, then Talk and "ripeti": the same words come back on the screen and the
    // audio plays again (a second Speaking), with nothing sent to Home Assistant.
    let steps = vec![
        Step::Connect,
        Step::RunSatellite,
        Step::Sleep { ms: 300 },
        Step::Announce {
            path: fake_ha::announce_wav(),
        },
        Step::Sleep { ms: 9000 },
        Step::Disconnect,
    ];
    let presses = [
        Press {
            on: Phase::Speaking,
            wait: Duration::ZERO,
            event: tap,
        },
        Press {
            on: Phase::Home,
            wait: Duration::from_millis(200),
            event: talk,
        },
    ];
    let r = run_with(Scenario { ping: false, steps }, &presses, Some("ripeti"));
    assert!(r.record.failures.is_empty(), "{:?}", r.record.failures);
    assert_eq!(r.summary.detections_sent, 0);
    let speaking = r.views.iter().filter(|p| **p == Phase::Speaking).count();
    assert!(speaking >= 2, "{:?}", r.views);
    assert_eq!(
        outcomes(&r.summary),
        vec![
            ("announcement".into(), "played".into()),
            ("talk".into(), "reflex".into())
        ]
    );
    // The announcement had no words: the replay shows none (not "nothing to repeat").
    assert_eq!(r.summary.conversations[1].answer, "");
}
