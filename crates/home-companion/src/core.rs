// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The state machine. One thread, one channel: every other thread (Wyoming readers, audio,
//! wake word, UI) sends a [`CoreEvent`]; the core answers Home Assistant through the Wyoming
//! [`wyoming::Sender`], plays through the audio [`audio::Player`] and tells the screen what
//! to show with [`UiCommand`]s. A 1 s tick drives the timers, the deadlines and the stats.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender as ChanSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audio::{AudioEvent, CaptureFrame, Player, Recorder, SourceId, StreamFormat};
use wyoming::{AudioFormat, Disconnect, Message, ServerEvent};

use crate::alarms::{Alarm, Alarms};
use crate::conversation::{Conversation, Record, Trigger};
use crate::cues::Cues;
use crate::reflex::{ReflexEvent, Reflexes};
use crate::settings::Settings;
use crate::timers::{TimerView, Timers, now_unix};
use crate::volume::{Volume, VolumeRequest};
use reflex::proto::Request as ReflexRequest;
use reflex::vad::Vad;
use reflex::{Command as Reflex, Decision};

/// What the screen sends.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    /// A control of the screen was pressed: the soft tap sound.
    Tap,
    /// A reminder or message card appeared (by day): its soft sound.
    Reminder,
    /// Back home from Home Assistant's view, by the user: its own sound.
    Home,
    /// Something the user waited for worked (linked to Home Assistant): the "ok" sound.
    Success,
    Talk,
    Stop,
    MessageOk(usize),
    Language(String),
    HomeAssistant,
    Screenshot,
    /// Settings → Suspend, confirmed: the whole device to sleep (suspend.rs).
    Suspend,
    /// Timer e sveglie: a new timer of this many seconds (a local one).
    TimerStart(u64),
    /// Pause a running timer or resume a paused one.
    TimerPause(String),
    /// Cancel a timer (after the 5 s the screen gives to change one's mind).
    TimerCancel(String),
    /// A ringing timer or alarm: stop it and ring again in a minute.
    Snooze,
    /// A new alarm: hour, minute, days (bit 0 = Monday; 0 = once).
    AlarmSave(u8, u8, u8),
    AlarmToggle(u32, bool),
    /// Delete an alarm (after the 5 s chip).
    AlarmDelete(u32),
    /// A due reminder is full screen (true) or not any more: the stop word acknowledges it.
    Notice(bool),
    /// A tile of the control center: "airplay" or "mic" (the others are the screen's own).
    ControlTile(String),
    /// The control center's pause/play of AirPlay (a local mute: the sender keeps playing).
    MusicToggle,
    /// The screen's next / previous track.
    MusicNext,
    /// "Interrompi" on the mirroring page.
    MirrorStop,
    /// The control center's "do not disturb": on until this unix time (Some) or off (None), the
    /// same as the reflex.
    DoNotDisturb(Option<u64>),
    MusicPrevious,
}

pub enum CoreEvent {
    Server(ServerEvent),
    WakeWord(wakeword::Detection),
    /// The stop word ("basta", "stop"), while the character thinks, speaks or rings.
    StopWord(wakeword::Detection),
    /// Only while streaming to Home Assistant.
    MicFrame(CaptureFrame),
    Audio(AudioEvent),
    Ui(UiEvent),
    /// A volume key (the keys thread) or, later, Home Assistant (MQTT, M2b).
    Volume(VolumeRequest),
    /// The reflex helper (crates/home-companion/src/reflex.rs).
    Reflex(ReflexEvent),
    /// AirPlay: what plays (crates/home-companion/src/airplay.rs).
    Music(crate::airplay::MusicEvent),
    /// SIGTERM/SIGINT, `--exit-after`, the window closed.
    Quit,
}

/// The phases of the state machine; also what the screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Home,
    Listening,
    Thinking,
    Speaking,
    Error,
    /// Home Assistant not connected, not ready yet, or paused: home with the offline pose.
    Offline,
    /// A timer rings: home with the timer_finished pose and an OK card.
    Ringing,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Home => "home",
            Phase::Listening => "listening",
            Phase::Thinking => "thinking",
            Phase::Speaking => "speaking",
            Phase::Error => "error",
            Phase::Offline => "offline",
            Phase::Ringing => "ringing",
        }
    }

    pub fn parse(s: &str) -> Option<Phase> {
        [
            Phase::Home,
            Phase::Listening,
            Phase::Thinking,
            Phase::Speaking,
            Phase::Error,
            Phase::Offline,
            Phase::Ringing,
        ]
        .into_iter()
        .find(|p| p.name() == s)
    }

    pub fn in_conversation(self) -> bool {
        matches!(self, Phase::Listening | Phase::Thinking | Phase::Speaking)
    }
}

/// What the core tells the screen (plain data: Slint objects stay on the UI thread).
#[derive(Debug, Clone, PartialEq)]
pub enum UiCommand {
    View(Phase),
    Heard(String),
    Answer(String),
    /// The timers, soonest first.
    Timers(Vec<TimerView>),
    /// The alarms, by time of day.
    Alarms(Vec<Alarm>),
    /// (name, kind): what rings ("timer" or "alarm"), the first one on screen.
    Messages(Vec<(String, String)>),
    /// 72 bars, 0..1.
    Wave(Vec<f32>),
    Screenshot(PathBuf),
    /// The volume changed or a key was pressed: show the bar with this percentage.
    Volume(u8),
    /// The volume as it is (at start): no bar.
    VolumeNow(u8),
    /// The device is about to suspend: screen off now.
    Suspending,
    /// "Spegni lo schermo" (a reflex): the backlight off until a touch, a key or a wake word.
    ScreenOff,
    /// "Non disturbare" (a reflex): no sounds nor cards' sounds, the screen dim, until this
    /// unix time (Some) or off again (None).
    DoNotDisturb(Option<u64>),
    /// Back from a suspend: wake and redraw the screen.
    Resumed,
    /// "Stop" or "basta" over a due reminder: its OK.
    NoticeStop,
    /// What AirPlay plays (None: nothing); `playing` false while paused or muted here.
    Music(Option<crate::airplay::NowPlaying>),
    /// AirPlay screen mirroring: the sender's name and the frames' socket (None = over).
    MirrorVideo(Option<(String, std::path::PathBuf)>),
    /// The mirrored picture's aspect ratio (the frames' padding is cropped).
    MirrorAspect(f32),
    /// What the control center's tiles of the core show: AirPlay (None = off or not installed,
    /// Some(None) = on and waiting, Some(Some(sender)) = playing) and the microphone.
    Control {
        airplay: Option<Option<String>>,
        mic_on: bool,
    },
    /// A phone or a tablet mirrors its screen here (AirPlay): who and the frame's size; the
    /// page opens when a session starts, a new size (the sender rotated) only re-lays it out.
    /// None: the session is over, the page closes back to where it was. The frames themselves
    /// do not come this way (the transport sets the image on the UI thread).
    Mirror(Option<MirrorInfo>),
}

/// A screen-mirroring session (UiCommand::Mirror).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirrorInfo {
    /// The sender's name ("iPhone di Andrea").
    pub source: String,
    /// The video frame's size in pixels (at most 1280x800, the sender's aspect ratio).
    pub width: u32,
    pub height: u32,
}

/// Where the microphone frames go (the wake word thread reads it for every frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MicMode {
    /// Into the wake word detector.
    Wake = 0,
    /// To the core, which streams them to Home Assistant.
    Stream = 1,
    Off = 2,
}

#[derive(Debug, Default)]
pub struct MicControl {
    mode: AtomicU8,
    reset: AtomicBool,
    /// The stop word detector runs too (the character thinks, speaks, rings or announces).
    stop: AtomicBool,
}

impl MicControl {
    pub fn mode(&self) -> MicMode {
        match self.mode.load(Ordering::Acquire) {
            1 => MicMode::Stream,
            2 => MicMode::Off,
            _ => MicMode::Wake,
        }
    }

    pub fn set(&self, m: MicMode) {
        self.mode.store(m as u8, Ordering::Release);
    }

    /// Asks the wake word thread to reset its detector before the next frame.
    pub fn request_reset(&self) {
        self.reset.store(true, Ordering::Release);
    }

    pub fn take_reset(&self) -> bool {
        self.reset.swap(false, Ordering::AcqRel)
    }

    pub fn set_stop_listening(&self, on: bool) {
        self.stop.store(on, Ordering::Release);
    }

    pub fn stop_listening(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HaLink {
    Disconnected,
    Connected { paused: bool, ready: bool },
}

#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// The wake model's id (`detection.name`, must match `Info`).
    pub wake_name: String,
    pub bell_repeats: u32,
    pub bell_gap: Duration,
    pub listen_timeout: Duration,
    pub think_timeout: Duration,
    pub error_show: Duration,
    pub offline_flash: Duration,
    pub fps: u32,
    pub stats_every: Duration,
    pub screenshot_dir: PathBuf,
    /// The character's `sounds/` (missing files: plain tones).
    pub sounds_dir: Option<PathBuf>,
}

impl Default for CoreConfig {
    fn default() -> Self {
        CoreConfig {
            wake_name: "okay_alfred".into(),
            bell_repeats: 6,
            bell_gap: Duration::from_secs(1),
            listen_timeout: Duration::from_secs(20),
            think_timeout: Duration::from_secs(60), // a language model with tools can take 20-40 s
            error_show: Duration::from_secs(3),
            offline_flash: Duration::from_secs(3),
            fps: 15,
            stats_every: Duration::from_secs(60),
            screenshot_dir: PathBuf::from("."),
            sounds_dir: None,
        }
    }
}

/// What happened, for tests and the exit status.
#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub exit_code: i32,
    pub conversations: Vec<Record>,
    /// Timer ids rung by us, in order.
    pub rung: Vec<String>,
    pub phases: Vec<Phase>,
    pub detections_sent: u32,
}

/// Everything the core drives. `audio` and `server` are owned so the core can stop them at
/// shutdown (the audio drained first, then the server: Home Assistant sees EOF).
pub struct Deps {
    pub sender: wyoming::Sender,
    pub player: Player,
    pub recorder: Option<Recorder>,
    pub ui: ChanSender<UiCommand>,
    pub mic: Arc<MicControl>,
    pub audio: Option<audio::Audio>,
    pub server: Option<wyoming::Server>,
    pub timers: Timers,
    pub alarms: Alarms,
    pub settings: Option<Settings>,
    /// The speaker volume (None: the keys only show nothing; tests).
    pub volume: Option<Volume>,
    /// Updated by the wake word thread (for the stats line).
    pub wake_stats: Arc<Mutex<Option<wakeword::Stats>>>,
    /// Checked on every wake-up (the signal handler sets it).
    pub stop_flag: Option<&'static AtomicBool>,
    /// Local reflexes (None: every utterance streams to Home Assistant at once).
    pub reflexes: Option<Reflexes>,
    /// AirPlay (None: off).
    pub music: Option<crate::airplay::Music>,
}

/// An utterance held back for the reflexes: buffered and fed to the helper while the user speaks;
/// at the end of speech either done locally or sent to Home Assistant in one burst.
struct Gate {
    id: u64,
    buf: Vec<i16>,
    vad: Vad,
    started: Instant,
    /// `E` sent to the helper (the end of speech came before its own).
    end_sent: Option<Instant>,
    /// The helper's result, kept until the end of speech.
    result: Option<(String, Vec<reflex::Word>, bool, f64)>,
}

/// After the end of speech, how long the helper may take before the utterance goes to Home
/// Assistant anyway.
const REFLEX_ANSWER_WAIT: Duration = Duration::from_millis(1500);
/// A local answer stays on screen this long.
const LOCAL_ANSWER: Duration = Duration::from_secs(5);

const BARS: usize = 72;

pub struct Core {
    cfg: CoreConfig,
    d: Deps,
    cues: Cues,
    phase: Phase,
    ha: HaLink,
    conv: Option<Conversation>,
    next_id: u64,
    bell: Option<SourceId>,
    /// The beep of the last volume step (a new step replaces it).
    volume_beep: Option<SourceId>,
    ringing: Vec<String>,
    /// A due reminder is full screen (UiEvent::Notice).
    noticing: bool,
    error_until: Option<Instant>,
    flash_until: Option<Instant>,
    listen_deadline: Option<Instant>,
    think_deadline: Option<Instant>,
    speak_deadline: Option<Instant>,
    wave: VecDeque<f32>,
    wave_t0: Instant,
    last_rows: Vec<TimerView>,
    /// Local timers that are alarms ringing (the screen says "Sveglia").
    alarm_ids: Vec<String>,
    last_stats: Instant,
    last_cpu: f64,
    chunk_buf: Vec<u8>,
    answer_dropped: u64,
    /// The utterance held back for the reflexes.
    gate: Option<Gate>,
    /// A reflex's answer is on screen until then.
    local_until: Option<Instant>,
    /// The reflex result, written next to the recording when it stops.
    sidecar: Option<serde_json::Value>,
    quit: bool,
    summary: Summary,
    /// The control center's microphone switch: off, no wake word (Parla still works).
    mic_muted: bool,
    airplay_on: bool,
    now_playing: Option<crate::airplay::NowPlaying>,
    /// The last spoken answer, for "ripeti".
    tape: crate::repeat::Tape,
    /// "Ripeti": the last answer playing again.
    replay: Option<audio::Stream>,
    /// "Non disturbare" until then (seconds since the epoch): no card, reminder or announcement
    /// sounds, no final ticks; timers and alarms still ring.
    dnd_until: Option<u64>,
}

/// A sentence ending on a word that cannot end it (a preposition, an article, "e"): the speaker
/// paused in the middle ("metti una sveglia alle …").
fn unfinished(text: &str) -> bool {
    const OPEN: &[&str] = &[
        "alle", "alla", "allo", "al", "all", "a", "ad", "di", "del", "della", "da", "dal", "dalle",
        "tra", "fra", "per", "in", "con", "su", "e", "il", "lo", "la", "le", "gli", "un", "una",
        "uno", "at", "to", "for", "in", "of", "the", "a", "an", "and",
    ];
    text.split_whitespace()
        .last()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .is_some_and(|w| OPEN.contains(&w.as_str()))
}

fn level_to_bar(db: f32) -> f32 {
    ((db + 50.0) / 40.0).clamp(0.0, 1.0)
}

impl Core {
    pub fn new(cfg: CoreConfig, d: Deps) -> Core {
        let cues = cfg.sounds_dir.as_deref().map_or_else(Cues::new, Cues::load);
        Core {
            cfg,
            d,
            cues,
            phase: Phase::Offline,
            ha: HaLink::Disconnected,
            conv: None,
            next_id: 1,
            bell: None,
            volume_beep: None,
            ringing: Vec::new(),
            noticing: false,
            error_until: None,
            flash_until: None,
            listen_deadline: None,
            think_deadline: None,
            speak_deadline: None,
            wave: VecDeque::with_capacity(BARS),
            wave_t0: Instant::now(),
            last_rows: Vec::new(),
            alarm_ids: Vec::new(),
            last_stats: Instant::now(),
            last_cpu: audio::process_cpu_seconds(),
            chunk_buf: Vec::with_capacity(640),
            answer_dropped: 0,
            gate: None,
            local_until: None,
            sidecar: None,
            quit: false,
            summary: Summary::default(),
            mic_muted: false,
            airplay_on: true,
            now_playing: None,
            tape: crate::repeat::Tape::default(),
            replay: None,
            dnd_until: None,
        }
    }

    /// The event loop; returns when asked to quit (or every sender is gone).
    pub fn run(mut self, rx: Receiver<CoreEvent>) -> Summary {
        self.ui(UiCommand::View(self.phase));
        self.summary.phases.push(self.phase);
        if let Some(s) = &self.d.settings {
            self.mic_muted = s.mic_muted();
            self.airplay_on = s.airplay_on();
        }
        if let Some(m) = &self.d.music {
            m.set_enabled(self.airplay_on);
        }
        self.mic_idle();
        self.send_control();
        self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
        if let Some(v) = &self.d.volume {
            self.ui(UiCommand::VolumeNow(v.percent()));
        }
        self.tick();
        let second = Duration::from_secs(1);
        let frame = Duration::from_secs_f64(1.0 / self.cfg.fps.max(1) as f64);
        let mut next_tick = Instant::now() + second;
        let mut next_wave = Instant::now() + frame;
        while !self.quit {
            let now = Instant::now();
            let mut until = next_tick;
            if self.phase.in_conversation() {
                until = until.min(next_wave);
            }
            match rx.recv_timeout(
                until
                    .saturating_duration_since(now)
                    .max(Duration::from_millis(1)),
            ) {
                Ok(ev) => self.handle(ev),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.d.stop_flag.is_some_and(|f| f.load(Ordering::Relaxed)) {
                log::info!("signal received: shutting down");
                self.quit = true;
            }
            let now = Instant::now();
            if now >= next_tick {
                self.tick();
                next_tick += second;
                if next_tick < now {
                    next_tick = now + second;
                }
            }
            if self.phase.in_conversation() && now >= next_wave {
                self.wave_tick();
                next_wave = now + frame;
            }
        }
        self.shutdown();
        self.summary
    }

    pub fn handle(&mut self, ev: CoreEvent) {
        match ev {
            CoreEvent::Server(e) => self.on_server(e),
            CoreEvent::WakeWord(d) => self.on_wake(d),
            CoreEvent::StopWord(d) => self.on_stop_word(d),
            CoreEvent::MicFrame(f) => self.on_mic(f),
            CoreEvent::Audio(e) => self.on_audio(e),
            CoreEvent::Ui(e) => self.on_ui(e),
            CoreEvent::Volume(r) => self.on_volume(r),
            CoreEvent::Reflex(e) => self.on_reflex(e),
            CoreEvent::Music(e) => self.on_music(e),
            CoreEvent::Quit => self.quit = true,
        }
    }

    // ---- helpers ------------------------------------------------------------------------

    fn ui(&self, c: UiCommand) {
        let _ = self.d.ui.send(c);
    }

    fn set_phase(&mut self, p: Phase) {
        if p == Phase::Error {
            self.error_until = Some(Instant::now() + self.cfg.error_show);
        }
        let stop = self.stop_wanted(p);
        self.d.mic.set_stop_listening(stop);
        if let Some(m) = &self.d.music {
            // Music goes down under the character's voice, a ringing timer and the user's.
            m.duck(p.in_conversation() || p == Phase::Ringing);
        }
        if p != self.phase {
            log::info!(event = "phase", phase = p.name(), from = self.phase.name(); "phase {} -> {}", self.phase.name(), p.name());
            self.phase = p;
            self.summary.phases.push(p);
            if p.in_conversation() && self.wave.is_empty() {
                self.wave_t0 = Instant::now();
            }
        }
        // Always re-sent: a flash of the offline pose may be on screen.
        self.ui(UiCommand::View(p));
    }

    fn ha_ready(&self) -> bool {
        matches!(
            self.ha,
            HaLink::Connected {
                paused: false,
                ready: true
            }
        )
    }

    fn idle_phase(&self) -> Phase {
        if !self.ringing.is_empty() {
            Phase::Ringing
        } else if self.ha_ready() {
            Phase::Home
        } else {
            Phase::Offline
        }
    }

    /// Back to waiting for the wake word.
    fn go_idle(&mut self) {
        self.mic_idle();
        self.d.mic.request_reset();
        self.listen_deadline = None;
        self.speak_deadline = None;
        if self.conv.as_ref().is_none_or(|c| !c.discard_answer) {
            self.think_deadline = None;
        }
        self.wave.clear();
        let p = self.idle_phase();
        self.set_phase(p);
        self.ring_due();
    }

    fn stop_recorder(&mut self) {
        let sidecar = self.sidecar.take();
        if let Some(r) = &self.d.recorder {
            if let Some(p) = r.stop() {
                log::info!(event = "recorded", path = p.to_string_lossy().as_ref(); "utterance saved in {}", p.display());
                // The reflex result next to it, to tune the threshold on real voices later.
                if let Some(j) = sidecar {
                    let side = p.with_extension("json");
                    if let Err(e) = std::fs::write(&side, format!("{j:#}\n")) {
                        log::warn!("{}: {e}", side.display());
                    }
                }
            }
        }
    }

    /// Ends the current conversation with its `conversation` line.
    fn close(&mut self, outcome: &str) {
        if let Some(mut c) = self.conv.take() {
            if let Some(s) = c.stream.take() {
                s.abort();
            }
            let r = c.log(outcome);
            self.summary.conversations.push(r);
        }
        self.think_deadline = None;
        self.speak_deadline = None;
    }

    fn send(&self, m: &Message) -> bool {
        match self.d.sender.send(m) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("sending {}: {e}", m.kind());
                false
            }
        }
    }

    fn send_played(&mut self) {
        self.send(&Message::Played);
        if let Some(c) = &mut self.conv {
            c.played_at = Some(Instant::now());
        }
    }

    // ---- conversations ----------------------------------------------------------------

    fn start_conversation(&mut self, trigger: Trigger) {
        // With the reflex helper up, the utterance is held back and decoded locally first; Home
        // Assistant is not needed for a reflex (Wi-Fi off, Home Assistant down).
        let id = self.next_id;
        let gated = self
            .d
            .reflexes
            .as_ref()
            .is_some_and(|r| r.usable() && r.link.send(ReflexRequest::Start(id)));
        if !self.ha_ready() && !gated {
            log::info!(event = "no_home_assistant", trigger = trigger.name(); "{} with no Home Assistant", trigger.name());
            self.d.player.play(&self.cues.error);
            self.flash_until = Some(Instant::now() + self.cfg.offline_flash);
            self.ui(UiCommand::View(Phase::Offline));
            return;
        }
        if self.conv.is_some() {
            // A stopped conversation still waiting for its late answer.
            self.close("stopped");
        }
        self.local_until = None;
        self.sidecar = None;
        self.next_id += 1;
        let mut c = Conversation::new(id, trigger);
        c.detection_at = Some(Instant::now());
        self.d.player.play(&self.cues.wake);
        self.d.mic.set(MicMode::Stream);
        if let Some(r) = &self.d.recorder {
            r.start("utterance");
        }
        log::info!(event = "conversation_start", id = id, trigger = trigger.name(), reflexes = gated; "conversation {id} started ({}{})", trigger.name(), if gated { ", reflexes first" } else { "" });
        self.conv = Some(c);
        self.ui(UiCommand::Heard(String::new()));
        self.ui(UiCommand::Answer(String::new()));
        let ok = if gated {
            self.gate = Some(Gate {
                id,
                buf: Vec::with_capacity(16_000 * 4),
                vad: Vad::new(),
                started: Instant::now(),
                end_sent: None,
                result: None,
            });
            true
        } else {
            self.gate = None;
            self.open_pipeline()
        };
        if !ok {
            self.mic_idle();
            self.stop_recorder();
            self.close("disconnected");
            self.go_idle();
            return;
        }
        self.listen_deadline = Some(Instant::now() + self.cfg.listen_timeout);
        self.wave.clear();
        self.set_phase(Phase::Listening);
    }

    /// Starts Home Assistant's pipeline for the current conversation (detection, run-pipeline,
    /// audio-start); false if the connection failed.
    fn open_pipeline(&mut self) -> bool {
        let detection = Message::Detection(wyoming::Detection {
            name: Some(self.cfg.wake_name.clone()),
            timestamp: None,
            speaker: None,
        });
        let ok = self.send(&detection)
            && self.send(&Message::RunPipeline(wyoming::RunPipeline::asr_to_tts()))
            && self.send(&Message::AudioStart {
                format: AudioFormat::MIC,
                timestamp: Some(0),
            });
        if let Some(c) = &mut self.conv {
            c.run_pipeline_at = Some(Instant::now());
        }
        self.summary.detections_sent += 1;
        ok
    }

    /// End of the user's speech (voice-stopped or transcript): stop streaming.
    fn end_listening(&mut self) {
        self.mic_idle();
        self.d.mic.request_reset();
        self.stop_recorder();
        self.listen_deadline = None;
        self.think_deadline = Some(Instant::now() + self.cfg.think_timeout);
        self.set_phase(Phase::Thinking);
    }

    fn fail(&mut self, outcome: &str) {
        self.mic_idle();
        self.d.mic.request_reset();
        self.stop_recorder();
        self.d.player.play(&self.cues.error);
        self.close(outcome);
        self.listen_deadline = None;
        self.wave.clear();
        self.set_phase(Phase::Error);
    }

    /// The stop word listens only while there is something to stop: an answer, a bell, a due
    /// reminder on the screen.
    fn stop_wanted(&self, p: Phase) -> bool {
        matches!(p, Phase::Thinking | Phase::Speaking | Phase::Ringing)
            || (self.noticing && matches!(p, Phase::Home | Phase::Offline))
    }

    fn on_stop_word(&mut self, d: wakeword::Detection) {
        log::info!(event = "stop_word", name = d.name.as_str(), probability = d.probability as f64, phase = self.phase.name(); "stop word {} ({:.3}) in {}", d.name, d.probability, self.phase.name());
        match self.phase {
            // Thinking: a wrong transcript on the screen; the answer is dropped (the pipeline
            // itself cannot be cancelled).
            Phase::Thinking | Phase::Speaking | Phase::Ringing => self.on_stop(),
            Phase::Home | Phase::Offline if self.noticing => self.ui(UiCommand::NoticeStop),
            _ => {}
        }
    }

    fn on_wake(&mut self, d: wakeword::Detection) {
        log::info!(event = "wake_word", name = d.name.as_str(), probability = d.probability as f64, at_s = d.at.as_secs_f64(); "wake word {} ({:.3})", d.name, d.probability);
        match self.phase {
            Phase::Home | Phase::Offline | Phase::Error => self.start_conversation(Trigger::Wake),
            Phase::Ringing => {
                self.stop_bell();
                self.start_conversation(Trigger::Wake);
            }
            _ => log::debug!("wake word ignored in {}", self.phase.name()),
        }
        // The wake thread already streams the frames after a detection: back to the detector
        // when no conversation listens to them.
        if self.phase != Phase::Listening {
            self.mic_idle();
            self.d.mic.request_reset();
        }
    }

    fn on_mic(&mut self, f: CaptureFrame) {
        if self.phase != Phase::Listening {
            return;
        }
        if self.gate.is_some() {
            self.on_mic_gated(&f);
            return;
        }
        let Some(c) = &mut self.conv else { return };
        let first = *c.first_frame.get_or_insert(f.index);
        let ts = (f.index.saturating_sub(first) * audio::FRAME_MS as u64) as i64;
        self.chunk_buf.clear();
        for s in f.samples.iter() {
            self.chunk_buf.extend_from_slice(&s.to_le_bytes());
        }
        if self
            .d
            .sender
            .send_chunk(&AudioFormat::MIC, Some(ts), &self.chunk_buf)
            .is_ok()
        {
            c.frames_sent += 1;
            c.first_chunk_at.get_or_insert_with(Instant::now);
        }
        if self.wave.len() == BARS / 2 {
            self.wave.pop_back();
        }
        self.wave.push_front(level_to_bar(f.level.rms_dbfs));
    }

    // ---- reflexes -----------------------------------------------------------------------

    /// A frame of an utterance held back: buffered, fed to the helper and to the speech
    /// detector; at the end of speech the decision (or `E` to the helper first).
    fn on_mic_gated(&mut self, f: &CaptureFrame) {
        if self.wave.len() == BARS / 2 {
            self.wave.pop_back();
        }
        self.wave.push_front(level_to_bar(f.level.rms_dbfs));
        let max = self
            .d
            .reflexes
            .as_ref()
            .map_or(Duration::from_secs(8), |r| r.max_listen);
        let Some(g) = &mut self.gate else { return };
        g.buf.extend_from_slice(&f.samples);
        g.vad.push(&f.samples);
        let fed = g.end_sent.is_some()
            || self
                .d
                .reflexes
                .as_ref()
                .is_some_and(|r| r.link.send(ReflexRequest::Audio(f.samples.to_vec())));
        if !fed {
            self.release(false, "the helper is not taking audio");
            return;
        }
        let over = g.vad.ended() || g.vad.nobody_spoke();
        if g.end_sent.is_none() && over {
            if g.result.is_some() {
                self.decide();
                return;
            }
            g.end_sent = Some(Instant::now());
            let id = g.id;
            if let Some(r) = &self.d.reflexes {
                r.link.send(ReflexRequest::End(id));
            }
            return;
        }
        if g.end_sent
            .is_some_and(|t| t.elapsed() >= REFLEX_ANSWER_WAIT)
        {
            self.release(true, "no answer from the helper");
        } else if g.end_sent.is_none() && g.started.elapsed() >= max {
            self.release(false, "still speaking");
        }
    }

    fn on_reflex(&mut self, e: ReflexEvent) {
        match e {
            ReflexEvent::Ready { .. } => {}
            ReflexEvent::Lost => {
                if let Some(g) = &self.gate {
                    let over = g.end_sent.is_some();
                    self.release(over, "the helper stopped");
                }
            }
            ReflexEvent::Result {
                id,
                endpoint,
                text,
                words,
                ms,
            } => {
                let Some(g) = &mut self.gate else { return };
                if g.id != id {
                    return;
                }
                g.result = Some((text, words, endpoint, ms));
                // Vosk's own end of speech can come first: the decision waits for the
                // detector's, so the words are checked against everything that was said.
                if g.end_sent.is_some() || g.vad.ended() || g.vad.nobody_spoke() {
                    self.decide();
                }
            }
        }
    }

    /// The rule (design section 5) on the helper's result, at the end of speech.
    fn decide(&mut self) {
        let Some(g) = &self.gate else { return };
        let Some((text, words, endpoint, ms)) = g.result.clone() else {
            return;
        };
        let (decision, threshold) = match &self.d.reflexes {
            Some(r) => (
                r.grammar
                    .as_ref()
                    .map_or(Decision::Reject("no grammar"), |gr| {
                        gr.decide_covered(&text, &words, r.threshold, &g.vad)
                    }),
                r.threshold,
            ),
            None => (Decision::Reject("no reflexes"), 1.0),
        };
        let min = reflex::grammar::min_conf(&words);
        let heard_s = g.vad.heard_ms() as f32 / 1000.0;
        let after_speech = g.vad.span().map_or(0.0, |(_, b)| heard_s - b);
        let wait_ms = g
            .end_sent
            .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
        let speech_ms = g.vad.speech_ms();
        let (verdict, command) = match &decision {
            Decision::Accept(c) => ("accepted".to_string(), serde_json::to_value(c).ok()),
            Decision::Reject(why) => (format!("rejected: {why}"), None),
            Decision::Empty => ("empty".to_string(), None),
        };
        log::info!(
            event = "reflex", text = text.as_str(), min_conf = min as f64, verdict = verdict.as_str(),
            vosk_end = endpoint, decided_after_speech_s = after_speech as f64, helper_wait_ms = wait_ms, helper_ms = ms;
            "reflex: {text:?} (min {min:.2}) {verdict}, {after_speech:.2} s after the speech (helper {wait_ms:.0} ms)"
        );
        self.sidecar = Some(serde_json::json!({
            "reflex": {
                "text": text,
                "words": words,
                "min_conf": min,
                "threshold": threshold,
                "verdict": verdict,
                "command": command,
                "vosk_end": endpoint,
                "speech_ms": speech_ms,
                "decided_after_speech_s": after_speech,
            }
        }));
        match decision {
            Decision::Accept(c) => self.act(c, &text),
            Decision::Empty if speech_ms < Vad::MIN_SPEECH_MS => {
                // Nobody spoke after the wake word (or after a question back: then quietly).
                self.gate = None;
                self.mic_idle();
                self.d.mic.request_reset();
                self.stop_recorder();
                if self
                    .conv
                    .as_ref()
                    .is_none_or(|c| c.trigger != Trigger::FollowUp)
                {
                    self.d.player.play(&self.cues.not_understood);
                }
                self.close("no speech");
                self.listen_deadline = None;
                self.go_idle();
            }
            // Sound but no words yet (the tail of the wake word, a breath, a pause before
            // speaking): the sentence has not started, so it is not over. Home Assistant
            // listens on, live, and decides when it ends.
            Decision::Empty => self.release(false, "no words yet"),
            // "metti una sveglia alle" and a pause: the sentence is not over.
            _ if unfinished(&text) => self.release(false, "sentence not finished"),
            _ => self.release(true, "not a reflex"),
        }
    }

    /// The utterance goes to Home Assistant: the pipeline starts, the buffered audio follows in
    /// one burst, then `audio-stop` (`over`) or the live stream (still speaking).
    fn release(&mut self, over: bool, why: &str) {
        let Some(g) = self.gate.take() else { return };
        if !over {
            if let Some(r) = &self.d.reflexes {
                r.link.send(ReflexRequest::End(g.id));
            }
        }
        if !self.ha_ready() {
            log::info!(event = "no_home_assistant", why = why; "not a reflex ({why}) and no Home Assistant");
            self.mic_idle();
            self.d.mic.request_reset();
            self.stop_recorder();
            self.d.player.play(&self.cues.not_understood);
            self.close("no_home_assistant");
            self.listen_deadline = None;
            self.go_idle();
            self.flash_until = Some(Instant::now() + self.cfg.offline_flash);
            self.ui(UiCommand::View(Phase::Offline));
            return;
        }
        let t = Instant::now();
        let mut ok = self.open_pipeline();
        let mut frames = 0u64;
        for (i, chunk) in g.buf.chunks(reflex::vad::FRAME).enumerate() {
            if !ok {
                break;
            }
            self.chunk_buf.clear();
            for s in chunk {
                self.chunk_buf.extend_from_slice(&s.to_le_bytes());
            }
            let ts = (i * audio::FRAME_MS as usize) as i64;
            ok = self
                .d
                .sender
                .send_chunk(&AudioFormat::MIC, Some(ts), &self.chunk_buf)
                .is_ok();
            frames += 1;
        }
        if let Some(c) = &mut self.conv {
            c.frames_sent += frames;
            c.first_chunk_at.get_or_insert(t);
        }
        log::info!(
            event = "reflex_release", why = why, audio_s = g.buf.len() as f64 / 16_000.0, over = over, burst_ms = t.elapsed().as_secs_f64() * 1000.0;
            "to Home Assistant ({why}): {:.1} s of audio in {:.0} ms{}",
            g.buf.len() as f64 / 16_000.0,
            t.elapsed().as_secs_f64() * 1000.0,
            if over { ", speech over" } else { ", then live" }
        );
        if !ok {
            self.mic_idle();
            self.stop_recorder();
            self.close("disconnected");
            self.go_idle();
            return;
        }
        if over {
            self.send(&Message::AudioStop { timestamp: None });
            if let Some(c) = &mut self.conv {
                c.audio_stop_sent = true;
            }
            self.end_listening();
        }
    }

    /// A reflex: done by the body, answered on the screen with the done sound.
    fn act(&mut self, cmd: Reflex, heard: &str) {
        self.gate = None;
        self.mic_idle();
        self.d.mic.request_reset();
        self.listen_deadline = None;
        let now = now_unix();
        // Sent after the conversation ends, so they are the screen's last word ("spegni lo
        // schermo" after the view went back home).
        let mut after: Vec<UiCommand> = Vec::new();
        // "Ripeti": how long the replay plays (the answer stays on screen as long).
        let mut replay_for: Option<Duration> = None;
        let answer = match &cmd {
            Reflex::Time => {
                let n = crate::alarms::Now::local();
                let time = format!("{:02}:{:02}", n.hour, n.minute);
                self.reflex_text("reflex_time", &[("time", &time)])
            }
            Reflex::TimerStart { seconds, name } => {
                let seconds = *seconds;
                let id = self.d.timers.start_local(name.clone(), seconds, now);
                log::info!(event = "timer_started", id = id.as_str(), seconds = seconds, name = name.as_deref().unwrap_or(""), by = "reflex"; "timer {id}: {seconds} s (reflex)");
                self.refresh_rows(true);
                let d = crate::timers::format_left(seconds as f64);
                match name {
                    Some(n) => {
                        self.reflex_text("reflex_timer_set_named", &[("duration", &d), ("name", n)])
                    }
                    None => self.reflex_text("reflex_timer_set", &[("duration", &d)]),
                }
            }
            Reflex::TimerCancel { name } => {
                let ids: Vec<String> = match name {
                    Some(n) => self.d.timers.named(n),
                    None => self.d.timers.rows(now).into_iter().map(|t| t.id).collect(),
                };
                for id in &ids {
                    self.d.timers.cancel(id);
                    log::info!(event = "timer_cancelled", id = id.as_str(), by = "reflex"; "timer {id} cancelled (reflex)");
                }
                self.refresh_rows(true);
                match (ids.len(), name) {
                    (0, Some(n)) => self.reflex_text("reflex_no_timer_named", &[("name", n)]),
                    (0, None) => self.reflex_text("reflex_no_timer", &[]),
                    (1, Some(n)) => {
                        self.reflex_text("reflex_timer_cancelled_named", &[("name", n)])
                    }
                    (1, None) => self.reflex_text("reflex_timer_cancelled", &[]),
                    (n, _) => self.reflex_text("reflex_timers_cancelled", &[("n", &n.to_string())]),
                }
            }
            Reflex::TimerLeft { name: None } => match self.d.timers.next_due() {
                Some(due) => {
                    let left = crate::timers::format_left(due - now);
                    self.reflex_text("reflex_timer_left", &[("left", &left)])
                }
                None => self.reflex_text("reflex_no_timer", &[]),
            },
            Reflex::TimerLeft { name: Some(n) } => {
                let left = self
                    .d
                    .timers
                    .named(n)
                    .iter()
                    .filter_map(|id| self.d.timers.get(id))
                    .map(|t| t.remaining(now))
                    .min_by(f64::total_cmp);
                match left {
                    Some(l) => {
                        let left = crate::timers::format_left(l);
                        self.reflex_text("reflex_timer_left_named", &[("name", n), ("left", &left)])
                    }
                    None => self.reflex_text("reflex_no_timer_named", &[("name", n)]),
                }
            }
            Reflex::TimerList => {
                let rows = self.d.timers.rows(now);
                if rows.is_empty() {
                    self.reflex_text("reflex_no_timer", &[])
                } else {
                    let items: Vec<String> = rows
                        .iter()
                        .map(|r| {
                            let key = if r.paused {
                                "reflex_timer_item_paused"
                            } else {
                                "reflex_timer_item"
                            };
                            self.reflex_text(key, &[("name", &r.name), ("left", &r.left)])
                        })
                        .collect();
                    format!("{}.", items.join("; "))
                }
            }
            Reflex::AlarmSet { hour, minute } => {
                let (hour, minute) = (*hour, *minute);
                let id = self.d.alarms.add(hour, minute, 0);
                log::info!(event = "alarm_set", id = id, days = 0, by = "reflex"; "alarm {hour:02}:{minute:02} set (reflex)");
                self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
                let time = format!("{hour:02}:{minute:02}");
                self.reflex_text("reflex_alarm_set", &[("time", &time)])
            }
            Reflex::AlarmCancel => {
                let next = self
                    .d
                    .alarms
                    .next_within(crate::alarms::Now::local(), 24 * 60)
                    .map(|(a, _)| a.clone());
                match next {
                    Some(a) if a.days == 0 => {
                        self.d.alarms.remove(a.id);
                        self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
                        self.reflex_text("reflex_alarm_cancelled", &[("time", &a.time())])
                    }
                    Some(a) => self.reflex_text("reflex_alarm_repeats", &[("time", &a.time())]),
                    None => self.reflex_text("reflex_no_alarm", &[]),
                }
            }
            Reflex::VolumeUp | Reflex::VolumeDown => {
                let dir = if cmd == Reflex::VolumeUp { 1 } else { -1 };
                // Two steps: a spoken "alza" should be heard.
                for _ in 0..2 {
                    self.on_volume(VolumeRequest::Step { dir, repeat: true });
                }
                let p = self.d.volume.as_ref().map_or(0, |v| v.percent());
                self.reflex_text("reflex_volume", &[("percent", &p.to_string())])
            }
            Reflex::VolumeSet { percent } => {
                let percent = *percent;
                self.on_volume(VolumeRequest::Set(percent.max(5)));
                let p = self.d.volume.as_ref().map_or(percent, |v| v.percent());
                self.reflex_text("reflex_volume", &[("percent", &p.to_string())])
            }
            Reflex::Silence => String::new(),
            Reflex::MusicPause | Reflex::MusicPlay | Reflex::MusicNext | Reflex::MusicPrevious => {
                use crate::airplay::Remote;
                let (remote, key) = match cmd {
                    Reflex::MusicPause => (Remote::Pause, "reflex_music_pause"),
                    Reflex::MusicPlay => (Remote::Play, "reflex_music_play"),
                    Reflex::MusicNext => (Remote::Next, "reflex_music_next"),
                    _ => (Remote::Previous, "reflex_music_previous"),
                };
                match self.d.music.as_ref().filter(|m| m.session()) {
                    Some(m) => {
                        // "Play" after Alfred's own Stop: the local mute goes too.
                        if remote == Remote::Play && m.muted() {
                            m.toggle_mute();
                        }
                        m.remote(remote);
                        self.reflex_text(key, &[])
                    }
                    None => self.reflex_text("reflex_no_music", &[]),
                }
            }
            Reflex::Repeat => match self.tape.last(Instant::now()) {
                Some(a) => {
                    let s = self.d.player.open_stream(a.format);
                    if let Err(e) = s.push(&a.pcm) {
                        log::warn!("repeat: {e}");
                    }
                    s.finish();
                    replay_for = Some(a.duration());
                    log::info!(event = "repeat", seconds = a.duration().as_secs_f64(); "repeating the last answer ({:.1} s)", a.duration().as_secs_f64());
                    let text = a.text.clone();
                    self.replay = Some(s);
                    text
                }
                None => self.reflex_text("reflex_nothing_to_repeat", &[]),
            },
            Reflex::ScreenOff => {
                after.push(UiCommand::ScreenOff);
                String::new()
            }
            Reflex::DoNotDisturb { .. } | Reflex::DoNotDisturbMorning | Reflex::GoodNight => {
                let local = local_seconds_of_day(now as i64);
                let until = match &cmd {
                    Reflex::DoNotDisturb { seconds: Some(s) } => now as u64 + s,
                    Reflex::DoNotDisturb { seconds: None } => dnd_default_until(now as i64, local),
                    _ => next_morning(now as i64, local),
                };
                self.set_dnd(Some(until));
                after.push(UiCommand::DoNotDisturb(Some(until)));
                if cmd == Reflex::GoodNight {
                    after.push(UiCommand::ScreenOff);
                    String::new()
                } else {
                    let n = crate::alarms::Now::at(until as i64);
                    let time = format!("{:02}:{:02}", n.hour, n.minute);
                    self.reflex_text("reflex_dnd_on", &[("time", &time)])
                }
            }
            Reflex::DoNotDisturbOff => {
                self.set_dnd(None);
                after.push(UiCommand::DoNotDisturb(None));
                self.reflex_text("reflex_dnd_off", &[])
            }
        };
        self.stop_recorder();
        log::info!(event = "reflex_done", heard = heard, answer = answer.as_str(); "reflex done: {heard:?} → {answer:?}");
        // Each reflex has its own sound: it is clear the body did it (Home Assistant answers aloud).
        let cue = match &cmd {
            Reflex::Time => Some(&self.cues.time),
            Reflex::TimerStart { .. } => Some(&self.cues.timer_set),
            Reflex::AlarmSet { .. } => Some(&self.cues.alarm_set),
            Reflex::TimerCancel { .. } | Reflex::AlarmCancel => Some(&self.cues.cancelled),
            // Soft and low: the screen goes dark, the evening goes quiet.
            Reflex::Silence | Reflex::ScreenOff | Reflex::GoodNight => Some(&self.cues.silence),
            Reflex::TimerLeft { .. } | Reflex::TimerList => Some(&self.cues.done),
            Reflex::DoNotDisturb { .. } | Reflex::DoNotDisturbMorning | Reflex::DoNotDisturbOff => {
                Some(&self.cues.done)
            }
            Reflex::MusicPause | Reflex::MusicPlay | Reflex::MusicNext | Reflex::MusicPrevious => {
                Some(&self.cues.done)
            }
            // The voice itself is the answer; with nothing to repeat, the done sound.
            Reflex::Repeat if replay_for.is_some() => None,
            Reflex::Repeat => Some(&self.cues.done),
            // The volume keys' own tick already plays at the new level.
            Reflex::VolumeUp | Reflex::VolumeDown | Reflex::VolumeSet { .. } => None,
        };
        if let Some(c) = cue {
            self.d.player.play(c);
        }
        if let Some(c) = &mut self.conv {
            c.heard = heard.to_string();
            c.answer = answer.clone();
            c.transcript_at = Some(Instant::now());
        }
        if answer.is_empty() && replay_for.is_none() {
            self.close("reflex");
            self.go_idle();
            for c in after {
                self.ui(c);
            }
            return;
        }
        self.ui(UiCommand::Heard(heard.to_string()));
        self.ui(UiCommand::Answer(answer));
        self.wave.clear();
        // A replay ends the view when it has played (AudioEvent::Finished); this is the
        // fallback.
        self.local_until =
            Some(Instant::now() + replay_for.map_or(LOCAL_ANSWER, |d| d + Duration::from_secs(2)));
        self.set_phase(Phase::Speaking);
        for c in after {
            self.ui(c);
        }
    }

    /// "Non disturbare" on until then, or off.
    fn set_dnd(&mut self, until: Option<u64>) {
        self.dnd_until = until;
        log::info!(event = "do_not_disturb", until = until.map_or(-1, |u| u as i64); "do not disturb {}", until.map_or("off".to_string(), |u| format!("until {u}")));
    }

    fn dnd(&self, now: f64) -> bool {
        self.dnd_until.is_some_and(|u| now < u as f64)
    }

    /// Night hours of Settings → Screen, now.
    fn night(&self) -> bool {
        self.d.settings.as_ref().is_some_and(|s| {
            let (on, start, end) = s.night();
            crate::ui::screen::is_night(crate::alarms::Now::local().hour, on, start, end)
        })
    }

    fn reflex_text(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.d
            .reflexes
            .as_ref()
            .map_or_else(|| key.to_string(), |r| r.text(key, args))
    }

    // ---- Home Assistant -----------------------------------------------------------------

    fn on_server(&mut self, e: ServerEvent) {
        match e {
            ServerEvent::Connected(peer) => {
                self.ha = HaLink::Connected {
                    paused: false,
                    ready: false,
                };
                log::info!(event = "connected", peer = peer.to_string().as_str(); "connected from {peer}");
            }
            ServerEvent::Disconnected(peer, reason) => {
                let was_ready = matches!(self.ha, HaLink::Connected { ready: true, .. });
                self.ha = HaLink::Disconnected;
                let reason = match reason {
                    Disconnect::Io(s) => format!("io: {s}"),
                    r => format!("{r:?}").to_lowercase(),
                };
                if was_ready || self.conv.is_some() {
                    log::info!(event = "disconnected", peer = peer.to_string().as_str(), reason = reason.as_str(); "{peer} disconnected ({reason})");
                } else {
                    log::debug!("{peer} disconnected ({reason}) before run-satellite (a probe)");
                }
                if self.gate.is_some() || self.local_until.is_some() {
                    // A reflex in progress does not need Home Assistant.
                    return;
                }
                if self.conv.is_some() {
                    self.stop_recorder();
                    self.close("disconnected");
                }
                self.go_idle();
            }
            ServerEvent::Rejected(peer) => log::debug!("rejected {peer}: not in satellite.allowed"),
            ServerEvent::Message(m) => self.on_message(m),
        }
    }

    fn on_message(&mut self, m: Message) {
        match m {
            Message::Describe | Message::Ping { .. } | Message::Pong { .. } | Message::Info(_) => {
                log::trace!("{}", m.kind())
            }
            Message::RunSatellite => {
                if let HaLink::Connected { .. } = self.ha {
                    self.ha = HaLink::Connected {
                        paused: false,
                        ready: true,
                    };
                }
                log::info!(event = "run_satellite"; "run-satellite");
                if matches!(self.phase, Phase::Offline | Phase::Home) {
                    self.go_idle();
                }
            }
            Message::PauseSatellite => {
                if let HaLink::Connected { ready, .. } = self.ha {
                    self.ha = HaLink::Connected {
                        paused: true,
                        ready,
                    };
                }
                log::info!(event = "pause_satellite"; "pause-satellite");
                if self.conv.is_some() {
                    self.stop_recorder();
                    self.close("paused");
                }
                self.go_idle();
            }
            Message::Transcribe { language } => {
                log::info!(event = "transcribe", language = language.as_deref().unwrap_or(""); "pipeline language {}", language.as_deref().unwrap_or("?"))
            }
            Message::VoiceStarted { .. } => log::debug!("voice-started"),
            Message::VoiceStopped { .. } => {
                log::debug!("voice-stopped");
                if self.phase == Phase::Listening
                    && self.conv.as_ref().is_some_and(|c| !c.discard_answer)
                {
                    self.end_listening();
                }
            }
            Message::Transcript(t) => self.on_transcript(t.text),
            Message::Synthesize(s) => {
                if let Some(c) = &mut self.conv {
                    c.synth_at = Some(Instant::now());
                    if !c.discard_answer {
                        c.answer = s.text.clone();
                        crate::brain_link::tell("said", &s.text);
                        self.ui(UiCommand::Answer(s.text));
                    }
                }
            }
            Message::AudioStart { format, .. } => self.on_audio_start(format),
            Message::AudioChunk { audio, .. } => self.on_audio_chunk(&audio),
            Message::AudioStop { .. } => self.on_audio_stop(),
            Message::Error { text, code } => self.on_error(text, code),
            Message::TimerStarted(t) => {
                let now = now_unix();
                self.d.timers.start(&t, now);
                log::info!(event = "timer_started", id = t.id.as_str(), seconds = t.total_seconds, name = t.name.as_deref().unwrap_or(""); "timer {} started: {} s", t.id, t.total_seconds);
                self.refresh_rows(true);
            }
            Message::TimerUpdated(u) => {
                let created = self.d.timers.update(&u, now_unix());
                log::info!(event = "timer_updated", id = u.id.as_str(), seconds = u.total_seconds, active = u.is_active, created = created; "timer {} updated: {} s left{}", u.id, u.total_seconds, if u.is_active { "" } else { " (paused)" });
                self.refresh_rows(true);
            }
            Message::TimerCancelled { id } => {
                let removed = self.d.timers.remove(&id).is_some();
                log::info!(event = "timer_cancelled", id = id.as_str(), known = removed; "timer {id} cancelled");
                if let Some(i) = self.ringing.iter().position(|r| *r == id) {
                    self.ringing.remove(i);
                    if self.ringing.is_empty() {
                        self.stop_bell();
                        if self.phase == Phase::Ringing {
                            self.go_idle();
                        }
                    } else {
                        self.show_ringing();
                    }
                }
                self.refresh_rows(true);
            }
            Message::TimerFinished { id } => {
                if self.d.timers.was_rung(&id) {
                    if !self.ringing.contains(&id) {
                        self.d.timers.remove(&id);
                    }
                    log::info!(event = "timer_finished", id = id.as_str(), rung_locally = true; "timer {id} finished by Home Assistant after the local ring");
                } else {
                    log::info!(event = "timer_finished", id = id.as_str(), rung_locally = false; "timer {id} finished by Home Assistant: ringing");
                    if self.d.timers.get(&id).is_none() {
                        let t = wyoming::TimerStarted {
                            id: id.clone(),
                            total_seconds: 0,
                            name: None,
                            start_hours: None,
                            start_minutes: None,
                            start_seconds: None,
                        };
                        self.d.timers.start(&t, now_unix());
                    }
                    if self.phase.in_conversation() {
                        // Rung by the next tick after the conversation (due now).
                    } else {
                        self.ring(vec![id]);
                    }
                }
                self.refresh_rows(true);
            }
            Message::Played => log::debug!("played from Home Assistant (ignored)"),
            Message::Unknown(e) => log::debug!("ignored message {}", e.kind),
            other => log::debug!("ignored message {}", other.kind()),
        }
    }

    fn on_transcript(&mut self, text: String) {
        let Some(c) = &mut self.conv else {
            log::debug!("transcript with no conversation");
            return;
        };
        c.transcript_at = Some(Instant::now());
        c.heard = text.clone();
        log::info!(event = "transcript", text = text.as_str(); "heard: {text}");
        crate::brain_link::tell("heard", &text);
        if c.discard_answer {
            return;
        }
        if self.phase == Phase::Listening {
            self.end_listening();
        }
        // An empty transcript is followed by an `error`: wait for it.
        self.ui(UiCommand::Heard(text));
    }

    fn on_audio_start(&mut self, format: AudioFormat) {
        let sf = StreamFormat {
            rate: format.rate,
            width: format.width,
            channels: format.channels,
        };
        match &mut self.conv {
            Some(c) if c.discard_answer => {
                c.audio_start_at = Some(Instant::now());
                c.answer_format = Some(format);
                self.tape.cancel();
                log::debug!("answer of a stopped conversation: swallowed");
            }
            Some(c) if c.trigger != Trigger::Announcement => {
                if c.stream.is_some() {
                    log::debug!("second audio-start in one answer: ignored");
                    return;
                }
                c.audio_start_at = Some(Instant::now());
                c.answer_format = Some(format);
                c.stream = Some(self.d.player.open_stream(sf));
                self.tape.start(sf);
                self.mic_idle();
                self.listen_deadline = None;
                self.think_deadline = None;
                if self.phase == Phase::Listening {
                    self.stop_recorder();
                }
                self.wave.clear();
                self.set_phase(Phase::Speaking);
            }
            Some(_) => log::debug!("audio-start during an announcement: ignored"),
            None => {
                // An announcement (audio outside a pipeline).
                let id = self.next_id;
                self.next_id += 1;
                let mut c = Conversation::new(id, Trigger::Announcement);
                c.audio_start_at = Some(Instant::now());
                c.answer_format = Some(format);
                if self.dnd(now_unix()) {
                    // Swallowed like a stopped answer: `played` at its audio-stop.
                    c.discard_answer = true;
                    log::info!(event = "announcement", id = id, muted = true; "announcement {id}: muted (do not disturb)");
                    self.conv = Some(c);
                    return;
                }
                c.stream = Some(self.d.player.open_stream(sf));
                self.tape.start(sf);
                log::info!(event = "announcement", id = id; "announcement {id}");
                self.conv = Some(c);
                self.ui(UiCommand::Heard(String::new()));
                self.ui(UiCommand::Answer(String::new()));
                if self.phase != Phase::Ringing {
                    self.wave.clear();
                    self.set_phase(Phase::Speaking);
                }
            }
        }
    }

    fn on_audio_chunk(&mut self, pcm: &[u8]) {
        let Some(c) = &mut self.conv else {
            self.answer_dropped += 1;
            log::debug!(
                "audio-chunk with no answer: dropped ({} so far)",
                self.answer_dropped
            );
            return;
        };
        c.answer_bytes += pcm.len() as u64;
        let Some(s) = &c.stream else { return };
        self.tape.push(pcm);
        if let Err(e) = s.push(pcm) {
            log::warn!("answer audio: {e}");
        }
        // Playback starts once 100 ms are buffered (the audio crate's rule).
        if c.first_sample_at.is_none() && c.answer_seconds() >= 0.1 {
            c.first_sample_at = Some(Instant::now());
        }
    }

    fn on_audio_stop(&mut self) {
        let Some(c) = &mut self.conv else {
            log::debug!("audio-stop with no answer");
            return;
        };
        let now = Instant::now();
        c.audio_stop_at = Some(now);
        if c.discard_answer || c.stream.is_none() {
            self.tape.cancel();
        } else {
            self.tape.commit(&c.answer, now);
        }
        if c.first_sample_at.is_none() && c.answer_bytes > 0 {
            c.first_sample_at = Some(now);
        }
        match &c.stream {
            Some(s) => {
                s.finish();
                // Finished normally comes when the last sample left the card; this is the
                // fallback if it never does.
                self.speak_deadline = Some(now + s.queued() + Duration::from_secs(3));
            }
            None => {
                // Swallowed (Stop was pressed) or no audio-start: acknowledge at once.
                let discard = c.discard_answer;
                self.send_played();
                self.close(if discard { "stopped" } else { "played" });
                if self.phase.in_conversation() {
                    self.go_idle();
                }
            }
        }
    }

    fn on_error(&mut self, text: String, code: Option<String>) {
        let code = code.unwrap_or_else(|| "error".into());
        log::warn!(event = "pipeline_error", code = code.as_str(), text = text.as_str(); "Home Assistant: {text} ({code})");
        match &self.conv {
            Some(c) if c.discard_answer => self.close("stopped"),
            Some(_) if self.phase.in_conversation() || self.phase == Phase::Ringing => {
                let outcome = format!("error:{code}");
                self.fail(&outcome);
            }
            _ => {}
        }
    }

    // ---- audio ------------------------------------------------------------------------------

    fn on_audio(&mut self, e: AudioEvent) {
        match e {
            AudioEvent::Finished(id) => {
                let is_answer = self
                    .conv
                    .as_ref()
                    .and_then(|c| c.stream.as_ref())
                    .is_some_and(|s| s.id() == id);
                if is_answer {
                    // A question back ("Cosa devo ricordarti?"): listen again without the wake
                    // word, as Home Assistant would ask a voice satellite of its own.
                    let follow = self.conv.as_ref().is_some_and(|c| {
                        !c.discard_answer
                            && c.trigger != Trigger::Announcement
                            && c.answer.trim_end().ends_with('?')
                    });
                    if let Some(c) = &mut self.conv {
                        c.stream = None;
                    }
                    self.send_played();
                    self.close("played");
                    if self.phase == Phase::Speaking {
                        self.go_idle();
                        if follow {
                            log::info!(event = "follow_up"; "the answer was a question: listening again");
                            self.start_conversation(Trigger::FollowUp);
                        }
                    }
                } else if self.bell == Some(id) {
                    self.bell = None;
                    self.clear_ringing();
                } else if self.replay.as_ref().is_some_and(|s| s.id() == id) {
                    self.replay = None;
                    if self.phase == Phase::Speaking && self.local_until.take().is_some() {
                        self.close("reflex");
                        self.go_idle();
                    }
                }
            }
            AudioEvent::Stopped(id) => log::debug!("source {} stopped", id.0),
            AudioEvent::Underrun { total } => {
                log::warn!(event = "underrun", total = total; "playback underrun ({total} so far)")
            }
            AudioEvent::Overrun { total } => {
                log::warn!(event = "overrun", total = total; "capture overrun ({total} so far)")
            }
            AudioEvent::CaptureDropped { total } => {
                log::warn!(event = "capture_dropped", total = total; "capture frames dropped ({total} so far)")
            }
            AudioEvent::Error(e) => {
                log::error!(event = "audio_error", error = e.as_str(); "audio: {e}");
                self.summary.exit_code = 2;
                self.quit = true;
            }
        }
    }

    // ---- timers -------------------------------------------------------------------------

    fn ring_due(&mut self) {
        // Alarms become timers that are already over (they ring once the conversation ends).
        let rung = self.d.alarms.due(crate::alarms::Now::local());
        if !rung.is_empty() {
            for a in &rung {
                log::info!(event = "alarm_rung", id = a.id; "alarm {} rings", a.time());
                let id = self.d.timers.start_local(Some(a.time()), 0, now_unix());
                self.alarm_ids.push(id);
            }
            self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
        }
        if self.phase.in_conversation() {
            return;
        }
        let due = self.d.timers.due(now_unix());
        if !due.is_empty() {
            self.ring(due);
        }
    }

    fn ring(&mut self, ids: Vec<String>) {
        let connected = self.ha_ready();
        for id in ids {
            self.d.timers.mark_rung(&id);
            log::info!(event = "timer_rung", id = id.as_str(), connected = connected; "timer {id} rings");
            self.summary.rung.push(id.clone());
            if !self.ringing.contains(&id) {
                self.ringing.push(id);
            }
        }
        if self.bell.is_none() {
            self.bell = Some(self.d.player.play_repeated(
                &self.cues.bell,
                self.cfg.bell_repeats.max(1),
                self.cfg.bell_gap,
            ));
        }
        self.show_ringing();
        self.refresh_rows(true);
        self.set_phase(Phase::Ringing);
    }

    fn show_ringing(&self) {
        let cards = self
            .ringing
            .iter()
            .map(|id| {
                (
                    self.d
                        .timers
                        .get(id)
                        .map_or_else(|| "Timer".into(), |t| t.label()),
                    if self.alarm_ids.contains(id) {
                        "alarm"
                    } else {
                        "timer"
                    }
                    .to_string(),
                )
            })
            .collect();
        self.ui(UiCommand::Messages(cards));
    }

    fn stop_bell(&mut self) {
        if let Some(b) = self.bell.take() {
            self.d.player.stop(b);
        }
        self.clear_ringing();
    }

    /// The bell is over (stopped or played out): the rung timers are done.
    fn clear_ringing(&mut self) {
        if let Some(b) = self.bell.take() {
            self.d.player.stop(b);
        }
        for id in std::mem::take(&mut self.ringing) {
            self.alarm_ids.retain(|a| *a != id);
            self.d.timers.remove(&id);
            log::info!(event = "timer_removed", id = id.as_str(); "timer {id} done");
        }
        self.ui(UiCommand::Messages(Vec::new()));
        self.refresh_rows(true);
        if self.phase == Phase::Ringing {
            self.go_idle();
        }
    }

    fn refresh_rows(&mut self, force: bool) {
        let rows = self.d.timers.rows(now_unix());
        if force || rows != self.last_rows {
            self.last_rows = rows.clone();
            self.ui(UiCommand::Timers(rows));
        }
    }

    // ---- the screen -----------------------------------------------------------------------

    fn on_ui(&mut self, e: UiEvent) {
        log::debug!("ui: {e:?}");
        match e {
            UiEvent::Talk => match self.phase {
                Phase::Home | Phase::Offline | Phase::Error => {
                    self.start_conversation(Trigger::Talk)
                }
                Phase::Ringing => {
                    self.stop_bell();
                    self.start_conversation(Trigger::Talk);
                }
                _ => {}
            },
            UiEvent::Stop => self.on_stop(),
            UiEvent::MessageOk(_) => {
                if !self.ringing.is_empty() {
                    log::info!(event = "bell_stopped", by = "ok"; "bell stopped (OK)");
                    self.stop_bell();
                }
            }
            UiEvent::Language(code) => {
                if let Some(s) = &self.d.settings {
                    s.save_language(&code);
                }
                if let Some(r) = &mut self.d.reflexes {
                    r.set_language(&code);
                }
            }
            UiEvent::HomeAssistant => log::info!("Home Assistant button: dashboards are M6"),
            UiEvent::Suspend => self.suspend(),
            UiEvent::ControlTile(k) => self.on_control_tile(&k),
            UiEvent::MusicToggle => self.on_music_toggle(),
            UiEvent::MusicNext => self.music_remote(crate::airplay::Remote::Next),
            UiEvent::MirrorStop => {
                if let Some(m) = &self.d.music {
                    m.drop_session();
                }
            }
            UiEvent::MusicPrevious => self.music_remote(crate::airplay::Remote::Previous),
            // The screen's tile: as the reflex would (the core's side of it, the sounds, is
            // the reflexes'; the screen hears it back as UiCommand::DoNotDisturb).
            UiEvent::DoNotDisturb(until) => {
                log::info!(event = "dnd", on = until.is_some(), by = "control"; "do not disturb {}", if until.is_some() { "on" } else { "off" });
                self.ui(UiCommand::DoNotDisturb(until));
            }
            UiEvent::TimerStart(secs) => {
                let id = self.d.timers.start_local(None, secs, now_unix());
                log::info!(event = "timer_started", id = id.as_str(), seconds = secs, by = "touch"; "timer {id}: {secs} s (touch)");
                self.refresh_rows(true);
            }
            UiEvent::TimerPause(id) => {
                self.d.timers.toggle_pause(&id, now_unix());
                self.refresh_rows(true);
            }
            UiEvent::TimerCancel(id) => {
                if self.d.timers.cancel(&id).is_some() {
                    log::info!(event = "timer_cancelled", id = id.as_str(), by = "touch"; "timer {id} cancelled (touch)");
                }
                self.refresh_rows(true);
            }
            UiEvent::Snooze => {
                if self.phase == Phase::Ringing {
                    let name = self
                        .ringing
                        .first()
                        .and_then(|id| self.d.timers.get(id))
                        .map(|t| t.label());
                    self.stop_bell();
                    let id = self.d.timers.start_local(name, 60, now_unix());
                    log::info!(event = "timer_snoozed", id = id.as_str(); "one more minute: {id}");
                    self.refresh_rows(true);
                }
            }
            UiEvent::AlarmSave(h, m, days) => {
                let id = self.d.alarms.add(h, m, days);
                log::info!(event = "alarm_set", id = id, days = days; "alarm {h:02}:{m:02} set");
                self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
            }
            UiEvent::AlarmToggle(id, on) => {
                self.d.alarms.set_on(id, on);
                self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
            }
            UiEvent::Notice(on) => {
                self.noticing = on;
                let stop = self.stop_wanted(self.phase);
                self.d.mic.set_stop_listening(stop);
            }
            UiEvent::AlarmDelete(id) => {
                self.d.alarms.remove(id);
                self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
            }
            UiEvent::Tap => {
                // Not over an answer, the bell or a stream (the speaker is busy then).
                if !self.speaker_busy() && !matches!(self.phase, Phase::Speaking | Phase::Ringing) {
                    self.d.player.play(&self.cues.tap);
                }
            }
            UiEvent::Home => {
                if !self.speaker_busy() && !matches!(self.phase, Phase::Speaking | Phase::Ringing) {
                    self.d.player.play(&self.cues.home);
                }
            }
            UiEvent::Success => {
                if !self.speaker_busy() && !matches!(self.phase, Phase::Speaking | Phase::Ringing) {
                    self.d.player.play(&self.cues.success);
                }
            }
            UiEvent::Reminder => {
                if self.dnd(now_unix()) {
                    log::info!("card sound muted (do not disturb)");
                } else if !self.speaker_busy() {
                    self.d.player.play(&self.cues.reminder);
                }
            }
            UiEvent::Screenshot => {
                let ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                // Keep the last 20 (SIGUSR2 on the device can be sent at any time).
                if let Ok(rd) = std::fs::read_dir(&self.cfg.screenshot_dir) {
                    let mut old: Vec<PathBuf> = rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| {
                            p.file_name()
                                .is_some_and(|n| n.to_string_lossy().starts_with("home-companion-"))
                        })
                        .collect();
                    old.sort();
                    let extra = old.len().saturating_sub(19);
                    for p in old.into_iter().take(extra) {
                        let _ = std::fs::remove_file(p);
                    }
                }
                self.ui(UiCommand::Screenshot(
                    self.cfg
                        .screenshot_dir
                        .join(format!("home-companion-{ms}.png")),
                ));
            }
        }
    }

    /// The whole device to sleep (suspend.rs), from the home screen only. Blocks the core
    /// until the device is back: nothing runs while it sleeps anyway.
    fn suspend(&mut self) {
        if !matches!(self.phase, Phase::Home | Phase::Offline) {
            log::info!("suspend: not now ({})", self.phase.name());
            return;
        }
        let now = now_unix();
        let next = self.d.timers.next_due();
        let alarm = crate::suspend::alarm_in(now, next);
        let dry = crate::suspend::dry_run();
        log::info!(
            event = "suspend", rtc_alarm_s = alarm.map_or(-1, |a| a as i64), dry_run = dry;
            "suspend: going to sleep{}{}",
            alarm.map_or(String::new(), |a| format!(", RTC alarm in {a} s for the next timer")),
            if dry { " (dry run)" } else { "" }
        );
        self.ui(UiCommand::Suspending);
        // Let the screen go dark first.
        std::thread::sleep(Duration::from_millis(400));
        match crate::suspend::suspend(alarm, dry) {
            Ok(slept) => {
                log::info!(event = "resume", slept_s = slept.as_secs_f64(); "resume: slept {:.0} s", slept.as_secs_f64())
            }
            Err(e) => log::error!("suspend: {e}"),
        }
        if dry {
            std::thread::sleep(Duration::from_secs(2));
        }
        // The key that woke the device is not a volume step.
        crate::ui::keycombo::resumed_shared();
        self.ui(UiCommand::Resumed);
        self.refresh_rows(true);
    }

    fn on_stop(&mut self) {
        log::info!(event = "stop", phase = self.phase.name(); "Stop pressed in {}", self.phase.name());
        if self.phase == Phase::Listening && self.gate.is_some() {
            // Held back for the reflexes: Home Assistant never heard of it.
            if let (Some(g), Some(r)) = (self.gate.take(), &self.d.reflexes) {
                r.link.send(ReflexRequest::End(g.id));
            }
            self.mic_idle();
            self.d.mic.request_reset();
            self.stop_recorder();
            self.close("stopped");
            self.listen_deadline = None;
            self.go_idle();
            return;
        }
        if self.phase == Phase::Speaking && self.local_until.take().is_some() {
            if let Some(s) = self.replay.take() {
                s.abort();
            }
            self.close("stopped");
            self.go_idle();
            return;
        }
        match self.phase {
            Phase::Listening => {
                self.mic_idle();
                self.d.mic.request_reset();
                // Home Assistant ends speech-to-text ("Client requested pipeline to stop").
                self.send(&Message::AudioStop { timestamp: None });
                self.stop_recorder();
                if let Some(c) = &mut self.conv {
                    c.discard_answer = true;
                    c.audio_stop_sent = true;
                }
                self.listen_deadline = None;
                self.think_deadline = Some(Instant::now() + self.cfg.think_timeout);
                self.go_idle();
            }
            Phase::Thinking => {
                // The pipeline cannot be cancelled (design section 3): the answer is swallowed.
                // Before the transcript, our microphone stream is also ended formally with
                // `audio-stop` (Home Assistant ignores it once its own end of speech came).
                let end_stream = self
                    .conv
                    .as_ref()
                    .is_some_and(|c| !c.audio_stop_sent && c.transcript_at.is_none());
                if end_stream {
                    self.send(&Message::AudioStop { timestamp: None });
                }
                if let Some(c) = &mut self.conv {
                    c.discard_answer = true;
                    c.audio_stop_sent |= end_stream;
                }
                self.go_idle();
            }
            Phase::Speaking => {
                if let Some(c) = &mut self.conv {
                    if let Some(s) = c.stream.take() {
                        s.abort();
                    }
                }
                self.send_played();
                self.close("stopped");
                self.go_idle();
            }
            Phase::Ringing => self.stop_bell(),
            Phase::Home | Phase::Offline => {
                if let Some(m) = self.d.music.as_ref().filter(|m| m.playing()) {
                    m.mute();
                }
            }
            _ => {}
        }
    }

    // ---- music and the control center ----------------------------------------------------

    /// Back to waiting for the wake word, or to nothing while the microphone is switched off.
    fn mic_idle(&self) {
        self.d.mic.set(if self.mic_muted {
            MicMode::Off
        } else {
            MicMode::Wake
        });
    }

    fn send_control(&self) {
        let airplay = self.d.music.as_ref().filter(|_| self.airplay_on).map(|_| {
            self.now_playing
                .as_ref()
                .filter(|n| n.playing && !n.source.is_empty())
                .map(|n| n.source.clone())
        });
        self.ui(UiCommand::Control {
            airplay,
            mic_on: !self.mic_muted,
        });
    }

    fn on_control_tile(&mut self, key: &str) {
        match key {
            "mic" => {
                self.mic_muted = !self.mic_muted;
                log::info!(event = "mic_switch", on = !self.mic_muted; "microphone {}", if self.mic_muted { "off" } else { "on" });
                if let Some(s) = &self.d.settings {
                    s.save_mic_muted(self.mic_muted);
                }
                if !self.phase.in_conversation() {
                    self.mic_idle();
                    self.d.mic.request_reset();
                }
            }
            "airplay" => {
                let Some(m) = &self.d.music else { return };
                self.airplay_on = !self.airplay_on;
                m.set_enabled(self.airplay_on);
                if let Some(s) = &self.d.settings {
                    s.save_airplay_on(self.airplay_on);
                }
                if !self.airplay_on {
                    self.now_playing = None;
                    self.ui(UiCommand::Music(None));
                }
            }
            _ => return,
        }
        self.send_control();
    }

    fn music_remote(&mut self, cmd: crate::airplay::Remote) {
        if let Some(m) = &self.d.music {
            m.remote(cmd);
        }
    }

    /// Pause/play: the sender's own (the track stops there too, the screen follows its
    /// metadata). A local mute only while Alfred has muted it, or if the sender has no remote.
    fn on_music_toggle(&mut self) {
        let Some(m) = &self.d.music else { return };
        if !m.muted() {
            m.remote(crate::airplay::Remote::PlayPause);
            return;
        }
        self.music_mute_toggle();
    }

    fn music_mute_toggle(&mut self) {
        let Some(m) = &self.d.music else { return };
        let muted = m.toggle_mute();
        if let Some(n) = &mut self.now_playing {
            n.playing = !muted;
        }
        if let Some(n) = self.now_playing.clone() {
            self.ui(UiCommand::Music(Some(n)));
        }
        self.send_control();
    }

    fn on_music(&mut self, e: crate::airplay::MusicEvent) {
        match e.clone() {
            crate::airplay::MusicEvent::Now(mut n) => {
                if self.d.music.as_ref().is_some_and(|m| m.muted()) {
                    n.playing = false;
                }
                self.now_playing = Some(n.clone());
                self.ui(UiCommand::Music(Some(n.clone())));
                log::info!(event = "airplay_now", playing = n.playing, title = n.title.as_str(), artist = n.artist.as_str(), source = n.source.as_str(); "AirPlay: {} \"{}\" {} ({})", if n.playing { "playing" } else { "paused" }, n.title, n.artist, n.source)
            }
            crate::airplay::MusicEvent::Volume(p) => {
                // The sender's slider is the volume: same steps, bar and saving as the keys.
                log::info!(event = "airplay_volume", percent = p; "AirPlay: volume {p}% from the sender");
                self.on_volume(VolumeRequest::Exact(p));
                return;
            }
            crate::airplay::MusicEvent::RemoteFailed(cmd) => {
                // No remote on this sender: pause/play becomes a local mute.
                let muted = self.d.music.as_ref().is_some_and(|m| m.muted());
                match cmd {
                    crate::airplay::Remote::PlayPause => self.music_mute_toggle(),
                    crate::airplay::Remote::Pause if !muted => self.music_mute_toggle(),
                    crate::airplay::Remote::Play if muted => self.music_mute_toggle(),
                    _ => {}
                }
                return;
            }
            crate::airplay::MusicEvent::MirrorAspect(a) => {
                self.ui(UiCommand::MirrorAspect(a));
                return;
            }
            crate::airplay::MusicEvent::Mirror(m) => {
                // The mirroring page is wired with the screen (ui/mirror.rs).
                self.ui(UiCommand::MirrorVideo(m));
                return;
            }
            crate::airplay::MusicEvent::Gone => {
                self.now_playing = None;
                self.ui(UiCommand::Music(None));
                log::info!(event = "airplay_now", playing = false; "AirPlay: session over")
            }
        }
        self.send_control();
    }

    // ---- volume ---------------------------------------------------------------------------

    /// Something else is using the speaker (or the microphone is streaming): no beep.
    fn speaker_busy(&self) -> bool {
        self.phase.in_conversation()
            || self.bell.is_some()
            || self.replay.is_some()
            || self.conv.as_ref().is_some_and(|c| c.stream.is_some())
    }

    fn on_volume(&mut self, r: VolumeRequest) {
        let Some(v) = &mut self.d.volume else {
            log::debug!("volume {r:?}: no volume control");
            return;
        };
        let changed = v.apply(r, Instant::now());
        let percent = v.percent();
        self.ui(UiCommand::Volume(percent));
        // A beep at the new level; also at a fresh press at the end of the range (nothing
        // changed, but the press is heard), never for Home Assistant's own changes.
        let fresh = matches!(r, VolumeRequest::Step { repeat: false, .. });
        let is_key = matches!(r, VolumeRequest::Step { .. });
        // With AirPlay playing the keys also step the sender's slider, which sends its new
        // value back (`MusicEvent::Volume`): the phone and the device show the same volume.
        if let VolumeRequest::Step { dir, .. } = r {
            if let Some(m) = self.d.music.as_ref().filter(|m| m.playing()) {
                m.remote(if dir > 0 {
                    crate::airplay::Remote::VolumeUp
                } else {
                    crate::airplay::Remote::VolumeDown
                });
            }
        }
        if is_key && (changed || fresh) && !self.speaker_busy() {
            if let Some(b) = self.volume_beep.take() {
                self.d.player.stop(b);
            }
            self.volume_beep = Some(self.d.player.play(&self.cues.volume));
        }
    }

    /// Saves the volume a second after the last change (`quiet` = 0: now).
    fn save_volume(&mut self, quiet: Duration) {
        let Some(v) = &mut self.d.volume else { return };
        if let Some(p) = v.take_unsaved(Instant::now(), quiet) {
            if let Some(s) = &self.d.settings {
                s.save_volume(p);
            }
        }
    }

    // ---- clocks ---------------------------------------------------------------------------

    fn tick(&mut self) {
        let now = Instant::now();
        self.ring_due();
        self.refresh_rows(false);
        let unix = now_unix();
        if self.dnd_until.is_some() && !self.dnd(unix) {
            self.set_dnd(None);
            self.ui(UiCommand::DoNotDisturb(None));
        }
        self.tape.expire(now);
        if self.d.timers.in_final_seconds(unix)
            && final_tick_allowed(
                self.phase,
                self.speaker_busy(),
                self.dnd(unix),
                self.night(),
            )
        {
            self.d.player.play(&self.cues.tick);
        }
        self.save_volume(Duration::from_secs(1));
        if self.phase == Phase::Error && self.error_until.is_some_and(|t| now >= t) {
            self.error_until = None;
            self.go_idle();
        }
        if self.flash_until.is_some_and(|t| now >= t) {
            self.flash_until = None;
            self.ui(UiCommand::View(self.phase));
        }
        if self.local_until.is_some_and(|t| now >= t) {
            self.local_until = None;
            if let Some(s) = self.replay.take() {
                s.abort();
            }
            if self.phase == Phase::Speaking {
                self.close("reflex");
                self.go_idle();
            }
        }
        if self.phase == Phase::Listening
            && self.gate.is_some()
            && self.listen_deadline.is_some_and(|t| now >= t)
        {
            self.release(false, "listening too long");
        }
        if self.phase == Phase::Listening && self.listen_deadline.is_some_and(|t| now >= t) {
            log::warn!(event = "timeout", stage = "listening"; "no answer from Home Assistant while listening");
            self.send(&Message::AudioStop { timestamp: None });
            self.fail("timeout");
        }
        if self.think_deadline.is_some_and(|t| now >= t) {
            self.think_deadline = None;
            match &self.conv {
                Some(c) if c.discard_answer => self.close("stopped"),
                Some(_) if self.phase == Phase::Thinking => {
                    log::warn!(event = "timeout", stage = "thinking"; "no answer from Home Assistant");
                    self.fail("timeout");
                }
                _ => {}
            }
        }
        if self.speak_deadline.is_some_and(|t| now >= t) {
            self.speak_deadline = None;
            if self.conv.as_ref().is_some_and(|c| c.stream.is_some()) {
                log::warn!("the answer's end was never reported by the audio: played sent anyway");
                if let Some(c) = &mut self.conv {
                    c.stream = None;
                }
                self.send_played();
                self.close("played");
                if self.phase == Phase::Speaking {
                    self.go_idle();
                }
            }
        }
        if now.duration_since(self.last_stats) >= self.cfg.stats_every {
            self.stats_line();
        }
    }

    fn wave_tick(&mut self) {
        let bars: Vec<f32> = match self.phase {
            Phase::Speaking => {
                let lvl = self.d.player.output_level();
                if self.wave.len() == BARS / 2 {
                    self.wave.pop_back();
                }
                self.wave.push_front(level_to_bar(lvl.rms_dbfs));
                mirror(&self.wave)
            }
            Phase::Listening => mirror(&self.wave),
            _ => thinking_wave(self.wave_t0.elapsed().as_secs_f64()),
        };
        self.ui(UiCommand::Wave(bars));
    }

    fn stats_line(&mut self) {
        let wall = self.last_stats.elapsed().as_secs_f64().max(1e-3);
        self.last_stats = Instant::now();
        let cpu = audio::process_cpu_seconds();
        let cpu_pct = (cpu - self.last_cpu) / wall * 100.0;
        self.last_cpu = cpu;
        let a = self.d.audio.as_ref().map(|a| a.stats()).unwrap_or_default();
        let w = self
            .d
            .wake_stats
            .lock()
            .ok()
            .and_then(|g| *g)
            .unwrap_or_default();
        log::info!(
            target: "home_companion::stats",
            event = "stats",
            phase = self.phase.name(),
            connected = self.ha_ready(),
            cpu_pct = (cpu_pct * 100.0).round() / 100.0,
            rss_mb = rss_mb(),
            underruns = a.underruns,
            overruns = a.overruns,
            recoveries = a.recoveries,
            capture_dropped = a.capture_dropped,
            playback_cpu_s = a.playback_cpu_s,
            capture_cpu_s = a.capture_cpu_s,
            queued_ms = a.queued_ms.unwrap_or(-1.0),
            pcm_open = a.pcm_open,
            wake_invocations = w.invocations,
            wake_detections = w.detections,
            wake_cpu_s = w.cpu_s,
            wake_audio_s = w.samples as f64 / 16000.0,
            timers = self.d.timers.len() as u64;
            "stats"
        );
    }

    fn shutdown(&mut self) {
        if self.conv.is_some() {
            self.stop_recorder();
            self.close("shutdown");
        }
        self.d.mic.set(MicMode::Off);
        self.save_volume(Duration::ZERO);
        self.stats_line();
        if let Some(m) = self.d.music.take() {
            m.shutdown();
        }
        if let Some(a) = self.d.audio.take() {
            a.stop();
        }
        if let Some(s) = self.d.server.take() {
            s.shutdown();
        }
        log::info!(event = "core_stopped"; "core stopped");
    }
}

/// Seconds since the local midnight at `t` (seconds since the epoch).
fn local_seconds_of_day(t: i64) -> i64 {
    // SAFETY: localtime_r only writes into the struct we pass.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = t as libc::time_t;
        libc::localtime_r(&tt, &mut tm);
        tm
    };
    tm.tm_hour as i64 * 3600 + tm.tm_min as i64 * 60 + tm.tm_sec as i64
}

/// The morning do not disturb lasts until (local time).
const MORNING_HOUR: i64 = 7;

/// The next 07:00 after `now`, given the local time of day `local` (seconds since midnight).
/// A daylight saving change that night moves it by an hour: harmless for "until the morning".
fn next_morning(now: i64, local: i64) -> u64 {
    let at = now - local + MORNING_HOUR * 3600;
    (if at <= now { at + 86_400 } else { at }) as u64
}

/// "Non disturbare" without a duration: in the evening and at night (21:00-07:00) until the
/// morning, as one says it before going to bed; by day for an hour, so it is not forgotten on.
fn dnd_default_until(now: i64, local: i64) -> u64 {
    let hour = local / 3600;
    if !(MORNING_HOUR..21).contains(&hour) {
        next_morning(now, local)
    } else {
        (now + 3600) as u64
    }
}

/// The soft tick of a timer's last seconds: only on the quiet home (not over a conversation,
/// the bell or anything else playing), never with do not disturb or at night.
fn final_tick_allowed(phase: Phase, speaker_busy: bool, dnd: bool, night: bool) -> bool {
    matches!(phase, Phase::Home | Phase::Offline) && !speaker_busy && !dnd && !night
}

/// Newest level in the centre, older ones outwards on both sides.
fn mirror(hist: &VecDeque<f32>) -> Vec<f32> {
    let half: Vec<f32> = (0..BARS / 2)
        .map(|i| hist.get(i).copied().unwrap_or(0.0))
        .collect();
    half.iter().rev().chain(half.iter()).copied().collect()
}

/// A slow, low wobble while Home Assistant thinks (as companion-sim's).
pub fn thinking_wave(t: f64) -> Vec<f32> {
    let half: Vec<f32> = (0..BARS / 2)
        .map(|i| {
            let k = i as f64 / (BARS / 2) as f64;
            let env = (1.0 - k).powf(1.5);
            let wob = 0.5
                + 0.5
                    * (std::f64::consts::TAU * (t * 0.5) + i as f64 * 0.9).sin()
                    * (t * 0.66 + i as f64).cos();
            (0.12 * env * wob) as f32
        })
        .collect();
    half.iter().rev().chain(half.iter()).copied().collect()
}

/// Resident memory from /proc/self/statm.
pub fn rss_mb() -> f64 {
    let pages = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<f64>().ok())
        })
        .unwrap_or(0.0);
    // SAFETY: sysconf only reads a constant.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as f64;
    (pages * page / 1048576.0 * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_round_trip() {
        for p in [
            "home",
            "listening",
            "thinking",
            "speaking",
            "error",
            "offline",
            "ringing",
        ] {
            assert_eq!(Phase::parse(p).unwrap().name(), p);
        }
        assert!(Phase::parse("nope").is_none());
    }

    #[test]
    fn wave_shapes() {
        let mut h = VecDeque::new();
        h.push_front(1.0);
        let w = mirror(&h);
        assert_eq!(w.len(), BARS);
        assert_eq!(w[BARS / 2 - 1], 1.0);
        assert_eq!(w[BARS / 2], 1.0);
        assert_eq!(w[0], 0.0);
        assert_eq!(thinking_wave(1.0).len(), BARS);
        assert_eq!(level_to_bar(-60.0), 0.0);
        assert_eq!(level_to_bar(0.0), 1.0);
    }

    #[test]
    fn mic_control() {
        let m = MicControl::default();
        assert_eq!(m.mode(), MicMode::Wake);
        m.set(MicMode::Stream);
        assert_eq!(m.mode(), MicMode::Stream);
        m.request_reset();
        assert!(m.take_reset());
        assert!(!m.take_reset());
    }

    #[test]
    fn sentences_cut_on_an_open_word() {
        assert!(super::unfinished("metti una sveglia alle"));
        assert!(super::unfinished("metti un timer di"));
        assert!(super::unfinished("set an alarm at"));
        assert!(!super::unfinished("metti una sveglia alle sette"));
        assert!(!super::unfinished("che ore sono"));
        assert!(!super::unfinished(""));
    }

    #[test]
    fn do_not_disturb_until() {
        let day = 86_400 * 1000; // a midnight (local time of day is given apart)
        // 22:30: until 07:00 tomorrow; 03:00: until 07:00 today; 10:00: an hour.
        assert_eq!(
            next_morning(day + 22 * 3600 + 1800, 22 * 3600 + 1800),
            (day + 86_400 + 7 * 3600) as u64
        );
        assert_eq!(
            next_morning(day + 3 * 3600, 3 * 3600),
            (day + 7 * 3600) as u64
        );
        assert_eq!(
            next_morning(day + 7 * 3600, 7 * 3600),
            (day + 86_400 + 7 * 3600) as u64
        );
        assert_eq!(
            dnd_default_until(day + 22 * 3600, 22 * 3600),
            (day + 86_400 + 7 * 3600) as u64
        );
        assert_eq!(
            dnd_default_until(day + 2 * 3600, 2 * 3600),
            (day + 7 * 3600) as u64
        );
        assert_eq!(
            dnd_default_until(day + 10 * 3600, 10 * 3600),
            (day + 11 * 3600) as u64
        );
        assert_eq!(
            dnd_default_until(day + 20 * 3600, 20 * 3600),
            (day + 21 * 3600) as u64
        );
        assert_eq!(
            dnd_default_until(day + 21 * 3600, 21 * 3600),
            (day + 86_400 + 7 * 3600) as u64
        );
    }

    #[test]
    fn final_ticks_only_on_a_quiet_home() {
        assert!(final_tick_allowed(Phase::Home, false, false, false));
        assert!(final_tick_allowed(Phase::Offline, false, false, false));
        assert!(
            !final_tick_allowed(Phase::Home, false, true, false),
            "do not disturb"
        );
        assert!(
            !final_tick_allowed(Phase::Home, false, false, true),
            "night"
        );
        assert!(
            !final_tick_allowed(Phase::Home, true, false, false),
            "speaker busy"
        );
        for p in [
            Phase::Listening,
            Phase::Thinking,
            Phase::Speaking,
            Phase::Ringing,
        ] {
            assert!(!final_tick_allowed(p, false, false, false), "{p:?}");
        }
    }
}
