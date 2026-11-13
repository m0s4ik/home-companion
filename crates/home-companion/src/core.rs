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
use crate::conversation::{AnswerPolicy, Conversation, Record, Swallow, Trigger};
use crate::cues::Cues;
use crate::reflex::{ReflexEvent, Reflexes};
use crate::settings::Settings;
use crate::timers::{TimerView, Timers, now_unix};
use crate::volume::{Volume, VolumeRequest};
use reflex::Command as Reflex;
use reflex::proto::Request as ReflexRequest;
use reflex::vad::Vad;
use reflex::verdict::{self as rv, Verdict};

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
    /// Something the character says aloud in Home Assistant's voice (Home Assistant's setup is
    /// done: "Tutto pronto"): the success sound now, the words when they come.
    Say(String),
    Talk,
    Stop,
    /// "Falso allarme": Stop, and the wake word's audio kept as a mistake (to train it).
    FalseWake,
    /// The brain's Yes/No question `id` is on the screen (BrainMsg::Confirm): when the character
    /// is still busy with the request that asked it, the microphone opens at once for the
    /// answer, where "sì" and "no" are reflexes.
    Question(i64, String),
    /// Yes or No touched on the question `id`: the core sends the answer to the brain.
    QuestionAnswer {
        id: i64,
        yes: bool,
    },
    /// A spoken yes to question `id` was received (BrainMsg::ConfirmPending): its commands run
    /// at `until_ms` (unix ms) unless a no (touched, or Stop) comes first.
    QuestionPending(i64, u64),
    /// The question `id` is off the screen: the brain's `confirm_done` (`yes` Some: answered,
    /// `via` as the brain names it) or the screen's own safety net (`yes` None, `via` "screen").
    QuestionClosed {
        id: i64,
        yes: Option<bool>,
        via: String,
    },
    MessageOk(usize),
    Language(String),
    HomeAssistant,
    Screenshot,
    /// Settings → Suspend, confirmed: the whole device to sleep (suspend.rs).
    Suspend,
    /// Timer e sveglie: a new timer of this many seconds (a local one).
    /// Seconds and what it is for, when said.
    TimerStart(u64, Option<String>),
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
    /// The brain's "done" card for a house command is on the screen (BrainMsg::Changed).
    HouseDone,
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
    /// A reflex's answer in Home Assistant's voice (crates/home-companion/src/voice.rs).
    Voice(crate::voice::VoiceReply),
    /// The microphone's switch on the device (the keys thread): on or muted; `initial` = how
    /// it is at start (no card on screen). On the ThinkSmart View it cuts the microphones in
    /// hardware: muted, they give only zeros.
    MicSwitch {
        on: bool,
        initial: bool,
    },
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
    /// An announcement (a reminder said by Home Assistant) plays: a due reminder on the screen
    /// stays, the voice over it, no conversation screen.
    Announcing(bool),
    /// "Stop" or "basta" over a due reminder: its OK.
    NoticeStop,
    /// What AirPlay plays (None: nothing); `playing` false while paused or muted here.
    Music(Option<crate::airplay::NowPlaying>),
    /// The music page's spectrum: band levels and peaks, 0..1.
    MusicSpectrum(Vec<f32>, Vec<f32>),
    /// AirPlay screen mirroring: the sender's name and the frames' socket (None = over).
    MirrorVideo(Option<(String, std::path::PathBuf)>),
    /// The mirrored picture's aspect ratio (the frames' padding is cropped).
    MirrorAspect(f32),
    /// What the control center's tiles of the core show: AirPlay (None = off or not installed,
    /// Some(None) = on and waiting, Some(Some(sender)) = playing) and the microphone.
    Control {
        airplay: Option<Option<String>>,
        mic_on: bool,
        /// The device's switch is on mute (the tile cannot turn the microphone back on).
        mic_switch_off: bool,
    },
    /// The microphone's switch was moved (or Talk pressed with it on mute): its card, on or off.
    MicSwitch(bool),
    /// The cloud services' use: today, this month, the last days (usage.rs).
    Usage(crate::usage::View),
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

impl HaLink {
    fn is_connected(&self) -> bool {
        matches!(self, HaLink::Connected { .. })
    }
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
    /// Prices for the cloud services' rough cost (usage.rs).
    pub usage_prices: crate::usage::Prices,
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
            usage_prices: Default::default(),
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
    /// The cues played, by name ("success", "wake"…), in order.
    pub cues: Vec<&'static str>,
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
    /// The reflexes' answers said aloud in Home Assistant's voice (None: their sounds only).
    pub voice: Option<Box<dyn crate::voice::VoiceLink>>,
    /// home-companion-brain, in order: the turns of the conversations and the answers to its
    /// questions (None: no brain, nothing is sent).
    pub brain: Option<Box<dyn crate::brain_link::BrainPort>>,
}

/// A reflex's answer waiting for its voice; the sound plays instead at `deadline`.
struct VoiceWait {
    id: u64,
    cue: Arc<audio::Clip>,
    text: String,
    deadline: Instant,
}

/// The brain's question said aloud (its voice requested, then playing); the microphone opens
/// for the answer when it has played, or at `deadline` if the voice never came.
struct AskVoice {
    id: i64,
    voice: u64,
    deadline: Instant,
    stream: Option<audio::Stream>,
}

/// How long the answer's words may take after the transcript (see `on_transcript`).
const AFTER_TRANSCRIPT: Duration = Duration::from_secs(20);

/// How long the question's voice may take to come before listening starts without it.
const ASK_VOICE_WAIT: Duration = Duration::from_millis(2500);

/// An utterance held back: buffered and fed to the reflex helper while the user speaks, the
/// helper's events judged by [`reflex::verdict`]; then either done locally or sent to Home
/// Assistant in one burst (and live after it while the user still speaks). Also without a
/// helper, while Home Assistant's previous run is still open (one run at a time).
struct Gate {
    id: u64,
    buf: Vec<i16>,
    vad: Vad,
    /// The verdict's memory (None: no helper, only waiting for the previous run).
    st: Option<reflex::verdict::GateState>,
    /// The utterance is for Home Assistant (decided): it waits for the previous run to end.
    want: Option<Want>,
    /// The helper's last partial and the audio it had decoded (for the logs).
    partial: Option<(String, f32)>,
    /// The helper's last result's words (what a reflex heard).
    heard: String,
}

impl Gate {
    /// `helper`: the reflexes judge it; false: it only waits for the previous run.
    fn new(id: u64, helper: bool) -> Gate {
        Gate {
            id,
            buf: Vec::with_capacity(16_000 * 4),
            vad: Vad::new(),
            st: helper.then(rv::GateState::new),
            want: None,
            partial: None,
            heard: String::new(),
        }
    }
}

/// What happened to a held utterance, for the verdict.
enum Seen {
    Tick,
    Lost,
    Partial(String, f32),
    Final(String, Vec<reflex::Word>, bool),
}

/// A release waiting for Home Assistant's previous run to end.
#[derive(Debug, Clone, Copy)]
struct Want {
    /// The speech is over (buffer, then `audio-stop`); else live after the buffer.
    over: bool,
    why: &'static str,
    since: Instant,
}

/// Home Assistant's pipeline run of a conversation: from `run-pipeline` to its final message
/// (`audio-stop` or `error`), the think timeout or a disconnect. One at a time.
#[derive(Debug, Clone)]
struct Run {
    conv: u64,
    opened: Instant,
    /// The question the conversation answered when the run opened (its `turn`'s `answers` if
    /// the transcript comes after the conversation is gone).
    answering: Option<i64>,
    /// `played` sent already (a stopped answer): not again at its `audio-stop`.
    played: bool,
    /// Its last message from Home Assistant (or its start).
    last_heard: Instant,
    /// The brain asked a question in it (then): the model is told to say nothing after it.
    asked_at: Option<Instant>,
}

impl Run {
    /// Over though nothing said so. Home Assistant tells a Wyoming satellite nothing at a run's
    /// end: a model that answers nothing (always after a question) leaves the run open here
    /// for good. Over: quiet [`ASKED_QUIET`] after its question, or quiet as long as a
    /// conversation waits for its answer at most ([`AFTER_TRANSCRIPT`]).
    fn quiet(&self, now: Instant) -> bool {
        let quiet = now.saturating_duration_since(self.last_heard);
        quiet >= AFTER_TRANSCRIPT || (self.asked_at.is_some() && quiet >= ASKED_QUIET)
    }
}

/// A conversation's `turn` for the brain: sent once (its transcript), ended once.
#[derive(Debug, Clone, Default)]
struct Turn {
    conv: u64,
    sent: bool,
    ended: bool,
    /// Stopped by the user: it ends with nothing said (the user did not hear it).
    silenced: bool,
}

/// The turns remembered (one per conversation; the oldest go).
const TURNS_KEPT: usize = 8;

/// A new utterance waits at most this long for the previous run's final message.
const PREVIOUS_RUN_CAP: Duration = Duration::from_secs(3);
/// After its question, a run quiet this long is over (Home Assistant ends it in about 2 s, the
/// model's empty answer, without a word to the satellite).
const ASKED_QUIET: Duration = Duration::from_secs(6);
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
    /// After a live release: our own end of speech, so Home Assistant gets `audio-stop` as soon
    /// as the user is silent instead of waiting for its own (slower, "relaxed") detector.
    live_vad: Option<Vad>,
    /// A reflex's answer is on screen until then.
    local_until: Option<Instant>,
    /// The reflex result, written next to the recording when it stops.
    sidecar: Option<serde_json::Value>,
    quit: bool,
    summary: Summary,
    /// The control center's microphone switch: off, no wake word (Parla still works).
    mic_muted: bool,
    /// The device's switch is on mute (the microphones give zeros): it wins over the tile.
    mic_switch_off: bool,
    /// What the cloud services did, by day (`<state_dir>/usage.json`).
    usage: Option<crate::usage::Usage>,
    airplay_on: bool,
    now_playing: Option<crate::airplay::NowPlaying>,
    /// The last spoken answer, for "ripeti".
    tape: crate::repeat::Tape,
    /// "Ripeti": the last answer playing again.
    replay: Option<audio::Stream>,
    /// "Non disturbare" until then (seconds since the epoch): no card, reminder or announcement
    /// sounds, no final ticks; timers and alarms still ring.
    dnd_until: Option<u64>,
    /// The brain's Yes/No question on the screen (UiEvent::Question).
    question: Option<i64>,
    /// The open question's words ("Chiudo la tapparella?"), said aloud and echoed on a yes.
    question_text: String,
    /// The question being said aloud before the microphone opens for its answer.
    asking_voice: Option<AskVoice>,
    /// A short notice about the question ("Chiudo …", "Va bene, lascio stare") awaited from
    /// the voice: played when it comes in time, in any phase but a ringing bell.
    notice_wait: Option<(u64, Instant)>,
    /// A spoken yes to that question was received: its commands run then, unless a no comes
    /// first (the stop word listens meanwhile).
    confirming: Option<(i64, Instant)>,
    /// The success sound already played for this question's yes (its "done" card is silent).
    dinged: Option<i64>,
    /// Home Assistant's pipeline run that is open.
    run: Option<Run>,
    /// An announcement arrived over a conversation of ours: swallowed until its `audio-stop`.
    stray_audio: bool,
    /// The conversations' turns for the brain.
    turns: Vec<Turn>,
    /// The conversation of the last run that ended (its words may follow its audio).
    last_run_conv: Option<u64>,
    /// A reflex's answer waiting for its voice.
    voice_wait: Option<VoiceWait>,
    next_voice_id: u64,
    /// A UiEvent::Say waiting for its voice: its id and until when it is still worth saying.
    say_wait: Option<(u64, Instant)>,
    /// What the body is saying in its own voice (a reflex's answer, a timer's announcement):
    /// a stop word that sounds like one of its words is the body's own voice ([`echoes`]).
    speaking_text: String,
    /// The last recording saved and the last wake word heard (name, probability), for
    /// "Falso allarme"; when the conversation began (unix seconds).
    last_recording: Option<PathBuf>,
    last_wake: Option<(String, f32)>,
    conv_started_wall: Option<u64>,
    /// When the character's own phrase began to play, and how long it lasts.
    speaking_since: Option<Instant>,
    speaking_len: Option<Duration>,
    /// A timer or alarm rang: its words wait for their voice (id, until when); the bell after.
    ring_wait: Option<(u64, Instant)>,
    /// The ringing's words playing.
    ring_speech: Option<audio::Stream>,
    /// The bell starts then (after the ringing's words).
    bell_at: Option<Instant>,
}

/// How long a timer's words may keep its bell waiting (a phrase not in the cache comes from
/// Home Assistant in 1-2 s).
const RING_VOICE_WAIT: Duration = Duration::from_millis(3000);

/// The voice's pace when the length of a phrase is not known yet (Home Assistant's answer still
/// arriving): characters a second.
const VOICE_CHARS_PER_S: f32 = 14.0;
/// Around the moment a word like a stop word is said, its detection is the character's own
/// voice: from this long before (an early estimate) to this long after (the detector answers
/// at the word's end, with its window).
const ECHO_BEFORE_S: f32 = 2.0;
const ECHO_AFTER_S: f32 = 3.0;

/// A word like a stop word in what the character says, and when.
#[derive(Debug, Clone, PartialEq)]
struct Echo {
    word: String,
    /// When the word is said, from the start of the voice (seconds), and the voice's length
    /// (known, or estimated from the text).
    at_s: f32,
    len_s: f32,
    estimated: bool,
    /// When the stop word was heard, from the same start (None: no start known).
    heard_s: Option<f32>,
    /// Inside the window around the word: the detection is ignored.
    ignored: bool,
}

/// The stop word's name ("stop_alfred" → "stop") against the words of `text`: a word that sounds
/// like it (the same, one letter away like "pasta" for "basta", or holding it like
/// "abbastanza"), placed in time by its position in the text; `elapsed` since the voice began,
/// `len` its length when known. The closest such word, or None.
fn echo_at(
    stop_word: &str,
    text: &str,
    elapsed: Option<Duration>,
    len: Option<Duration>,
) -> Option<Echo> {
    let name = stop_word
        .split('_')
        .next()
        .unwrap_or(stop_word)
        .to_lowercase();
    if name.chars().count() < 4 {
        return None;
    }
    let total = text.chars().count().max(1) as f32;
    let (len_s, estimated) = match len {
        Some(d) => (d.as_secs_f32(), false),
        None => (total / VOICE_CHARS_PER_S, true),
    };
    let heard_s = elapsed.map(|d| d.as_secs_f32());
    let mut best: Option<Echo> = None;
    for (start, w) in word_spans(text) {
        let lw = w.to_lowercase();
        if !(one_letter_away(&lw, &name) || lw.contains(&name)) {
            continue;
        }
        let mid = start as f32 + w.chars().count() as f32 / 2.0;
        let at_s = mid / total * len_s;
        let ignored = heard_s.is_none_or(|h| h >= at_s - ECHO_BEFORE_S && h <= at_s + ECHO_AFTER_S);
        let e = Echo {
            word: w.to_string(),
            at_s,
            len_s,
            estimated,
            heard_s,
            ignored,
        };
        let closer = |a: &Echo| heard_s.map_or(0.0, |h| (h - a.at_s).abs());
        if best.as_ref().is_none_or(|b| {
            (e.ignored && !b.ignored) || (e.ignored == b.ignored && closer(&e) < closer(b))
        }) {
            best = Some(e);
        }
    }
    best
}

/// The words of `text` with their first character's index.
fn word_spans(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<(usize, usize)> = None;
    for (ci, (bi, c)) in text.char_indices().enumerate() {
        if c.is_alphanumeric() {
            if start.is_none() {
                start = Some((ci, bi));
            }
        } else if let Some((sc, sb)) = start.take() {
            out.push((sc, &text[sb..bi]));
        }
    }
    if let Some((sc, sb)) = start {
        out.push((sc, &text[sb..]));
    }
    out
}

/// Equal, or one letter changed, added or removed.
fn one_letter_away(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if long.len() - short.len() > 1 {
        return false;
    }
    let pre = short
        .iter()
        .zip(long.iter())
        .take_while(|(x, y)| x == y)
        .count();
    if pre == short.len() {
        return true;
    }
    if short.len() == long.len() {
        short[pre + 1..] == long[pre + 1..]
    } else {
        short[pre..] == long[pre + 1..]
    }
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
            live_vad: None,
            local_until: None,
            sidecar: None,
            quit: false,
            summary: Summary::default(),
            mic_muted: false,
            mic_switch_off: false,
            usage: None,
            airplay_on: true,
            now_playing: None,
            tape: crate::repeat::Tape::default(),
            replay: None,
            dnd_until: None,
            question: None,
            question_text: String::new(),
            asking_voice: None,
            notice_wait: None,
            confirming: None,
            dinged: None,
            run: None,
            stray_audio: false,
            turns: Vec::new(),
            last_run_conv: None,
            voice_wait: None,
            next_voice_id: 1,
            say_wait: None,
            speaking_text: String::new(),
            last_recording: None,
            last_wake: None,
            conv_started_wall: None,
            speaking_since: None,
            speaking_len: None,
            ring_wait: None,
            ring_speech: None,
            bell_at: None,
        }
    }

    /// The event loop; returns when asked to quit (or every sender is gone).
    pub fn run(mut self, rx: Receiver<CoreEvent>) -> Summary {
        self.ui(UiCommand::View(self.phase));
        self.summary.phases.push(self.phase);
        if let Some(s) = &self.d.settings {
            self.mic_muted = s.mic_muted();
            self.airplay_on = s.airplay_on();
            if let Some(dir) = s.path.parent() {
                self.usage = Some(crate::usage::Usage::in_dir(dir));
            }
        }
        self.send_usage();
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
            if let Some(w) = &self.voice_wait {
                until = until.min(w.deadline);
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
            if self.voice_wait.as_ref().is_some_and(|w| now >= w.deadline) {
                self.voice_late();
            }
            if self.ring_wait.is_some_and(|(_, until)| now >= until) {
                log::info!(event = "ring_voice", voiced = false; "no voice in time for the ringing: the bell");
                self.ring_wait = None;
                self.bell_at = Some(now);
            }
            if self.bell_at.is_some_and(|t| now >= t) {
                self.bell_at = None;
                self.start_bell();
            }
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
            CoreEvent::Voice(r) => self.on_voice(r),
            CoreEvent::MicSwitch { on, initial } => self.on_mic_switch(on, initial),
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
        if self.conv.as_ref().is_none_or(|c| !c.policy.swallows()) {
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
                self.last_recording = Some(p.clone());
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

    /// A short answer after a house command that went through: the success sound and the card
    /// already said it, so the conversation ends now and the voice (seconds away) is swallowed
    /// when it comes, acknowledged with `played` as a stopped answer.
    fn end_quietly(&mut self) {
        let Some(c) = &mut self.conv else { return };
        c.policy = AnswerPolicy::Swallow(Swallow::HouseQuiet);
        log::info!(event = "house_quiet", id = c.id, text = c.answer.as_str(); "short answer after a house command: not spoken");
        self.mic_idle();
        self.d.mic.request_reset();
        self.stop_recorder();
        self.listen_deadline = None;
        self.think_deadline = Some(Instant::now() + self.cfg.think_timeout);
        self.wave.clear();
        self.go_idle();
    }

    /// Ends the current conversation with its `conversation` line.
    fn close(&mut self, outcome: &str) {
        if let Some(mut c) = self.conv.take() {
            if c.trigger == Trigger::Announcement {
                self.ui(UiCommand::Announcing(false));
            }
            let outcome = match (c.policy, outcome) {
                (AnswerPolicy::Swallow(Swallow::HouseQuiet), "stopped") => "house_quiet",
                (AnswerPolicy::Swallow(Swallow::Asked), "stopped") => "question",
                _ => outcome,
            };
            // Its turn ends with nothing said: now, or when its run ends if Home Assistant still
            // works on it (the model may call a tool of the brain again for that turn).
            self.end_turn_unheard(c.id);
            if let Some(s) = c.stream.take() {
                s.abort();
            }
            let r = c.log(outcome);
            // Home Assistant's services worked on it (speech sent or an answer back).
            if r.audio_seconds_sent > 0.0 || !r.answer.is_empty() {
                if let Some(u) = &mut self.usage {
                    let day = crate::usage::local_day(crate::timers::now_unix() as i64);
                    u.add(&day, r.audio_seconds_sent, r.answer_seconds, &r.answer);
                }
                self.send_usage();
            }
            self.summary.conversations.push(r);
        }
        self.think_deadline = None;
        self.speak_deadline = None;
    }

    /// Today's questions and the rough cost today and this month, for Settings.
    fn send_usage(&self) {
        let Some(u) = &self.usage else { return };
        let now = crate::timers::now_unix() as i64;
        self.ui(UiCommand::Usage(u.view(now, &self.cfg.usage_prices)));
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
            if let Some(r) = self.run.as_mut().filter(|r| r.conv == c.id) {
                r.played = true;
            }
        }
    }

    /// A cue by its name, recorded in the summary.
    fn cue(&mut self, name: &'static str) {
        if let Some(clip) = self.clip(name) {
            self.d.player.play(&clip);
            self.summary.cues.push(name);
        }
    }

    fn clip(&self, name: &str) -> Option<Arc<audio::Clip>> {
        let c = &self.cues;
        Some(
            match name {
                "wake" => &c.wake,
                "error" => &c.error,
                "not_understood" => &c.not_understood,
                "timer_set" => &c.timer_set,
                "alarm_set" => &c.alarm_set,
                "cancelled" => &c.cancelled,
                "time" => &c.time,
                "done" => &c.done,
                "success" => &c.success,
                "silence" => &c.silence,
                "reminder" => &c.reminder,
                "tap" => &c.tap,
                "home" => &c.home,
                "tick" => &c.tick,
                _ => return None,
            }
            .clone(),
        )
    }

    /// A request to the brain, in order (none without a brain).
    fn brain_send(&self, req: serde_json::Value) {
        if let Some(b) = &self.d.brain {
            b.send(req);
        }
    }

    /// The turn of conversation `conv`, made if missing.
    fn turn_of(&mut self, conv: u64) -> &mut Turn {
        let i = match self.turns.iter().position(|t| t.conv == conv) {
            Some(i) => i,
            None => {
                if self.turns.len() >= TURNS_KEPT {
                    self.turns.remove(0);
                }
                self.turns.push(Turn {
                    conv,
                    ..Turn::default()
                });
                self.turns.len() - 1
            }
        };
        &mut self.turns[i]
    }

    /// The `turn` of conversation `conv` for the brain: once, with a transcript.
    fn start_turn(&mut self, conv: u64, text: &str, answering: Option<i64>) {
        if text.trim().is_empty() {
            return;
        }
        let t = self.turn_of(conv);
        if t.sent {
            return;
        }
        t.sent = true;
        self.brain_send(crate::brain_link::turn(conv, text, answering));
    }

    /// The `turn` of conversation `conv` ends (once): Home Assistant's words, or "".
    fn end_turn(&mut self, conv: u64, said: &str) {
        let Some(t) = self
            .turns
            .iter_mut()
            .find(|t| t.conv == conv && t.sent && !t.ended)
        else {
            return;
        };
        t.ended = true;
        let said = if t.silenced { "" } else { said };
        let req = crate::brain_link::turn_end(conv, said);
        self.brain_send(req);
    }

    /// The turn of conversation `conv` ends with nothing said (the user did not hear it): now,
    /// or, while Home Assistant's run for it is open, when that run ends (its words in
    /// [`Core::on_previous_run`] or the current answer, [`Core::run_over`], the cap of
    /// [`Core::release_waiting`], a disconnect), so that the brain keeps the turn the model's
    /// tool calls belong to.
    fn end_turn_unheard(&mut self, conv: u64) {
        if self.run.as_ref().is_some_and(|r| r.conv == conv) {
            self.turn_of(conv).silenced = true;
            log::debug!("turn of conversation {conv} ends with its run");
            return;
        }
        self.end_turn(conv, "");
    }

    /// The run is over (its final message, the think timeout, a disconnect): the turn of a
    /// conversation already gone ends if nothing ended it (the current one's ends when it
    /// closes), and an utterance waiting for it goes to Home Assistant now.
    fn run_over(&mut self, why: &str) {
        let Some(r) = self.run.clone() else { return };
        self.last_run_conv = Some(r.conv);
        if self.conv.as_ref().is_none_or(|c| c.id != r.conv) {
            self.end_turn(r.conv, "");
        }
        log::debug!(
            "run of conversation {} over ({why}) after {:.1} s",
            r.conv,
            r.opened.elapsed().as_secs_f64()
        );
        self.run = None;
        self.release_waiting();
    }

    /// The success sound for a house command or a spoken yes received: never over an answer
    /// speaking or the bell (do not disturb does not silence it: the user just asked).
    fn success_cue(&mut self) {
        if !matches!(self.phase, Phase::Speaking | Phase::Ringing) {
            self.cue("success");
        }
    }

    // ---- conversations ----------------------------------------------------------------

    fn start_conversation(&mut self, trigger: Trigger) {
        self.start_conversation_for(trigger, None);
    }

    /// A new conversation; `answering`: the brain's question it hears the answer to.
    fn start_conversation_for(&mut self, trigger: Trigger, answering: Option<i64>) {
        // With the reflex helper up, the utterance is held back and decoded locally first; Home
        // Assistant is not needed for a reflex (Wi-Fi off, Home Assistant down).
        self.speaking_text.clear();
        self.speaking_since = None;
        self.speaking_len = None;
        let id = self.next_id;
        let gated = self.d.reflexes.as_ref().is_some_and(|r| {
            // The answer to a question: decoded against the answers only (a short "no" is
            // lost against the whole grammar).
            let start = if answering.is_some() {
                ReflexRequest::StartAnswer(id)
            } else {
                ReflexRequest::Start(id)
            };
            r.usable() && r.link.send(start)
        });
        if !self.ha_ready() && !gated {
            log::info!(event = "no_home_assistant", trigger = trigger.name(); "{} with no Home Assistant", trigger.name());
            self.cue("error");
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
        c.answering = answering;
        self.cue("wake");
        self.d.mic.set(MicMode::Stream);
        self.last_recording = None;
        self.conv_started_wall = Some(now_unix() as u64);
        if let Some(r) = &self.d.recorder {
            r.start("utterance");
        }
        log::info!(event = "conversation_start", id = id, trigger = trigger.name(), reflexes = gated, answering = answering.unwrap_or(-1); "conversation {id} started ({}{})", trigger.name(), if gated { ", reflexes first" } else { "" });
        self.conv = Some(c);
        self.live_vad = None;
        self.ui(UiCommand::Heard(String::new()));
        self.ui(UiCommand::Answer(String::new()));
        let ok = if gated {
            self.gate = Some(Gate::new(id, true));
            true
        } else if self.run.is_some() {
            // Home Assistant's previous run is still open: the speech is held until it ends.
            self.gate = Some(Gate::new(id, false));
            self.release(false, "no reflexes");
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
    /// audio-start); false if the connection failed. Only with no run open (see [`Run`]).
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
            self.run = Some(Run {
                conv: c.id,
                opened: Instant::now(),
                answering: c.answering,
                played: false,
                last_heard: Instant::now(),
                asked_at: None,
            });
        }
        self.summary.detections_sent += 1;
        ok
    }

    /// End of the user's speech (voice-stopped or transcript): stop streaming.
    fn end_listening(&mut self) {
        self.live_vad = None;
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
        self.cue("error");
        self.close(outcome);
        self.listen_deadline = None;
        self.wave.clear();
        self.set_phase(Phase::Error);
    }

    /// The stop word listens only while there is something to stop: an answer, a bell, a due
    /// reminder on the screen, the second in which a spoken yes can still be taken back.
    fn stop_wanted(&self, p: Phase) -> bool {
        matches!(p, Phase::Thinking | Phase::Speaking | Phase::Ringing)
            || (self.noticing && matches!(p, Phase::Home | Phase::Offline))
            || self.confirming_now().is_some()
    }

    /// The stop word detector follows [`Self::stop_wanted`] now.
    fn refresh_stop_listening(&self) {
        self.d.mic.set_stop_listening(self.stop_wanted(self.phase));
    }

    /// The question whose spoken yes is in its confirming second, if it still is.
    fn confirming_now(&self) -> Option<i64> {
        self.confirming
            .filter(|(_, until)| Instant::now() < *until)
            .map(|(id, _)| id)
    }

    fn on_stop_word(&mut self, d: wakeword::Detection) {
        log::info!(event = "stop_word", name = d.name.as_str(), probability = d.probability as f64, phase = self.phase.name(); "stop word {} ({:.3}) in {}", d.name, d.probability, self.phase.name());
        if matches!(self.phase, Phase::Speaking | Phase::Ringing) {
            // What the character is saying, since when, and how long it lasts: its own phrase
            // (a reflex's answer, a ringing timer's words) or Home Assistant's answer.
            let (text, since, len) =
                if !self.speaking_text.is_empty() && self.speaking_since.is_some() {
                    (
                        self.speaking_text.clone(),
                        self.speaking_since,
                        self.speaking_len,
                    )
                } else if let Some(c) = &self.conv {
                    (
                        c.answer.clone(),
                        c.first_sample_at.or(c.audio_start_at),
                        None,
                    )
                } else {
                    (String::new(), None, None)
                };
            if let Some(e) = echo_at(&d.name, &text, since.map(|t| t.elapsed()), len) {
                self.save_echo(&d, &text, &e);
                if e.ignored {
                    log::info!(event = "stop_word_echo", name = d.name.as_str(), word = e.word.as_str(), at_s = e.at_s as f64, heard_s = e.heard_s.unwrap_or(-1.0) as f64; "stop word {} ignored: the body says {:?} near then", d.name, e.word);
                    return;
                }
                log::info!(event = "stop_word_near", name = d.name.as_str(), word = e.word.as_str(), at_s = e.at_s as f64, heard_s = e.heard_s.unwrap_or(-1.0) as f64; "stop word {}: {:?} is said at another time, so it is the user's", d.name, e.word);
            }
        }
        match self.phase {
            // Thinking: a wrong transcript on the screen; the answer is dropped (the pipeline
            // itself cannot be cancelled).
            Phase::Thinking | Phase::Speaking | Phase::Ringing => self.on_stop(),
            _ if self.confirming_now().is_some() => self.on_stop(),
            Phase::Home | Phase::Offline if self.noticing => self.ui(UiCommand::NoticeStop),
            _ => {}
        }
    }

    /// Debug: a stop word met a word like it in the character's phrase (ignored or not): the
    /// microphone's last seconds and what was known, next to the recordings, to check the
    /// estimate of when the word is said.
    fn save_echo(&self, d: &wakeword::Detection, text: &str, e: &Echo) {
        let Some(r) = &self.d.recorder else { return };
        let label = format!(
            "stop-{}-{}",
            if e.ignored { "ignored" } else { "near" },
            d.name
        );
        let Some(path) = r.save_context(&label) else {
            return;
        };
        let info = serde_json::json!({
            "stop_word": d.name,
            "probability": d.probability,
            "ignored": e.ignored,
            "text": text,
            "word": e.word,
            "word_at_s": e.at_s,
            "voice_len_s": e.len_s,
            "len_estimated": e.estimated,
            "heard_at_s": e.heard_s,
            "window_s": [-ECHO_BEFORE_S, ECHO_AFTER_S],
            "audio": "the microphone's last seconds, ending when the stop word was heard",
        });
        let _ = std::fs::write(path.with_extension("json"), info.to_string());
        log::info!(event = "stop_word_saved", path = path.display().to_string().as_str(); "stop word debug: {}", path.display());
    }

    /// "Falso allarme": the conversation stops, and its recording (the 3.5 s before the wake
    /// word, what followed, the reflexes' verdict) is copied to `false-wakes/` beside the
    /// recordings, out of their rotation: negatives to train the wake word again.
    fn on_false_wake(&mut self) {
        let wake = self.last_wake.clone();
        self.on_stop();
        // The recording of this conversation (stopped just now or at the end of the speech).
        let started = self.conv_started_wall;
        let Some(p) = self.last_recording.clone() else {
            log::info!(event = "false_wake", saved = false; "false alarm: nothing recorded (recording off)");
            return;
        };
        let Some(dir) = p
            .parent()
            .and_then(|d| d.parent())
            .map(|d| d.join("false-wakes"))
        else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("{}: {e}", dir.display());
            return;
        }
        let stem = p
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut copied = 0;
        for name in [
            format!("{stem}.wav"),
            format!("{stem}-wake.wav"),
            format!("{stem}.json"),
        ] {
            let from = p.with_file_name(&name);
            if from.exists() && std::fs::copy(&from, dir.join(&name)).is_ok() {
                copied += 1;
            }
        }
        let info = serde_json::json!({
            "false_wake": true,
            "wake_word": wake.as_ref().map(|w| w.0.clone()),
            "probability": wake.as_ref().map(|w| w.1),
            "conversation_started_unix": started,
            "files": stem,
        });
        let _ = std::fs::write(
            dir.join(format!("{stem}-false.json")),
            format!("{info:#}\n"),
        );
        log::info!(event = "false_wake", saved = true, files = copied, dir = dir.display().to_string().as_str(); "false alarm: {copied} files kept in {}", dir.display());
    }

    fn on_wake(&mut self, d: wakeword::Detection) {
        self.last_wake = Some((d.name.clone(), d.probability));
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
        let ts = ((f.index.saturating_sub(first) + c.live_offset) * audio::FRAME_MS as u64) as i64;
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
        let over = self.live_vad.as_mut().is_some_and(|v| {
            v.push(&f.samples);
            live_speech_over(v)
        });
        if over {
            self.live_vad = None;
            log::info!(event = "live_end"; "end of speech heard by the body: audio-stop to Home Assistant");
            self.send(&Message::AudioStop { timestamp: None });
            if let Some(c) = &mut self.conv {
                c.audio_stop_sent = true;
            }
            self.end_listening();
        }
    }

    // ---- reflexes -----------------------------------------------------------------------

    /// A frame of an utterance held back: buffered, fed to the helper and to the speech
    /// detector, then the verdict (reflex::verdict) on time passing. Waiting for the previous
    /// run: buffered only (live speech), or dropped (the speech is over).
    fn on_mic_gated(&mut self, f: &CaptureFrame) {
        if self.wave.len() == BARS / 2 {
            self.wave.pop_back();
        }
        self.wave.push_front(level_to_bar(f.level.rms_dbfs));
        let Some(g) = &mut self.gate else { return };
        if let Some(w) = g.want {
            if !w.over {
                g.buf.extend_from_slice(&f.samples);
            }
            self.release_waiting();
            return;
        }
        g.buf.extend_from_slice(&f.samples);
        g.vad.push(&f.samples);
        let fed = self
            .d
            .reflexes
            .as_ref()
            .is_some_and(|r| r.link.send(ReflexRequest::Audio(f.samples.to_vec())));
        self.judge(if fed { Seen::Tick } else { Seen::Lost });
    }

    fn on_reflex(&mut self, e: ReflexEvent) {
        let ours = |g: &Gate, id: u64| g.id == id && g.want.is_none() && g.st.is_some();
        match e {
            ReflexEvent::Ready { .. } => {}
            ReflexEvent::Lost => {
                self.judge(Seen::Lost);
            }
            ReflexEvent::Partial { id, text, audio_s } => {
                let Some(g) = self.gate.as_mut().filter(|g| ours(g, id)) else {
                    return;
                };
                log::debug!("reflex partial at {audio_s:.2} s: {text:?}");
                g.partial = Some((text.clone(), audio_s));
                self.judge(Seen::Partial(text, audio_s));
            }
            ReflexEvent::Result {
                id,
                endpoint,
                text,
                words,
                ms,
            } => {
                let Some(g) = self.gate.as_mut().filter(|g| ours(g, id)) else {
                    return;
                };
                g.heard = text.clone();
                let min = reflex::grammar::min_conf(&words);
                let heard_s = g.vad.heard_ms() as f32 / 1000.0;
                let after_speech = g.vad.span().map_or(0.0, |(_, b)| heard_s - b);
                let speech_ms = g.vad.speech_ms();
                let threshold = self.d.reflexes.as_ref().map_or(1.0, |r| r.threshold);
                let (v, why) = self.judge(Seen::Final(text.clone(), words.clone(), endpoint));
                let verdict = format!("{v:?}");
                log::info!(
                    event = "reflex", text = text.as_str(), min_conf = min as f64, verdict = verdict.as_str(), why = why,
                    vosk_end = endpoint, decided_after_speech_s = after_speech as f64, helper_ms = ms;
                    "reflex: {text:?} (min {min:.2}) {verdict} ({why}), {after_speech:.2} s after the speech"
                );
                let command = match &v {
                    Verdict::Act(c) => serde_json::to_value(c).ok(),
                    _ => None,
                };
                let side = serde_json::json!({
                    "text": text,
                    "words": words,
                    "min_conf": min,
                    "threshold": threshold,
                    "verdict": verdict,
                    "why": why,
                    "command": command,
                    "vosk_end": endpoint,
                    "speech_ms": speech_ms,
                    "decided_after_speech_s": after_speech,
                });
                // The last result is the one that decided (an empty one may come first).
                match self.sidecar.as_mut() {
                    Some(j) => j["reflex"] = side,
                    None => self.sidecar = Some(serde_json::json!({ "reflex": side })),
                }
            }
        }
    }

    /// One event of the held utterance through the verdict, and what it says done. Returns the
    /// verdict and why.
    fn judge(&mut self, seen: Seen) -> (Verdict, &'static str) {
        let answering = self.conv.as_ref().is_some_and(|c| c.answering.is_some());
        let Some(g) = self.gate.as_mut().filter(|g| g.want.is_none()) else {
            return (Verdict::Hold, "");
        };
        let Some(st) = g.st.as_mut() else {
            return (Verdict::Hold, "");
        };
        let Some((r, grammar)) = self
            .d
            .reflexes
            .as_ref()
            .and_then(|r| r.grammar.as_ref().map(|gr| (r, gr)))
        else {
            self.release(false, "no reflexes");
            return (Verdict::Release { live: true }, "no reflexes");
        };
        let ctx = rv::Ctx {
            grammar,
            threshold: r.threshold,
            rule: rv::Rule::default(),
            limits: rv::Limits {
                max_listen_s: r.max_listen.as_secs_f32(),
                ..rv::Limits::default()
            },
            vad: &g.vad,
            answering,
            // The utterance's audio: the helper's and the detector's clock.
            elapsed_s: g.buf.len() as f32 / 16_000.0,
        };
        let v = match &seen {
            Seen::Tick => rv::verdict(st, &ctx, rv::Event::Tick),
            Seen::Lost => rv::verdict(st, &ctx, rv::Event::HelperLost),
            Seen::Partial(text, audio_s) => rv::verdict(
                st,
                &ctx,
                rv::Event::Partial {
                    text,
                    audio_s: *audio_s,
                },
            ),
            Seen::Final(text, words, endpoint) => rv::verdict(
                st,
                &ctx,
                rv::Event::Final {
                    text,
                    words,
                    endpoint: *endpoint,
                },
            ),
        };
        let why = st.why();
        let by_partial = st.released_by_partial();
        let (id, heard) = (g.id, g.heard.clone());
        let partial = g.partial.clone();
        match v.clone() {
            Verdict::Hold => {}
            Verdict::Flush => {
                if let Some(r) = &self.d.reflexes {
                    r.link.send(ReflexRequest::End(id));
                }
            }
            Verdict::Release { live } => {
                if by_partial {
                    if let Some((text, audio_s)) = &partial {
                        let j = self.sidecar.get_or_insert_with(|| serde_json::json!({}));
                        j["partial"] = serde_json::json!({
                            "text": text,
                            "audio_s": audio_s,
                            "verdict": format!("{v:?}"),
                            "why": why,
                        });
                    }
                }
                self.release(!live, why);
            }
            Verdict::Act(c) => self.reflex(c, &heard),
            Verdict::Quiet => self.no_speech(),
        }
        (v, why)
    }

    /// Nobody spoke after the wake word (or after a question back: then quietly).
    fn no_speech(&mut self) {
        self.gate = None;
        self.mic_idle();
        self.d.mic.request_reset();
        self.stop_recorder();
        if self
            .conv
            .as_ref()
            .is_none_or(|c| c.trigger != Trigger::FollowUp)
        {
            self.cue("not_understood");
        }
        self.close("no speech");
        self.listen_deadline = None;
        self.go_idle();
    }

    /// A reflex the verdict accepted. "Sì", "no" and "basta" answer the brain's question when
    /// this conversation was opened for it; any other time "sì" is for Home Assistant and "no"
    /// or "basta" end the conversation.
    fn reflex(&mut self, cmd: Reflex, heard: &str) {
        let answering = self.conv.as_ref().and_then(|c| c.answering);
        match (&cmd, answering) {
            (Reflex::Yes | Reflex::No | Reflex::Silence, Some(id)) => {
                let yes = cmd == Reflex::Yes;
                log::info!(event = "question_answer", id = id, yes = yes, via = "voice_reflex"; "question {id}: {} (said)", if yes { "yes" } else { "no" });
                self.brain_send(crate::brain_link::confirm_answer(
                    id,
                    yes,
                    crate::brain_link::Via::VoiceReflex,
                ));
                if let Some(c) = &mut self.conv {
                    c.answering = None;
                }
                self.act(cmd, heard);
            }
            (Reflex::Yes, None) => self.release(true, "yes without a question"),
            _ => self.act(cmd, heard),
        }
    }

    /// The utterance goes to Home Assistant (`over`: the speech is over, else live after the
    /// buffer). With Home Assistant's previous run still open it waits, buffering, for that
    /// run's end ([`Self::release_waiting`]).
    fn release(&mut self, over: bool, why: &'static str) {
        let Some(g) = &mut self.gate else { return };
        if g.want.is_some() {
            return;
        }
        if !over && g.st.is_some() {
            // The helper stops: the speech goes on to Home Assistant.
            if let Some(r) = &self.d.reflexes {
                r.link.send(ReflexRequest::End(g.id));
            }
        }
        g.want = Some(Want {
            over,
            why,
            since: Instant::now(),
        });
        if let Some(r) = self.run.as_ref().filter(|r| !r.quiet(Instant::now())) {
            log::info!(event = "previous_run_wait", conversation = r.conv, why = why; "to Home Assistant ({why}) when its previous run ends");
        }
        self.release_waiting();
    }

    /// A release waiting for the previous run goes now if that run is over, or after
    /// [`PREVIOUS_RUN_CAP`] anyway.
    fn release_waiting(&mut self) {
        let Some(w) = self.gate.as_ref().and_then(|g| g.want) else {
            return;
        };
        if let Some(r) = self.run.as_ref().filter(|r| r.quiet(Instant::now())) {
            let conv = r.conv;
            log::info!(event = "previous_run_quiet", conversation = conv, quiet_s = r.last_heard.elapsed().as_secs_f64(), asked = r.asked_at.is_some(); "Home Assistant's run of conversation {conv} is quiet: over");
            self.run_over("quiet");
            return;
        }
        if let Some(r) = &self.run {
            if w.since.elapsed() < PREVIOUS_RUN_CAP {
                return;
            }
            let conv = r.conv;
            log::warn!(event = "previous_run_cap", conversation = conv, waited_s = w.since.elapsed().as_secs_f64(); "Home Assistant's run of conversation {conv} never ended: a new one anyway");
            self.end_turn(conv, "");
            self.run = None;
        }
        let Some(g) = self.gate.take() else { return };
        self.send_release(g, w.over, w.why);
    }

    /// The pipeline starts, the buffered audio follows in one burst, then `audio-stop` (`over`)
    /// or the live stream (still speaking).
    fn send_release(&mut self, g: Gate, over: bool, why: &str) {
        if !self.ha_ready() {
            log::info!(event = "no_home_assistant", why = why; "not a reflex ({why}) and no Home Assistant");
            self.mic_idle();
            self.d.mic.request_reset();
            self.stop_recorder();
            self.cue("not_understood");
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
            // The live frames go on from the buffer's end (timestamps from 0, as without it).
            c.first_frame = None;
            c.live_offset = frames;
        }
        log::info!(
            event = "reflex_release", why = why, audio_s = g.buf.len() as f64 / 16_000.0, over = over, live = !over, burst_ms = t.elapsed().as_secs_f64() * 1000.0;
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
        } else {
            // Home Assistant listens on, live; our detector (it has heard everything so far)
            // ends it at the user's silence, Home Assistant's own end of speech otherwise.
            self.listen_deadline = Some(Instant::now() + self.cfg.listen_timeout);
            self.live_vad = Some(g.vad);
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
        // The words said aloud when they differ from the screen's ("5 minuti" for "05:00").
        let mut spoken: Option<String> = None;
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
                let said = self.said_duration(seconds as f64);
                let (key, mut args) = match name {
                    Some(n) => ("reflex_timer_set_named", vec![("name", n.as_str())]),
                    None => ("reflex_timer_set", vec![]),
                };
                args.push(("duration", &said));
                spoken = Some(self.reflex_text(key, &args));
                args.pop();
                args.push(("duration", &d));
                self.reflex_text(key, &args)
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
                    let said = self.said_duration(due - now);
                    spoken = Some(self.reflex_text("reflex_timer_left", &[("left", &said)]));
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
                        let said = self.said_duration(l);
                        spoken = Some(self.reflex_text(
                            "reflex_timer_left_named",
                            &[("name", n), ("left", &said)],
                        ));
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
                    let item = |r: &TimerView, left: &str| {
                        let key = if r.paused {
                            "reflex_timer_item_paused"
                        } else {
                            "reflex_timer_item"
                        };
                        self.reflex_text(key, &[("name", &r.name), ("left", left)])
                    };
                    let items: Vec<String> = rows.iter().map(|r| item(r, &r.left)).collect();
                    let said: Vec<String> = rows
                        .iter()
                        .map(|r| {
                            let left = self.d.timers.get(&r.id).map(|t| t.remaining(now));
                            item(r, &left.map_or(r.left.clone(), |l| self.said_duration(l)))
                        })
                        .collect();
                    spoken = Some(format!("{}.", said.join("; ")));
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
            // The answer to the brain's question went with `reflex`; otherwise nothing to do.
            Reflex::Silence | Reflex::No | Reflex::Yes => String::new(),
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
        let cue_name: Option<&'static str> = match &cmd {
            Reflex::Time => Some("time"),
            Reflex::TimerStart { .. } => Some("timer_set"),
            Reflex::AlarmSet { .. } => Some("alarm_set"),
            Reflex::TimerCancel { .. } | Reflex::AlarmCancel => Some("cancelled"),
            // Soft and low: the screen goes dark, the evening goes quiet.
            Reflex::Silence | Reflex::No | Reflex::ScreenOff | Reflex::GoodNight => Some("silence"),
            // The house card shows what follows (done, with Undo).
            Reflex::Yes => None,
            Reflex::TimerLeft { .. } | Reflex::TimerList => Some("done"),
            Reflex::DoNotDisturb { .. } | Reflex::DoNotDisturbMorning | Reflex::DoNotDisturbOff => {
                Some("done")
            }
            Reflex::MusicPause | Reflex::MusicPlay | Reflex::MusicNext | Reflex::MusicPrevious => {
                Some("done")
            }
            // The voice itself is the answer; with nothing to repeat, the done sound.
            Reflex::Repeat if replay_for.is_some() => None,
            Reflex::Repeat => Some("done"),
            // The volume keys' own tick already plays at the new level.
            Reflex::VolumeUp | Reflex::VolumeDown | Reflex::VolumeSet { .. } => None,
        };
        let cue = cue_name.and_then(|n| self.clip(n));
        // Said aloud in Home Assistant's voice when it comes in time; else the sound, as
        // offline (never both).
        let say = spoken.unwrap_or_else(|| answer.clone());
        // Do not disturb is quiet, but the answer to the command that just switched it on is
        // said: the user asked and waits for it.
        let just_asked_quiet = matches!(
            cmd,
            Reflex::DoNotDisturb { .. } | Reflex::DoNotDisturbMorning | Reflex::GoodNight
        );
        let quiet = self.dnd(now_unix()) && !just_asked_quiet;
        let voiced = cue
            .as_ref()
            .filter(|_| crate::voice::speak_reflex(true, &answer, self.d.voice.is_some(), quiet));
        let id = self.next_voice_id;
        let online = self.ha.is_connected();
        self.speaking_text = say.clone();
        if let (Some(c), Some(v)) = (voiced, &self.d.voice) {
            v.say(id, &say, online);
            self.next_voice_id += 1;
            self.voice_wait = Some(VoiceWait {
                id,
                cue: c.clone(),
                text: say,
                deadline: Instant::now() + crate::voice::VOICE_WAIT,
            });
        } else if let Some(n) = cue_name {
            self.cue(n);
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

    /// The reflex's answer still on screen (not stopped, not replaced by a new conversation).
    fn reflex_showing(&self) -> bool {
        self.phase == Phase::Speaking && self.local_until.is_some()
    }

    /// The voice of a reflex's answer: played if still awaited, else (late) dropped.
    fn on_voice(&mut self, r: crate::voice::VoiceReply) {
        if self.asking_voice.as_ref().is_some_and(|a| a.voice == r.id) {
            self.on_ask_voice(r.speech);
            return;
        }
        if let Some((id, until)) = self.notice_wait
            && id == r.id
        {
            self.notice_wait = None;
            if let Some(sp) = r.speech
                && Instant::now() < until
                && self.phase != Phase::Ringing
            {
                let s = self.d.player.open_stream(sp.format);
                if let Err(e) = s.push(&sp.pcm) {
                    log::warn!("question notice: {e}");
                }
                s.finish();
                log::info!(event = "question_notice", seconds = sp.duration().as_secs_f64(); "question notice said aloud");
            }
            return;
        }
        // The words of a timer that rang: said, then the bell.
        if let Some((id, until)) = self.ring_wait
            && id == r.id
        {
            self.ring_wait = None;
            let mut after = Duration::ZERO;
            if let Some(sp) = r.speech
                && Instant::now() < until
                && self.phase == Phase::Ringing
            {
                let s = self.d.player.open_stream(sp.format);
                if let Err(e) = s.push(&sp.pcm) {
                    log::warn!("ring voice: {e}");
                }
                s.finish();
                after = sp.duration() + Duration::from_millis(400);
                self.speaking_since = Some(Instant::now());
                self.speaking_len = Some(sp.duration());
                log::info!(event = "ring_voice", seconds = sp.duration().as_secs_f64(); "the ringing said aloud: {:?}", self.speaking_text);
                self.ring_speech = Some(s);
            }
            if self.phase == Phase::Ringing {
                self.bell_at = Some(Instant::now() + after);
            }
            return;
        }
        // Words of a UiEvent::Say: played if they came in time and nothing else speaks.
        if let Some((id, until)) = self.say_wait
            && id == r.id
        {
            self.say_wait = None;
            if let Some(sp) = r.speech
                && Instant::now() < until
                && !self.speaker_busy()
                && matches!(self.phase, Phase::Home | Phase::Offline)
            {
                let s = self.d.player.open_stream(sp.format);
                if let Err(e) = s.push(&sp.pcm) {
                    log::warn!("say: {e}");
                }
                s.finish();
                log::info!(event = "say", seconds = sp.duration().as_secs_f64(); "said aloud");
            }
            return;
        }
        let Some(w) = self.voice_wait.take_if(|w| w.id == r.id) else {
            log::debug!("voice {}: too late, not played", r.id);
            return;
        };
        if !self.reflex_showing() {
            return;
        }
        let Some(sp) = r.speech else {
            log::info!(event = "reflex_voice", voiced = false; "no voice for {:?}: the sound", w.text);
            self.d.player.play(&w.cue);
            return;
        };
        let dur = sp.duration();
        self.speaking_since = Some(Instant::now());
        self.speaking_len = Some(dur);
        let s = self.d.player.open_stream(sp.format);
        if let Err(e) = s.push(&sp.pcm) {
            log::warn!("reflex voice: {e}");
        }
        s.finish();
        // "Ripeti" repeats it as it repeats Home Assistant's answers.
        self.tape.start(sp.format);
        self.tape.push(&sp.pcm);
        self.tape.commit(&w.text, Instant::now());
        log::info!(event = "reflex_voice", voiced = true, seconds = dur.as_secs_f64(); "reflex answer said aloud ({:.1} s)", dur.as_secs_f64());
        if let Some(old) = self.replay.replace(s) {
            old.abort();
        }
        // The view ends when it has played (AudioEvent::Finished, as a replay); the fallback.
        self.local_until = Some(Instant::now() + dur + Duration::from_secs(2));
    }

    /// The voice did not come in time: the reflex's sound, as offline.
    fn voice_late(&mut self) {
        if let Some(w) = self.voice_wait.take() {
            log::info!(event = "reflex_voice", voiced = false, late = true; "the voice is late for {:?}: the sound", w.text);
            if self.reflex_showing() {
                self.d.player.play(&w.cue);
            }
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

    /// A duration as words to say ("5 minuti e 30 secondi"), not as the screen's "05:30".
    fn said_duration(&self, secs: f64) -> String {
        let parts: Vec<String> = duration_parts(secs)
            .into_iter()
            .map(|(unit, n)| {
                let key = format!("say_{unit}{}", if n == 1 { "" } else { "s" });
                self.reflex_text(&key, &[("n", &n.to_string())])
            })
            .collect();
        match parts.split_last() {
            Some((last, rest)) if !rest.is_empty() => {
                let and = self.reflex_text("say_and", &[]);
                format!("{} {and} {last}", rest.join(", "))
            }
            Some((last, _)) => last.clone(),
            None => self.reflex_text("say_seconds", &[("n", "0")]),
        }
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
                self.stray_audio = false;
                if let Some(r) = self.run.clone() {
                    self.end_turn(r.conv, "");
                    self.run = None;
                }
                if self.gate.as_ref().is_some_and(|g| g.want.is_some()) {
                    // Held for Home Assistant: it is gone.
                    self.release_waiting();
                    return;
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
        let pipeline = matches!(
            m,
            Message::Transcript(_)
                | Message::Synthesize(_)
                | Message::AudioStart { .. }
                | Message::AudioChunk { .. }
                | Message::AudioStop { .. }
                | Message::VoiceStarted { .. }
                | Message::VoiceStopped { .. }
                | Message::Error { .. }
        );
        if pipeline {
            if let Some(r) = &mut self.run {
                r.last_heard = Instant::now();
            }
            let audio = matches!(
                m,
                Message::AudioStart { .. } | Message::AudioChunk { .. } | Message::AudioStop { .. }
            );
            let conv = self.conv.as_ref().map(|c| (c.id, c.trigger));
            match (&self.run, conv) {
                // The current conversation's run.
                (Some(r), Some((id, _))) if r.conv == id => {}
                // A run whose conversation is gone or replaced.
                (Some(_), _) => return self.on_previous_run(m),
                // Audio with no run: an announcement (or its end), unless a conversation of
                // ours is on (held back for the reflexes): then it is swallowed.
                (None, Some((_, t))) if t != Trigger::Announcement && audio => {
                    return self.on_stray_audio(m);
                }
                (None, None) if self.stray_audio && audio => return self.on_stray_audio(m),
                (None, _) if audio => {}
                // Its words after its audio (Home Assistant may send them in either order).
                (None, Some((id, _))) if self.last_run_conv == Some(id) => {}
                (None, _) => {
                    log::debug!("{} with no run open: ignored", m.kind());
                    return;
                }
            }
        }
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
                    && self.conv.as_ref().is_some_and(|c| !c.policy.swallows())
                {
                    self.end_listening();
                }
            }
            Message::Transcript(t) => self.on_transcript(t.text),
            Message::Synthesize(s) => {
                let mut quiet = false;
                if let Some(id) = self.conv.as_ref().map(|c| c.id) {
                    self.end_turn(id, &s.text);
                }
                if let Some(c) = &mut self.conv {
                    c.synth_at = Some(Instant::now());
                    if !c.policy.swallows() {
                        c.answer = s.text.clone();
                        quiet =
                            c.policy == AnswerPolicy::AfterCommand && quiet_after_command(&s.text);
                        if !quiet {
                            self.ui(UiCommand::Answer(s.text));
                        }
                    }
                }
                if quiet {
                    self.end_quietly();
                }
            }
            Message::AudioStart { format, .. } => self.on_audio_start(format),
            Message::AudioChunk { audio, .. } => self.on_audio_chunk(&audio),
            Message::AudioStop { .. } => {
                self.on_audio_stop();
                self.run_over("audio-stop");
            }
            Message::Error { text, code } => {
                self.on_error(text, code);
                self.run_over("error");
            }
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
        let (id, answering, swallowed) = (c.id, c.answering, c.policy.swallows());
        self.start_turn(id, &text, answering);
        if swallowed {
            return;
        }
        if self.phase == Phase::Listening {
            self.end_listening();
        }
        // Heard: the answer's words come within seconds (a web search included). An empty
        // answer (the model said nothing) or a model that hangs sends nothing at all: the
        // conversation ends then, not after the whole think timeout.
        let after = Instant::now() + AFTER_TRANSCRIPT;
        if self.think_deadline.is_none_or(|t| t > after) {
            self.think_deadline = Some(after);
        }
        // An empty transcript is followed by an `error`: wait for it.
        self.ui(UiCommand::Heard(text));
    }

    /// A message of Home Assistant's previous run (its conversation stopped, replaced, or the
    /// question's asking request): never shown nor played; its turn still reaches the brain,
    /// its `audio-stop` is acknowledged with `played` and its end lets a waiting utterance go.
    fn on_previous_run(&mut self, m: Message) {
        let Some(r) = self.run.clone() else { return };
        match m {
            Message::Transcript(t) => {
                log::info!(event = "previous_run", conversation = r.conv, text = t.text.as_str(); "transcript of conversation {}'s run: {}", r.conv, t.text);
                self.start_turn(r.conv, &t.text, r.answering);
            }
            Message::Synthesize(s) => {
                log::info!(event = "previous_run", conversation = r.conv, answer = s.text.as_str(); "answer of conversation {}'s run: swallowed", r.conv);
                self.end_turn(r.conv, &s.text);
            }
            Message::AudioStop { .. } => {
                if !r.played {
                    self.send(&Message::Played);
                }
                log::info!(event = "previous_run_over", conversation = r.conv; "conversation {}'s run ended: swallowed", r.conv);
                self.run_over("audio-stop");
            }
            Message::Error { text, .. } => {
                log::info!(event = "previous_run_over", conversation = r.conv, error = text.as_str(); "conversation {}'s run ended with an error: swallowed", r.conv);
                self.run_over("error");
            }
            other => log::trace!(
                "{} of conversation {}'s run: swallowed",
                other.kind(),
                r.conv
            ),
        }
    }

    /// Audio from Home Assistant with no run open while a conversation of ours is on (an
    /// announcement over it): swallowed, acknowledged with `played` at its end.
    fn on_stray_audio(&mut self, m: Message) {
        match m {
            Message::AudioStart { .. } => {
                log::info!(event = "announcement", muted = true; "announcement during a conversation: swallowed");
                self.stray_audio = true;
            }
            Message::AudioStop { .. } if self.stray_audio => {
                self.stray_audio = false;
                self.send(&Message::Played);
            }
            _ => {}
        }
    }

    fn on_audio_start(&mut self, format: AudioFormat) {
        let sf = StreamFormat {
            rate: format.rate,
            width: format.width,
            channels: format.channels,
        };
        match &mut self.conv {
            Some(c) if c.policy.swallows() => {
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
                    c.policy = AnswerPolicy::Swallow(Swallow::Stopped);
                    log::info!(event = "announcement", id = id, muted = true; "announcement {id}: muted (do not disturb)");
                    self.conv = Some(c);
                    return;
                }
                c.stream = Some(self.d.player.open_stream(sf));
                self.tape.start(sf);
                log::info!(event = "announcement", id = id; "announcement {id}");
                self.conv = Some(c);
                self.ui(UiCommand::Announcing(true));
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
        if c.policy.swallows() || c.stream.is_none() {
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
                let discard = c.policy.swallows();
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
            Some(c) if c.policy.swallows() => self.close("stopped"),
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
            AudioEvent::Finished(id)
                if self
                    .asking_voice
                    .as_ref()
                    .and_then(|a| a.stream.as_ref())
                    .is_some_and(|s| s.id() == id) =>
            {
                if let Some(q) = self.asking_voice.as_ref().map(|a| a.id) {
                    self.listen_for_question(q);
                }
            }
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
                        !c.policy.swallows()
                            && c.trigger != Trigger::Announcement
                            && asks_for_answer(&c.answer)
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
                } else if self.ring_speech.as_ref().is_some_and(|s| s.id() == id) {
                    self.ring_speech = None;
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
        let waiting =
            self.ring_wait.is_some() || self.bell_at.is_some() || self.ring_speech.is_some();
        if self.bell.is_none() && !waiting {
            // First the words ("Il timer «Pasta» è finito"), then the bell; without a voice in
            // time, the bell at once.
            let text = self.ring_words();
            match &self.d.voice {
                Some(v) if !text.is_empty() => {
                    let id = self.next_voice_id;
                    self.next_voice_id += 1;
                    v.say(id, &text, self.ha.is_connected());
                    self.speaking_text = text;
                    self.ring_wait = Some((id, Instant::now() + RING_VOICE_WAIT));
                }
                _ => self.start_bell(),
            }
        }
        self.show_ringing();
        self.refresh_rows(true);
        self.set_phase(Phase::Ringing);
    }

    fn start_bell(&mut self) {
        // The words are over: "basta" is the user's again.
        self.speaking_text.clear();
        self.speaking_since = None;
        self.speaking_len = None;
        if self.bell.is_none() && self.phase == Phase::Ringing {
            self.bell = Some(self.d.player.play_repeated(
                &self.cues.bell,
                self.cfg.bell_repeats.max(1),
                self.cfg.bell_gap,
            ));
        }
    }

    /// What the ringing says: each timer by its name (or its length), each alarm by its time.
    fn ring_words(&self) -> String {
        let words: Vec<String> = self
            .ringing
            .iter()
            .filter_map(|id| {
                let t = self.d.timers.get(id)?;
                Some(if self.alarm_ids.contains(id) {
                    // "7:30", not "07:30" (said "zero sette").
                    let l = t.label();
                    let time = l
                        .strip_prefix('0')
                        .filter(|r| r.starts_with(|c: char| c.is_ascii_digit()))
                        .unwrap_or(&l);
                    self.reflex_text("ring_alarm", &[("time", time)])
                } else if t.name.as_ref().is_some_and(|n| !n.trim().is_empty()) {
                    self.reflex_text("ring_timer_named", &[("name", &t.label())])
                } else {
                    let d = self.said_duration(t.total_seconds as f64);
                    self.reflex_text("ring_timer", &[("duration", &d)])
                })
            })
            .collect();
        words.join(" ")
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
        self.ring_wait = None;
        self.bell_at = None;
        if let Some(s) = self.ring_speech.take() {
            s.abort();
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
            UiEvent::Talk if self.mic_switch_off => {
                // It would listen to silence: the card says why.
                self.ui(UiCommand::MicSwitch(false));
            }
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
            UiEvent::FalseWake => self.on_false_wake(),
            UiEvent::Question(id, text) => self.on_question(id, text),
            UiEvent::QuestionAnswer { id, yes } => self.on_question_answer(id, yes),
            UiEvent::QuestionPending(id, until_ms) => self.on_question_pending(id, until_ms),
            UiEvent::QuestionClosed { id, yes, via } => self.on_question_closed(id, yes, &via),
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
            UiEvent::HouseDone => {
                let first = self.conv.as_mut().is_some_and(|c| {
                    let first = c.policy == AnswerPolicy::Play;
                    if first {
                        c.policy = AnswerPolicy::AfterCommand;
                    }
                    first
                });
                // The command went through: the "ok" at once, with the card (the AI's words
                // and their voice come many seconds later). Once per conversation, never over
                // an answer already speaking or the bell, and not again after a spoken yes's
                // own (QuestionPending).
                if first && self.dinged.is_none() {
                    self.success_cue();
                }
            }
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
                self.set_dnd(until);
                self.ui(UiCommand::DoNotDisturb(until));
            }
            UiEvent::TimerStart(secs, name) => {
                let id = self.d.timers.start_local(name, secs, now_unix());
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
                self.refresh_stop_listening();
            }
            UiEvent::AlarmDelete(id) => {
                self.d.alarms.remove(id);
                self.ui(UiCommand::Alarms(self.d.alarms.list().to_vec()));
            }
            UiEvent::Tap => {
                // Not over an answer, the bell or a stream (the speaker is busy then), and not
                // with do not disturb (the buttons are silent then, as the cards).
                if !self.speaker_busy()
                    && !matches!(self.phase, Phase::Speaking | Phase::Ringing)
                    && !self.dnd(now_unix())
                {
                    self.cue("tap");
                }
            }
            UiEvent::Home => {
                if !self.speaker_busy()
                    && !matches!(self.phase, Phase::Speaking | Phase::Ringing)
                    && !self.dnd(now_unix())
                {
                    self.cue("home");
                }
            }
            UiEvent::Say(text) => {
                if !self.speaker_busy() && !matches!(self.phase, Phase::Speaking | Phase::Ringing) {
                    self.cue("success");
                }
                if let Some(v) = &self.d.voice
                    && !self.dnd(now_unix())
                {
                    let id = self.next_voice_id;
                    self.next_voice_id += 1;
                    v.say(id, &text, self.ha.is_connected());
                    self.say_wait = Some((id, Instant::now() + Duration::from_secs(6)));
                }
            }
            UiEvent::Success => {
                if !self.speaker_busy() && !matches!(self.phase, Phase::Speaking | Phase::Ringing) {
                    self.cue("success");
                }
            }
            UiEvent::Reminder => {
                if self.dnd(now_unix()) {
                    log::info!("card sound muted (do not disturb)");
                } else if !self.speaker_busy() {
                    self.cue("reminder");
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

    /// The brain's question `id` is on the screen. While the character works on the request
    /// that asked it (thinking, or saying it), the spoken question is dropped (the screen shows
    /// it) and the microphone opens at once for "sì" or "no": a new conversation answering
    /// `id`. Any other time (home, a bell, an announcement) only the pop-up.
    fn on_question(&mut self, id: i64, text: String) {
        if self.question == Some(id) {
            // The brain's replay after a new subscription.
            return;
        }
        self.question = Some(id);
        self.question_text = text;
        if let Some(r) = &mut self.run {
            let now = Instant::now();
            r.asked_at = Some(now);
            r.last_heard = now;
        }
        let asked_here = self
            .conv
            .as_ref()
            .is_some_and(|c| c.trigger != Trigger::Announcement);
        if !asked_here || !matches!(self.phase, Phase::Thinking | Phase::Speaking) {
            log::info!(event = "question", id = id, listening = false, phase = self.phase.name(); "question {id} on the screen");
            return;
        }
        match self.phase {
            Phase::Thinking => {
                let end_stream = self
                    .conv
                    .as_ref()
                    .is_some_and(|c| !c.audio_stop_sent && c.transcript_at.is_none());
                if end_stream {
                    self.send(&Message::AudioStop { timestamp: None });
                }
                if let Some(c) = &mut self.conv {
                    c.policy = AnswerPolicy::Swallow(Swallow::Asked);
                    c.audio_stop_sent |= end_stream;
                }
            }
            _ => {
                if let Some(c) = &mut self.conv {
                    c.policy = AnswerPolicy::Swallow(Swallow::Asked);
                    if let Some(s) = c.stream.take() {
                        s.abort();
                    }
                }
                self.send_played();
                self.close("question");
            }
        }
        self.ask_aloud(id);
    }

    /// The question said aloud (the brain's own words, not the model's), then the microphone
    /// opens for the answer. Without a voice (offline, do not disturb, no voice in time) the
    /// microphone opens at once, the pop-up shows the question.
    fn ask_aloud(&mut self, id: i64) {
        let quiet = self.dnd(now_unix());
        match &self.d.voice {
            Some(v) if !quiet && !self.question_text.is_empty() => {
                let voice = self.next_voice_id;
                self.next_voice_id += 1;
                v.say(voice, &self.question_text, self.ha.is_connected());
                // What may be said right after the answer, ready in the cache by then (else a
                // yes's "Chiudo la tapparella." comes 1.5 s into its two seconds to take it back).
                if self.ha.is_connected() {
                    v.prepare(&self.yes_notice());
                    v.prepare(&self.reflex_text("question_no", &[]));
                }
                self.asking_voice = Some(AskVoice {
                    id,
                    voice,
                    deadline: Instant::now() + ASK_VOICE_WAIT,
                    stream: None,
                });
                self.speaking_text = self.question_text.clone();
                self.set_phase(Phase::Speaking);
                self.refresh_stop_listening();
                log::info!(event = "question_ask", id = id; "question {id}: said aloud, then listening");
            }
            _ => self.listen_for_question(id),
        }
    }

    /// The microphone opens for the answer to question `id`.
    fn listen_for_question(&mut self, id: i64) {
        self.asking_voice = None;
        if self.question != Some(id) {
            // Answered (touched) or closed while it was being said.
            self.go_idle();
            return;
        }
        log::info!(event = "question_listen", id = id; "question {id}: listening for the answer now");
        self.start_conversation_for(Trigger::FollowUp, Some(id));
    }

    /// The question's voice came: played, or (none) listening at once.
    fn on_ask_voice(&mut self, sp: Option<crate::voice::Speech>) {
        let Some(a) = &mut self.asking_voice else {
            return;
        };
        let id = a.id;
        match sp {
            Some(sp) if a.stream.is_none() => {
                let s = self.d.player.open_stream(sp.format);
                if let Err(e) = s.push(&sp.pcm) {
                    log::warn!("question voice: {e}");
                }
                s.finish();
                self.speaking_since = Some(Instant::now());
                self.speaking_len = Some(sp.duration());
                // Fallback if the end of the stream is never reported.
                a.deadline = Instant::now() + sp.duration() + Duration::from_secs(1);
                a.stream = Some(s);
            }
            _ => self.listen_for_question(id),
        }
    }

    /// A notice about the question said aloud ("Chiudo la tapparella.", "Va bene, lascio
    /// stare."), never with do not disturb (the cues are enough there).
    fn notice(&mut self, text: &str) {
        if text.is_empty() || self.dnd(now_unix()) {
            return;
        }
        if let Some(v) = &self.d.voice {
            let id = self.next_voice_id;
            self.next_voice_id += 1;
            v.say(id, text, self.ha.is_connected());
            self.notice_wait = Some((id, Instant::now() + Duration::from_secs(4)));
        }
    }

    /// Yes or No touched: the answer goes to the brain; the conversation that waited for it
    /// (or the one that asked it) ends without a word.
    fn on_question_answer(&mut self, id: i64, yes: bool) {
        log::info!(event = "question_answer", id = id, yes = yes, via = "screen"; "question {id}: {} (touched)", if yes { "yes" } else { "no" });
        self.brain_send(crate::brain_link::confirm_answer(
            id,
            yes,
            crate::brain_link::Via::Screen,
        ));
        let ours = self.conv.as_mut().is_some_and(|c| {
            let ours = c.answering == Some(id) || c.policy == AnswerPolicy::Swallow(Swallow::Asked);
            if ours {
                // Answered: its ending says nothing more to the brain.
                c.answering = None;
                c.policy = AnswerPolicy::Swallow(Swallow::Asked);
            }
            ours
        });
        if ours && self.phase.in_conversation() {
            self.stop_conversation();
        }
    }

    /// "Chiudo la tapparella?" → "Chiudo la tapparella.": what is about to happen after a yes.
    fn yes_notice(&self) -> String {
        format!(
            "{}.",
            self.question_text.trim().trim_end_matches('?').trim()
        )
    }

    /// A spoken yes was received: the success sound now (once for this question), the stop
    /// word listens until its commands run, and Home Assistant's words about it are swallowed.
    fn on_question_pending(&mut self, id: i64, until_ms: u64) {
        let left = until_ms.saturating_sub(now_unix_ms()).min(5_000);
        self.confirming = Some((id, Instant::now() + Duration::from_millis(left)));
        self.refresh_stop_listening();
        log::info!(event = "question_pending", id = id, left_ms = left; "question {id}: yes received, carried out in {left} ms");
        if self.dinged != Some(id) {
            self.dinged = Some(id);
            self.success_cue();
            let said = self.yes_notice();
            self.notice(&said);
        }
        let with_ha = self.gate.is_none() && self.phase.in_conversation();
        if let Some(c) = self.conv.as_mut().filter(|c| c.answering == Some(id)) {
            if with_ha {
                c.policy = AnswerPolicy::Swallow(Swallow::Asked);
            }
        }
    }

    /// The question is off the screen. Its answering conversation still held for the reflexes
    /// (nothing said yet) ends without a sound; one with Home Assistant goes on, its answer
    /// swallowed when the question was answered (the brain heard it in its transcript).
    fn on_question_closed(&mut self, id: i64, yes: Option<bool>, via: &str) {
        log::info!(event = "question_closed", id = id, answered = yes.is_some(), via = via; "question {id} closed ({via})");
        if self.question == Some(id) {
            self.question = None;
        }
        if self.confirming.is_some_and(|(c, _)| c == id) {
            self.confirming = None;
            self.refresh_stop_listening();
        }
        if self.dinged == Some(id) {
            self.dinged = None;
        }
        if let Some(a) = self.asking_voice.take_if(|a| a.id == id) {
            // Answered on the screen while it was being said.
            if let Some(s) = a.stream {
                s.abort();
            }
            if self.phase == Phase::Speaking {
                self.go_idle();
            }
        }
        match (yes, via) {
            (Some(false), _) => {
                let t = self.reflex_text("question_no", &[]);
                self.notice(&t);
            }
            // Unanswered: the brain's timeout, or the screen's own safety net a moment before it
            // (never a question replaced by a newer one).
            (None, v) if v != "replaced" => {
                let t = self.reflex_text("question_expired", &[]);
                self.notice(&t);
            }
            _ => {}
        }
        let held = self.gate.as_ref().is_some_and(|g| g.want.is_none());
        let Some(c) = self.conv.as_mut().filter(|c| c.answering == Some(id)) else {
            return;
        };
        c.answering = None;
        if self.phase == Phase::Listening && held {
            log::info!(event = "question_window_over", id = id; "question {id} over: its listening ends");
            if c.policy == AnswerPolicy::Play {
                c.policy = AnswerPolicy::Swallow(Swallow::Asked);
            }
            self.stop_conversation();
        } else if yes.is_some() && self.phase.in_conversation() {
            c.policy = AnswerPolicy::Swallow(Swallow::Asked);
        }
    }

    /// Stop (the button, the stop word, "Falso allarme"). In the conversation opened to hear
    /// the answer to the brain's question, or in the second a spoken yes can be taken back, it
    /// is a No; never otherwise (a question open on the screen stays).
    fn on_stop(&mut self) {
        log::info!(event = "stop", phase = self.phase.name(); "Stop pressed in {}", self.phase.name());
        if let Some(a) = self.asking_voice.take() {
            // "No" or "stop" over the question being said: a No.
            log::info!(event = "question_answer", id = a.id, yes = false, via = "stop"; "question {}: no (stop over the question)", a.id);
            if let Some(s) = a.stream {
                s.abort();
            }
            self.brain_send(crate::brain_link::confirm_answer(
                a.id,
                false,
                crate::brain_link::Via::Stop,
            ));
            self.go_idle();
            return;
        }
        let answering = self
            .conv
            .as_ref()
            .and_then(|c| c.answering)
            .filter(|_| self.phase.in_conversation());
        let confirming = self.confirming_now();
        if let Some(id) = answering.or(confirming) {
            log::info!(event = "question_answer", id = id, yes = false, via = "stop"; "question {id}: no (stop)");
            self.brain_send(crate::brain_link::confirm_answer(
                id,
                false,
                crate::brain_link::Via::Stop,
            ));
            if let Some(c) = self.conv.as_mut().filter(|c| c.answering == Some(id)) {
                c.answering = None;
            }
            if answering.is_none() && matches!(self.phase, Phase::Home | Phase::Offline) {
                return;
            }
        }
        self.stop_conversation();
    }

    /// What Stop does to what is going on (no answer to the brain's question).
    fn stop_conversation(&mut self) {
        if self.phase == Phase::Listening && self.gate.is_some() {
            // Held back for the reflexes: Home Assistant never heard of it.
            if let (Some(g), Some(r)) = (self.gate.take(), &self.d.reflexes) {
                if g.st.is_some() && g.want.is_none() {
                    r.link.send(ReflexRequest::End(g.id));
                }
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
            self.voice_wait = None;
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
                    c.swallow(Swallow::Stopped);
                    c.audio_stop_sent = true;
                }
                self.silence_turn();
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
                    c.swallow(Swallow::Stopped);
                    c.audio_stop_sent |= end_stream;
                }
                self.silence_turn();
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

    /// The current conversation's answer will not be heard: its turn ends now with nothing
    /// said (or, before the transcript, when Home Assistant's words come).
    fn silence_turn(&mut self) {
        let Some(id) = self.conv.as_ref().map(|c| c.id) else {
            return;
        };
        self.turn_of(id).silenced = true;
        self.end_turn_unheard(id);
    }

    // ---- music and the control center ----------------------------------------------------

    /// Back to waiting for the wake word, or to nothing while the microphone is switched off.
    fn mic_idle(&self) {
        self.d.mic.set(if self.mic_muted || self.mic_switch_off {
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
            mic_on: !self.mic_muted && !self.mic_switch_off,
            mic_switch_off: self.mic_switch_off,
        });
    }

    /// The device's switch moved (or is read at start). Muted: whatever listens stops (Stop),
    /// the wake word waits; on: back to the wake word, the detector reset (the switch's click
    /// is in its window).
    fn on_mic_switch(&mut self, on: bool, initial: bool) {
        if self.mic_switch_off == !on && !initial {
            return;
        }
        self.mic_switch_off = !on;
        log::info!(event = "mic_hw_switch", on = on, initial = initial; "microphone switch {}", if on { "on" } else { "muted" });
        if !on && self.phase == Phase::Listening {
            self.stop_conversation();
        }
        if !self.phase.in_conversation() {
            self.mic_idle();
            self.d.mic.request_reset();
        }
        self.send_control();
        if !initial {
            self.ui(UiCommand::MicSwitch(on));
        }
    }

    fn on_control_tile(&mut self, key: &str) {
        match key {
            "mic" if self.mic_switch_off => {
                self.ui(UiCommand::MicSwitch(false));
            }
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
            crate::airplay::MusicEvent::Spectrum(b, p) => {
                self.ui(UiCommand::MusicSpectrum(b.to_vec(), p.to_vec()));
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
            || self.ring_speech.is_some()
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
            self.cue("tick");
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
        if let Some(q) = self
            .asking_voice
            .as_ref()
            .filter(|a| now >= a.deadline)
            .map(|a| a.id)
        {
            log::info!(event = "question_ask_late", id = q; "question {q}: its voice did not come or end in time: listening");
            if let Some(s) = self.asking_voice.as_mut().and_then(|a| a.stream.take()) {
                s.abort();
            }
            self.listen_for_question(q);
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
        if self.phase == Phase::Listening
            && self.gate.is_none()
            && self.listen_deadline.is_some_and(|t| now >= t)
        {
            log::warn!(event = "timeout", stage = "listening"; "no answer from Home Assistant while listening");
            self.send(&Message::AudioStop { timestamp: None });
            self.fail("timeout");
        }
        if self.think_deadline.is_some_and(|t| now >= t) {
            self.think_deadline = None;
            let conv = self.conv.as_ref().map(|c| c.id);
            match &self.conv {
                Some(c) if c.policy.swallows() => self.close("stopped"),
                // The command was carried out and its card is up: the AI's words never came
                // (it hung after the tool), but nothing failed for the user.
                Some(c)
                    if c.policy == AnswerPolicy::AfterCommand && self.phase == Phase::Thinking =>
                {
                    log::warn!(event = "timeout", stage = "thinking", house_done = true; "no answer from Home Assistant after a house command: ended quietly");
                    self.mic_idle();
                    self.d.mic.request_reset();
                    self.stop_recorder();
                    self.close("house_done_silent");
                    self.listen_deadline = None;
                    self.wave.clear();
                    self.go_idle();
                }
                Some(_) if self.phase == Phase::Thinking => {
                    log::warn!(event = "timeout", stage = "thinking"; "no answer from Home Assistant");
                    self.fail("timeout");
                }
                _ => {}
            }
            if conv.is_some() && self.run.as_ref().map(|r| r.conv) == conv {
                self.run_over("think timeout");
            }
        }
        // A run nobody ended (its conversation went on without it).
        if self
            .run
            .as_ref()
            .is_some_and(|r| r.opened.elapsed() > self.cfg.listen_timeout + self.cfg.think_timeout)
        {
            log::warn!(event = "timeout", stage = "run"; "Home Assistant's run never ended: forgotten");
            self.run_over("too old");
        }
        self.release_waiting();
        if self.confirming.is_some_and(|(_, t)| now >= t) {
            self.confirming = None;
            self.refresh_stop_listening();
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

/// Milliseconds since the epoch.
fn now_unix_ms() -> u64 {
    (now_unix() * 1000.0) as u64
}

/// Seconds since the local midnight at `t` (seconds since the epoch).
pub(crate) fn local_seconds_of_day(t: i64) -> i64 {
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

/// A duration in whole hours, minutes and seconds (rounded up, as the screen shows it), the
/// units that are not zero: [("hour", 1), ("second", 5)].
fn duration_parts(secs: f64) -> Vec<(&'static str, u64)> {
    let s = secs.max(0.0).ceil() as u64;
    [
        ("hour", s / 3600),
        ("minute", s / 60 % 60),
        ("second", s % 60),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .collect()
}

/// "Non disturbare" without a duration: in the evening and at night (21:00-07:00) until the
/// morning, as one says it before going to bed; by day for an hour, so it is not forgotten on.
/// The answer ends with a question that waits for one: "A che ora?" yes, a closing courtesy
/// ("Sono le 18:32. Ha bisogno d'altro?") no — that one would open the microphone on whoever
/// talks in the room.
/// The live stream's end of speech: the reflexes' detector, with a longer silence than theirs
/// (a pause inside a sentence, "metti una sveglia alle… sette", must not cut it).
fn live_speech_over(v: &Vad) -> bool {
    v.speech_ms() >= Vad::MIN_SPEECH_MS && v.silence_ms() >= LIVE_END_SILENCE_MS
}

/// See [`live_speech_over`].
const LIVE_END_SILENCE_MS: u32 = 1_500;

fn asks_for_answer(answer: &str) -> bool {
    let a = answer.trim_end();
    if !a.ends_with('?') {
        return false;
    }
    // The last sentence only.
    let body = &a[..a.len() - 1];
    let last = body
        .rfind(['.', '!', '?', '\n'])
        .map_or(body, |i| &body[i + 1..])
        .to_lowercase();
    const COURTESY: &[&str] = &[
        "posso aiutar",
        "posso assister",
        "posso fare altro",
        "posso esserle utile",
        "posso esserti utile",
        "ulteriore aiuto",
        "bisogno d'altro",
        "bisogno di altro",
        "serve altro",
        "c'è altro",
        "c'è qualcos'altro",
        "altro in cui",
        "desidera altro",
        "vuole altro",
        "vuoi altro",
        "qualcos'altro",
        "in altro modo",
        "anything else",
        "can i help",
        "help with anything",
        "something else",
    ];
    !COURTESY.iter().any(|c| last.contains(c))
}

pub(crate) fn dnd_default_until(now: i64, local: i64) -> u64 {
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

/// After a house command that went through, an answer this short only confirms it ("Fatto.",
/// "Ho acceso la luce dello studio."): not spoken. A question or a longer answer (something
/// failed, or the user also asked something) still is.
const QUIET_MAX_WORDS: usize = 8;

fn quiet_after_command(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && !t.ends_with('?') && t.split_whitespace().count() <= QUIET_MAX_WORDS
}

#[cfg(test)]
mod tests {

    #[test]
    fn courtesy_questions_do_not_open_the_microphone() {
        assert!(asks_for_answer("A che ora devo ricordarglielo?"));
        assert!(asks_for_answer("Chiudo la tapparella del salotto?"));
        assert!(!asks_for_answer(
            "Sono le 18:32 di oggi. Ha bisogno di ulteriore aiuto?"
        ));
        assert!(!asks_for_answer(
            "Non posso farlo. Posso assisterLa in altro modo?"
        ));
        assert!(!asks_for_answer("Fatto. C'è altro?"));
        assert!(!asks_for_answer("Done. Anything else I can do?"));
        assert!(!asks_for_answer("Fatto."));
    }
    use super::*;

    #[test]
    fn a_stop_word_like_a_word_being_said_is_the_bodys_voice_near_that_word() {
        let secs = |s: f32| Some(Duration::from_secs_f32(s));
        let text = "Tè un minuto e 45 secondi; Pasta 4 minuti e 32 secondi.";
        // 55 characters in 6.2 s: "Pasta" (28-32) at about 3.4 s.
        let e = echo_at("basta", text, secs(4.0), secs(6.2)).unwrap();
        assert_eq!(e.word, "Pasta");
        assert!((e.at_s - 3.4).abs() < 0.3, "{}", e.at_s);
        assert!(e.ignored && !e.estimated);
        // Heard long before the word: the user's.
        let e = echo_at("basta", text, secs(0.5), secs(6.2)).unwrap();
        assert!(!e.ignored);
        // A long answer: only near the word.
        let long = format!(
            "{} Ne ho abbastanza. {}",
            "parole ".repeat(40),
            "altre ".repeat(40)
        );
        let e = echo_at("basta", &long, secs(1.0), None).unwrap();
        assert_eq!(e.word, "abbastanza");
        assert!(e.estimated && !e.ignored, "{e:?}");
        let at = e.at_s;
        assert!(
            echo_at("basta", &long, secs(at + 1.0), None)
                .unwrap()
                .ignored
        );
        // No start known: ignored as before (the whole phrase).
        assert!(
            echo_at("basta", "Il timer «Pasta» è finito.", None, None)
                .unwrap()
                .ignored
        );
        assert_eq!(
            echo_at("basta", "Il timer «Tè» è finito.", secs(1.0), None),
            None
        );
        assert_eq!(echo_at("stop", "Pasta pronta.", secs(1.0), None), None);
        assert!(
            echo_at("stop_alfred", "Stop.", secs(0.2), None)
                .unwrap()
                .ignored
        );
        assert!(one_letter_away("basta", "basti"));
        assert!(!one_letter_away("basta", "bestia"));
        assert_eq!(word_spans("È «Pasta»!"), vec![(0, "È"), (3, "Pasta")]);
    }

    #[test]
    fn durations_to_say() {
        assert_eq!(duration_parts(300.0), vec![("minute", 5)]);
        assert_eq!(duration_parts(59.2), vec![("minute", 1)]);
        assert_eq!(duration_parts(3605.0), vec![("hour", 1), ("second", 5)]);
        assert_eq!(
            duration_parts(5430.0),
            vec![("hour", 1), ("minute", 30), ("second", 30)]
        );
        assert_eq!(duration_parts(0.0), vec![]);
    }

    #[test]
    fn short_answers_after_a_command_are_not_spoken() {
        assert!(quiet_after_command("Fatto."));
        assert!(quiet_after_command("Ho acceso la luce dello studio."));
        assert!(quiet_after_command(" Done. "));
        assert!(!quiet_after_command(""));
        assert!(!quiet_after_command("Vuoi che accenda anche la cucina?"));
        assert!(!quiet_after_command(
            "Ho acceso la luce dello studio, ma in cucina non trovo nessuna lampada."
        ));
    }

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
