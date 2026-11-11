// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's side of the local reflexes: it keeps the `home-companion-reflex`
//! helper running (started at once, restarted with a growing pause when it dies, restarted for a
//! new language) and never waits for it: requests go through a bounded queue to a writer thread,
//! replies come back to the core as [`CoreEvent::Reflex`](crate::core::CoreEvent).
//!
//! The decision itself (grammar, confidence, coverage) is in the core with [`Reflexes`]; Vosk's
//! C++ code only ever runs in the helper.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reflex::proto::{Reply, Request};
use reflex::{Grammar, Word};

/// What the helper tells the core.
#[derive(Debug, Clone, PartialEq)]
pub enum ReflexEvent {
    /// Loaded: reflexes on for this language.
    Ready {
        language: String,
        phrases: usize,
        load_ms: f64,
    },
    /// The helper stopped (crash, language change): reflexes off until the next `Ready`.
    Lost,
    Result {
        id: u64,
        endpoint: bool,
        text: String,
        words: Vec<Word>,
        ms: f64,
    },
}

/// The helper as the core sees it (a fake one in the tests).
pub trait ReflexLink: Send {
    /// The helper answered `ready` for the current language.
    fn ready(&self) -> bool;
    /// Queues a request; false when it could not (not ready, queue full: the helper is behind).
    fn send(&self, r: Request) -> bool;
    /// Another language: the helper restarts with its model and grammar.
    fn set_language(&self, code: &str);
}

/// Where the helper and its data are.
#[derive(Debug, Clone)]
pub struct HelperPaths {
    pub helper: PathBuf,
    /// `<model_dir>/<lang>`.
    pub model_dir: PathBuf,
    /// `<grammar_dir>/<lang>.toml` (the character's `grammar/`).
    pub grammar_dir: PathBuf,
}

impl HelperPaths {
    fn model(&self, lang: &str) -> PathBuf {
        self.model_dir.join(lang)
    }

    pub fn grammar(&self, lang: &str) -> PathBuf {
        self.grammar_dir.join(format!("{lang}.toml"))
    }
}

struct Shared {
    paths: HelperPaths,
    language: Mutex<String>,
    /// Bumped by a language change: the supervisor restarts the helper at once.
    generation: AtomicU64,
    ready: AtomicBool,
    queue: Mutex<Option<SyncSender<Vec<u8>>>>,
    child_pid: Mutex<Option<u32>>,
    stop: AtomicBool,
}

/// The real helper: a supervisor thread owns the process.
pub struct ReflexClient {
    shared: Arc<Shared>,
}

/// About 5 s of 20 ms blocks: more means the helper is stuck.
const QUEUE: usize = 256;

impl ReflexClient {
    pub fn start(
        paths: HelperPaths,
        language: &str,
        emit: impl Fn(ReflexEvent) + Send + 'static,
    ) -> ReflexClient {
        let shared = Arc::new(Shared {
            paths,
            language: Mutex::new(language.to_string()),
            generation: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            queue: Mutex::new(None),
            child_pid: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let s = shared.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("reflex".into())
            .spawn(move || supervise(&s, &emit))
        {
            log::error!("reflexes: cannot start the supervisor: {e}");
        }
        ReflexClient { shared }
    }
}

impl ReflexLink for ReflexClient {
    fn ready(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire)
    }

    fn send(&self, r: Request) -> bool {
        if !self.ready() {
            return false;
        }
        let q = self.shared.queue.lock().unwrap();
        match q.as_ref().map(|q| q.try_send(r.encode())) {
            Some(Ok(())) => true,
            Some(Err(TrySendError::Full(_))) => {
                log::warn!("reflexes: the helper is behind (queue full)");
                false
            }
            _ => false,
        }
    }

    fn set_language(&self, code: &str) {
        let mut l = self.shared.language.lock().unwrap();
        if *l == code {
            return;
        }
        *l = code.to_string();
        drop(l);
        self.shared.ready.store(false, Ordering::Release);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        kill(&self.shared);
    }
}

impl Drop for ReflexClient {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        kill(&self.shared);
    }
}

fn kill(s: &Shared) {
    if let Some(pid) = *s.child_pid.lock().unwrap() {
        // SAFETY: a plain signal to our own child.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
}

/// Runs the helper again and again: back-off from 1 s to 60 s, back to 1 s after a helper that
/// lived a minute; at once after a language change; waits quietly while the language has no
/// model or grammar.
fn supervise(s: &Shared, emit: &dyn Fn(ReflexEvent)) {
    let mut pause = Duration::from_secs(1);
    let mut said_missing: Option<String> = None;
    while !s.stop.load(Ordering::Acquire) {
        let generation = s.generation.load(Ordering::Acquire);
        let lang = s.language.lock().unwrap().clone();
        let (model, grammar) = (s.paths.model(&lang), s.paths.grammar(&lang));
        let missing = [&s.paths.helper, &model, &grammar]
            .into_iter()
            .find(|p| !p.exists())
            .cloned();
        if let Some(m) = missing {
            if said_missing.as_deref() != Some(&lang) {
                log::info!(event = "reflexes_off", language = lang.as_str(); "reflexes off for {lang}: {} is missing", m.display());
                said_missing = Some(lang);
            }
            wait(s, generation, Duration::from_secs(3600));
            continue;
        }
        said_missing = None;
        let started = Instant::now();
        match run_once(s, &model, &grammar, emit) {
            Ok(()) => {}
            Err(e) => log::warn!(event = "reflex_helper", error = e.as_str(); "reflexes: {e}"),
        }
        s.ready.store(false, Ordering::Release);
        *s.queue.lock().unwrap() = None;
        *s.child_pid.lock().unwrap() = None;
        emit(ReflexEvent::Lost);
        if s.stop.load(Ordering::Acquire) {
            break;
        }
        if s.generation.load(Ordering::Acquire) != generation {
            pause = Duration::from_secs(1);
            continue;
        }
        if started.elapsed() > Duration::from_secs(60) {
            pause = Duration::from_secs(1);
        }
        log::info!("reflexes: helper restarts in {} s", pause.as_secs());
        wait(s, generation, pause);
        pause = (pause * 2).min(Duration::from_secs(60));
    }
}

/// Sleeps up to `d`, waking early for a language change or the end.
fn wait(s: &Shared, generation: u64, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until
        && !s.stop.load(Ordering::Acquire)
        && s.generation.load(Ordering::Acquire) == generation
    {
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn run_once(
    s: &Shared,
    model: &Path,
    grammar: &Path,
    emit: &dyn Fn(ReflexEvent),
) -> Result<(), String> {
    let mut child: Child = Command::new(&s.paths.helper)
        .arg("--model")
        .arg(model)
        .arg("--grammar")
        .arg(grammar)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("{}: {e}", s.paths.helper.display()))?;
    *s.child_pid.lock().unwrap() = Some(child.id());
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE);
    let writer = std::thread::Builder::new()
        .name("reflex-writer".into())
        .spawn(move || {
            for b in rx {
                if stdin.write_all(&b).and_then(|()| stdin.flush()).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    *s.queue.lock().unwrap() = Some(tx);
    let started = Instant::now();
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| e.to_string())?;
        match Reply::parse(&line) {
            Ok(Reply::Ready {
                language,
                phrases,
                load_ms,
            }) => {
                log::info!(
                    event = "reflexes_ready", language = language.as_str(), phrases = phrases, load_ms = load_ms, rss_mb = child_rss_mb(child.id());
                    "reflexes ready: {language}, {phrases} phrases, loaded in {load_ms:.0} ms ({:.0} MB)",
                    child_rss_mb(child.id())
                );
                s.ready.store(true, Ordering::Release);
                emit(ReflexEvent::Ready {
                    language,
                    phrases,
                    load_ms,
                });
            }
            Ok(Reply::Result {
                id,
                endpoint,
                text,
                words,
                ms,
                ..
            }) => emit(ReflexEvent::Result {
                id,
                endpoint,
                text,
                words,
                ms,
            }),
            // Partials are for the gate's early release (reflex::verdict), not wired yet.
            Ok(Reply::Partial { id, text, audio_s }) => {
                log::trace!("reflexes: partial {id} at {audio_s:.2} s: {text:?}")
            }
            Ok(Reply::Error { message }) => log::warn!("reflexes: helper: {message}"),
            Err(e) => log::warn!("reflexes: {e}"),
        }
    }
    // Its output closed: it is gone (or going).
    *s.queue.lock().unwrap() = None;
    let _ = writer.join();
    let status = child.wait().map_err(|e| e.to_string())?;
    if !s.stop.load(Ordering::Acquire) {
        log::info!(
            "reflexes: helper ended ({status}) after {:.0} s",
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn child_rss_mb(pid: u32) -> f64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|k| k.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

// ---- the core's side ------------------------------------------------------------------------

/// What the core needs for reflexes: the link, the grammar of the language the helper runs, the
/// rule's threshold, and the words of the answers.
pub struct Reflexes {
    pub link: Box<dyn ReflexLink>,
    pub grammar: Option<Grammar>,
    pub threshold: f32,
    pub max_listen: Duration,
    pub grammar_dir: PathBuf,
    pub locales_dir: PathBuf,
    pub texts: Option<locale::Locale>,
}

impl Reflexes {
    pub fn new(
        link: Box<dyn ReflexLink>,
        cfg: &crate::config::ReflexesCfg,
        grammar_dir: PathBuf,
        locales_dir: PathBuf,
        language: &str,
    ) -> Reflexes {
        let mut r = Reflexes {
            link,
            grammar: None,
            threshold: cfg.threshold,
            max_listen: Duration::from_secs_f32(cfg.max_seconds),
            grammar_dir,
            locales_dir,
            texts: None,
        };
        r.load(language);
        r
    }

    fn load(&mut self, language: &str) {
        let path = self.grammar_dir.join(format!("{language}.toml"));
        self.grammar = if path.exists() {
            match Grammar::load(&path) {
                Ok(g) => Some(g),
                Err(e) => {
                    log::warn!("reflexes: {e}");
                    None
                }
            }
        } else {
            None
        };
        self.texts = locale::load_or_english(&self.locales_dir, language).ok();
    }

    pub fn set_language(&mut self, language: &str) {
        self.load(language);
        self.link.set_language(language);
    }

    /// On for this utterance: the helper is up and the body has the same grammar.
    pub fn usable(&self) -> bool {
        self.grammar.is_some() && self.link.ready()
    }

    /// An answer's words (`reflex_*` in locales/<lang>.toml) with `{x}` replaced.
    pub fn text(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut t = self
            .texts
            .as_ref()
            .map(|l| l.text(key).to_string())
            .unwrap_or_else(|| key.to_string());
        for (k, v) in args {
            t = t.replace(&format!("{{{k}}}"), v);
        }
        t
    }
}

/// The helper next to the body's own binary unless the configuration names one.
pub fn helper_path(configured: &str) -> PathBuf {
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("home-companion-reflex")))
        .unwrap_or_else(|| PathBuf::from("home-companion-reflex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_next_to_the_body() {
        assert_eq!(helper_path("/x/y"), PathBuf::from("/x/y"));
        assert!(helper_path("").ends_with("home-companion-reflex"));
    }

    #[test]
    fn missing_helper_is_quietly_off() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-reflex-test-{}", std::process::id()));
        let (tx, rx) = std::sync::mpsc::channel();
        let c = ReflexClient::start(
            HelperPaths {
                helper: dir.join("nope"),
                model_dir: dir.clone(),
                grammar_dir: dir.clone(),
            },
            "it",
            move |e| {
                let _ = tx.send(e);
            },
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(!c.ready());
        assert!(!c.send(Request::Start(1)));
        assert!(rx.try_recv().is_err());
    }
}
