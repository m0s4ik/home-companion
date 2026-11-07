// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A scripted Home Assistant that drives a satellite through scenarios.
//!
//! It connects like Home Assistant does (Home Assistant is the TCP client): `Probe` is the config
//! flow's short `describe -> info -> close` on a separate connection; `Connect` opens the live
//! connection. With [`Scenario::ping`] the runner behaves like `_run_pipeline_loop`: from
//! `RunSatellite` until `Pause`/`Disconnect` it sends `ping` 2 s after every `pong`, and fails
//! the scenario with "no event within 5 s" when the satellite stays silent for 5 s
//! (`_PING_TIMEOUT`). The satellite's own pings are always answered.
//!
//! Checks applied to every message, whatever the current step: microphone `audio-chunk`s must be
//! 16000 Hz / 2 bytes / 1 channel with non-decreasing timestamps; `played` is only valid after
//! an `audio-stop` sent by us that was not answered yet ("played before audio-stop"); the
//! satellite must not close the live connection.

use std::io::BufReader;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Error;
use crate::client::Client;
use crate::event::{encode_event, read_event, with_buffers};
use crate::info::Info;
use crate::message::{
    AudioFormat, Message, Stage, Synthesize, TimerStarted, TimerUpdated, Transcript, Voice,
};

/// Home Assistant's `_PING_TIMEOUT`.
pub const PING_TIMEOUT: Duration = Duration::from_secs(5);
/// Home Assistant's `_PING_SEND_DELAY`.
pub const PING_SEND_DELAY: Duration = Duration::from_secs(2);
/// Home Assistant's `_AUDIO_CHUNK_BYTES`.
pub const AUDIO_CHUNK_BYTES: usize = 2048;

/// A scenario: steps run in order; the first failure stops it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Mimic Home Assistant's ping and 5 s rule (see the module docs). Default true.
    #[serde(default = "yes")]
    pub ping: bool,
    pub steps: Vec<Step>,
}

fn yes() -> bool {
    true
}

/// One step. In TOML: `[[steps]]` tables with `step = "kebab-case-name"` and the fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Step {
    /// Separate connection: `describe`, expect `info` within 5 s, close (the config flow).
    Probe,
    /// Open the live connection (closing a previous one).
    Connect,
    RunSatellite,
    Describe,
    /// Expect `info` with `satellite.installed == true` and at least one wake model.
    ExpectInfo {
        timeout_ms: u64,
    },
    Pause,
    Disconnect,
    Sleep {
        ms: u64,
    },
    /// Expect, in order, `detection` with a name, `run-pipeline` asr -> tts, `audio-start`
    /// 16000/2/1.
    WaitDetection {
        timeout_ms: u64,
    },
    /// Read microphone audio until `min_ms` of it arrived (`transcribe` on the first chunk,
    /// `voice-started` after 300 ms of audio, `voice-stopped` at `min_ms`); fail after `max_ms`.
    CollectAudio {
        min_ms: u64,
        max_ms: u64,
    },
    SendTranscript {
        text: String,
    },
    SendSynthesize {
        text: String,
    },
    /// `audio-start` / 2048-byte `audio-chunk`s / `audio-stop` at the WAV's format, timestamps as
    /// Home Assistant's `_stream_tts` writes them.
    SendWav {
        path: PathBuf,
    },
    /// Expect `played`; the first one sets `audio_stop_to_played_ms`.
    ExpectPlayed {
        timeout_ms: u64,
    },
    SendError {
        text: String,
        code: String,
    },
    TimerStarted(TimerStarted),
    TimerUpdated(TimerUpdated),
    TimerCancelled {
        id: String,
    },
    TimerFinished {
        id: String,
    },
    /// An announcement outside a pipeline as `async_announce` sends it (`audio-start` with
    /// timestamp 0, chunks, `audio-stop` without timestamp), then expect `played` within the
    /// audio length + 2 s.
    Announce {
        path: PathBuf,
    },
    /// Expect the satellite to end its microphone stream (Stop button, timeout).
    ExpectAudioStop {
        timeout_ms: u64,
    },
}

/// What happened, written as JSON by `fake-ha --out`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Record {
    /// The `info` data (from `ExpectInfo`, else from the first `Probe`).
    pub info: Option<Value>,
    /// Every message received: seconds since the scenario started, type, data (no payload).
    pub received: Vec<(f64, String, Value)>,
    /// Seconds of microphone audio received.
    pub audio_seconds: f64,
    pub detection_to_first_chunk_ms: Option<f64>,
    pub audio_stop_to_played_ms: Option<f64>,
    pub failures: Vec<String>,
}

/// State shared by the step runner, the reader and the pinger.
struct State {
    t0: Instant,
    record: Record,
    ping_enabled: bool,
    /// Between RunSatellite and Pause/Disconnect, as Home Assistant's pipeline loop.
    loop_active: bool,
    last_event: Instant,
    next_ping: Option<Instant>,
    /// `audio-stop`s we sent that were not answered by `played` yet.
    stops_pending: u32,
    last_stop_at: Option<Instant>,
    detection_at: Option<Instant>,
    last_chunk_ts: Option<i64>,
    audio_bytes: u64,
    /// We are closing the live connection: EOF is expected.
    closing: bool,
}

impl State {
    fn fail(&mut self, why: String) {
        log::warn!("fake-ha: FAILURE: {why}");
        self.record.failures.push(why);
    }
}

type Shared = Arc<Mutex<State>>;

fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

struct Live {
    writer: Arc<Mutex<TcpStream>>,
    rx: Receiver<(Instant, Message)>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Live {
    fn close(mut self, state: &Shared) {
        lock(state).closing = true;
        self.stop.store(true, Ordering::SeqCst);
        let _ = self
            .writer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .shutdown(Shutdown::Both);
        for h in self.threads.drain(..) {
            let _ = h.join();
        }
    }
}

fn write(writer: &Mutex<TcpStream>, m: &Message) -> std::io::Result<()> {
    use std::io::Write;
    let e = m.to_event();
    with_buffers(|buf, scratch| {
        encode_event(buf, scratch, &e.kind, &e.data, e.payload.as_deref());
        let mut w = writer.lock().unwrap_or_else(|p| p.into_inner());
        w.write_all(buf)?;
        w.flush()
    })
}

/// Runs a scenario against the satellite at `addr`. `Err` only when a connection cannot be
/// opened; failed checks go to [`Record::failures`].
pub fn run(addr: SocketAddr, scenario: &Scenario) -> Result<Record, Error> {
    let now = Instant::now();
    let state: Shared = Arc::new(Mutex::new(State {
        t0: now,
        record: Record::default(),
        ping_enabled: scenario.ping,
        loop_active: false,
        last_event: now,
        next_ping: None,
        stops_pending: 0,
        last_stop_at: None,
        detection_at: None,
        last_chunk_ts: None,
        audio_bytes: 0,
        closing: false,
    }));
    let mut live: Option<Live> = None;
    let mut result = Ok(());
    for (i, step) in scenario.steps.iter().enumerate() {
        log::info!("fake-ha: step {}: {step:?}", i + 1);
        match run_step(addr, step, &state, &mut live) {
            Ok(()) => {}
            Err(StepError::Failed) => {}
            Err(StepError::Io(e)) => {
                result = Err(e);
                break;
            }
        }
        if !lock(&state).record.failures.is_empty() {
            break;
        }
    }
    if let Some(l) = live.take() {
        l.close(&state);
    }
    result?;
    let mut st = lock(&state);
    st.record.audio_seconds = st.audio_bytes as f64 / AudioFormat::MIC.bytes_per_second() as f64;
    Ok(std::mem::take(&mut st.record))
}

enum StepError {
    /// A check failed (already in `failures`).
    Failed,
    /// Could not connect.
    Io(Error),
}

type StepResult = Result<(), StepError>;

fn failed(state: &Shared, why: String) -> StepError {
    lock(state).fail(why);
    StepError::Failed
}

fn need_live<'a>(
    live: &'a mut Option<Live>,
    state: &Shared,
    step: &str,
) -> Result<&'a mut Live, StepError> {
    match live {
        Some(l) => Ok(l),
        None => Err(failed(
            state,
            format!("{step}: not connected (missing Connect step)"),
        )),
    }
}

fn send(live: &Live, state: &Shared, m: &Message) -> StepResult {
    write(&live.writer, m).map_err(|e| failed(state, format!("sending {}: {e}", m.kind())))
}

/// Waits for a message accepted by `f` (which returns `Some` to stop), checking for failures
/// every 100 ms.
fn wait_for<T>(
    live: &Live,
    state: &Shared,
    timeout: Duration,
    what: &str,
    mut f: impl FnMut(Instant, &Message) -> Result<Option<T>, String>,
) -> Result<T, StepError> {
    let deadline = Instant::now() + timeout;
    loop {
        if !lock(state).record.failures.is_empty() {
            return Err(StepError::Failed);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(failed(
                state,
                format!("{what}: nothing within {} ms", timeout.as_millis()),
            ));
        }
        match live
            .rx
            .recv_timeout((deadline - now).min(Duration::from_millis(100)))
        {
            Ok((at, m)) => match f(at, &m) {
                Ok(Some(t)) => return Ok(t),
                Ok(None) => {}
                Err(why) => return Err(failed(state, format!("{what}: {why}"))),
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // The reader ended; it recorded why.
                if lock(state).record.failures.is_empty() {
                    return Err(failed(state, format!("{what}: connection closed")));
                }
                return Err(StepError::Failed);
            }
        }
    }
}

fn check_info(v: &Value) -> Result<(), String> {
    let info = Info::from_json(v);
    if !info.satellite_installed() {
        return Err("info without an installed satellite".into());
    }
    if info.wake_model_names().is_empty() {
        return Err("info without a wake model".into());
    }
    Ok(())
}

fn run_step(addr: SocketAddr, step: &Step, state: &Shared, live: &mut Option<Live>) -> StepResult {
    match step {
        Step::Probe => probe(addr, state),
        Step::Connect => {
            if let Some(l) = live.take() {
                l.close(state);
            }
            *live = Some(connect(addr, state).map_err(StepError::Io)?);
            Ok(())
        }
        Step::RunSatellite => {
            let l = need_live(live, state, "run-satellite")?;
            send(l, state, &Message::RunSatellite)?;
            let mut st = lock(state);
            st.loop_active = true;
            st.last_event = Instant::now();
            st.next_ping = Some(Instant::now() + PING_SEND_DELAY);
            Ok(())
        }
        Step::Describe => send(
            need_live(live, state, "describe")?,
            state,
            &Message::Describe,
        ),
        Step::ExpectInfo { timeout_ms } => {
            let l = need_live(live, state, "expect-info")?;
            let v = wait_for(
                l,
                state,
                Duration::from_millis(*timeout_ms),
                "expect-info",
                |_, m| match m {
                    Message::Info(i) => Ok(Some(i.to_json())),
                    _ => Ok(None),
                },
            )?;
            check_info(&v).map_err(|e| failed(state, format!("expect-info: {e}")))?;
            lock(state).record.info = Some(v);
            Ok(())
        }
        Step::Pause => {
            let l = need_live(live, state, "pause")?;
            send(l, state, &Message::PauseSatellite)?;
            let mut st = lock(state);
            st.loop_active = false;
            st.next_ping = None;
            Ok(())
        }
        Step::Disconnect => {
            if let Some(l) = live.take() {
                l.close(state);
            }
            let mut st = lock(state);
            st.loop_active = false;
            st.next_ping = None;
            Ok(())
        }
        Step::Sleep { ms } => {
            let end = Instant::now() + Duration::from_millis(*ms);
            loop {
                if !lock(state).record.failures.is_empty() {
                    return Err(StepError::Failed);
                }
                let now = Instant::now();
                if now >= end {
                    return Ok(());
                }
                std::thread::sleep((end - now).min(Duration::from_millis(50)));
            }
        }
        Step::WaitDetection { timeout_ms } => {
            let l = need_live(live, state, "wait-detection")?;
            let mut stage = 0;
            wait_for(
                l,
                state,
                Duration::from_millis(*timeout_ms),
                "wait-detection",
                |_, m| {
                    match (stage, m) {
                        (0, Message::Detection(d)) => {
                            if d.name.is_none() {
                                return Err("detection without a name".into());
                            }
                            stage = 1;
                        }
                        (1, Message::RunPipeline(p)) => {
                            if p.start_stage != Stage::Asr || p.end_stage != Stage::Tts {
                                return Err(format!(
                                    "run-pipeline {:?} -> {:?}, expected asr -> tts",
                                    p.start_stage, p.end_stage
                                ));
                            }
                            stage = 2;
                        }
                        (2, Message::AudioStart { format, .. }) => {
                            if *format != AudioFormat::MIC {
                                return Err(format!("audio-start {format:?}, expected 16000/2/1"));
                            }
                            return Ok(Some(()));
                        }
                        (
                            _,
                            Message::Detection(_)
                            | Message::RunPipeline(_)
                            | Message::AudioStart { .. },
                        ) => {
                            return Err(format!(
                                "unexpected {} (expected detection, run-pipeline, audio-start in order)",
                                m.kind()
                            ));
                        }
                        _ => {}
                    }
                    Ok(None)
                },
            )
        }
        Step::CollectAudio { min_ms, max_ms } => {
            let l = need_live(live, state, "collect-audio")?;
            let mut ms = 0u64;
            let mut first = true;
            let mut started = false;
            let (min_ms, writer) = (*min_ms, l.writer.clone());
            wait_for(
                l,
                state,
                Duration::from_millis(*max_ms),
                "collect-audio",
                |_, m| {
                    match m {
                        Message::AudioChunk { format, audio, .. } => {
                            if first {
                                first = false;
                                write(
                                    &writer,
                                    &Message::Transcribe {
                                        language: Some("en".into()),
                                    },
                                )
                                .map_err(|e| e.to_string())?;
                            }
                            ms += format.millis(audio.len()).max(0) as u64;
                            if !started && ms >= 300 {
                                started = true;
                                write(
                                    &writer,
                                    &Message::VoiceStarted {
                                        timestamp: Some(ms as i64),
                                    },
                                )
                                .map_err(|e| e.to_string())?;
                            }
                            if ms >= min_ms {
                                write(
                                    &writer,
                                    &Message::VoiceStopped {
                                        timestamp: Some(ms as i64),
                                    },
                                )
                                .map_err(|e| e.to_string())?;
                                return Ok(Some(()));
                            }
                        }
                        Message::AudioStop { .. } => {
                            return Err(format!("audio-stop after only {ms} ms of audio"));
                        }
                        _ => {}
                    }
                    Ok(None)
                },
            )
        }
        Step::SendTranscript { text } => send(
            need_live(live, state, "send-transcript")?,
            state,
            &Message::Transcript(Transcript {
                text: text.clone(),
                language: None,
            }),
        ),
        Step::SendSynthesize { text } => send(
            need_live(live, state, "send-synthesize")?,
            state,
            &Message::Synthesize(Synthesize {
                text: text.clone(),
                voice: Some(Voice {
                    name: None,
                    language: Some("en".into()),
                    speaker: None,
                }),
            }),
        ),
        Step::SendWav { path } => {
            let l = need_live(live, state, "send-wav")?;
            send_audio(l, state, path, true).map(|_| ())
        }
        Step::ExpectPlayed { timeout_ms } => {
            let l = need_live(live, state, "expect-played")?;
            let at = wait_for(
                l,
                state,
                Duration::from_millis(*timeout_ms),
                "expect-played",
                |at, m| Ok(matches!(m, Message::Played).then_some(at)),
            )?;
            let mut st = lock(state);
            if st.record.audio_stop_to_played_ms.is_none() {
                if let Some(stop) = st.last_stop_at {
                    st.record.audio_stop_to_played_ms = Some(ms_between(stop, at));
                }
            }
            Ok(())
        }
        Step::SendError { text, code } => send(
            need_live(live, state, "send-error")?,
            state,
            &Message::Error {
                text: text.clone(),
                code: Some(code.clone()),
            },
        ),
        Step::TimerStarted(t) => send(
            need_live(live, state, "timer-started")?,
            state,
            &Message::TimerStarted(t.clone()),
        ),
        Step::TimerUpdated(t) => send(
            need_live(live, state, "timer-updated")?,
            state,
            &Message::TimerUpdated(t.clone()),
        ),
        Step::TimerCancelled { id } => send(
            need_live(live, state, "timer-cancelled")?,
            state,
            &Message::TimerCancelled { id: id.clone() },
        ),
        Step::TimerFinished { id } => send(
            need_live(live, state, "timer-finished")?,
            state,
            &Message::TimerFinished { id: id.clone() },
        ),
        Step::Announce { path } => {
            let l = need_live(live, state, "announce")?;
            let length = send_audio(l, state, path, false)?;
            let timeout = length + Duration::from_secs(2);
            wait_for(l, state, timeout, "announce: expect played", |_, m| {
                Ok(matches!(m, Message::Played).then_some(()))
            })
        }
        Step::ExpectAudioStop { timeout_ms } => {
            let l = need_live(live, state, "expect-audio-stop")?;
            wait_for(
                l,
                state,
                Duration::from_millis(*timeout_ms),
                "expect-audio-stop",
                |_, m| Ok(matches!(m, Message::AudioStop { .. }).then_some(())),
            )
        }
    }
}

fn ms_between(a: Instant, b: Instant) -> f64 {
    b.saturating_duration_since(a).as_secs_f64() * 1000.0
}

/// Sends a WAV as Home Assistant does (`_stream_tts` when `tts`, else `async_announce`).
/// Returns the audio length.
fn send_audio(l: &Live, state: &Shared, path: &Path, tts: bool) -> Result<Duration, StepError> {
    let (format, pcm) =
        read_wav(path).map_err(|e| failed(state, format!("{}: {e}", path.display())))?;
    send(
        l,
        state,
        &Message::AudioStart {
            format,
            timestamp: Some(0),
        },
    )?;
    let mut ts = 0i64;
    for chunk in pcm.chunks(AUDIO_CHUNK_BYTES) {
        send(
            l,
            state,
            &Message::AudioChunk {
                format,
                timestamp: Some(ts),
                audio: chunk.to_vec(),
            },
        )?;
        ts += format.millis(chunk.len());
    }
    // Hold the state lock while writing audio-stop, so a `played` the reader sees afterwards is
    // always counted against it.
    let mut st = lock(state);
    let stop = Message::AudioStop {
        timestamp: tts.then_some(ts),
    };
    if let Err(e) = write(&l.writer, &stop) {
        st.fail(format!("sending audio-stop: {e}"));
        return Err(StepError::Failed);
    }
    st.stops_pending += 1;
    st.last_stop_at = Some(Instant::now());
    let bytes = format.bytes_per_second().max(1);
    Ok(Duration::from_secs_f64(pcm.len() as f64 / bytes as f64))
}

/// Reads a PCM WAV (8/16/24/32-bit integer) into its format and little-endian bytes.
pub fn read_wav(path: &Path) -> Result<(AudioFormat, Vec<u8>), String> {
    let mut r = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = r.spec();
    if spec.sample_format != hound::SampleFormat::Int {
        return Err("only integer PCM WAVs are supported".into());
    }
    let width = spec.bits_per_sample.div_ceil(8);
    let mut pcm = Vec::with_capacity(r.len() as usize * width as usize);
    match width {
        1 => {
            for s in r.samples::<i8>() {
                pcm.push(s.map_err(|e| e.to_string())? as u8);
            }
        }
        2 => {
            for s in r.samples::<i16>() {
                pcm.extend_from_slice(&s.map_err(|e| e.to_string())?.to_le_bytes());
            }
        }
        3 | 4 => {
            for s in r.samples::<i32>() {
                pcm.extend_from_slice(
                    &s.map_err(|e| e.to_string())?.to_le_bytes()[..width as usize],
                );
            }
        }
        _ => return Err(format!("unsupported sample width {width}")),
    }
    Ok((
        AudioFormat {
            rate: spec.sample_rate,
            width,
            channels: spec.channels,
        },
        pcm,
    ))
}

fn probe(addr: SocketAddr, state: &Shared) -> StepResult {
    let mut c = Client::connect(addr, Duration::from_secs(5)).map_err(StepError::Io)?;
    c.send(&Message::Describe)
        .map_err(|e| failed(state, format!("probe: {e}")))?;
    let deadline = Instant::now() + PING_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(failed(state, "probe: no info within 5 s".into()));
        }
        match c.recv(Some(left)) {
            Ok(Some(Message::Info(i))) => {
                let v = i.to_json();
                let mut st = lock(state);
                let t = st.t0.elapsed().as_secs_f64();
                st.record.received.push((t, "info".into(), v.clone()));
                if let Err(e) = check_info(&v) {
                    st.fail(format!("probe: {e}"));
                    return Err(StepError::Failed);
                }
                if st.record.info.is_none() {
                    st.record.info = Some(v);
                }
                return Ok(());
            }
            Ok(Some(_)) => {}
            Ok(None) => return Err(failed(state, "probe: connection closed before info".into())),
            Err(e) if crate::client::is_timeout(&e) => {}
            Err(e) => return Err(failed(state, format!("probe: {e}"))),
        }
    }
}

fn connect(addr: SocketAddr, state: &Shared) -> Result<Live, Error> {
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_nodelay(true)?;
    let read_half = stream.try_clone()?;
    let writer = Arc::new(Mutex::new(stream));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    {
        let mut st = lock(state);
        st.closing = false;
        st.loop_active = false;
        st.next_ping = None;
    }
    let reader = {
        let (state, writer) = (state.clone(), writer.clone());
        std::thread::Builder::new()
            .name("fake-ha-read".into())
            .spawn(move || {
                let mut r = BufReader::new(read_half);
                loop {
                    let e = match read_event(&mut r) {
                        Ok(Some(e)) => e,
                        Ok(None) => {
                            let mut st = lock(&state);
                            if !st.closing {
                                st.fail("the satellite closed the connection".into());
                            }
                            return;
                        }
                        Err(e) => {
                            let mut st = lock(&state);
                            if !st.closing {
                                st.fail(format!("reading from the satellite: {e}"));
                            }
                            return;
                        }
                    };
                    let at = Instant::now();
                    let kind = e.kind.clone();
                    let data = Value::Object(e.data.clone());
                    let m = Message::from_event(e);
                    let mut reply = None;
                    {
                        let mut st = lock(&state);
                        let t = at.duration_since(st.t0).as_secs_f64();
                        st.record.received.push((t, kind, data));
                        st.last_event = at;
                        check_message(&mut st, at, &m, &mut reply);
                    }
                    if let Some(reply) = reply {
                        let _ = write(&writer, &reply);
                    }
                    if !m.is_audio() {
                        log::debug!("fake-ha: <- {}", m.event_type());
                    }
                    let _ = tx.send((at, m));
                }
            })?
    };
    let pinger = {
        let (state, writer, stop) = (state.clone(), writer.clone(), stop.clone());
        std::thread::Builder::new()
            .name("fake-ha-ping".into())
            .spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(50));
                    let mut st = lock(&state);
                    if !st.ping_enabled || !st.loop_active || st.closing {
                        continue;
                    }
                    let now = Instant::now();
                    if now.duration_since(st.last_event) > PING_TIMEOUT {
                        st.fail("no event within 5 s (Home Assistant's _PING_TIMEOUT)".into());
                        st.loop_active = false;
                        continue;
                    }
                    if st.next_ping.is_some_and(|t| now >= t) {
                        st.next_ping = None;
                        drop(st);
                        let _ = write(&writer, &Message::Ping { text: None });
                    }
                }
            })?
    };
    Ok(Live {
        writer,
        rx,
        stop,
        threads: vec![reader, pinger],
    })
}

/// Checks applied to every message from the live connection.
fn check_message(st: &mut State, at: Instant, m: &Message, reply: &mut Option<Message>) {
    match m {
        Message::Ping { text } => *reply = Some(Message::Pong { text: text.clone() }),
        Message::Pong { .. } => {
            if st.loop_active {
                st.next_ping = Some(at + PING_SEND_DELAY);
            }
        }
        Message::Detection(_) => st.detection_at = Some(at),
        Message::AudioStart { .. } => st.last_chunk_ts = None,
        Message::AudioChunk {
            format,
            timestamp,
            audio,
        } => {
            if *format != AudioFormat::MIC {
                st.fail(format!("audio-chunk {format:?}, expected 16000/2/1"));
            }
            if let Some(ts) = *timestamp {
                if st.last_chunk_ts.is_some_and(|last| ts < last) {
                    st.fail(format!(
                        "audio-chunk timestamp {ts} after {:?}",
                        st.last_chunk_ts
                    ));
                }
                st.last_chunk_ts = Some(ts);
            }
            st.audio_bytes += audio.len() as u64;
            if let Some(d) = st.detection_at.take() {
                if st.record.detection_to_first_chunk_ms.is_none() {
                    st.record.detection_to_first_chunk_ms = Some(ms_between(d, at));
                }
            }
        }
        Message::Played => {
            if st.stops_pending == 0 {
                st.fail("played before audio-stop".into());
            } else {
                st.stops_pending -= 1;
            }
        }
        _ => {}
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

/// The answer WAV of the built-in scenarios (1.5 s at 22050 Hz).
pub fn answer_wav() -> PathBuf {
    fixture("answer-22050.wav")
}

/// The announcement WAV of the built-in scenarios (0.8 s at 22050 Hz).
pub fn announce_wav() -> PathBuf {
    fixture("announce-22050.wav")
}

fn conversation(tts_first: bool) -> Vec<Step> {
    let mut steps = vec![
        Step::Probe,
        Step::Connect,
        Step::RunSatellite,
        Step::Describe,
        Step::ExpectInfo { timeout_ms: 5000 },
        Step::WaitDetection { timeout_ms: 15000 },
        Step::CollectAudio {
            min_ms: 1500,
            max_ms: 20000,
        },
        Step::SendTranscript {
            text: "what time is it".into(),
        },
        Step::Sleep { ms: 200 },
    ];
    let synth = Step::SendSynthesize {
        text: "It is ten past two.".into(),
    };
    let wav = Step::SendWav { path: answer_wav() };
    if tts_first {
        steps.extend([wav, synth]);
    } else {
        steps.extend([synth, wav]);
    }
    steps.extend([
        // 1.5 s of answer + 2 s
        Step::ExpectPlayed { timeout_ms: 3500 },
        Step::TimerStarted(TimerStarted {
            id: "t1".into(),
            total_seconds: 90,
            name: Some("Pasta".into()),
            start_hours: None,
            start_minutes: Some(1),
            start_seconds: Some(30),
        }),
        Step::Sleep { ms: 1500 },
        Step::TimerUpdated(TimerUpdated {
            id: "t1".into(),
            is_active: true,
            total_seconds: 88,
        }),
        Step::Sleep { ms: 500 },
        Step::TimerCancelled { id: "t1".into() },
        Step::Announce {
            path: announce_wav(),
        },
        Step::Pause,
        Step::Sleep { ms: 500 },
        Step::Disconnect,
    ]);
    steps
}

/// Names of the built-in scenarios.
pub const BUILTINS: &[&str] = &[
    "what-time",
    "tts-before-synthesize",
    "stop-button",
    "timers",
    "announce",
    "probe-only",
    "probe-while-connected",
];

/// A built-in scenario by name.
pub fn builtin(name: &str) -> Option<Scenario> {
    let steps = match name {
        "what-time" => conversation(false),
        "tts-before-synthesize" => conversation(true),
        "stop-button" => vec![
            Step::Connect,
            Step::RunSatellite,
            Step::Describe,
            Step::ExpectInfo { timeout_ms: 5000 },
            Step::WaitDetection { timeout_ms: 15000 },
            Step::CollectAudio {
                min_ms: 500,
                max_ms: 60000,
            },
            Step::ExpectAudioStop { timeout_ms: 10000 },
            Step::SendError {
                text: "No text recognized".into(),
                code: "stt-no-text-recognized".into(),
            },
            Step::Sleep { ms: 500 },
            Step::Pause,
            Step::Disconnect,
        ],
        "timers" => vec![
            Step::Connect,
            Step::RunSatellite,
            Step::TimerStarted(TimerStarted {
                id: "t2".into(),
                total_seconds: 5,
                name: None,
                start_hours: None,
                start_minutes: None,
                start_seconds: Some(5),
            }),
            // The satellite must ring by itself: nothing is sent.
            Step::Sleep { ms: 7000 },
            // Home Assistant's own finish arrives late: the satellite must not ring twice.
            Step::TimerFinished { id: "t2".into() },
            Step::Disconnect,
        ],
        "announce" => vec![
            Step::Connect,
            Step::RunSatellite,
            Step::Announce {
                path: announce_wav(),
            },
            Step::Pause,
            Step::Disconnect,
        ],
        "probe-only" => vec![
            Step::Probe,
            Step::Sleep { ms: 500 },
            Step::Probe,
            Step::Sleep { ms: 500 },
            Step::Probe,
        ],
        // Home Assistant opens short describe-only connections (load_wyoming_info: config flow,
        // zeroconf rediscovery, entry setup) while the satellite connection is up; the satellite
        // must keep that connection (the fake fails on "the satellite closed the connection")
        // and keep answering pings.
        "probe-while-connected" => vec![
            Step::Connect,
            Step::RunSatellite,
            Step::Describe,
            Step::ExpectInfo { timeout_ms: 5000 },
            Step::Sleep { ms: 2500 },
            Step::Probe,
            Step::Sleep { ms: 2500 },
            Step::Probe,
            Step::Probe,
            Step::Sleep { ms: 2500 },
            Step::Pause,
            Step::Disconnect,
        ],
        _ => return None,
    };
    Some(Scenario { ping: true, steps })
}

/// Loads a TOML scenario; relative WAV paths are resolved against the file's directory.
pub fn load_scenario(path: &Path) -> Result<Scenario, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut s: Scenario = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("."));
    for step in &mut s.steps {
        if let Step::SendWav { path } | Step::Announce { path } = step {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
    }
    Ok(s)
}
