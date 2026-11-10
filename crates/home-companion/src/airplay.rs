// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! AirPlay receiver: the companion runs UxPlay as a child and stays the
//! only owner of the speaker. UxPlay never opens ALSA (the q6asm DSP refuses writes that do not
//! start with whole periods): its GStreamer audio sink is `fdsink fd=3`, raw PCM (S16LE,
//! 44.1 kHz, stereo) on a pipe read here into a mixer stream with its own gain, ducked while the
//! character listens, thinks, speaks or rings.
//!
//! Our patch (tools/airplay/uxplay-1.73.7-home-companion.patch, option `-hc <dir>`) adds `@hc …` lines
//! to UxPlay's stdout: the sender's name, its volume slider, the DACP identity for the remote,
//! title/artist/album, the cover art (a file in `<dir>`) and the end of the session. With it
//! UxPlay leaves the samples alone: the slider becomes the companion's own volume, the one of
//! the Vol± keys and the screen's bar.
//!
//! The sender's transport (pause, next, previous, volume steps) is DACP: the sender announces
//! `iTunes_Ctrl_<DACP-ID>._dacp._tcp` over mDNS (resolved through the system's avahi-daemon
//! with `busctl`), and takes `GET /ctrl-int/1/<command>` with the `Active-Remote` header.
//!
//! A stream exists only while audio flows: it opens on the first data and is finished after
//! `IDLE_CLOSE` without any (pause, end of the session), so the pcm can close again; the same
//! two moments are "playing" and "paused" for the screen. Stop (the screen's button) mutes
//! AirPlay until the sender plays again after a pause.

use audio::{Player, Stream, StreamFormat};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The PCM of UxPlay's audio sink (see `audio_sink`): 44.1 kHz, 16 bit, two channels.
const PCM: StreamFormat = StreamFormat {
    rate: 44100,
    width: 2,
    channels: 2,
};
/// The music page's spectrum (crate::spectrum), measured from the audio: off, the page shows a
/// larger picture instead (its `music-spectrum-shown`). Both switch on together.
const SPECTRUM: bool = false;
/// No audio for this long: the stream is finished (a pause or the end of the session).
const IDLE_CLOSE: Duration = Duration::from_millis(1500);
/// Audio after a gap this long is "playing again": a Stop is forgotten.
const RESUME_GAP: Duration = Duration::from_secs(2);
/// A cover this recent belongs to the title that follows it (see `apply`).
const COVER_BEFORE_TITLE: Duration = Duration::from_secs(3);
/// More than this queued in the mixer: data is dropped to stay in time with the sender. With
/// `-async` UxPlay hands the audio on at its own time, so this is only a safety net.
const MAX_QUEUED: Duration = Duration::from_millis(300);
/// A DACP request: the sender answers at once or not at all.
const DACP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AirplayCfg {
    /// Off: no AirPlay daemon runs at all.
    pub enabled: bool,
    /// The name in the AirPlay menu ("" = the satellite's name).
    pub name: String,
    /// UxPlay with our patch (tools/airplay/uxplay-1.73.7-home-companion.patch).
    pub uxplay: String,
    /// UxPlay's ports: TCP this one and the next, UDP this one and the next two (packaging/home-companion.nft).
    pub port: u16,
    /// Music level while the character listens, thinks, speaks or rings (0..1).
    pub duck: f32,
}

impl Default for AirplayCfg {
    fn default() -> Self {
        AirplayCfg {
            enabled: false,
            name: String::new(),
            uxplay: "/opt/home-companion/airplay/uxplay".into(),
            port: 6010,
            duck: 0.15,
        }
    }
}

/// What the core hears about the music.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The sender ("iPhone di Andrea").
    pub source: String,
    /// The last cover art, written in the state directory.
    pub cover: Option<PathBuf>,
    /// When the cover arrived: a sender may send it just before the new track's title.
    pub cover_at: Option<Instant>,
    pub playing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MusicEvent {
    /// Something changed (track, cover, playing or paused).
    Now(NowPlaying),
    /// The session ended: nothing to show any more.
    Gone,
    /// The sender did not take a remote command (no DACP, session gone).
    RemoteFailed(Remote),
    /// Screen mirroring: the sender's name, the frames' socket (None = over).
    Mirror(Option<(String, PathBuf)>),
    /// The mirrored picture's aspect ratio (the frames may be padded to whole macroblocks).
    MirrorAspect(f32),
    /// The sender moved its volume slider: the companion's volume, in percent (0 = the bottom
    /// or muted; the volume's own lowest step applies).
    Volume(u8),
    /// The music page's spectrum (crate::spectrum): band levels and peaks, 0..1.
    Spectrum([f32; crate::spectrum::BANDS], [f32; crate::spectrum::BANDS]),
}

/// What the screen's buttons and the Vol± keys ask the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    PlayPause,
    Pause,
    Play,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
}

impl Remote {
    /// The DACP command (`/ctrl-int/1/<command>`).
    pub fn command(self) -> &'static str {
        match self {
            Remote::PlayPause => "playpause",
            Remote::Pause => "pause",
            Remote::Play => "play",
            Remote::Next => "nextitem",
            Remote::Previous => "previtem",
            Remote::VolumeUp => "volumeup",
            Remote::VolumeDown => "volumedown",
        }
    }
}

/// The sender's remote control, from `@hc dacp <id> <active-remote>`.
#[derive(Debug, Clone, PartialEq)]
struct Dacp {
    id: String,
    active_remote: String,
    /// Resolved once per session.
    addr: Option<SocketAddr>,
}

struct State {
    stream: Option<Stream>,
    last_data: Option<Instant>,
    ducked: bool,
    muted: bool,
    dropped: u64,
    /// When the stream opened, until its queue was logged (latency check).
    opened: Option<Instant>,
    now: NowPlaying,
    /// A sender is connected (from its first sign of life to the end of the session).
    session: bool,
    /// The sender mirrors its screen.
    mirroring: bool,
    dacp: Option<Dacp>,
    /// The levels of the audio on its way to the mixer, for the music page.
    spectrum: crate::spectrum::Spectrum,
}

struct Inner {
    player: Player,
    cfg: AirplayCfg,
    name: String,
    dir: PathBuf,
    state: Mutex<State>,
    child: Mutex<Option<Child>>,
    stop: AtomicBool,
    /// The child was stopped on purpose (drop_session): start it again without waiting.
    quick: AtomicBool,
    /// The control center's switch: off, no daemon runs (the device leaves the AirPlay menu).
    enabled: AtomicBool,
    events: Arc<dyn Fn(MusicEvent) + Send + Sync>,
}

/// Handle kept by the core: duck, Stop, shutdown. Cheap to clone.
#[derive(Clone)]
pub struct Music(Arc<Inner>);

impl Music {
    /// Starts UxPlay and the reader threads. `events` gets what to show.
    pub fn start(
        cfg: &AirplayCfg,
        name: &str,
        state_dir: &Path,
        player: Player,
        events: impl Fn(MusicEvent) + Send + Sync + 'static,
    ) -> Result<Music, String> {
        if !Path::new(&cfg.uxplay).is_file() {
            return Err(format!("{}: not found", cfg.uxplay));
        }
        let dir = state_dir.join("airplay");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = if cfg.name.is_empty() { name } else { &cfg.name }.to_string();
        let m = Music(Arc::new(Inner {
            player,
            cfg: cfg.clone(),
            name: name.clone(),
            dir,
            state: Mutex::new(State {
                stream: None,
                last_data: None,
                ducked: false,
                muted: false,
                dropped: 0,
                opened: None,
                now: NowPlaying::default(),
                session: false,
                mirroring: false,
                dacp: None,
                spectrum: crate::spectrum::Spectrum::new(PCM.rate),
            }),
            child: Mutex::new(None),
            stop: AtomicBool::new(false),
            quick: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
            events: Arc::new(events),
        }));
        {
            let m = m.clone();
            std::thread::Builder::new()
                .name("airplay".into())
                .spawn(move || m.supervise())
                .map_err(|e| e.to_string())?;
        }
        {
            let m = m.clone();
            std::thread::Builder::new()
                .name("airplay-idle".into())
                .spawn(move || m.watch_idle())
                .map_err(|e| e.to_string())?;
        }
        open_debug_log();
        log::info!(event = "airplay", name = name.as_str(); "AirPlay: \"{name}\"");
        Ok(m)
    }

    /// Lower the music under the character (or bring it back).
    pub fn duck(&self, on: bool) {
        let mut st = lock(&self.0.state);
        if st.ducked != on {
            st.ducked = on;
            if let Some(s) = &st.stream {
                s.set_gain(if on { self.0.cfg.duck } else { 1.0 });
            }
        }
    }

    /// Music is coming out of the speaker now.
    pub fn playing(&self) -> bool {
        let st = lock(&self.0.state);
        st.stream.is_some() && !st.muted
    }

    /// The control center's switch: off stops the daemon now, on starts it again.
    pub fn set_enabled(&self, on: bool) {
        if self.0.enabled.swap(on, Ordering::Relaxed) != on {
            log::info!(event = "airplay_switch", on = on; "AirPlay {}", if on { "on" } else { "off" });
            if !on {
                self.kill_child();
            }
        }
    }

    pub fn muted(&self) -> bool {
        lock(&self.0.state).muted
    }

    /// Asks the sender to pause/play, change track or step its volume (in the background:
    /// resolving and asking take a moment). A refusal comes back as `RemoteFailed`.
    pub fn remote(&self, cmd: Remote) {
        // Pause/play shows at once: the sound follows about 2 s later (each packet at its
        // time), the sender's own answer is the end of the stream or new audio.
        let flipped = match cmd {
            Remote::PlayPause => self.flip_playing(),
            Remote::Pause => self.set_playing(false),
            Remote::Play => self.set_playing(true),
            _ => false,
        };
        let m = self.clone();
        let _ = std::thread::Builder::new()
            .name("airplay-remote".into())
            .spawn(move || {
                let res = m.send_remote(cmd);
                match &res {
                    Ok(()) => log::info!(event = "airplay_remote", cmd = cmd.command(); "AirPlay: {} sent to the sender", cmd.command()),
                    Err(e) => log::warn!(event = "airplay_remote", cmd = cmd.command(); "AirPlay: {} refused: {e}", cmd.command()),
                }
                if res.is_err() {
                    if flipped {
                        m.flip_playing();
                    }
                    (m.0.events)(MusicEvent::RemoteFailed(cmd));
                }
            });
    }

    /// Playing or paused on the screen now (only during a session); true if it changed.
    fn set_playing(&self, on: bool) -> bool {
        if lock(&self.0.state).now.playing == on {
            return false;
        }
        self.flip_playing()
    }

    /// A sender is connected (music or a mirrored screen).
    pub fn session(&self) -> bool {
        lock(&self.0.state).session
    }

    /// Playing ⇄ paused on the screen now (only during a session); true if it changed.
    fn flip_playing(&self) -> bool {
        let mut st = lock(&self.0.state);
        if !st.session {
            return false;
        }
        st.now.playing = !st.now.playing;
        (self.0.events)(MusicEvent::Now(st.now.clone()));
        true
    }

    /// The control center's pause/play: a local mute (the sender keeps playing), or back.
    /// Returns muted.
    pub fn toggle_mute(&self) -> bool {
        if self.muted() {
            lock(&self.0.state).muted = false;
            log::info!(event = "airplay_stop", muted = false; "AirPlay: back on");
            false
        } else {
            self.mute();
            true
        }
    }

    /// Stop: silence until the sender plays again after a pause.
    pub fn mute(&self) {
        let mut st = lock(&self.0.state);
        if let Some(s) = st.stream.take() {
            s.abort();
        }
        st.muted = true;
        log::info!(event = "airplay_stop"; "AirPlay: muted until the sender plays again");
    }

    pub fn shutdown(&self) {
        self.0.stop.store(true, Ordering::Relaxed);
        if let Some(c) = lock(&self.0.child).as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(s) = lock(&self.0.state).stream.take() {
            s.abort();
        }
    }

    /// Closes the sender's session (screen mirroring or music) from here: UxPlay is stopped and
    /// started again at once; the sender sees the end of the session, as if it had stopped.
    pub fn drop_session(&self) {
        log::info!(event = "airplay_session", state = "dropped"; "AirPlay: session closed from the device");
        self.0.quick.store(true, Ordering::Relaxed);
        // UxPlay goes without a word: the mirroring is over now (the home's card goes too).
        self.set_mirroring(false);
        self.kill_child();
    }

    fn kill_child(&self) {
        if let Some(c) = lock(&self.0.child).as_mut() {
            let _ = c.kill();
        }
    }

    // ---- remote ------------------------------------------------------------------------

    fn send_remote(&self, cmd: Remote) -> Result<(), String> {
        let dacp = lock(&self.0.state)
            .dacp
            .clone()
            .ok_or("no remote from this sender")?;
        let addr = match dacp.addr {
            Some(a) => a,
            None => {
                let a = resolve_dacp(&dacp.id)?;
                if let Some(d) = lock(&self.0.state)
                    .dacp
                    .as_mut()
                    .filter(|d| d.id == dacp.id)
                {
                    d.addr = Some(a);
                }
                a
            }
        };
        let status = dacp_get(addr, cmd.command(), &dacp.active_remote);
        if status.is_err() {
            // The sender may have moved to another port: resolve again next time.
            if let Some(d) = lock(&self.0.state).dacp.as_mut() {
                d.addr = None;
            }
        }
        match status? {
            200..=299 => Ok(()),
            s => Err(format!("HTTP {s}")),
        }
    }

    // ---- threads ----------------------------------------------------------------------

    /// Runs UxPlay, again after a crash, waiting longer each time it fails quickly.
    fn supervise(&self) {
        let mut backoff = Duration::from_secs(2);
        while !self.0.stop.load(Ordering::Relaxed) {
            if !self.0.enabled.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            let started = Instant::now();
            match self.spawn_uxplay() {
                Ok((mut c, pcm)) => {
                    let out = c.stdout.take();
                    let err = c.stderr.take();
                    *lock(&self.0.child) = Some(c);
                    if let Some(err) = err {
                        std::thread::spawn(move || {
                            for line in BufReader::new(err).lines().map_while(Result::ok) {
                                log_uxplay(&line);
                            }
                        });
                    }
                    let reader = {
                        let m = self.clone();
                        std::thread::Builder::new()
                            .name("airplay-pcm".into())
                            .spawn(move || m.read_pcm(&mut std::fs::File::from(pcm)))
                    };
                    if let Some(out) = out {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            self.on_line(&line);
                        }
                    }
                    if let Ok(r) = reader {
                        let _ = r.join();
                    }
                }
                Err(e) => log::warn!(event = "airplay_error"; "AirPlay: uxplay: {e}"),
            }
            // UxPlay's stdout closed: it died (or we are stopping).
            let code = lock(&self.0.child)
                .take()
                .map(|mut c| {
                    let _ = c.kill();
                    c.wait().map(|s| s.to_string()).unwrap_or_default()
                })
                .unwrap_or_default();
            self.close_stream();
            self.set_mirroring(false);
            self.end_session();
            if self.0.stop.load(Ordering::Relaxed) {
                break;
            }
            if !self.0.enabled.load(Ordering::Relaxed) {
                log::info!(event = "airplay_switch", on = false; "AirPlay: daemon stopped");
                continue;
            }
            if self.0.quick.swap(false, Ordering::Relaxed) {
                continue;
            }
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(2);
            }
            log::warn!(event = "airplay_restart", exit = code.as_str(); "AirPlay: uxplay ended ({code}), again in {} s", backoff.as_secs());
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }

    /// UxPlay with its audio on a pipe (its fd 3); returns the child and the pipe's read end.
    fn spawn_uxplay(&self) -> std::io::Result<(Child, OwnedFd)> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        // unixfdsink binds the video socket when UxPlay starts and never removes it: one left
        // by the previous run makes the bind (and UxPlay) fail.
        let _ = std::fs::remove_file(video_socket(&self.0.dir));
        let args = uxplay_args(&self.0.name, self.0.cfg.port, &self.0.dir);
        let mut c = Command::new(&self.0.cfg.uxplay);
        c.args(&args)
            // GStreamer's warnings and errors on stderr (the RAM debug log), without colours.
            .env("GST_DEBUG", "2")
            .env("GST_DEBUG_NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let w = wr.as_raw_fd();
        use std::os::unix::process::CommandExt;
        unsafe {
            c.pre_exec(move || {
                // The audio sink's fd; dup2 leaves the copy without O_CLOEXEC.
                if w == 3 {
                    libc::fcntl(3, libc::F_SETFD, 0);
                } else if libc::dup2(w, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // The child goes with us, even if we are killed.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = c.spawn()?;
        drop(wr); // only the child writes: its end closing ends our reads
        Ok((child, rd))
    }

    fn read_pcm(&self, out: &mut impl Read) {
        let mut buf = vec![0u8; 8192];
        loop {
            match out.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => self.on_pcm(&buf[..n]),
            }
        }
    }

    fn on_pcm(&self, data: &[u8]) {
        let now = Instant::now();
        let mut st = lock(&self.0.state);
        let gap = st.last_data.map(|t| now.duration_since(t));
        st.last_data = Some(now);
        if st.muted {
            if gap.is_some_and(|g| g >= RESUME_GAP) {
                st.muted = false;
            } else {
                return;
            }
        }
        if st.stream.is_none() {
            let s = self.0.player.open_stream(PCM);
            s.set_gain(if st.ducked { self.0.cfg.duck } else { 1.0 });
            log::info!(event = "airplay_audio", state = "start"; "AirPlay: audio");
            st.stream = Some(s);
            st.opened = Some(now);
            st.session = true;
            st.now.playing = true;
            (self.0.events)(MusicEvent::Now(st.now.clone()));
        }
        let queued = st.stream.as_ref().expect("opened above").queued();
        if st
            .opened
            .is_some_and(|t| now.duration_since(t) > Duration::from_secs(5))
        {
            st.opened = None;
            log::info!(event = "airplay_latency", queued_ms = queued.as_millis() as u64; "AirPlay: {} ms queued in the mixer", queued.as_millis());
        }
        if queued > MAX_QUEUED {
            st.dropped += 1;
            if st.dropped.is_power_of_two() {
                log::info!(event = "airplay_drop", chunks = st.dropped; "AirPlay: {} chunks dropped to stay in time (a burst from the sender)", st.dropped);
            }
            return;
        }
        if let Some(s) = &st.stream {
            let _ = s.push(data);
        }
        // The spectrum is off on the music page: not measured.
        if SPECTRUM && let Some((b, p)) = st.spectrum.feed(data, now) {
            (self.0.events)(MusicEvent::Spectrum(b, p));
        }
    }

    /// No more audio: the stream goes, the screen shows the track paused.
    fn close_stream(&self) {
        let mut st = lock(&self.0.state);
        if let Some(s) = st.stream.take() {
            s.finish();
            if SPECTRUM {
                let (b, p) = st.spectrum.silence();
                (self.0.events)(MusicEvent::Spectrum(b, p));
            }
            log::info!(event = "airplay_audio", state = "stop"; "AirPlay: no audio");
            if st.session && st.now.playing {
                st.now.playing = false;
                (self.0.events)(MusicEvent::Now(st.now.clone()));
            }
        }
    }

    fn set_mirroring(&self, on: bool) {
        let mut st = lock(&self.0.state);
        if st.mirroring == on {
            return;
        }
        st.mirroring = on;
        log::info!(event = "airplay_mirror", on = on; "AirPlay: screen mirroring {}", if on { "on" } else { "off" });
        let ev = on.then(|| (st.now.source.clone(), video_socket(&self.0.dir)));
        (self.0.events)(MusicEvent::Mirror(ev));
    }

    fn end_session(&self) {
        let mut st = lock(&self.0.state);
        st.dacp = None;
        if std::mem::take(&mut st.session) {
            st.now = NowPlaying::default();
            (self.0.events)(MusicEvent::Gone);
        }
    }

    fn watch_idle(&self) {
        while !self.0.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
            let idle = {
                let st = lock(&self.0.state);
                st.stream.is_some() && st.last_data.is_none_or(|t| t.elapsed() >= IDLE_CLOSE)
            };
            if idle {
                self.close_stream();
            }
        }
    }

    /// One line of UxPlay's stdout: an `@hc` event, or its own log.
    fn on_line(&self, line: &str) {
        let Some(ev) = parse_event(line) else {
            log_uxplay(line);
            return;
        };
        match ev {
            Event::Dacp { id, active_remote } => {
                log::info!(event = "airplay_dacp"; "AirPlay: the sender has a remote");
                lock(&self.0.state).dacp = Some(Dacp {
                    id,
                    active_remote,
                    addr: None,
                });
            }
            Event::End => {
                log::info!(event = "airplay_session", state = "end"; "AirPlay: session over");
                self.set_mirroring(false);
                self.end_session();
            }
            Event::Mirror(on) => self.set_mirroring(on),
            // The audio stream torn down: some apps do it at every track change, the
            // connection, its sender and its remote stay. Playing or not follows the audio.
            // A flush (a jump), or the audio stream torn down: how iOS pauses, and some apps
            // change track. The connection, its sender and its remote stay; our queue goes, the
            // stream opens again with the next audio.
            Event::Flush | Event::Stop => {
                let mut st = lock(&self.0.state);
                if let Some(s) = st.stream.take() {
                    s.abort();
                    log::info!(event = "airplay_audio", state = "flush"; "AirPlay: flushed (pause or jump)");
                    if st.session && st.now.playing {
                        st.now.playing = false;
                        (self.0.events)(MusicEvent::Now(st.now.clone()));
                    }
                }
            }
            ev => {
                let mut st = lock(&self.0.state);
                st.session = true;
                if let Some(out) = apply(&mut st.now, &ev) {
                    (self.0.events)(out);
                }
            }
        }
    }
}

/// UxPlay's debug log (`-d 1`) and GStreamer's warnings, always on during development, kept in RAM
/// (the runtime directory is a tmpfs: no wear on the eMMC) in two files of `DEBUG_LOG_MAX` bytes.
/// The journal gets only the errors.
const DEBUG_LOG_MAX: u64 = 4 << 20;

struct DebugLog {
    path: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
}

static DEBUG_LOG: Mutex<Option<DebugLog>> = Mutex::new(None);

/// Where UxPlay's debug log goes: the service's runtime directory, if it has one.
fn open_debug_log() {
    let dir = Path::new("/run/home-companion");
    if !dir.is_dir() {
        return;
    }
    let path = dir.join("uxplay.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();
    let size = file
        .as_ref()
        .and_then(|f| f.metadata().ok())
        .map_or(0, |m| m.len());
    *lock(&DEBUG_LOG) = Some(DebugLog { path, file, size });
}

fn debug_log_line(line: &str) {
    let mut guard = lock(&DEBUG_LOG);
    let Some(d) = guard.as_mut() else { return };
    if d.size > DEBUG_LOG_MAX {
        let _ = std::fs::rename(&d.path, d.path.with_extension("log.1"));
        d.file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&d.path)
            .ok();
        d.size = 0;
    }
    if let Some(f) = d.file.as_mut() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |t| t.as_secs_f64());
        let text = format!("{t:.3} {line}\n");
        if f.write_all(text.as_bytes()).is_ok() {
            d.size += text.len() as u64;
        }
    }
}

/// UxPlay's and GStreamer's own lines: all of them in the RAM debug log, the errors in our log too.
fn log_uxplay(line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    debug_log_line(line);
    if is_problem(line) {
        log::warn!(target: "uxplay", "{line}");
    }
}

/// A line worth the journal: UxPlay's "*** ERROR" lines, GStreamer's ERROR and CRITICAL ones.
pub fn is_problem(line: &str) -> bool {
    line.starts_with("*** ERROR")
        || [" ERROR ", "CRITICAL", "not-negotiated"]
            .iter()
            .any(|w| line.contains(w))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// UxPlay's GStreamer audio sink: raw PCM on fd 3 (UxPlay adds `sync=…` after it).
pub const AUDIO_SINK: &str = "audio/x-raw,format=S16LE,rate=44100,channels=2 ! fdsink fd=3";

/// The mirrored screen's frames: UxPlay's video sink (crates/home-companion/src/ui/mirror_video.rs reads it).
pub fn video_socket(dir: &Path) -> PathBuf {
    dir.join("video.sock")
}

/// UxPlay's command line: music and screen mirroring, a new sender takes over from the
/// current one, a stable pairing key, our events.
///
/// The video: the iPhone declares an sRGB transfer the hardware decoder refuses, so the caps
/// say BT.709; venus decodes into dma-bufs (no display delay), handed on as they are through
/// `unixfdsink` (the patch tells the decoder the receiver reads their layout, else it copies).
pub fn uxplay_args(name: &str, port: u16, dir: &Path) -> Vec<String> {
    let d = dir.to_string_lossy().into_owned();
    vec![
        "-n".into(),
        name.into(),
        "-nh".into(),
        "-p".into(),
        port.to_string(),
        "-s".into(),
        "1280x800".into(),
        "-fps".into(),
        "30".into(),
        "-srgb".into(),
        "no".into(),
        "-vp".into(),
        "h264parse ! capssetter caps=\"video/x-h264,colorimetry=(string)1:3:5:1\"".into(),
        "-vd".into(),
        "v4l2h264dec capture-io-mode=dmabuf extra-controls=\"c,display_delay_enable=1,display_delay=0\"".into(),
        "-vc".into(),
        "video/x-raw(memory:DMABuf)".into(),
        "-vs".into(),
        format!("unixfdsink socket-path={}", video_socket(dir).display()),
        "-as".into(),
        AUDIO_SINK.into(),
        // Each packet at the time it carries: the sender sends about 2 s ahead of its own
        // playback (its video, its lyrics); on arrival the sound would run ahead of the phone.
        "-async".into(),
        "-nohold".into(),
        "-key".into(),
        format!("{d}/uxplay.pem"),
        "-d".into(),
        "1".into(),
        "-hc".into(),
        d,
    ]
}

/// One `@hc` line of our UxPlay patch.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The sender's name, when it connects.
    Client(String),
    /// The sender's slider in AirPlay dB: -30 (bottom) to 0 (top), -144 muted.
    Volume(f32),
    Dacp {
        id: String,
        active_remote: String,
    },
    Title(String),
    Artist(String),
    Album(String),
    /// The end of a metadata bundle.
    MetaEnd,
    /// The cover art's file, or none.
    Cover(Option<PathBuf>),
    /// The sender paused or jumped: what is queued must not be heard.
    Flush,
    /// The audio stream was torn down: a pause on iOS (the connection stays).
    Stop,
    /// Screen mirroring started (true) or stopped.
    Mirror(bool),
    /// The sender's real picture's aspect ratio (width / height): the frames may be padded.
    MirrorAspect(f32),
    /// The last connection closed.
    End,
}

pub fn parse_event(line: &str) -> Option<Event> {
    let rest = line.strip_prefix("@hc ")?.trim_end_matches(['\r', '\n']);
    let (word, arg) = rest.split_once(' ').unwrap_or((rest, ""));
    Some(match word {
        "client" => Event::Client(arg.trim().into()),
        "volume" => Event::Volume(arg.trim().parse().ok()?),
        "dacp" => {
            let (id, ar) = arg.trim().split_once(' ')?;
            Event::Dacp {
                id: id.into(),
                active_remote: ar.trim().into(),
            }
        }
        "meta" => {
            let (tag, text) = arg.split_once(' ').unwrap_or((arg, ""));
            let text = text.trim().to_string();
            match tag {
                "minm" => Event::Title(text),
                "asar" => Event::Artist(text),
                "asal" => Event::Album(text),
                _ => return None,
            }
        }
        "meta-end" => Event::MetaEnd,
        "cover" if arg.trim().is_empty() => Event::Cover(None),
        "cover" => Event::Cover(Some(PathBuf::from(arg.trim()))),
        "flush" => Event::Flush,
        "stop" => Event::Stop,
        "mirror" if arg.trim() == "start" => Event::Mirror(true),
        "mirror" if arg.trim() == "stop" => Event::Mirror(false),
        "mirror-size" => {
            let v: Vec<f32> = arg
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            let a = match v.as_slice() {
                [ws, hs, ..] if *ws > 0.0 && *hs > 0.0 => ws / hs,
                [_, _, w, h] if *w > 0.0 && *h > 0.0 => w / h,
                _ => return None,
            };
            Event::MirrorAspect(a)
        }
        "end" => Event::End,
        _ => return None,
    })
}

/// Applies one event to what is playing; an event for the core when something worth showing
/// changed.
pub fn apply(now: &mut NowPlaying, ev: &Event) -> Option<MusicEvent> {
    match ev {
        Event::Title(title) => {
            // Another track: its own cover or none, never the previous one's. The iPhone may
            // send the new cover a moment before the new title: that one stays.
            let fresh = now
                .cover_at
                .is_some_and(|t| t.elapsed() < COVER_BEFORE_TITLE);
            if *title != now.title && !fresh {
                now.cover = None;
            }
            now.title = title.clone();
        }
        Event::Artist(a) => now.artist = a.clone(),
        Event::Album(a) => now.album = a.clone(),
        Event::Client(name) => {
            now.source = name.clone();
            return Some(MusicEvent::Now(now.clone()));
        }
        Event::MetaEnd => return Some(MusicEvent::Now(now.clone())),
        Event::Cover(path) => {
            now.cover = path.clone();
            now.cover_at = Some(Instant::now());
            return Some(MusicEvent::Now(now.clone()));
        }
        Event::Volume(db) => return Some(MusicEvent::Volume(volume_percent(*db))),
        Event::MirrorAspect(a) => return Some(MusicEvent::MirrorAspect(*a)),
        Event::Dacp { .. } | Event::Flush | Event::Stop | Event::End | Event::Mirror(_) => {}
    }
    None
}

/// The slider, -30 (bottom) to 0 (top) dB, -144 when muted, as a percentage of its length.
pub fn volume_percent(db: f32) -> u8 {
    if db <= -100.0 {
        return 0;
    }
    ((db.clamp(-30.0, 0.0) + 30.0) / 30.0 * 100.0).round() as u8
}

/// The sender's DACP service through avahi-daemon (it already hears the network's mDNS):
/// `busctl` asks its D-Bus API, the system bus allows that to anyone.
fn resolve_dacp(id: &str) -> Result<SocketAddr, String> {
    let name = dacp_service(id);
    let out = Command::new("busctl")
        .args([
            "--system",
            "--json=short",
            "call",
            "org.freedesktop.Avahi",
            "/",
        ])
        .args([
            "org.freedesktop.Avahi.Server",
            "ResolveService",
            "iisssiu",
            "--",
        ])
        // any interface, IPv4, the name, its type, domain, IPv4 address, no flags
        .args(["-1", "0", &name, "_dacp._tcp", "local", "0", "0"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("busctl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_resolved(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("{name}: no address"))
}

/// The sender's DACP-ID header drops leading zeros, its service name has all 16 digits.
pub fn dacp_service(id: &str) -> String {
    format!("iTunes_Ctrl_{id:0>16}")
}

/// busctl's JSON answer of ResolveService (`iissssisqaayu`): the address is the 8th field, the
/// port the 9th.
pub fn parse_resolved(json: &str) -> Option<SocketAddr> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let data = v.get("data")?.as_array()?;
    let ip: std::net::IpAddr = data.get(7)?.as_str()?.parse().ok()?;
    let port = u16::try_from(data.get(8)?.as_u64()?).ok()?;
    Some(SocketAddr::new(ip, port))
}

/// One DACP command; the HTTP status.
fn dacp_get(addr: SocketAddr, cmd: &str, active_remote: &str) -> Result<u16, String> {
    let mut s =
        TcpStream::connect_timeout(&addr, DACP_TIMEOUT).map_err(|e| format!("{addr}: {e}"))?;
    let _ = s.set_read_timeout(Some(DACP_TIMEOUT));
    let _ = s.set_write_timeout(Some(DACP_TIMEOUT));
    s.write_all(dacp_request(addr, cmd, active_remote).as_bytes())
        .map_err(|e| format!("{addr}: {e}"))?;
    let mut head = [0u8; 64];
    let n = s.read(&mut head).map_err(|e| format!("{addr}: {e}"))?;
    http_status(&head[..n]).ok_or_else(|| format!("{addr}: not an HTTP answer"))
}

pub fn dacp_request(addr: SocketAddr, cmd: &str, active_remote: &str) -> String {
    format!(
        "GET /ctrl-int/1/{cmd} HTTP/1.1\r\nHost: {addr}\r\nActive-Remote: {active_remote}\r\nConnection: close\r\n\r\n"
    )
}

/// "HTTP/1.1 204 No Content" → 204.
pub fn http_status(head: &[u8]) -> Option<u16> {
    let line = std::str::from_utf8(head).ok()?.lines().next()?;
    let mut parts = line.split_whitespace();
    parts.next().filter(|p| p.starts_with("HTTP/"))?;
    parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_patchs_lines() {
        assert_eq!(
            parse_event("@hc client iPhone di Test"),
            Some(Event::Client("iPhone di Test".into()))
        );
        assert_eq!(
            parse_event("@hc volume -12.500000"),
            Some(Event::Volume(-12.5))
        );
        assert_eq!(
            parse_event("@hc dacp 1A2B3C4D5E6F7788 123456789"),
            Some(Event::Dacp {
                id: "1A2B3C4D5E6F7788".into(),
                active_remote: "123456789".into()
            })
        );
        assert_eq!(
            parse_event("@hc meta minm Via con me"),
            Some(Event::Title("Via con me".into()))
        );
        assert_eq!(
            parse_event("@hc meta asar Paolo Conte"),
            Some(Event::Artist("Paolo Conte".into()))
        );
        assert_eq!(
            parse_event("@hc meta asal Gli anni"),
            Some(Event::Album("Gli anni".into()))
        );
        assert_eq!(parse_event("@hc meta asgn Jazz"), None);
        assert_eq!(parse_event("@hc meta-end"), Some(Event::MetaEnd));
        assert_eq!(
            parse_event("@hc cover /var/lib/home-companion/airplay/cover.jpg"),
            Some(Event::Cover(Some(
                "/var/lib/home-companion/airplay/cover.jpg".into()
            )))
        );
        assert_eq!(parse_event("@hc cover"), Some(Event::Cover(None)));
        assert_eq!(parse_event("@hc stop"), Some(Event::Stop));
        assert_eq!(parse_event("@hc flush"), Some(Event::Flush));
        assert_eq!(parse_event("@hc mirror start"), Some(Event::Mirror(true)));
        assert_eq!(parse_event("@hc mirror stop"), Some(Event::Mirror(false)));
        assert_eq!(
            parse_event("@hc mirror-size 1112 1600 556 800"),
            Some(Event::MirrorAspect(1112.0 / 1600.0))
        );
        assert_eq!(
            parse_event("@hc mirror-size 0 0 556 800"),
            Some(Event::MirrorAspect(556.0 / 800.0))
        );
        assert_eq!(parse_event("@hc mirror-size"), None);
        assert_eq!(parse_event("@hc end\r"), Some(Event::End));
        assert_eq!(parse_event("@hc volume loud"), None);
        assert_eq!(parse_event("connection request from iPhone"), None);
        assert_eq!(parse_event("Title: @hc end"), None);
    }

    #[test]
    fn a_track_with_its_sender() {
        let mut now = NowPlaying::default();
        let evs: Vec<MusicEvent> = [
            "@hc client iPhone",
            "@hc meta minm Azzurro",
            "@hc meta asar Paolo Conte",
            "@hc meta asal Gli anni",
            "@hc meta-end",
        ]
        .iter()
        .filter_map(|l| parse_event(l))
        .filter_map(|e| apply(&mut now, &e))
        .collect();
        assert_eq!(evs.len(), 2);
        let MusicEvent::Now(n) = &evs[1] else {
            panic!()
        };
        assert_eq!(
            (n.title.as_str(), n.artist.as_str(), n.album.as_str()),
            ("Azzurro", "Paolo Conte", "Gli anni")
        );
        assert_eq!(n.source, "iPhone");
    }

    #[test]
    fn the_senders_slider_is_a_percentage() {
        assert_eq!(volume_percent(-30.0), 0);
        assert_eq!(volume_percent(-15.0), 50);
        assert_eq!(volume_percent(0.0), 100);
        assert_eq!(volume_percent(-144.0), 0);
        let mut now = NowPlaying::default();
        assert_eq!(
            apply(&mut now, &Event::Volume(-7.5)),
            Some(MusicEvent::Volume(75))
        );
    }

    #[test]
    fn a_cover_just_before_the_title_stays_an_old_one_goes() {
        let mut now = NowPlaying::default();
        apply(&mut now, &Event::Title("First".into()));
        apply(&mut now, &Event::Cover(Some("/x/cover.jpg".into())));
        apply(&mut now, &Event::Title("Second".into()));
        assert!(
            now.cover.is_some(),
            "the cover sent just before the new title"
        );
        now.cover_at = Some(Instant::now() - Duration::from_secs(10));
        apply(&mut now, &Event::Title("Third".into()));
        assert!(now.cover.is_none(), "an old cover is not the new track's");
    }

    #[test]
    fn problems_reach_the_log() {
        assert!(is_problem(
            "0:00:01.2 1 0x1 ERROR   v4l2 gstv4l2object.c: error"
        ));
        assert!(is_problem(
            "*** ERROR: Failed to initialize GStreamer video renderer 1"
        ));
        assert!(!is_problem("Accepted IPv4 client on socket 22"));
    }

    #[test]
    fn command_line() {
        let a = uxplay_args("Alfred", 6010, Path::new("/var/lib/home-companion/airplay"));
        let pos = |s: &str| a.iter().position(|x| x == s).unwrap();
        assert_eq!(a[pos("-n") + 1], "Alfred");
        assert_eq!(a[pos("-p") + 1], "6010");
        assert_eq!(
            a[pos("-vs") + 1],
            "unixfdsink socket-path=/var/lib/home-companion/airplay/video.sock"
        );
        assert!(a[pos("-vd") + 1].starts_with("v4l2h264dec capture-io-mode=dmabuf"));
        assert_eq!(a[pos("-as") + 1], AUDIO_SINK);
        assert_eq!(a[pos("-hc") + 1], "/var/lib/home-companion/airplay");
        assert_eq!(
            a[pos("-key") + 1],
            "/var/lib/home-companion/airplay/uxplay.pem"
        );
        assert!(a.contains(&"-nh".to_string()) && a.contains(&"-nohold".to_string()));
        assert!(
            a.contains(&"-async".to_string()),
            "packets played at their own time"
        );
    }

    #[test]
    fn avahi_answer() {
        let j = r#"{"type":"iissssisqaayu","data":[4,0,"iTunes_Ctrl_0D5FBE6CF5C2BE3C","_dacp._tcp","local","phone.example",0,"192.0.2.7",49813,[],5]}"#;
        assert_eq!(parse_resolved(j), Some("192.0.2.7:49813".parse().unwrap()));
        assert_eq!(
            dacp_service("D5FBE6CF5C2BE3C"),
            "iTunes_Ctrl_0D5FBE6CF5C2BE3C"
        );
        assert_eq!(
            dacp_service("1A2B3C4D5E6F7788"),
            "iTunes_Ctrl_1A2B3C4D5E6F7788"
        );
        assert_eq!(parse_resolved("{}"), None);
    }

    #[test]
    fn dacp_http() {
        let r = dacp_request("192.0.2.7:3689".parse().unwrap(), "nextitem", "42");
        assert!(r.starts_with("GET /ctrl-int/1/nextitem HTTP/1.1\r\n"));
        assert!(r.contains("\r\nActive-Remote: 42\r\n") && r.ends_with("\r\n\r\n"));
        assert_eq!(http_status(b"HTTP/1.1 204 No Content\r\n"), Some(204));
        assert_eq!(
            http_status(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0"),
            Some(400)
        );
        assert_eq!(http_status(b"garbage"), None);
        assert_eq!(Remote::VolumeDown.command(), "volumedown");
    }
}
