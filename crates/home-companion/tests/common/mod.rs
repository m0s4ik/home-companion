// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the core's integration tests share: the core in-process against the fake Home Assistant
//! (null audio, the real Wyoming server on a free port), the screen's events injected when the
//! screen or the brain's port shows the right state, fake reflex helpers, a fake brain that
//! records every request, and a microphone that "speaks" when the test says.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use home_companion::core::{
    Core, CoreConfig, CoreEvent, Deps, MicControl, MicMode, Phase, Summary, UiCommand, UiEvent,
};
use home_companion::reflex::ReflexEvent;
use home_companion::timers::Timers;
use serde_json::Value;
use wyoming::fake_ha::{self, Record, Scenario};

/// When to inject the next event (each one after the previous).
#[derive(Clone, Copy, Debug)]
pub enum When {
    /// The first `View(phase)` after the previous event.
    View(Phase),
    /// This long after the previous event.
    Ms(u64),
    /// The brain's port has recorded at least this many requests with this `cmd`.
    Port(&'static str, usize),
}

/// A screen event (a button, or the brain's events as the screen forwards them), `wait` after
/// its moment.
#[derive(Clone, Copy)]
pub struct Press {
    pub on: When,
    pub wait: Duration,
    pub event: fn() -> UiEvent,
}

pub fn press(on: When, wait_ms: u64, event: fn() -> UiEvent) -> Press {
    Press {
        on,
        wait: Duration::from_millis(wait_ms),
        event,
    }
}

pub fn talk() -> UiEvent {
    UiEvent::Talk
}
pub fn stop() -> UiEvent {
    UiEvent::Stop
}
pub fn tap() -> UiEvent {
    UiEvent::Tap
}

pub const TALK_AT_HOME: Press = Press {
    on: When::View(Phase::Home),
    wait: Duration::from_millis(100),
    event: talk,
};

/// Milliseconds since the epoch (a `confirm_pending`'s `until_ms`).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The brain's side of the port: every request, in order, with when it came.
#[derive(Clone, Default)]
pub struct FakeBrain(pub Arc<Mutex<Vec<(Instant, Value)>>>);

impl home_companion::brain_link::BrainPort for FakeBrain {
    fn send(&self, req: Value) {
        self.0.lock().unwrap().push((Instant::now(), req));
    }
}

impl FakeBrain {
    pub fn all(&self) -> Vec<Value> {
        self.0.lock().unwrap().iter().map(|r| r.1.clone()).collect()
    }
    pub fn timed(&self) -> Vec<(Instant, Value)> {
        self.0.lock().unwrap().clone()
    }
    pub fn count(&self, cmd: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.1["cmd"] == cmd)
            .count()
    }
}

/// One utterance as the fake helper and the fake microphone play it.
#[derive(Clone, Debug)]
pub struct Utt {
    /// Where the microphone hears speech, in seconds of the utterance's audio.
    pub speech: Vec<(f32, f32)>,
    /// Partial texts, each sent once the helper got this much audio.
    pub partials: Vec<(f32, &'static str)>,
    /// The result at `E` (None: none), its words sure at `conf`, over the first speech span.
    pub final_text: Option<&'static str>,
    pub conf: f32,
    /// The helper dies once it got this much audio.
    pub lost_at: Option<f32>,
}

impl Utt {
    /// "speech 0.3-1.3 s", then `text` at `E`.
    pub fn says(text: &'static str) -> Utt {
        Utt {
            speech: vec![(0.3, 1.3)],
            partials: Vec::new(),
            final_text: Some(text),
            conf: 1.0,
            lost_at: None,
        }
    }
    /// Nothing heard at all.
    pub fn silent() -> Utt {
        Utt {
            speech: Vec::new(),
            partials: Vec::new(),
            final_text: Some(""),
            conf: 1.0,
            lost_at: None,
        }
    }
    pub fn conf(mut self, c: f32) -> Utt {
        self.conf = c;
        self
    }
    pub fn speech(mut self, spans: &[(f32, f32)]) -> Utt {
        self.speech = spans.to_vec();
        self
    }
    pub fn partials(mut self, p: &[(f32, &'static str)]) -> Utt {
        self.partials = p.to_vec();
        self
    }
    pub fn lost_at(mut self, s: f32) -> Utt {
        self.lost_at = Some(s);
        self
    }
    fn speaking_at(&self, t: f32) -> bool {
        self.speech.iter().any(|(a, b)| t >= *a && t < *b)
    }
}

/// A helper playing [`Utt`]s, one per `S` (the utterances in order; past the end, silence).
pub struct ScriptReflex {
    tx: Mutex<Sender<CoreEvent>>,
    utts: Vec<Utt>,
    state: Mutex<ScriptState>,
    pub sent: Arc<Mutex<Vec<reflex::proto::Request>>>,
}

#[derive(Default)]
struct ScriptState {
    n: usize,
    id: u64,
    samples: usize,
    partials_sent: usize,
    lost: bool,
}

impl ScriptReflex {
    fn utt(&self, n: usize) -> Utt {
        self.utts.get(n).cloned().unwrap_or_else(Utt::silent)
    }
}

impl home_companion::reflex::ReflexLink for ScriptReflex {
    fn ready(&self) -> bool {
        !self.state.lock().unwrap().lost
    }
    fn send(&self, r: reflex::proto::Request) -> bool {
        use reflex::proto::Request;
        self.sent.lock().unwrap().push(r.clone());
        let mut st = self.state.lock().unwrap();
        if st.lost {
            return false;
        }
        let emit = |e: CoreEvent| {
            let _ = self.tx.lock().unwrap().send(e);
        };
        match r {
            Request::Start(id) => {
                st.id = id;
                st.samples = 0;
                st.partials_sent = 0;
                st.n += 1;
            }
            Request::Audio(s) => {
                st.samples += s.len();
                let u = self.utt(st.n - 1);
                let at = st.samples as f32 / 16_000.0;
                if u.lost_at.is_some_and(|l| at >= l) {
                    st.lost = true;
                    emit(CoreEvent::Reflex(ReflexEvent::Lost));
                    return false;
                }
                while let Some((p_at, text)) = u.partials.get(st.partials_sent) {
                    if at < *p_at {
                        break;
                    }
                    st.partials_sent += 1;
                    emit(CoreEvent::Reflex(ReflexEvent::Partial {
                        id: st.id,
                        text: text.to_string(),
                        audio_s: at,
                    }));
                }
            }
            Request::End(id) => {
                let u = self.utt(st.n - 1);
                if let Some(text) = u.final_text {
                    let (a, b) = u.speech.first().copied().unwrap_or((0.0, 0.0));
                    let t: Vec<&str> = text.split_whitespace().collect();
                    let d = (b - a) / t.len().max(1) as f32;
                    let words = t
                        .iter()
                        .enumerate()
                        .map(|(i, w)| reflex::Word {
                            word: w.to_string(),
                            conf: u.conf,
                            start: a + i as f32 * d,
                            end: a + (i + 1) as f32 * d,
                        })
                        .collect();
                    emit(CoreEvent::Reflex(ReflexEvent::Result {
                        id,
                        endpoint: false,
                        text: text.to_string(),
                        words,
                        ms: 3.0,
                    }));
                }
            }
        }
        true
    }
    fn set_language(&self, _: &str) {}
}

/// A helper that answers `text` (every word sure, covering 0-6 s) to each `E`.
pub struct AnsweringReflex {
    pub tx: Mutex<Sender<CoreEvent>>,
    pub text: &'static str,
}

impl home_companion::reflex::ReflexLink for AnsweringReflex {
    fn ready(&self) -> bool {
        true
    }
    fn send(&self, r: reflex::proto::Request) -> bool {
        if let reflex::proto::Request::End(id) = r {
            let _ = self
                .tx
                .lock()
                .unwrap()
                .send(CoreEvent::Reflex(ReflexEvent::Result {
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
                }));
        }
        true
    }
    fn set_language(&self, _: &str) {}
}

/// A helper that is always ready and records what the body sends.
pub struct FakeReflex(pub Arc<Mutex<Vec<reflex::proto::Request>>>);

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

/// A 20 ms microphone frame: a tone (`amp` 0.05: speech for the detector) or faint noise.
pub fn frame(index: u64, amp: f32) -> CoreEvent {
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

/// The reflexes of a test.
pub enum Helper {
    None,
    /// [`AnsweringReflex`] with this text (the real wake thread's silent microphone).
    Answering(&'static str),
    /// [`ScriptReflex`] and the scripted microphone.
    Script(Vec<Utt>),
}

/// A test's setup.
pub struct Setup {
    pub scenario: Scenario,
    pub presses: Vec<Press>,
    pub helper: Helper,
    /// The microphone: the utterances' speech (one per stream session), at twice real time;
    /// false: the real wake thread over the null (silent) microphone.
    pub scripted_mic: Option<Vec<Utt>>,
    pub think_timeout: Option<Duration>,
}

impl Setup {
    pub fn new(scenario: Scenario, presses: Vec<Press>) -> Setup {
        Setup {
            scenario,
            presses,
            helper: Helper::None,
            scripted_mic: None,
            think_timeout: None,
        }
    }
    /// The reflexes play `utts`, and the microphone speaks them.
    pub fn utts(mut self, utts: Vec<Utt>) -> Setup {
        self.scripted_mic = Some(utts.clone());
        self.helper = Helper::Script(utts);
        self
    }
    /// No reflexes; the microphone speaks `utts`.
    pub fn mic(mut self, utts: Vec<Utt>) -> Setup {
        self.scripted_mic = Some(utts);
        self.helper = Helper::None;
        self
    }
}

pub struct Run {
    pub record: Record,
    pub summary: Summary,
    pub views: Vec<Phase>,
    pub ui: Vec<UiCommand>,
    pub port: Vec<Value>,
    /// Each brain request's time, in seconds since just before the fake Home Assistant
    /// started (its own clock starts a little later: an instant of the fake's `sent` is never
    /// later than the same instant here).
    pub port_at: Vec<(f64, Value)>,
}

impl Run {
    /// The brain's requests with this `cmd`.
    pub fn port(&self, cmd: &str) -> Vec<Value> {
        self.port
            .iter()
            .filter(|r| r["cmd"] == cmd)
            .cloned()
            .collect()
    }
    /// When each brain request with this `cmd` came (see [`Run::port_at`]).
    pub fn port_times(&self, cmd: &str) -> Vec<f64> {
        self.port_at
            .iter()
            .filter(|r| r.1["cmd"] == cmd)
            .map(|r| r.0)
            .collect()
    }
    /// When the fake Home Assistant sent its messages of this type, on its clock.
    pub fn sent_times(&self, kind: &str) -> Vec<f64> {
        self.record
            .sent
            .iter()
            .filter(|s| s.1 == kind)
            .map(|s| s.0)
            .collect()
    }
    pub fn outcomes(&self) -> Vec<(String, String)> {
        outcomes(&self.summary)
    }
    pub fn cues(&self, name: &str) -> usize {
        self.summary.cues.iter().filter(|c| **c == name).count()
    }
    /// Messages the satellite sent, by type.
    pub fn received(&self, kind: &str) -> usize {
        self.record.received.iter().filter(|r| r.1 == kind).count()
    }
    pub fn answers(&self) -> Vec<String> {
        self.ui
            .iter()
            .filter_map(|c| match c {
                UiCommand::Answer(a) if !a.is_empty() => Some(a.clone()),
                _ => None,
            })
            .collect()
    }
}

pub fn outcomes(s: &Summary) -> Vec<(String, String)> {
    s.conversations
        .iter()
        .map(|c| (c.trigger.to_string(), c.outcome.clone()))
        .collect()
}

/// The core with the real wake thread (silent microphone), the fake Home Assistant and, if
/// `reflex`, a helper that answers this text at the end of every utterance.
pub fn run_with(scenario: Scenario, presses: &[Press], reflex: Option<&'static str>) -> Run {
    let mut s = Setup::new(scenario, presses.to_vec());
    if let Some(t) = reflex {
        s.helper = Helper::Answering(t);
    }
    run_setup(s)
}

pub fn run(scenario: Scenario, presses: &[Press]) -> Run {
    run_with(scenario, presses, None)
}

pub fn run_setup(setup: Setup) -> Run {
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
    let pump_stop = Arc::new(AtomicBool::new(false));
    let (wake, pump) = match setup.scripted_mic.clone() {
        None => (
            Some(
                home_companion::wake_thread::spawn(
                    frames,
                    None,
                    Vec::new(),
                    mic.clone(),
                    core_tx.clone(),
                    Arc::new(Mutex::new(None)),
                )
                .unwrap(),
            ),
            None,
        ),
        Some(utts) => {
            drop(frames);
            let (mic, tx, stop) = (mic.clone(), core_tx.clone(), pump_stop.clone());
            (
                None,
                Some(std::thread::spawn(move || pump_mic(&mic, &tx, &stop, utts))),
            )
        }
    };
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
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let link: Option<Box<dyn home_companion::reflex::ReflexLink>> = match setup.helper {
        Helper::None => None,
        Helper::Answering(text) => Some(Box::new(AnsweringReflex {
            tx: Mutex::new(core_tx.clone()),
            text,
        })),
        Helper::Script(utts) => Some(Box::new(ScriptReflex {
            tx: Mutex::new(core_tx.clone()),
            utts,
            state: Mutex::new(ScriptState::default()),
            sent: Arc::new(Mutex::new(Vec::new())),
        })),
    };
    let reflexes = link.map(|link| {
        home_companion::reflex::Reflexes::new(
            link,
            &home_companion::config::ReflexesCfg::default(),
            repo.join("characters/alfred/grammar"),
            repo.join("locales"),
            "it",
        )
    });
    let brain = FakeBrain::default();
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
        voice: None,
        brain: Some(Box::new(brain.clone())),
    };
    let mut core_cfg = CoreConfig {
        bell_repeats: 2,
        bell_gap: Duration::from_millis(200),
        ..Default::default()
    };
    if let Some(t) = setup.think_timeout {
        core_cfg.think_timeout = t;
    }
    let core = std::thread::spawn(move || Core::new(core_cfg, deps).run(core_rx));
    let scenario = setup.scenario;
    let t0 = Instant::now();
    let ha = std::thread::spawn(move || fake_ha::run(addr, &scenario).unwrap());
    let mut ui = drive(&ui_rx, &core_tx, &setup.presses, &ha, &brain);
    let record = ha.join().unwrap();
    // Let the last events settle (a bell ending, a phase change).
    std::thread::sleep(Duration::from_millis(300));
    core_tx.send(CoreEvent::Quit).unwrap();
    let summary = core.join().unwrap();
    pump_stop.store(true, Ordering::SeqCst);
    if let Some(w) = wake {
        w.join().unwrap();
    }
    if let Some(p) = pump {
        p.join().unwrap();
    }
    ui.extend(ui_rx.try_iter());
    let views = ui
        .iter()
        .filter_map(|c| match c {
            UiCommand::View(p) => Some(*p),
            _ => None,
        })
        .collect();
    Run {
        record,
        summary,
        views,
        ui,
        port: brain.all(),
        port_at: brain
            .timed()
            .into_iter()
            .map(|(at, r)| (at.saturating_duration_since(t0).as_secs_f64(), r))
            .collect(),
    }
}

/// The scripted microphone: while the core streams, one 20 ms frame every 10 ms (twice real
/// time), speech where the session's utterance has it; each stream session is the next
/// utterance.
fn pump_mic(mic: &MicControl, tx: &Sender<CoreEvent>, stop: &AtomicBool, utts: Vec<Utt>) {
    let mut utts: VecDeque<Utt> = utts.into();
    let mut current: Option<Utt> = None;
    let (mut index, mut n) = (0u64, 0u64);
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
        if mic.mode() != MicMode::Stream {
            current = None;
            continue;
        }
        let u = current.get_or_insert_with(|| {
            n = 0;
            utts.pop_front().unwrap_or_else(Utt::silent)
        });
        let t = n as f32 * 0.02;
        let amp = if u.speaking_at(t) { 0.05 } else { 0.0005 };
        if tx.send(frame(index, amp)).is_err() {
            return;
        }
        index += 1;
        n += 1;
    }
}

/// Watches the screen commands and injects the events; returns every command seen.
fn drive(
    ui: &Receiver<UiCommand>,
    core: &Sender<CoreEvent>,
    presses: &[Press],
    ha: &std::thread::JoinHandle<Record>,
    brain: &FakeBrain,
) -> Vec<UiCommand> {
    let mut seen = Vec::new();
    let mut next = 0;
    let mut since = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !ha.is_finished() {
        let got = ui.recv_timeout(Duration::from_millis(10)).ok();
        if let Some(c) = &got {
            seen.push(c.clone());
        }
        let Some(p) = presses.get(next) else {
            continue;
        };
        let due = match p.on {
            When::View(phase) => got == Some(UiCommand::View(phase)),
            When::Ms(ms) => since.elapsed() >= Duration::from_millis(ms),
            When::Port(cmd, n) => brain.count(cmd) >= n,
        };
        if due {
            std::thread::sleep(p.wait);
            core.send(CoreEvent::Ui((p.event)())).unwrap();
            next += 1;
            since = Instant::now();
        }
    }
    assert_eq!(
        next,
        presses.len(),
        "not every event was injected; seen {:?}",
        seen.iter()
            .filter(|c| matches!(c, UiCommand::View(_)))
            .collect::<Vec<_>>()
    );
    seen
}
