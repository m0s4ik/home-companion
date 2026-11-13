// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The reflexes' answers said aloud, in the voice of the body's Home Assistant pipeline.
//!
//! The core asks ([`VoiceLink::say`]) right after a reflex; a thread answers with the audio
//! ([`VoiceReply`]): from the cache (`<state_dir>/tts-cache/<key>.wav`, also offline) or from
//! Home Assistant ([`crate::ha::tts`]) when it is reachable. The core waits [`VOICE_WAIT`] at most:
//! later (or never) the reflex's sound plays instead, as without Home Assistant. A late answer
//! still lands in the cache, so the phrase is ready the next time.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, SystemTime};

use audio::StreamFormat;

use crate::ha::auth::Auth;
use crate::ha::tts::{self, TtsTarget};

/// How long the reflex's answer waits for its voice before the sound plays instead (its text is
/// on the screen at once). OpenAI's voice takes 1.7 s in the median, up to 3 s seen: a phrase that
/// changes every minute ("Sono le 22:24") is never in the cache.
pub const VOICE_WAIT: Duration = Duration::from_millis(4000);
/// The cache's bounds: the least recently used phrases go first.
pub const CACHE_FILES: usize = 300;
pub const CACHE_BYTES: u64 = 20 << 20;
/// One phrase from Home Assistant may take this long (a late one is cached, not played).
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// A phrase as audio: 16-bit (or 8/32-bit) little-endian PCM.
#[derive(Debug, Clone, PartialEq)]
pub struct Speech {
    pub format: StreamFormat,
    pub pcm: Vec<u8>,
}

impl Speech {
    pub fn duration(&self) -> Duration {
        let bps = self.format.rate as u64 * self.format.width as u64 * self.format.channels as u64;
        Duration::from_secs_f64(self.pcm.len() as f64 / bps.max(1) as f64)
    }
}

/// What the voice thread answers to [`VoiceLink::say`]: the audio, or None (no voice chosen,
/// not cached and Home Assistant away, an error).
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceReply {
    pub id: u64,
    pub speech: Option<Speech>,
}

/// The core's side of the voice thread (a fake in tests).
pub trait VoiceLink: Send {
    /// Asks for `text` said aloud; `online` = Home Assistant may be asked (else the cache only).
    fn say(&self, id: u64, text: &str, online: bool);
    /// Puts `text` in the cache ahead of time (nothing is answered): a phrase that may be said
    /// at once a moment later ("Chiudo la tapparella." after a yes) is not 1.5 s late then.
    fn prepare(&self, _text: &str) {}
}

/// The reflex's answer is said aloud rather than its sound played: there is a sound to replace
/// (the volume reflexes have none, a repeat has its own voice), words to say, a voice, and do not
/// disturb is off.
pub fn speak_reflex(has_cue: bool, answer: &str, has_voice: bool, dnd: bool) -> bool {
    has_cue && !answer.trim().is_empty() && has_voice && !dnd
}

// ---- WAV --------------------------------------------------------------------------------

/// A WAV file's PCM (integer, 1-2 channels, 8-48 kHz). Lenient as a stream from ffmpeg needs:
/// a data size of 0 or past the end (unknown when written to a pipe) means "to the end".
pub fn parse_wav(b: &[u8]) -> Option<Speech> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut format = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let size = u32::from_le_bytes(b[at + 4..at + 8].try_into().ok()?) as usize;
        let body = at + 8;
        if id == b"fmt " {
            let f = b.get(body..body + 16)?;
            let tag = u16::from_le_bytes([f[0], f[1]]);
            let channels = u16::from_le_bytes([f[2], f[3]]);
            let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
            let bits = u16::from_le_bytes([f[14], f[15]]);
            // 1 = PCM, 0xFFFE = extensible (ffmpeg's choice for some layouts).
            if (tag != 1 && tag != 0xFFFE)
                || !(1..=2).contains(&channels)
                || !(8000..=48000).contains(&rate)
                || !matches!(bits, 8 | 16 | 32)
            {
                return None;
            }
            format = Some(StreamFormat {
                rate,
                width: bits / 8,
                channels,
            });
        } else if id == b"data" {
            let format = format?;
            let end = if size == 0 || body.saturating_add(size) > b.len() {
                b.len()
            } else {
                body + size
            };
            let frame = format.width as usize * format.channels as usize;
            let n = (end - body) / frame * frame;
            return Some(Speech {
                format,
                pcm: b[body..body + n].to_vec(),
            });
        }
        at = body.saturating_add(size + (size & 1));
    }
    None
}

/// A plain WAV file (what the cache keeps).
pub fn wav_bytes(s: &Speech) -> Vec<u8> {
    let f = s.format;
    let block = f.width as u32 * f.channels as u32;
    let mut out = Vec::with_capacity(44 + s.pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + s.pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&f.channels.to_le_bytes());
    out.extend_from_slice(&f.rate.to_le_bytes());
    out.extend_from_slice(&(f.rate * block).to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&(f.width * 8).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(s.pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(&s.pcm);
    out
}

// ---- cache ------------------------------------------------------------------------------

/// The cache file's name for a phrase in a voice (FNV-1a, stable across builds): another voice,
/// language or rate is another file.
pub fn cache_key(t: &TtsTarget, text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let rate = tts::RATE.to_string();
    for part in [&t.engine, &t.voice, &t.language, &rate, text.trim()] {
        for b in part.bytes().chain([0u8]) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// A cached file: name, size, last use.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub used: SystemTime,
}

/// The files to delete so that at most `max_files` and `max_bytes` stay: the least recently
/// used first.
pub fn to_evict(mut entries: Vec<Entry>, max_files: usize, max_bytes: u64) -> Vec<String> {
    entries.sort_by(|a, b| b.used.cmp(&a.used).then_with(|| a.name.cmp(&b.name)));
    let mut kept = 0usize;
    let mut bytes = 0u64;
    let mut out = Vec::new();
    for e in entries {
        if kept < max_files && bytes + e.size <= max_bytes {
            kept += 1;
            bytes += e.size;
        } else {
            out.push(e.name);
        }
    }
    out
}

/// The phrases already heard, in the state directory.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
    max_files: usize,
    max_bytes: u64,
}

impl Cache {
    pub fn new(dir: PathBuf) -> Cache {
        Cache {
            dir,
            max_files: CACHE_FILES,
            max_bytes: CACHE_BYTES,
        }
    }

    pub fn with_bounds(dir: PathBuf, max_files: usize, max_bytes: u64) -> Cache {
        Cache {
            dir,
            max_files,
            max_bytes,
        }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.wav"))
    }

    /// The phrase if cached (its use is noted: the file's time).
    pub fn get(&self, key: &str) -> Option<Speech> {
        let path = self.path(key);
        let b = std::fs::read(&path).ok()?;
        let s = parse_wav(&b);
        match &s {
            Some(_) => {
                if let Ok(f) = std::fs::File::options().write(true).open(&path) {
                    let _ = f.set_modified(SystemTime::now());
                }
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
        s
    }

    /// Keeps a phrase, then trims the cache to its bounds.
    pub fn put(&self, key: &str, s: &Speech) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{key}.tmp"));
        std::fs::write(&tmp, wav_bytes(s))?;
        std::fs::rename(&tmp, self.path(key))?;
        self.trim();
        Ok(())
    }

    fn trim(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let entries: Vec<Entry> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let m = e.metadata().ok()?;
                (name.ends_with(".wav") && m.is_file()).then(|| Entry {
                    name,
                    size: m.len(),
                    used: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                })
            })
            .collect();
        for name in to_evict(entries, self.max_files, self.max_bytes) {
            let _ = std::fs::remove_file(self.dir.join(name));
        }
    }
}

// ---- the thread -------------------------------------------------------------------------

/// Where the voice comes from.
pub struct VoiceConfig {
    /// `[home_assistant] url`.
    pub url: String,
    pub auth: Arc<Auth>,
    /// Holds [`tts::TARGET_FILE`] and the cache.
    pub state_dir: PathBuf,
}

struct Request {
    id: u64,
    text: String,
    online: bool,
    /// Only into the cache: no reply.
    prepare: bool,
}

/// The voice thread: requests are made one at a time, in order.
pub struct Voice {
    tx: Sender<Request>,
}

impl Voice {
    pub fn start(
        cfg: VoiceConfig,
        reply: impl Fn(VoiceReply) + Send + 'static,
    ) -> std::io::Result<Voice> {
        let (tx, rx) = channel::<Request>();
        std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || {
                let cache = Cache::new(cfg.state_dir.join("tts-cache"));
                for r in rx {
                    let speech = make(&cfg, &cache, &r);
                    if !r.prepare {
                        reply(VoiceReply { id: r.id, speech });
                    }
                }
            })?;
        Ok(Voice { tx })
    }
}

impl VoiceLink for Voice {
    fn say(&self, id: u64, text: &str, online: bool) {
        let _ = self.tx.send(Request {
            id,
            text: text.to_string(),
            online,
            prepare: false,
        });
    }

    fn prepare(&self, text: &str) {
        let _ = self.tx.send(Request {
            id: 0,
            text: text.to_string(),
            online: true,
            prepare: true,
        });
    }
}

fn make(cfg: &VoiceConfig, cache: &Cache, r: &Request) -> Option<Speech> {
    let target = TtsTarget::load(&cfg.state_dir)?;
    let key = cache_key(&target, &r.text);
    if let Some(s) = cache.get(&key) {
        log::debug!("voice: {:?} from the cache", r.text);
        return Some(s);
    }
    if !r.online {
        return None;
    }
    let t0 = std::time::Instant::now();
    let token = cfg
        .auth
        .access_token()
        .map_err(|e| log::info!(event = "reflex_voice"; "no token: {e}"))
        .ok()?;
    let wav = tts::fetch(&cfg.url, &token, &target, &r.text, FETCH_TIMEOUT)
        .map_err(|e| log::warn!(event = "reflex_voice"; "{}: {e}", target.engine))
        .ok()?;
    let Some(s) = parse_wav(&wav) else {
        log::warn!(event = "reflex_voice"; "{}: not a PCM WAV ({} bytes)", target.engine, wav.len());
        return None;
    };
    log::info!(
        event = "reflex_voice", ms = t0.elapsed().as_millis() as u64, engine = target.engine.as_str();
        "{:?} made by {} in {} ms", r.text, target.engine, t0.elapsed().as_millis()
    );
    if let Err(e) = cache.put(&key, &s) {
        log::warn!(event = "reflex_voice"; "cache: {e}");
    }
    Some(s)
}

/// The cache's directory in a state directory (for tools and tests).
pub fn cache_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("tts-cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech(n: usize) -> Speech {
        Speech {
            format: StreamFormat {
                rate: 22050,
                width: 2,
                channels: 1,
            },
            pcm: (0..n * 2).map(|i| i as u8).collect(),
        }
    }

    #[test]
    fn wav_round_trip() {
        let s = speech(100);
        assert_eq!(parse_wav(&wav_bytes(&s)), Some(s));
    }

    #[test]
    fn wav_from_a_pipe() {
        // ffmpeg writing to a pipe: sizes unknown (0xFFFFFFFF), a LIST chunk before the data.
        let s = speech(10);
        let mut b = wav_bytes(&s);
        b[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let data = b.split_off(36);
        b.extend_from_slice(b"LIST\x04\x00\x00\x00INFO");
        let mut data = data;
        data[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        b.extend_from_slice(&data);
        assert_eq!(parse_wav(&b), Some(s.clone()));
        // An odd byte at the end is not half a sample.
        b.push(7);
        assert_eq!(parse_wav(&b), Some(s));
    }

    #[test]
    fn not_a_wav() {
        assert_eq!(parse_wav(b"ID3\x04 an mp3"), None);
        assert_eq!(parse_wav(b"RIFF"), None);
        let mut b = wav_bytes(&speech(4));
        b[20] = 3; // IEEE float
        assert_eq!(parse_wav(&b), None);
    }

    #[test]
    fn duration() {
        assert_eq!(speech(22050).duration(), Duration::from_secs(1));
    }

    #[test]
    fn cache_keys() {
        let t = TtsTarget {
            engine: "tts.piper".into(),
            voice: "paola".into(),
            language: "it_IT".into(),
        };
        let k = cache_key(&t, "Timer annullato.");
        assert_eq!(k.len(), 16);
        assert_eq!(k, cache_key(&t, " Timer annullato. "), "stable");
        assert_ne!(k, cache_key(&t, "Timer annullato"));
        let other = TtsTarget {
            voice: "riccardo".into(),
            ..t.clone()
        };
        assert_ne!(k, cache_key(&other, "Timer annullato."), "another voice");
        // The parts cannot run into each other.
        let a = TtsTarget {
            engine: "ab".into(),
            voice: "c".into(),
            language: String::new(),
        };
        let b = TtsTarget {
            engine: "a".into(),
            voice: "bc".into(),
            language: String::new(),
        };
        assert_ne!(cache_key(&a, "x"), cache_key(&b, "x"));
    }

    #[test]
    fn eviction_least_recently_used() {
        let at = |s: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        let e = |name: &str, size: u64, used: u64| Entry {
            name: name.into(),
            size,
            used: at(used),
        };
        let all = vec![e("a", 10, 1), e("b", 10, 3), e("c", 10, 2), e("d", 10, 4)];
        assert_eq!(to_evict(all.clone(), 10, 1000), Vec::<String>::new());
        assert_eq!(to_evict(all.clone(), 2, 1000), vec!["c", "a"]);
        assert_eq!(to_evict(all.clone(), 10, 25), vec!["c", "a"]);
        // A big old file goes, smaller newer ones stay.
        let mixed = vec![e("big", 100, 1), e("x", 10, 5), e("y", 10, 6)];
        assert_eq!(to_evict(mixed, 10, 50), vec!["big"]);
    }

    #[test]
    fn cache_keeps_and_trims() {
        let dir = std::env::temp_dir().join(format!("hc-voice-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let c = Cache::with_bounds(dir.clone(), 2, 1 << 20);
        assert_eq!(c.get("k1"), None);
        c.put("k1", &speech(5)).unwrap();
        // Distinct times, older first (a coarse file system clock would tie them).
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(dir.join("k1.wav"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        c.put("k2", &speech(6)).unwrap();
        assert_eq!(c.get("k1"), Some(speech(5)), "a hit is a use");
        c.put("k3", &speech(7)).unwrap();
        assert_eq!(c.get("k2"), None, "the least recently used went");
        assert!(c.get("k1").is_some() && c.get("k3").is_some());
        // A broken file is a miss, and goes.
        std::fs::write(dir.join("k9.wav"), b"junk").unwrap();
        assert_eq!(c.get("k9"), None);
        assert!(!dir.join("k9.wav").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn voice_or_sound() {
        assert!(speak_reflex(true, "Timer annullato.", true, false));
        assert!(
            !speak_reflex(false, "Volume al 60%.", true, false),
            "no sound to replace"
        );
        assert!(!speak_reflex(true, "", true, false), "nothing to say");
        assert!(
            !speak_reflex(true, "Sono le 7:30.", false, false),
            "no voice thread"
        );
        assert!(
            !speak_reflex(true, "Sono le 7:30.", true, true),
            "do not disturb"
        );
    }

    #[test]
    fn the_thread_answers_from_the_cache_offline() {
        let dir = std::env::temp_dir().join(format!("hc-voice-thread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let t = TtsTarget {
            engine: "tts.piper".into(),
            ..Default::default()
        };
        let cfg = VoiceConfig {
            // Nothing listens there: offline anyway.
            url: "http://127.0.0.1:9".into(),
            auth: Arc::new(Auth::new("http://127.0.0.1:9", &dir.join("no-token"), &dir)),
            state_dir: dir.clone(),
        };
        let (tx, rx) = channel();
        let v = Voice::start(cfg, move |r| {
            let _ = tx.send(r);
        })
        .unwrap();
        // No voice chosen yet.
        v.say(1, "Timer annullato.", true);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap().speech,
            None
        );
        tts::TtsTarget::publish(&dir, Some(&t)).unwrap();
        Cache::new(cache_dir(&dir))
            .put(&cache_key(&t, "Timer annullato."), &speech(3))
            .unwrap();
        v.say(2, "Timer annullato.", false);
        let r = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((r.id, r.speech), (2, Some(speech(3))));
        v.say(3, "Sono le 7:30.", false);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap().speech,
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
