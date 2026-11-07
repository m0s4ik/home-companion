// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Typed events, with the data keys of wyoming 1.11.0.
//!
//! Reading is tolerant (`.get`, unknown keys ignored, numbers accepted as integers or floats);
//! a known type missing a required key becomes [`Message::Unknown`] with a warning, so a
//! malformed event never tears down the connection. Writing produces the keys the Python
//! classes write, with `null` where Python writes `None`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::event::Event;
use crate::info::Info;

/// Audio format of a stream: rate in Hz, width in bytes, channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub rate: u32,
    pub width: u16,
    pub channels: u16,
}

impl AudioFormat {
    /// What Home Assistant expects from a satellite microphone: 16 kHz, 16 bit, mono.
    pub const MIC: AudioFormat = AudioFormat {
        rate: 16_000,
        width: 2,
        channels: 1,
    };

    /// Bytes per second of audio.
    pub fn bytes_per_second(&self) -> u64 {
        u64::from(self.rate) * u64::from(self.width) * u64::from(self.channels)
    }

    /// Milliseconds of audio in `bytes` bytes, truncated as `AudioChunk.milliseconds` does.
    pub fn millis(&self, bytes: usize) -> i64 {
        let frame = u64::from(self.width) * u64::from(self.channels);
        if frame == 0 || self.rate == 0 {
            return 0;
        }
        ((bytes as u64 / frame) * 1000 / u64::from(self.rate)) as i64
    }
}

/// Pipeline stage of `run-pipeline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Wake,
    Asr,
    Intent,
    Handle,
    Tts,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Wake => "wake",
            Stage::Asr => "asr",
            Stage::Intent => "intent",
            Stage::Handle => "handle",
            Stage::Tts => "tts",
        }
    }

    pub fn parse(s: &str) -> Option<Stage> {
        Some(match s {
            "wake" => Stage::Wake,
            "asr" => Stage::Asr,
            "intent" => Stage::Intent,
            "handle" => Stage::Handle,
            "tts" => Stage::Tts,
            _ => return None,
        })
    }
}

/// `run-pipeline`: the satellite asks Home Assistant to run a pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct RunPipeline {
    pub start_stage: Stage,
    pub end_stage: Stage,
    pub restart_on_end: bool,
    pub wake_word_name: Option<String>,
}

impl RunPipeline {
    /// What a satellite with a local wake word sends: speech-to-text to text-to-speech.
    pub fn asr_to_tts() -> RunPipeline {
        RunPipeline {
            start_stage: Stage::Asr,
            end_stage: Stage::Tts,
            restart_on_end: false,
            wake_word_name: None,
        }
    }
}

/// `detection`: a wake word was detected.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// The wake model id (e.g. `okay_alfred`); Home Assistant resolves it to the phrase through
    /// `info.wake[*].models`.
    pub name: Option<String>,
    pub timestamp: Option<i64>,
    pub speaker: Option<String>,
}

/// `transcript`: the speech-to-text result.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
}

/// The `voice` of `synthesize`.
#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    pub name: Option<String>,
    pub language: Option<String>,
    pub speaker: Option<String>,
}

/// `synthesize`: the text of the answer about to be spoken.
#[derive(Debug, Clone, PartialEq)]
pub struct Synthesize {
    pub text: String,
    pub voice: Option<Voice>,
}

/// `timer-started`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerStarted {
    pub id: String,
    pub total_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_hours: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_seconds: Option<u32>,
}

/// `timer-updated`: `total_seconds` is the time **left**; `is_active` false = paused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimerUpdated {
    pub id: String,
    pub is_active: bool,
    pub total_seconds: u64,
}

/// A typed Wyoming event.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Describe,
    Info(Info),
    Ping {
        text: Option<String>,
    },
    Pong {
        text: Option<String>,
    },
    RunSatellite,
    PauseSatellite,
    RunPipeline(RunPipeline),
    AudioStart {
        format: AudioFormat,
        timestamp: Option<i64>,
    },
    AudioChunk {
        format: AudioFormat,
        timestamp: Option<i64>,
        audio: Vec<u8>,
    },
    AudioStop {
        timestamp: Option<i64>,
    },
    Detect {
        names: Option<Vec<String>>,
    },
    Detection(Detection),
    NotDetected,
    Transcribe {
        language: Option<String>,
    },
    Transcript(Transcript),
    Synthesize(Synthesize),
    VoiceStarted {
        timestamp: Option<i64>,
    },
    VoiceStopped {
        timestamp: Option<i64>,
    },
    Error {
        text: String,
        code: Option<String>,
    },
    Played,
    TimerStarted(TimerStarted),
    TimerUpdated(TimerUpdated),
    TimerCancelled {
        id: String,
    },
    TimerFinished {
        id: String,
    },
    StreamingStarted,
    StreamingStopped,
    SatelliteConnected,
    SatelliteDisconnected,
    /// Any other type (`transcript-chunk`, `synthesize-start`, `select-program`, `handled`, ...)
    /// or a known type with a missing required key, kept verbatim.
    Unknown(Event),
}

/// Missing required key: the caller turns it into `Unknown`.
struct Missing(&'static str);

fn opt_str(d: &Map<String, Value>, k: &str) -> Option<String> {
    d.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn req_str(d: &Map<String, Value>, k: &'static str) -> Result<String, Missing> {
    opt_str(d, k).ok_or(Missing(k))
}

fn opt_i64(d: &Map<String, Value>, k: &str) -> Option<i64> {
    let v = d.get(k)?;
    v.as_i64()
        .or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
}

fn opt_u64(d: &Map<String, Value>, k: &str) -> Option<u64> {
    let v = d.get(k)?;
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f.round() as u64)
    })
}

fn req_u64(d: &Map<String, Value>, k: &'static str) -> Result<u64, Missing> {
    opt_u64(d, k).ok_or(Missing(k))
}

fn opt_u32(d: &Map<String, Value>, k: &str) -> Option<u32> {
    opt_u64(d, k).and_then(|v| u32::try_from(v).ok())
}

fn format_of(d: &Map<String, Value>) -> Result<AudioFormat, Missing> {
    let rate = u32::try_from(req_u64(d, "rate")?).map_err(|_| Missing("rate"))?;
    let width = u16::try_from(req_u64(d, "width")?).map_err(|_| Missing("width"))?;
    let channels = u16::try_from(req_u64(d, "channels")?).map_err(|_| Missing("channels"))?;
    Ok(AudioFormat {
        rate,
        width,
        channels,
    })
}

fn null_or<T: Into<Value>>(v: Option<T>) -> Value {
    v.map(Into::into).unwrap_or(Value::Null)
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn format_data(f: &AudioFormat, timestamp: Option<i64>) -> Map<String, Value> {
    obj(
        json!({"rate": f.rate, "width": f.width, "channels": f.channels, "timestamp": null_or(timestamp)}),
    )
}

impl Message {
    /// Types an event. Never fails: unknown types and malformed known types become `Unknown`.
    pub fn from_event(e: Event) -> Message {
        match Self::try_from_event(&e) {
            Ok(Some(m)) => m,
            Ok(None) => Message::Unknown(e),
            Err(Missing(key)) => {
                log::warn!(
                    "wyoming: {} without a valid \"{key}\", kept as unknown",
                    e.kind
                );
                Message::Unknown(e)
            }
        }
    }

    fn try_from_event(e: &Event) -> Result<Option<Message>, Missing> {
        let d = &e.data;
        Ok(Some(match e.kind.as_str() {
            "describe" => Message::Describe,
            "info" => Message::Info(Info::from_json(&Value::Object(d.clone()))),
            "ping" => Message::Ping {
                text: opt_str(d, "text"),
            },
            "pong" => Message::Pong {
                text: opt_str(d, "text"),
            },
            "run-satellite" => Message::RunSatellite,
            "pause-satellite" => Message::PauseSatellite,
            "run-pipeline" => {
                let stage = |k: &'static str| {
                    opt_str(d, k)
                        .and_then(|s| Stage::parse(&s))
                        .ok_or(Missing(k))
                };
                Message::RunPipeline(RunPipeline {
                    start_stage: stage("start_stage")?,
                    end_stage: stage("end_stage")?,
                    restart_on_end: d
                        .get("restart_on_end")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    wake_word_name: opt_str(d, "wake_word_name"),
                })
            }
            "audio-start" => Message::AudioStart {
                format: format_of(d)?,
                timestamp: opt_i64(d, "timestamp"),
            },
            "audio-chunk" => Message::AudioChunk {
                format: format_of(d)?,
                timestamp: opt_i64(d, "timestamp"),
                audio: e.payload.clone().unwrap_or_default(),
            },
            "audio-stop" => Message::AudioStop {
                timestamp: opt_i64(d, "timestamp"),
            },
            "detect" => Message::Detect {
                names: d.get("names").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                }),
            },
            "detection" => Message::Detection(Detection {
                name: opt_str(d, "name"),
                timestamp: opt_i64(d, "timestamp"),
                speaker: opt_str(d, "speaker"),
            }),
            "not-detected" => Message::NotDetected,
            "transcribe" => Message::Transcribe {
                language: opt_str(d, "language"),
            },
            "transcript" => Message::Transcript(Transcript {
                text: req_str(d, "text")?,
                language: opt_str(d, "language"),
            }),
            "synthesize" => {
                let voice = d.get("voice").and_then(Value::as_object).and_then(|v| {
                    // As SynthesizeVoice.from_dict: a voice only when one of its keys is present.
                    if ["name", "language", "speaker"]
                        .iter()
                        .any(|k| v.contains_key(*k))
                    {
                        Some(Voice {
                            name: opt_str(v, "name"),
                            language: opt_str(v, "language"),
                            speaker: opt_str(v, "speaker"),
                        })
                    } else {
                        None
                    }
                });
                Message::Synthesize(Synthesize {
                    text: req_str(d, "text")?,
                    voice,
                })
            }
            "voice-started" => Message::VoiceStarted {
                timestamp: opt_i64(d, "timestamp"),
            },
            "voice-stopped" => Message::VoiceStopped {
                timestamp: opt_i64(d, "timestamp"),
            },
            "error" => Message::Error {
                text: req_str(d, "text")?,
                code: opt_str(d, "code"),
            },
            "played" => Message::Played,
            "timer-started" => Message::TimerStarted(TimerStarted {
                id: req_str(d, "id")?,
                total_seconds: req_u64(d, "total_seconds")?,
                name: opt_str(d, "name"),
                start_hours: opt_u32(d, "start_hours"),
                start_minutes: opt_u32(d, "start_minutes"),
                start_seconds: opt_u32(d, "start_seconds"),
            }),
            "timer-updated" => Message::TimerUpdated(TimerUpdated {
                id: req_str(d, "id")?,
                is_active: d
                    .get("is_active")
                    .and_then(Value::as_bool)
                    .ok_or(Missing("is_active"))?,
                total_seconds: req_u64(d, "total_seconds")?,
            }),
            "timer-cancelled" => Message::TimerCancelled {
                id: req_str(d, "id")?,
            },
            "timer-finished" => Message::TimerFinished {
                id: req_str(d, "id")?,
            },
            "streaming-started" => Message::StreamingStarted,
            "streaming-stopped" => Message::StreamingStopped,
            "satellite-connected" => Message::SatelliteConnected,
            "satellite-disconnected" => Message::SatelliteDisconnected,
            _ => return Ok(None),
        }))
    }

    /// The event with the keys wyoming 1.11.0 writes.
    pub fn to_event(&self) -> Event {
        let kind = self.kind();
        let data: Map<String, Value> = match self {
            Message::Unknown(e) => return e.clone(),
            Message::Info(info) => obj(info.to_json()),
            Message::Ping { text } | Message::Pong { text } => {
                obj(json!({"text": null_or(text.clone())}))
            }
            Message::RunPipeline(p) => {
                let mut m = obj(json!({
                    "start_stage": p.start_stage.as_str(),
                    "end_stage": p.end_stage.as_str(),
                    "restart_on_end": p.restart_on_end,
                }));
                if let Some(n) = &p.wake_word_name {
                    m.insert("wake_word_name".into(), n.clone().into());
                }
                m
            }
            Message::AudioStart { format, timestamp } => format_data(format, *timestamp),
            Message::AudioChunk {
                format,
                timestamp,
                audio,
            } => {
                return Event {
                    kind: kind.into(),
                    data: format_data(format, *timestamp),
                    payload: Some(audio.clone()),
                };
            }
            Message::AudioStop { timestamp }
            | Message::VoiceStarted { timestamp }
            | Message::VoiceStopped { timestamp } => obj(json!({"timestamp": null_or(*timestamp)})),
            Message::Detect { names } => obj(json!({"names": names})),
            Message::Detection(d) => obj(json!({
                "name": null_or(d.name.clone()),
                "timestamp": null_or(d.timestamp),
                "speaker": null_or(d.speaker.clone()),
            })),
            Message::Transcribe { language } => {
                let mut m = Map::new();
                if let Some(l) = language {
                    m.insert("language".into(), l.clone().into());
                }
                m
            }
            Message::Transcript(t) => {
                let mut m = obj(json!({"text": t.text}));
                if let Some(l) = &t.language {
                    m.insert("language".into(), l.clone().into());
                }
                m
            }
            Message::Synthesize(s) => {
                let mut m = obj(json!({"text": s.text}));
                if let Some(v) = &s.voice {
                    let mut vm = Map::new();
                    for (k, val) in [
                        ("name", &v.name),
                        ("language", &v.language),
                        ("speaker", &v.speaker),
                    ] {
                        if let Some(val) = val {
                            vm.insert(k.into(), val.clone().into());
                        }
                    }
                    m.insert("voice".into(), Value::Object(vm));
                }
                m
            }
            Message::Error { text, code } => {
                let mut m = obj(json!({"text": text}));
                if let Some(c) = code {
                    m.insert("code".into(), c.clone().into());
                }
                m
            }
            Message::TimerStarted(t) => obj(serde_json::to_value(t).unwrap_or(Value::Null)),
            Message::TimerUpdated(t) => obj(serde_json::to_value(t).unwrap_or(Value::Null)),
            Message::TimerCancelled { id } | Message::TimerFinished { id } => {
                obj(json!({"id": id}))
            }
            Message::Describe
            | Message::RunSatellite
            | Message::PauseSatellite
            | Message::NotDetected
            | Message::Played
            | Message::StreamingStarted
            | Message::StreamingStopped
            | Message::SatelliteConnected
            | Message::SatelliteDisconnected => Map::new(),
        };
        Event::with_data(kind, data)
    }

    /// The Wyoming type; `"unknown"` for [`Message::Unknown`] (see [`Message::event_type`]).
    pub fn kind(&self) -> &'static str {
        match self {
            Message::Describe => "describe",
            Message::Info(_) => "info",
            Message::Ping { .. } => "ping",
            Message::Pong { .. } => "pong",
            Message::RunSatellite => "run-satellite",
            Message::PauseSatellite => "pause-satellite",
            Message::RunPipeline(_) => "run-pipeline",
            Message::AudioStart { .. } => "audio-start",
            Message::AudioChunk { .. } => "audio-chunk",
            Message::AudioStop { .. } => "audio-stop",
            Message::Detect { .. } => "detect",
            Message::Detection(_) => "detection",
            Message::NotDetected => "not-detected",
            Message::Transcribe { .. } => "transcribe",
            Message::Transcript(_) => "transcript",
            Message::Synthesize(_) => "synthesize",
            Message::VoiceStarted { .. } => "voice-started",
            Message::VoiceStopped { .. } => "voice-stopped",
            Message::Error { .. } => "error",
            Message::Played => "played",
            Message::TimerStarted(_) => "timer-started",
            Message::TimerUpdated(_) => "timer-updated",
            Message::TimerCancelled { .. } => "timer-cancelled",
            Message::TimerFinished { .. } => "timer-finished",
            Message::StreamingStarted => "streaming-started",
            Message::StreamingStopped => "streaming-stopped",
            Message::SatelliteConnected => "satellite-connected",
            Message::SatelliteDisconnected => "satellite-disconnected",
            Message::Unknown(_) => "unknown",
        }
    }

    /// The Wyoming type as on the wire, also for `Unknown`.
    pub fn event_type(&self) -> &str {
        match self {
            Message::Unknown(e) => &e.kind,
            m => m.kind(),
        }
    }

    /// True for `audio-chunk` (loggers skip them).
    pub fn is_audio(&self) -> bool {
        matches!(self, Message::AudioChunk { .. })
    }
}
