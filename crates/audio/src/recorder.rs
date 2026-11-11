// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Debug utterance recorder, off by default.
//!
//! Purpose: the user finds that Whisper often misunderstands him. Each file is exactly what
//! the brain heard (16 kHz mono after the channel pick and the AGC), one per utterance, to
//! build a speech-to-text bench from real requests. The capture thread writes the frames
//! through a buffered writer; the file is finalized on `stop`, and the oldest files beyond
//! `keep` are deleted. The directory is created with mode 0700 (voice is personal data).

use crate::{CAPTURE_RATE, RecorderConfig, lock};
use std::collections::VecDeque;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type Writer = hound::WavWriter<BufWriter<std::fs::File>>;

struct State {
    /// The last `pre_roll_ms` of capture while idle.
    ring: VecDeque<i16>,
    ring_cap: usize,
    /// The last `wake_context_ms` while idle, for `<name>-wake.wav`.
    context: VecDeque<i16>,
    context_cap: usize,
    writer: Option<(PathBuf, Writer)>,
}

struct Shared {
    cfg: RecorderConfig,
    state: Mutex<State>,
}

/// Records utterances to WAV files ([`crate::Audio::recorder`]).
#[derive(Clone)]
pub struct Recorder(Arc<Shared>);

impl Recorder {
    pub(crate) fn new(cfg: RecorderConfig) -> Self {
        let ring_cap = (CAPTURE_RATE as u64 * cfg.pre_roll_ms as u64 / 1000) as usize;
        let context_cap = (CAPTURE_RATE as u64 * cfg.wake_context_ms as u64 / 1000) as usize;
        Self(Arc::new(Shared {
            cfg,
            state: Mutex::new(State {
                ring: VecDeque::with_capacity(ring_cap),
                ring_cap,
                context: VecDeque::with_capacity(context_cap),
                context_cap,
                writer: None,
            }),
        }))
    }

    /// Begins `<dir>/<yyyymmdd-hhmmss>-<label>.wav` with the pre-roll. A recording in
    /// progress is closed first. Failures are logged (the recorder is a debug tool).
    pub fn start(&self, label: &str) {
        if self.is_recording() {
            self.stop();
        }
        let dir = &self.0.cfg.dir;
        if let Err(e) = create_private_dir(dir) {
            log::warn!("recorder: {}: {e}", dir.display());
            return;
        }
        let path = unique_path(dir, &format!("{}-{}", local_timestamp(), sanitize(label)));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: CAPTURE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = match hound::WavWriter::create(&path, spec) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("recorder: {}: {e}", path.display());
                return;
            }
        };
        let mut st = lock(&self.0.state);
        let mut ok = true;
        for &s in st.ring.iter() {
            ok &= w.write_sample(s).is_ok();
        }
        st.ring.clear();
        if !st.context.is_empty() {
            let ctx = path.with_file_name(format!(
                "{}-wake.wav",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ));
            let written = hound::WavWriter::create(&ctx, spec).and_then(|mut c| {
                for &s in st.context.iter() {
                    c.write_sample(s)?;
                }
                c.finalize()
            });
            if let Err(e) = written {
                log::warn!("recorder: {}: {e}", ctx.display());
            }
            st.context.clear();
        }
        if !ok {
            log::warn!("recorder: {}: write failed", path.display());
            return;
        }
        log::debug!("recorder: started {}", path.display());
        st.writer = Some((path, w));
    }

    /// Closes the file and returns its path; deletes the oldest files beyond `keep`.
    pub fn stop(&self) -> Option<PathBuf> {
        let (path, w) = lock(&self.0.state).writer.take()?;
        if let Err(e) = w.finalize() {
            log::warn!("recorder: {}: {e}", path.display());
        }
        prune(&self.0.cfg.dir, self.0.cfg.keep);
        log::info!("recorder: wrote {}", path.display());
        Some(path)
    }

    /// The last `wake_context_ms` heard while not recording (the character's own voice while it
    /// answers, a stop word over it) as `<dir>/<yyyymmdd-hhmmss>-<label>.wav`, kept as it is;
    /// its path, or None (nothing kept, no directory, a write error).
    pub fn save_context(&self, label: &str) -> Option<PathBuf> {
        let st = lock(&self.0.state);
        if st.context.is_empty() {
            return None;
        }
        let dir = &self.0.cfg.dir;
        if let Err(e) = create_private_dir(dir) {
            log::warn!("recorder: {}: {e}", dir.display());
            return None;
        }
        let path = unique_path(dir, &format!("{}-{}", local_timestamp(), sanitize(label)));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: CAPTURE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let written = hound::WavWriter::create(&path, spec).and_then(|mut w| {
            for &s in st.context.iter() {
                w.write_sample(s)?;
            }
            w.finalize()
        });
        match written {
            Ok(()) => Some(path),
            Err(e) => {
                log::warn!("recorder: {}: {e}", path.display());
                None
            }
        }
    }

    pub fn is_recording(&self) -> bool {
        lock(&self.0.state).writer.is_some()
    }

    /// Capture thread: one frame after the channel pick and the AGC.
    pub(crate) fn tap(&self, frame: &[i16]) {
        let mut st = lock(&self.0.state);
        if let Some((path, w)) = st.writer.as_mut() {
            let mut w16 = w.get_i16_writer(frame.len() as u32);
            for &s in frame {
                w16.write_sample(s);
            }
            if let Err(e) = w16.flush() {
                log::warn!("recorder: {}: {e}; recording dropped", path.display());
                st.writer = None;
            }
        } else {
            if st.ring_cap > 0 {
                st.ring.extend(frame.iter().copied());
                let extra = st.ring.len().saturating_sub(st.ring_cap);
                st.ring.drain(..extra);
            }
            if st.context_cap > 0 {
                st.context.extend(frame.iter().copied());
                let extra = st.context.len().saturating_sub(st.context_cap);
                st.context.drain(..extra);
            }
        }
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

fn sanitize(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    if s.is_empty() { "utterance".into() } else { s }
}

fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let mut p = dir.join(format!("{stem}.wav"));
    let mut k = 2;
    while p.exists() {
        p = dir.join(format!("{stem}-{k}.wav"));
        k += 1;
    }
    p
}

/// `yyyymmdd-hhmmss` in local time.
fn local_timestamp() -> String {
    // SAFETY: time(NULL) and localtime_r only write into the struct we pass.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return "00000000-000000".into();
        }
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// True for names written by the recorder: `dddddddd-dddddd-*.wav`.
fn is_recording_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() > 20
        && name.ends_with(".wav")
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'-'
        && !name.ends_with("-wake.wav")
}

fn prune(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, String, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_recording_name(&name) {
                return None;
            }
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, name, e.path()))
        })
        .collect();
    // Oldest first: by name (the timestamp), then by modification time for equal seconds.
    files.sort_by(|a, b| a.1[..15].cmp(&b.1[..15]).then(a.0.cmp(&b.0)));
    let extra = files.len().saturating_sub(keep);
    for (_, _, path) in files.into_iter().take(extra) {
        match std::fs::remove_file(&path) {
            Ok(()) => log::debug!("recorder: deleted {}", path.display()),
            Err(e) => log::warn!("recorder: {}: {e}", path.display()),
        }
        // Its sidecar (the body writes the reflex result next to an utterance) and what came
        // before the wake word, if any.
        let _ = std::fs::remove_file(path.with_extension("json"));
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(path.with_file_name(format!("{stem}-wake.wav")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_recording_name("20261103-101500-utterance.wav"));
        assert!(!is_recording_name("notes.wav"));
        assert!(!is_recording_name("20261103-101500-x.txt"));
        assert_eq!(sanitize("a b/../c"), "a_b____c");
        assert_eq!(sanitize(""), "utterance");
        assert_eq!(local_timestamp().len(), 15);
    }
}
