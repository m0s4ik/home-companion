// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body ↔ `home-companion-reflex` protocol, over the helper's standard input and output.
//!
//! Body → helper (stdin, binary, little endian): `S` + u64 utterance id (a new utterance: the
//! recognizer starts over), `A` + u32 sample count + that many s16 samples at 16 kHz, `E` + u64
//! id (the utterance is over: flush the final result unless an endpoint was already reported).
//!
//! Helper → body (stdout): one JSON object per line ([`Reply`]): `ready` once the model and the
//! grammar are loaded, then one `result` per utterance, either at Vosk's own end of speech
//! (`endpoint: true`, the body decides at once) or after `E` (`endpoint: false`).

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::grammar::Word;

/// What the body sends.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Start(u64),
    Audio(Vec<i16>),
    End(u64),
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Request::Start(id) => [&b"S"[..], &id.to_le_bytes()].concat(),
            Request::End(id) => [&b"E"[..], &id.to_le_bytes()].concat(),
            Request::Audio(s) => {
                let mut v = Vec::with_capacity(5 + s.len() * 2);
                v.push(b'A');
                v.extend_from_slice(&(s.len() as u32).to_le_bytes());
                for x in s {
                    v.extend_from_slice(&x.to_le_bytes());
                }
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
            b'S' | b'E' => {
                r.read_exact(&mut u64b)?;
                let id = u64::from_le_bytes(u64b);
                Ok(Some(if tag[0] == b'S' {
                    Request::Start(id)
                } else {
                    Request::End(id)
                }))
            }
            b'A' => {
                let mut n = [0u8; 4];
                r.read_exact(&mut n)?;
                let n = u32::from_le_bytes(n) as usize;
                if n > 16_000 * 10 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "audio block too long",
                    ));
                }
                let mut b = vec![0u8; n * 2];
                r.read_exact(&mut b)?;
                Ok(Some(Request::Audio(
                    b.chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]))
                        .collect(),
                )))
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
            Request::Audio(vec![0, -1, 32767, -32768]),
            Request::End(7),
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
}
