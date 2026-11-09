// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Home Assistant's own text-to-speech, for the reflexes' answers: the voice of the body's
//! pipeline ([`TtsTarget`], written by the setup thread to `<state_dir>/tts-target.json`) and
//! one phrase made into audio through the REST API every installation has: `POST
//! /api/tts_get_url` (with the user's token) gives a `/api/tts_proxy/<token>` path, whose `GET`
//! (no auth needed) streams the audio once it is made.

use std::io;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::auth;

/// The file in the state directory.
pub const TARGET_FILE: &str = "tts-target.json";
/// The rate asked of Home Assistant (Piper's own; the mixer resamples anything).
pub const RATE: u32 = 22050;
/// The largest answer taken (≈ 45 s at 22 kHz, 16-bit mono).
const MAX_AUDIO: usize = 2 << 20;

/// The voice of the body's pipeline: its `tts_engine`, `tts_voice` ("" = the engine's default)
/// and `tts_language` ("" = the engine's default).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsTarget {
    pub engine: String,
    #[serde(default)]
    pub voice: String,
    #[serde(default)]
    pub language: String,
}

impl TtsTarget {
    /// From an `assist_pipeline` pipeline object; None without a voice engine.
    pub fn of_pipeline(p: &Value) -> Option<TtsTarget> {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let engine = s("tts_engine");
        (!engine.is_empty()).then(|| TtsTarget {
            engine,
            voice: s("tts_voice"),
            language: s("tts_language"),
        })
    }

    pub fn load(dir: &Path) -> Option<TtsTarget> {
        let s = std::fs::read_to_string(dir.join(TARGET_FILE)).ok()?;
        serde_json::from_str(&s).ok()
    }

    /// Written only when it changed (the setup checks often); `None` removes the file.
    pub fn publish(dir: &Path, t: Option<&TtsTarget>) -> io::Result<()> {
        let path = dir.join(TARGET_FILE);
        match t {
            Some(t) if TtsTarget::load(dir).as_ref() == Some(t) => Ok(()),
            Some(t) => {
                let tmp = dir.join(format!("{TARGET_FILE}.tmp"));
                std::fs::write(&tmp, serde_json::to_vec(t).unwrap_or_default())?;
                std::fs::rename(tmp, path)
            }
            None => match std::fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        }
    }

    /// The `POST /api/tts_get_url` body: 16-bit mono WAV at [`RATE`] (Home Assistant converts
    /// with ffmpeg when the engine cannot make it), its own cache on.
    pub fn request(&self, text: &str) -> Value {
        let mut options = json!({
            "preferred_format": "wav",
            "preferred_sample_rate": RATE,
            "preferred_sample_channels": 1,
            "preferred_sample_bytes": 2,
        });
        if !self.voice.is_empty() {
            options["voice"] = json!(self.voice);
        }
        let mut r = json!({
            "engine_id": self.engine,
            "message": text,
            "cache": true,
            "options": options,
        });
        if !self.language.is_empty() {
            r["language"] = json!(self.language);
        }
        r
    }
}

/// `text` said by `target`: the WAV file's bytes as Home Assistant sends them.
pub fn fetch(
    url: &str,
    token: &str,
    target: &TtsTarget,
    text: &str,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let body = target.request(text).to_string();
    let r = auth::http(
        url,
        "POST",
        "/api/tts_get_url",
        Some(token),
        Some(("application/json", body.as_bytes())),
        64 << 10,
        timeout,
    )
    .map_err(|e| format!("tts_get_url: {e}"))?;
    if r.status != 200 {
        return Err(format!(
            "tts_get_url: HTTP {}: {}",
            r.status,
            r.body.chars().take(200).collect::<String>()
        ));
    }
    let v: Value = serde_json::from_str(&r.body).map_err(|e| format!("tts_get_url: {e}"))?;
    // `path`, not `url`: the url is built on Home Assistant's own idea of its address (maybe an
    // external one); the path goes to the address the body already uses.
    let path = v["path"]
        .as_str()
        .filter(|p| p.starts_with('/'))
        .ok_or("tts_get_url: no path in the answer")?;
    let (status, audio) = auth::http_bytes(url, "GET", path, None, None, MAX_AUDIO, timeout)
        .map_err(|e| format!("tts_proxy: {e}"))?;
    if status != 200 {
        return Err(format!("tts_proxy: HTTP {status}"));
    }
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn target_from_a_pipeline() {
        let p = json!({"tts_engine": "tts.piper", "tts_voice": null, "tts_language": "it_IT"});
        assert_eq!(
            TtsTarget::of_pipeline(&p),
            Some(TtsTarget {
                engine: "tts.piper".into(),
                voice: String::new(),
                language: "it_IT".into()
            })
        );
        assert_eq!(TtsTarget::of_pipeline(&json!({"tts_engine": null})), None);
    }

    #[test]
    fn request_body() {
        let t = TtsTarget {
            engine: "tts.cloud".into(),
            voice: "v".into(),
            language: String::new(),
        };
        let r = t.request("Sono le 7:30.");
        assert_eq!(r["engine_id"], "tts.cloud");
        assert_eq!(r["message"], "Sono le 7:30.");
        assert_eq!(r["options"]["voice"], "v");
        assert_eq!(r["options"]["preferred_format"], "wav");
        assert_eq!(r["options"]["preferred_sample_rate"], RATE);
        assert!(r.get("language").is_none(), "the engine's default");
        let t = TtsTarget {
            voice: String::new(),
            language: "it-IT".into(),
            ..t
        };
        let r = t.request("x");
        assert!(r["options"].get("voice").is_none());
        assert_eq!(r["language"], "it-IT");
    }

    #[test]
    fn publish_and_load() {
        let dir = std::env::temp_dir().join(format!("hc-tts-target-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(TtsTarget::load(&dir), None);
        let t = TtsTarget {
            engine: "tts.piper".into(),
            ..Default::default()
        };
        TtsTarget::publish(&dir, Some(&t)).unwrap();
        assert_eq!(TtsTarget::load(&dir), Some(t));
        TtsTarget::publish(&dir, None).unwrap();
        TtsTarget::publish(&dir, None).unwrap();
        assert_eq!(TtsTarget::load(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Serves the two requests as Home Assistant does: the url (only with the token), then the
    /// audio, streamed (chunked, as aiohttp does without a length).
    fn fake_ha(audio: &'static [u8]) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut s, _) = l.accept().unwrap();
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = s.read(&mut buf).unwrap();
                    req.extend_from_slice(&buf[..n]);
                    let t = String::from_utf8_lossy(&req).to_string();
                    if let Some(i) = t.find("\r\n\r\n") {
                        let len = t[..i]
                            .lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                        if req.len() >= i + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let t = String::from_utf8_lossy(&req).to_string();
                if t.starts_with("POST /api/tts_get_url ") {
                    let ok = t.contains("Authorization: Bearer T1\r\n");
                    let body = r#"{"url":"http://elsewhere.example/api/tts_proxy/abc.wav","path":"/api/tts_proxy/abc.wav"}"#;
                    let status = if ok { 200 } else { 401 };
                    write!(
                        s,
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                } else {
                    write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
                        audio.len()
                    )
                    .unwrap();
                    s.write_all(audio).unwrap();
                    s.write_all(b"\r\n0\r\n\r\n").unwrap();
                }
                seen.push(t);
            }
            seen
        });
        (url, h)
    }

    #[test]
    fn fetch_from_home_assistant() {
        let audio: &'static [u8] = b"RIFF\x00\xff\x80binary\r\n";
        let (url, h) = fake_ha(audio);
        let t = TtsTarget {
            engine: "tts.piper".into(),
            voice: "paola".into(),
            language: "it_IT".into(),
        };
        let got = fetch(&url, "T1", &t, "Timer annullato.", Duration::from_secs(5)).unwrap();
        assert_eq!(got, audio);
        let seen = h.join().unwrap();
        assert!(
            seen[0].contains(r#""message":"Timer annullato.""#),
            "{}",
            seen[0]
        );
        assert!(
            seen[1].starts_with("GET /api/tts_proxy/abc.wav "),
            "{}",
            seen[1]
        );
    }
}
