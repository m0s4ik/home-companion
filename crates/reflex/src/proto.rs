// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body ↔ `home-companion-reflex` protocol, over the helper's standard input and output.
//!
//! Body → helper (stdin, binary, little endian): `S` + u64 utterance id (a new utterance: the
//! recognizer starts over), `A` + u32 sample count + that many s16 samples at 16 kHz, `E` + u64
//! id (the utterance is over: flush the final result unless an endpoint was already reported),
//! `W` + u64 id + u16 byte length + the wake phrase (UTF-8) + u32 sample count + that many s16
//! samples (the audio that woke the character: is the phrase in it? Answered with `verify`, on a
//! recognizer of its own, so an utterance being decoded is not disturbed).
//!
//! Helper → body (stdout): one JSON object per line ([`Reply`]): `ready` once the model and the
//! grammar are loaded, then per utterance:
//!
//! - `partial` lines while it is decoded: Vosk's partial text, sent only when it changed since the
//!   last one of this utterance (a few per utterance), never after the utterance's last `result`;
//! - a `result`, either at Vosk's own end of speech (`endpoint: true`) or after `E`
//!   (`endpoint: false`). A result **with words** ends the utterance. An **empty** one does not:
//!   the helper goes on decoding the audio that follows (partials resume), and a later endpoint
//!   or another `E` with the same id gets another result. So sound without words never needs the
//!   utterance started over.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::grammar::Word;

/// What the body sends.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Start(u64),
    /// An utterance that answers a Yes/No question: decoded against the answers only.
    StartAnswer(u64),
    Audio(Vec<i16>),
    End(u64),
    /// The audio before a wake word detection, to check that the wake phrase is in it.
    Verify {
        id: u64,
        phrase: String,
        samples: Vec<i16>,
    },
}

/// Longest audio block the helper accepts (10 s).
const MAX_SAMPLES: usize = 16_000 * 10;

fn put_samples(v: &mut Vec<u8>, s: &[i16]) {
    v.extend_from_slice(&(s.len() as u32).to_le_bytes());
    for x in s {
        v.extend_from_slice(&x.to_le_bytes());
    }
}

fn read_samples(r: &mut impl Read) -> io::Result<Vec<i16>> {
    let mut n = [0u8; 4];
    r.read_exact(&mut n)?;
    let n = u32::from_le_bytes(n) as usize;
    if n > MAX_SAMPLES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "audio block too long",
        ));
    }
    let mut b = vec![0u8; n * 2];
    r.read_exact(&mut b)?;
    Ok(b.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Request::Start(id) => [&b"S"[..], &id.to_le_bytes()].concat(),
            Request::StartAnswer(id) => [&b"Q"[..], &id.to_le_bytes()].concat(),
            Request::End(id) => [&b"E"[..], &id.to_le_bytes()].concat(),
            Request::Audio(s) => {
                let mut v = Vec::with_capacity(5 + s.len() * 2);
                v.push(b'A');
                put_samples(&mut v, s);
                v
            }
            Request::Verify {
                id,
                phrase,
                samples,
            } => {
                let p = phrase.as_bytes();
                let p = &p[..p.len().min(u16::MAX as usize)];
                let mut v = Vec::with_capacity(16 + p.len() + samples.len() * 2);
                v.push(b'W');
                v.extend_from_slice(&id.to_le_bytes());
                v.extend_from_slice(&(p.len() as u16).to_le_bytes());
                v.extend_from_slice(p);
                put_samples(&mut v, samples);
                v
            }
        }
    }

    /// The next request; None at the end of the stream.
    pub fn read(r: &mut impl Read) -> io::Result<Option<Request>> {
        let mut tag = [0u8; 1];
        match r.read_exact(&mut tag) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let mut u64b = [0u8; 8];
        match tag[0] {
            b'S' | b'E' | b'Q' => {
                r.read_exact(&mut u64b)?;
                let id = u64::from_le_bytes(u64b);
                Ok(Some(match tag[0] {
                    b'S' => Request::Start(id),
                    b'Q' => Request::StartAnswer(id),
                    _ => Request::End(id),
                }))
            }
            b'A' => Ok(Some(Request::Audio(read_samples(r)?))),
            b'W' => {
                r.read_exact(&mut u64b)?;
                let id = u64::from_le_bytes(u64b);
                let mut n = [0u8; 2];
                r.read_exact(&mut n)?;
                let mut p = vec![0u8; u16::from_le_bytes(n) as usize];
                r.read_exact(&mut p)?;
                let phrase = String::from_utf8(p)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "phrase not UTF-8"))?;
                Ok(Some(Request::Verify {
                    id,
                    phrase,
                    samples: read_samples(r)?,
                }))
            }
            t => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown request {t:#x}"),
            )),
        }
    }
}

/// What the helper answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Ready {
        language: String,
        phrases: usize,
        load_ms: f64,
    },
    Result {
        id: u64,
        /// Vosk's own end of speech (true) or the body's `E` (false).
        endpoint: bool,
        text: String,
        words: Vec<Word>,
        /// Decoding time of the last block (the latency added at the end of speech).
        ms: f64,
        /// Audio received for this utterance.
        audio_s: f64,
    },
    /// Vosk's partial text of the utterance so far (changed since the last one).
    Partial {
        id: u64,
        text: String,
        /// Audio received for this utterance.
        audio_s: f64,
    },
    /// The answer to `W`: what a grammar of the wake phrase alone heard in that audio.
    Verify {
        id: u64,
        text: String,
        /// False when a word of the phrase is not in the model's vocabulary: no check possible.
        known: bool,
        ms: f64,
    },
    Error {
        message: String,
    },
}

impl Reply {
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        let line = serde_json::to_string(self).map_err(io::Error::other)?;
        writeln!(w, "{line}")?;
        w.flush()
    }

    pub fn parse(line: &str) -> Result<Reply, String> {
        serde_json::from_str(line).map_err(|e| format!("{e}: {line:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let all = [
            Request::Start(7),
            Request::StartAnswer(8),
            Request::Audio(vec![0, -1, 32767, -32768]),
            Request::End(7),
            Request::Verify {
                id: 9,
                phrase: "okay alfred".into(),
                samples: vec![1, -2, 3],
            },
        ];
        let bytes: Vec<u8> = all.iter().flat_map(|r| r.encode()).collect();
        let mut c = std::io::Cursor::new(bytes);
        for r in &all {
            assert_eq!(Request::read(&mut c).unwrap().as_ref(), Some(r));
        }
        assert_eq!(Request::read(&mut c).unwrap(), None);
        let mut bad = std::io::Cursor::new(vec![b'X']);
        assert!(Request::read(&mut bad).is_err());
    }

    #[test]
    fn replies_round_trip() {
        let r = Reply::Result {
            id: 3,
            endpoint: true,
            text: "che ore sono".into(),
            words: vec![Word {
                word: "che".into(),
                conf: 1.0,
                start: 0.1,
                end: 0.2,
            }],
            ms: 12.5,
            audio_s: 1.5,
        };
        let mut out = Vec::new();
        r.write(&mut out).unwrap();
        let line = String::from_utf8(out).unwrap();
        assert_eq!(Reply::parse(line.trim()).unwrap(), r);
    }

    #[test]
    fn verify_replies_round_trip() {
        let r = Reply::Verify {
            id: 9,
            text: "okay alfred".into(),
            known: true,
            ms: 40.0,
        };
        let mut out = Vec::new();
        r.write(&mut out).unwrap();
        let line = String::from_utf8(out).unwrap();
        assert!(line.starts_with(r#"{"type":"verify","id":9,"#), "{line}");
        assert_eq!(Reply::parse(line.trim()).unwrap(), r);
    }

    #[test]
    fn partials_round_trip() {
        let r = Reply::Partial {
            id: 4,
            text: "che ore".into(),
            audio_s: 1.12,
        };
        let mut out = Vec::new();
        r.write(&mut out).unwrap();
        let line = String::from_utf8(out).unwrap();
        assert!(line.starts_with(r#"{"type":"partial","id":4,"#), "{line}");
        assert_eq!(Reply::parse(line.trim()).unwrap(), r);
    }
}
