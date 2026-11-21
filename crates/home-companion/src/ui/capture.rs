// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The screen as a picture for the browser panel's remote screen (crate::panel::remote): half
//! size, in the screen's own orientation, as JPEG, made only while someone watches.
//!
//! Where the pixels come from, on the GPU renderer: the buffer the display scans out right now,
//! read through DRM, never through the renderer. `Window::take_snapshot` draws the scene a
//! second time inside Slint's GL context (the path of the old SIGUSR2 screenshot that ended with
//! "Error presenting framebuffer: EBUSY" during AirPlay); here nothing of Slint's is touched:
//! the body's own DRM file (the one Slint opened, found in /proc/self/fd, so it is the master
//! and may ask for the buffer's handle) answers three read-only questions (which framebuffer the
//! CRTC shows, its layout, an mmap offset), the buffer is mapped read-only, every other row is
//! copied out, the handle closed. What is on the glass is what is captured: AirPlay mirroring
//! and Home Assistant's view are in the same buffer, since Slint composes them.
//!
//! Cost is paid only while asked (a frame request in the last [`WANT_FOR`]): a thread that
//! leaves by itself. Each tick asks the CRTC which framebuffer it shows: Slint adds a new one at
//! every presented frame, so the same id means nothing was drawn and nothing is read (an id can
//! come back after two frames: a full read at least every [`RECHECK`] catches that). A read
//! frame equal to the last one is not encoded.
//!
//! Privacy ([`Capture::set_hidden`]): the screen says at every page or dialog change whether
//! what it shows may be sent. While hidden nothing is read and the last frame is dropped; every
//! change bumps a generation, and a frame read under an older generation is never published (the
//! check and the publishing happen under one lock). Once shown again, only a frame from a
//! framebuffer presented after the change and at least [`REVEAL_AFTER`] later is published (or
//! after [`REVEAL_AT_MOST`], for a screen that drew nothing new).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A frame request keeps the capture going this long.
pub const WANT_FOR: Duration = Duration::from_secs(5);
/// Between two looks while something changes or the remote touched the screen…
pub const FAST: Duration = Duration::from_millis(250);
/// …and otherwise (2 frames a second at most; 4 while fast).
pub const SLOW: Duration = Duration::from_millis(500);
/// Fast for this long after a change or a touch.
pub const FAST_FOR: Duration = Duration::from_secs(3);
/// A read of the buffer at least this often even when the CRTC shows the same id.
pub const RECHECK: Duration = Duration::from_secs(2);
pub const QUALITY: u8 = 70;
/// After the screen may be sent again: frames read this soon are not published (longer than
/// the longest page transition, 800 ms in companion.slint, so no frame shows a private page
/// sliding or fading out)…
pub const REVEAL_AFTER: Duration = Duration::from_millis(900);
/// …nor before a new framebuffer, unless this long passed.
pub const REVEAL_AT_MOST: Duration = Duration::from_millis(2000);
/// Reasons that do not go away by trying again.
const PERMANENT: &[&str] = &["renderer", "no_drm", "not_master", "tiled", "format"];

/// One encoded frame.
#[derive(Clone)]
pub struct Frame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub jpeg: Arc<Vec<u8>>,
    /// The JPEG as base64, made once on the capture thread (the panel socket's JSON).
    pub b64: Arc<String>,
}

/// The pixels of the scanned-out buffer, as they are in memory: rows of `stride` bytes, 4 bytes
/// a pixel. `bgr`: the bytes are B, G, R, X (XRGB8888 / ARGB8888 on a little-endian machine);
/// otherwise R, G, B, X (XBGR8888 / ABGR8888).
pub struct Raw<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub bgr: bool,
}

/// Half the buffer in each direction, as RGB, in the buffer's orientation: every other row,
/// each pair of pixels averaged (half the rows are never read: the buffer is write-combined
/// memory, slow to read, and the read is most of the cost).
pub fn half(raw: &Raw, row: &mut Vec<u8>) -> (Vec<u8>, u32, u32) {
    let (w, h) = (raw.width / 2, raw.height / 2);
    let mut out = vec![0u8; (w * h * 3) as usize];
    let bytes = raw.width as usize * 4;
    row.resize(bytes, 0);
    let (r_at, b_at) = if raw.bgr { (2, 0) } else { (0, 2) };
    for y in 0..h as usize {
        let start = y * 2 * raw.stride;
        let Some(src) = raw.data.get(start..start + bytes) else {
            break;
        };
        // One sequential copy out of the slow memory, then the arithmetic in the cache.
        row.copy_from_slice(src);
        let o = &mut out[y * w as usize * 3..(y + 1) * w as usize * 3];
        for x in 0..w as usize {
            let p = &row[x * 8..x * 8 + 8];
            o[x * 3] = (p[r_at] as u16 + p[4 + r_at] as u16).div_ceil(2) as u8;
            o[x * 3 + 1] = (p[1] as u16 + p[5] as u16).div_ceil(2) as u8;
            o[x * 3 + 2] = (p[b_at] as u16 + p[4 + b_at] as u16).div_ceil(2) as u8;
        }
    }
    (out, w, h)
}

/// Turns an RGB picture `degrees` clockwise (0, 90, 180, 270).
pub fn rotate(rgb: &[u8], w: u32, h: u32, degrees: u32) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    match degrees % 360 {
        90 | 270 => {
            let cw = degrees % 360 == 90;
            let mut out = vec![0u8; rgb.len()];
            // Out is h wide, w tall.
            for y in 0..h {
                for x in 0..w {
                    let (ox, oy) = if cw { (h - 1 - y, x) } else { (y, w - 1 - x) };
                    let s = (y * w + x) * 3;
                    let d = (oy * h + ox) * 3;
                    out[d..d + 3].copy_from_slice(&rgb[s..s + 3]);
                }
            }
            (out, h as u32, w as u32)
        }
        180 => {
            let mut out = vec![0u8; rgb.len()];
            let n = w * h;
            for i in 0..n {
                let d = (n - 1 - i) * 3;
                out[d..d + 3].copy_from_slice(&rgb[i * 3..i * 3 + 3]);
            }
            (out, w as u32, h as u32)
        }
        _ => (rgb.to_vec(), w as u32, h as u32),
    }
}

/// The screen's `rotation` (what the renderer turned the picture by to fit the panel) → how much
/// the scanned-out buffer must be turned clockwise to be upright again.
pub fn upright(rotation: u32) -> u32 {
    (360 - rotation % 360) % 360
}

pub fn encode_jpeg(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::with_capacity(64 << 10);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, QUALITY)
        .write_image(rgb, w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("jpeg: {e}"))?;
    Ok(out)
}

/// From a buffer to a frame, keeping the last picture to compare with.
#[derive(Default)]
pub struct Pipeline {
    last: Vec<u8>,
    row: Vec<u8>,
    seq: u64,
    /// What the steps took the last time (read, encode), for the log.
    pub took: (Duration, Duration),
}

impl Pipeline {
    /// The buffer turned upright and encoded; None when it shows what the last one showed.
    pub fn process(&mut self, raw: &Raw, rotation: u32) -> Result<Option<Frame>, String> {
        let t0 = Instant::now();
        let (small, w, h) = half(raw, &mut self.row);
        let (rgb, w, h) = rotate(&small, w, h, upright(rotation));
        let read = t0.elapsed();
        if rgb == self.last {
            self.took = (read, Duration::ZERO);
            return Ok(None);
        }
        let t1 = Instant::now();
        let jpeg = encode_jpeg(&rgb, w, h)?;
        self.took = (read, t1.elapsed());
        self.last = rgb;
        self.seq += 1;
        Ok(Some(Frame {
            seq: self.seq,
            width: w,
            height: h,
            b64: Arc::new(base64(&jpeg)),
            jpeg: Arc::new(jpeg),
        }))
    }

    /// The next frame is encoded even if it is the same (a new watcher needs one).
    pub fn forget(&mut self) {
        self.last.clear();
    }
}

/// Where the screen's pixels are read from.
pub trait Source: Send {
    /// What the display shows now: an id that changes when a new frame was put on it.
    fn current(&mut self) -> Result<u32, String>;
    /// Reads it and hands it to `f`.
    fn read(&mut self, f: &mut dyn FnMut(&Raw) -> Result<(), String>) -> Result<(), String>;
}

/// Why there are no frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nobody asked, or the first frame is on its way.
    Idle,
    Running,
    /// This renderer cannot be read (the key says why, for the log and the panel).
    Unavailable(&'static str),
}

/// Whether frames may be published: the screen's privacy state and its generation.
#[derive(Debug, Clone, Copy)]
pub struct Gate {
    pub generation: u64,
    pub hidden: bool,
    /// When it was last shown again.
    pub revealed: Option<Instant>,
}

impl Gate {
    fn new() -> Gate {
        Gate {
            generation: 0,
            hidden: false,
            revealed: None,
        }
    }

    /// May a frame be published? `seen`: the generation it was read under; `id`: its
    /// framebuffer; `id_at_change`: the framebuffer shown when the reader saw this generation.
    pub fn allows(&self, seen: u64, id: u32, id_at_change: Option<u32>, now: Instant) -> bool {
        if self.hidden || seen != self.generation {
            return false;
        }
        match self.revealed {
            None => true,
            Some(t) => {
                let since = now.saturating_duration_since(t);
                since >= REVEAL_AT_MOST
                    || (since >= REVEAL_AFTER && id_at_change.is_some_and(|old| old != id))
            }
        }
    }
}

/// Bumped at every published frame and privacy change: the panel socket waits on it.
static NEWS: (Mutex<u64>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

fn tell_news() {
    *lock(&NEWS.0) += 1;
    NEWS.1.notify_all();
}

/// The news counter now (to wait for anything after it).
pub fn news() -> u64 {
    *lock(&NEWS.0)
}

/// Waits until something happened after `seen`, at most `within`.
pub fn wait_news(seen: u64, within: Duration) {
    let g = lock(&NEWS.0);
    let guard = match NEWS.1.wait_timeout_while(g, within, |n| *n == seen) {
        Ok((g, _)) => g,
        Err(e) => e.into_inner().0,
    };
    drop(guard);
}

struct Shared {
    gate: Mutex<Gate>,
    latest: Mutex<Option<Frame>>,
    status: Mutex<Status>,
    wanted_until: Mutex<Instant>,
    fast_until: Mutex<Instant>,
    running: AtomicBool,
    stop: AtomicBool,
    forget: AtomicBool,
}

/// The capture thread's handle, kept by the screen.
pub struct Capture {
    shared: Arc<Shared>,
    make: Arc<dyn Fn() -> Result<Box<dyn Source>, &'static str> + Send + Sync>,
    rotation: u32,
}

impl Capture {
    /// `make` opens the source on the capture thread, each time it starts.
    pub fn new(
        rotation: u32,
        make: impl Fn() -> Result<Box<dyn Source>, &'static str> + Send + Sync + 'static,
    ) -> Capture {
        let now = Instant::now();
        Capture {
            shared: Arc::new(Shared {
                gate: Mutex::new(Gate::new()),
                latest: Mutex::new(None),
                status: Mutex::new(Status::Idle),
                wanted_until: Mutex::new(now),
                fast_until: Mutex::new(now),
                running: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                forget: AtomicBool::new(false),
            }),
            make: Arc::new(make),
            rotation,
        }
    }

    /// The renderer cannot be read at all.
    pub fn unavailable(why: &'static str) -> Capture {
        let c = Capture::new(0, move || Err(why));
        *lock(&c.shared.status) = Status::Unavailable(why);
        c
    }

    /// Someone asked for a frame: capture (on) for [`WANT_FOR`] more.
    pub fn want(&self) {
        let now = Instant::now();
        *lock(&self.shared.wanted_until) = now + WANT_FOR;
        // What cannot change while the body runs is not tried again (a failed read is: the
        // display may have been between two modes).
        if matches!(&*lock(&self.shared.status), Status::Unavailable(w) if PERMANENT.contains(w)) {
            return;
        }
        if self.shared.running.swap(true, Ordering::AcqRel) {
            return;
        }
        self.shared.stop.store(false, Ordering::Release);
        *lock(&self.shared.status) = Status::Idle;
        let (shared, make, rotation) = (self.shared.clone(), self.make.clone(), self.rotation);
        let spawned = std::thread::Builder::new()
            .name("remote-capture".into())
            .spawn(move || {
                run(&shared, &*make, rotation);
                shared.running.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.shared.running.store(false, Ordering::Release);
        }
    }

    /// Whether what the screen shows may be sent (the screen says so at every page or dialog
    /// change, and while the system suspends). Hidden: the last frame goes at once and nothing
    /// read before the change is ever published.
    pub fn set_hidden(&self, hidden: bool) {
        {
            let mut g = lock(&self.shared.gate);
            if g.hidden == hidden {
                return;
            }
            g.hidden = hidden;
            g.generation += 1;
            g.revealed = (!hidden).then(Instant::now);
            *lock(&self.shared.latest) = None;
        }
        self.shared.forget.store(true, Ordering::Release);
        tell_news();
    }

    pub fn hidden(&self) -> bool {
        lock(&self.shared.gate).hidden
    }

    /// The remote touched the screen: look often for a while.
    pub fn poke(&self) {
        *lock(&self.shared.fast_until) = Instant::now() + FAST_FOR;
    }

    /// A new watcher: the next frame is sent even if nothing changed.
    pub fn restart(&self) {
        self.shared.forget.store(true, Ordering::Release);
        *lock(&self.shared.latest) = None;
    }

    /// No more frames (the session ended): the thread leaves at its next tick.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        *lock(&self.shared.latest) = None;
        tell_news();
    }

    pub fn latest(&self) -> Option<Frame> {
        lock(&self.shared.latest).clone()
    }

    pub fn status(&self) -> Status {
        lock(&self.shared.status).clone()
    }

    pub fn running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn run(
    shared: &Shared,
    make: &(dyn Fn() -> Result<Box<dyn Source>, &'static str> + Send + Sync),
    rotation: u32,
) {
    let mut source = match make() {
        Ok(s) => s,
        Err(why) => {
            log::warn!(event = "remote_screen", why = why; "remote screen: the screen cannot be read ({why})");
            *lock(&shared.status) = Status::Unavailable(why);
            return;
        }
    };
    log::info!(event = "remote_screen"; "remote screen: capture started");
    let mut pipe = Pipeline::default();
    let mut last_id: Option<u32> = None;
    // The generation last seen, and the framebuffer shown when it was first seen.
    let mut seen_gen = u64::MAX;
    let mut id_at_change: Option<u32> = None;
    let mut last_read = Instant::now() - RECHECK;
    let mut frames = 0u32;
    let mut busy = Duration::ZERO;
    let started = Instant::now();
    loop {
        let now = Instant::now();
        if shared.stop.load(Ordering::Acquire) || now >= *lock(&shared.wanted_until) {
            break;
        }
        if shared.forget.swap(false, Ordering::AcqRel) {
            pipe.forget();
            last_id = None;
        }
        let t0 = Instant::now();
        let gate = *lock(&shared.gate);
        if gate.hidden {
            // Nothing is read while the screen shows what must not be sent.
            std::thread::sleep(SLOW);
            continue;
        }
        let id = match source.current() {
            Ok(id) => id,
            Err(e) => {
                log::warn!(event = "remote_screen"; "remote screen: {e}");
                *lock(&shared.status) = Status::Unavailable("read");
                return;
            }
        };
        if gate.generation != seen_gen {
            seen_gen = gate.generation;
            id_at_change = Some(id);
        }
        let waiting = gate.revealed.is_some_and(|t| t.elapsed() < REVEAL_AT_MOST)
            && !gate.allows(seen_gen, id, id_at_change, Instant::now());
        if waiting {
            // Not yet: look again soon, without reading.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if Some(id) != last_id || now.duration_since(last_read) >= RECHECK {
            last_id = Some(id);
            last_read = now;
            let mut made = None;
            let r = source.read(&mut |raw| {
                made = pipe.process(raw, rotation)?;
                Ok(())
            });
            if let Err(e) = r {
                log::warn!(event = "remote_screen"; "remote screen: {e}");
                *lock(&shared.status) = Status::Unavailable("read");
                return;
            }
            *lock(&shared.status) = Status::Running;
            if let Some(f) = made {
                // Published only if nothing changed meanwhile (under the gate's lock: a change
                // either comes before, and this is dropped, or after, and clears it).
                let g = lock(&shared.gate);
                if g.allows(seen_gen, id, id_at_change, Instant::now()) {
                    frames += 1;
                    *lock(&shared.fast_until) = now + FAST_FOR;
                    *lock(&shared.latest) = Some(f);
                    drop(g);
                    tell_news();
                } else {
                    // Read under an older state: read again from scratch.
                    pipe.forget();
                    last_id = None;
                }
            }
        }
        busy += t0.elapsed();
        let tick = if Instant::now() < *lock(&shared.fast_until) {
            FAST
        } else {
            SLOW
        };
        // Never more than half a core, whatever the frame costs.
        let work = t0.elapsed();
        std::thread::sleep(tick.saturating_sub(work).max(work));
    }
    let up = started.elapsed().as_secs_f64().max(0.001);
    log::info!(
        event = "remote_screen",
        frames = frames,
        cpu_percent = busy.as_secs_f64() * 100.0 / up,
        read_ms = pipe.took.0.as_secs_f64() * 1e3,
        encode_ms = pipe.took.1.as_secs_f64() * 1e3;
        "remote screen: capture stopped ({frames} frames in {up:.0} s)"
    );
}

/// The scanned-out buffer through the body's own DRM file.
#[cfg(target_os = "linux")]
pub mod drm {
    use super::{Raw, Source};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const fn iowr(nr: u64, size: usize) -> libc::c_ulong {
        ((3u64 << 30) | ((size as u64) << 16) | ((b'd' as u64) << 8) | nr) as libc::c_ulong
    }

    #[repr(C)]
    #[derive(Default)]
    struct CardRes {
        fb_id_ptr: u64,
        crtc_id_ptr: u64,
        connector_id_ptr: u64,
        encoder_id_ptr: u64,
        count_fbs: u32,
        count_crtcs: u32,
        count_connectors: u32,
        count_encoders: u32,
        min_width: u32,
        max_width: u32,
        min_height: u32,
        max_height: u32,
    }

    #[repr(C)]
    struct Crtc {
        set_connectors_ptr: u64,
        count_connectors: u32,
        crtc_id: u32,
        fb_id: u32,
        x: u32,
        y: u32,
        gamma_size: u32,
        mode_valid: u32,
        mode: [u8; 68],
    }

    #[repr(C)]
    #[derive(Default)]
    struct FbCmd2 {
        fb_id: u32,
        width: u32,
        height: u32,
        pixel_format: u32,
        flags: u32,
        handles: [u32; 4],
        pitches: [u32; 4],
        offsets: [u32; 4],
        modifier: [u64; 4],
    }

    #[repr(C)]
    #[derive(Default)]
    struct MapDumb {
        handle: u32,
        pad: u32,
        offset: u64,
    }

    const GETRESOURCES: libc::c_ulong = iowr(0xA0, std::mem::size_of::<CardRes>());
    const GETCRTC: libc::c_ulong = iowr(0xA1, std::mem::size_of::<Crtc>());
    const GETFB2: libc::c_ulong = iowr(0xCE, std::mem::size_of::<FbCmd2>());
    const MAP_DUMB: libc::c_ulong = iowr(0xB3, std::mem::size_of::<MapDumb>());
    const FB_MODIFIERS: u32 = 2;
    const fn fourcc(s: &[u8; 4]) -> u32 {
        u32::from_le_bytes(*s)
    }
    const XR24: u32 = fourcc(b"XR24");
    const AR24: u32 = fourcc(b"AR24");
    const XB24: u32 = fourcc(b"XB24");
    const AB24: u32 = fourcc(b"AB24");

    fn ioctl<T>(fd: &OwnedFd, req: libc::c_ulong, arg: &mut T) -> std::io::Result<()> {
        loop {
            // SAFETY: `arg` is the #[repr(C)] struct the request's number encodes the size of.
            let r = unsafe { libc::ioctl(fd.as_raw_fd(), req as _, arg as *mut T) };
            if r == 0 {
                return Ok(());
            }
            let e = std::io::Error::last_os_error();
            if !matches!(e.raw_os_error(), Some(libc::EINTR) | Some(libc::EAGAIN)) {
                return Err(e);
            }
        }
    }

    /// The display's DRM file of this process (Slint's, the master).
    pub struct Drm {
        fd: OwnedFd,
        crtcs: Vec<u32>,
        /// What each scanout framebuffer looked like the first time (by fb id), its handle kept.
        /// The file is the body's own: closing a handle there (GEM_CLOSE) makes msm drop the
        /// buffer's GPU mapping for the whole file, and the renderer then faults on its own
        /// scanout buffer (GPU faults, the screen corrupted and flashing). So every handle got
        /// with GETFB2 stays open for the process's life; the swapchain has two or three.
        layouts: std::cell::RefCell<std::collections::HashMap<u32, Result<Layout, &'static str>>>,
    }

    /// Framebuffers whose handles may be kept; past this the capture gives up (never closes).
    const MAX_FBS: usize = 8;

    /// The DRM card files this process has open (`/dev/dri/card*`), duplicated.
    fn own_cards() -> Vec<OwnedFd> {
        let Ok(dir) = std::fs::read_dir("/proc/self/fd") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in dir.flatten() {
            let Ok(target) = std::fs::read_link(e.path()) else {
                continue;
            };
            if !target.to_string_lossy().starts_with("/dev/dri/card") {
                continue;
            }
            let Some(n) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            // SAFETY: F_DUPFD_CLOEXEC returns a new descriptor (or -1), which we then own.
            let d = unsafe { libc::fcntl(n, libc::F_DUPFD_CLOEXEC, 0) };
            if d < 0 {
                continue;
            }
            // SAFETY: `d` was just made and is ours alone.
            let fd = unsafe { OwnedFd::from_raw_fd(d) };
            // The number may have been closed and reused between readlink and dup: the
            // duplicate must be the DRM character device the link named.
            if same_device(&fd, &target) {
                out.push(fd);
            }
        }
        out
    }

    /// `fd` is a character device of DRM (major 226), the one `path` names.
    fn same_device(fd: &OwnedFd, path: &std::path::Path) -> bool {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        // SAFETY: fstat fills the struct we pass.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
            return false;
        }
        let Ok(m) = std::fs::metadata(path) else {
            return false;
        };
        (st.st_mode & libc::S_IFMT) == libc::S_IFCHR
            && m.file_type().is_char_device()
            && st.st_rdev == m.rdev()
            && libc::major(st.st_rdev) == 226
    }

    impl Drm {
        /// The body's display file, if it has one that may read the scanout buffer.
        pub fn open() -> Result<Drm, &'static str> {
            let cards = own_cards();
            if cards.is_empty() {
                return Err("no_drm");
            }
            let mut last = "no_drm";
            for fd in cards {
                let mut res = CardRes::default();
                if ioctl(&fd, GETRESOURCES, &mut res).is_err() || res.count_crtcs == 0 {
                    continue;
                }
                let mut crtcs = vec![0u32; res.count_crtcs.min(16) as usize];
                let mut res2 = CardRes {
                    crtc_id_ptr: crtcs.as_mut_ptr() as u64,
                    count_crtcs: crtcs.len() as u32,
                    ..Default::default()
                };
                if ioctl(&fd, GETRESOURCES, &mut res2).is_err() {
                    continue;
                }
                crtcs.truncate(res2.count_crtcs.min(crtcs.len() as u32) as usize);
                let d = Drm {
                    fd,
                    crtcs,
                    layouts: Default::default(),
                };
                // The handle comes only to the master: try it once (kept, see `layouts`).
                match d.layout() {
                    Ok(_) => return Ok(d),
                    Err(why) => last = why,
                }
            }
            Err(last)
        }

        fn scanout(&self) -> Result<u32, String> {
            for &id in &self.crtcs {
                let mut c = Crtc {
                    set_connectors_ptr: 0,
                    count_connectors: 0,
                    crtc_id: id,
                    fb_id: 0,
                    x: 0,
                    y: 0,
                    gamma_size: 0,
                    mode_valid: 0,
                    mode: [0; 68],
                };
                ioctl(&self.fd, GETCRTC, &mut c).map_err(|e| format!("GETCRTC: {e}"))?;
                if c.mode_valid != 0 && c.fb_id != 0 {
                    return Ok(c.fb_id);
                }
            }
            Err("no CRTC is showing anything".into())
        }

        fn layout(&self) -> Result<Layout, &'static str> {
            let fb = self.scanout().map_err(|_| "no_scanout")?;
            if let Some(l) = self.layouts.borrow().get(&fb) {
                return l.clone();
            }
            if self.layouts.borrow().len() >= MAX_FBS {
                return Err("too_many_fbs");
            }
            let l = self.layout_of(fb);
            self.layouts.borrow_mut().insert(fb, l.clone());
            l
        }

        /// GETFB2 once for `fb`; the handle it makes is never closed (see `layouts`).
        fn layout_of(&self, fb: u32) -> Result<Layout, &'static str> {
            let mut f = FbCmd2 {
                fb_id: fb,
                ..Default::default()
            };
            ioctl(&self.fd, GETFB2, &mut f).map_err(|_| "no_fb")?;
            if f.handles[0] == 0 {
                return Err("not_master");
            }
            let l = Layout {
                handle: f.handles[0],
                width: f.width,
                height: f.height,
                pitch: f.pitches[0],
                offset: f.offsets[0],
                format: f.pixel_format,
            };
            let linear = f.flags & FB_MODIFIERS == 0 || f.modifier[0] == 0;
            if !linear {
                return Err("tiled");
            }
            if ![XR24, AR24, XB24, AB24].contains(&f.pixel_format) {
                return Err("format");
            }
            Ok(l)
        }
    }

    #[derive(Clone, Copy)]
    struct Layout {
        handle: u32,
        width: u32,
        height: u32,
        pitch: u32,
        offset: u32,
        format: u32,
    }

    impl Source for Drm {
        fn current(&mut self) -> Result<u32, String> {
            self.scanout()
        }

        fn read(&mut self, f: &mut dyn FnMut(&Raw) -> Result<(), String>) -> Result<(), String> {
            let l = self.layout().map_err(|why| format!("framebuffer: {why}"))?;
            self.map_and(&l, f)
        }
    }

    impl Drm {
        fn map_and(
            &mut self,
            l: &Layout,
            f: &mut dyn FnMut(&Raw) -> Result<(), String>,
        ) -> Result<(), String> {
            if (l.pitch as u64) < l.width as u64 * 4 {
                return Err("framebuffer: pitch".into());
            }
            let mut m = MapDumb {
                handle: l.handle,
                ..Default::default()
            };
            ioctl(&self.fd, MAP_DUMB, &mut m).map_err(|e| format!("MAP_DUMB: {e}"))?;
            let len = l.offset as usize + l.pitch as usize * l.height as usize;
            // SAFETY: a read-only shared mapping of the buffer's pages, offset from the kernel.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    self.fd.as_raw_fd(),
                    m.offset as libc::off_t,
                )
            };
            if p == libc::MAP_FAILED {
                return Err(format!("mmap: {}", std::io::Error::last_os_error()));
            }
            // SAFETY: `len` bytes mapped just above, unmapped below before the slice goes.
            let data = unsafe { std::slice::from_raw_parts(p as *const u8, len) };
            let raw = Raw {
                data: &data[l.offset as usize..],
                width: l.width,
                height: l.height,
                stride: l.pitch as usize,
                bgr: matches!(l.format, XR24 | AR24),
            };
            let r = f(&raw);
            // SAFETY: the mapping made above.
            unsafe { libc::munmap(p, len) };
            r
        }
    }
}

/// Base64 (RFC 4648, with padding): the frames travel in the panel socket's JSON lines.
pub fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(w: u32, h: u32, stride: usize, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut v = vec![0u8; stride * h as usize];
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(x, y);
                let o = y as usize * stride + x as usize * 4;
                v[o..o + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        v
    }

    #[test]
    fn half_averages_pairs_and_skips_rows() {
        let data = buffer(4, 4, 20, |x, y| [(x * 10) as u8, (y * 50) as u8, 7]);
        let raw = Raw {
            data: &data,
            width: 4,
            height: 4,
            stride: 20,
            bgr: true,
        };
        let (out, w, h) = half(&raw, &mut Vec::new());
        assert_eq!((w, h), (2, 2));
        // Row 0 and row 2 read; x pairs (0,10) → 5, (20,30) → 25.
        assert_eq!(&out[..6], &[5, 0, 7, 25, 0, 7]);
        assert_eq!(&out[6..], &[5, 100, 7, 25, 100, 7]);
    }

    #[test]
    fn rotation_turns_clockwise() {
        // 2x1: A B → 90 cw: a column A over B.
        let rgb = [1, 1, 1, 2, 2, 2];
        let (o, w, h) = rotate(&rgb, 2, 1, 90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(o, [1, 1, 1, 2, 2, 2]);
        // 270 cw: B over A.
        let (o, _, _) = rotate(&rgb, 2, 1, 270);
        assert_eq!(o, [2, 2, 2, 1, 1, 1]);
        // 2x2 [[1,2],[3,4]] cw → [[3,1],[4,2]].
        let rgb: Vec<u8> = [1u8, 2, 3, 4].iter().flat_map(|v| [*v; 3]).collect();
        let (o, _, _) = rotate(&rgb, 2, 2, 90);
        let px: Vec<u8> = o.chunks(3).map(|c| c[0]).collect();
        assert_eq!(px, [3, 1, 4, 2]);
        let (o, _, _) = rotate(&rgb, 2, 2, 180);
        let px: Vec<u8> = o.chunks(3).map(|c| c[0]).collect();
        assert_eq!(px, [4, 3, 2, 1]);
        assert_eq!(upright(270), 90);
        assert_eq!(upright(90), 270);
        assert_eq!(upright(0), 0);
    }

    #[test]
    fn a_portrait_buffer_comes_out_landscape_and_only_changes_are_encoded() {
        // The panel's buffer: 800x1280, the screen drawn turned by 270.
        let data = buffer(800, 1280, 800 * 4 + 64, |x, y| {
            [(x / 4) as u8, (y / 6) as u8, 0]
        });
        let raw = Raw {
            data: &data,
            width: 800,
            height: 1280,
            stride: 800 * 4 + 64,
            bgr: true,
        };
        let mut p = Pipeline::default();
        let f = p.process(&raw, 270).unwrap().expect("a first frame");
        assert_eq!((f.width, f.height, f.seq), (640, 400, 1));
        assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8]);
        assert!(p.process(&raw, 270).unwrap().is_none(), "nothing changed");
        p.forget();
        assert_eq!(p.process(&raw, 270).unwrap().unwrap().seq, 2);
    }

    /// What it costs on this machine: `cargo test --release -p home-companion capture_cost --
    /// --ignored --nocapture` (the ThinkSmart View's Cortex-A53 is several times slower).
    #[test]
    #[ignore]
    fn capture_cost() {
        let mut data = buffer(800, 1280, 800 * 4, |x, y| {
            [(x ^ y) as u8, (x / 3) as u8, (y / 5) as u8]
        });
        let mut p = Pipeline::default();
        let n = 20;
        let (mut read, mut enc) = (Duration::ZERO, Duration::ZERO);
        for i in 0..n {
            data[i * 4096] ^= 0xFF;
            let raw = Raw {
                data: &data,
                width: 800,
                height: 1280,
                stride: 3200,
                bgr: true,
            };
            let f = p.process(&raw, 270).unwrap().unwrap();
            read += p.took.0;
            enc += p.took.1;
            if i == 0 {
                println!("jpeg {} bytes", f.jpeg.len());
            }
        }
        println!(
            "read+half+rotate {:.2} ms, encode {:.2} ms per frame",
            read.as_secs_f64() * 1e3 / n as f64,
            enc.as_secs_f64() * 1e3 / n as f64
        );
    }

    #[test]
    fn base64_as_the_rfc_says() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    struct Fake {
        id: u32,
        reads: Arc<Mutex<u32>>,
    }

    impl Source for Fake {
        fn current(&mut self) -> Result<u32, String> {
            Ok(self.id)
        }
        fn read(&mut self, f: &mut dyn FnMut(&Raw) -> Result<(), String>) -> Result<(), String> {
            *self.reads.lock().unwrap() += 1;
            let data = buffer(8, 8, 32, |x, y| [x as u8, y as u8, 0]);
            f(&Raw {
                data: &data,
                width: 8,
                height: 8,
                stride: 32,
                bgr: true,
            })
        }
    }

    #[test]
    fn the_thread_runs_only_while_wanted() {
        let reads = Arc::new(Mutex::new(0));
        let r = reads.clone();
        let c = Capture::new(0, move || {
            Ok(Box::new(Fake {
                id: 1,
                reads: r.clone(),
            }) as Box<dyn Source>)
        });
        assert!(!c.running());
        c.want();
        let t = Instant::now();
        while c.latest().is_none() && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let f = c.latest().expect("a frame");
        assert_eq!((f.width, f.height), (4, 4));
        assert_eq!(c.status(), Status::Running);
        c.stop();
        let t = Instant::now();
        while c.running() && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!c.running(), "stopped");
        assert!(c.latest().is_none());
        // The same id, read once (then again only after RECHECK).
        assert_eq!(*reads.lock().unwrap(), 1);
    }

    #[test]
    fn an_unreadable_screen_says_so() {
        let c = Capture::unavailable("renderer");
        c.want();
        assert_eq!(c.status(), Status::Unavailable("renderer"));
        assert!(!c.running(), "no thread for nothing");
        // A source that fails to open says why once its thread tried.
        let c = Capture::new(270, || Err("no_drm"));
        c.want();
        let t = Instant::now();
        while c.status() != Status::Unavailable("no_drm") && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(c.status(), Status::Unavailable("no_drm"));
    }

    #[test]
    fn the_gate_publishes_only_fresh_frames() {
        let t = Instant::now();
        let mut g = Gate::new();
        assert!(g.allows(0, 5, Some(5), t));
        // Hidden: nothing; read under an older generation: nothing.
        g.hidden = true;
        g.generation = 1;
        assert!(!g.allows(1, 6, Some(5), t));
        g.hidden = false;
        g.generation = 2;
        g.revealed = Some(t);
        assert!(
            !g.allows(1, 6, Some(5), t + REVEAL_AFTER),
            "older generation"
        );
        // Shown again: a new framebuffer and REVEAL_AFTER, or REVEAL_AT_MOST whatever.
        assert!(!g.allows(2, 6, Some(5), t), "too soon");
        assert!(
            !g.allows(2, 5, Some(5), t + REVEAL_AFTER),
            "the same framebuffer"
        );
        assert!(g.allows(2, 6, Some(5), t + REVEAL_AFTER));
        assert!(g.allows(2, 5, Some(5), t + REVEAL_AT_MOST));
    }

    /// A source whose framebuffer id and content the test moves.
    struct Moving {
        id: Arc<std::sync::atomic::AtomicU32>,
    }

    impl Source for Moving {
        fn current(&mut self) -> Result<u32, String> {
            Ok(self.id.load(Ordering::Acquire))
        }
        fn read(&mut self, f: &mut dyn FnMut(&Raw) -> Result<(), String>) -> Result<(), String> {
            let v = self.id.load(Ordering::Acquire) as u8;
            let data = buffer(8, 8, 32, |x, y| [x as u8 ^ v, y as u8, v]);
            f(&Raw {
                data: &data,
                width: 8,
                height: 8,
                stride: 32,
                bgr: true,
            })
        }
    }

    fn until(what: impl Fn() -> bool) -> bool {
        let t = Instant::now();
        while !what() && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
        }
        what()
    }

    /// Finding: a page that must not be sent (a QR code) captured, then the remote goes home and
    /// asks again: the cached frame must never come back.
    #[test]
    fn a_hidden_page_never_leaks_through_the_last_frame() {
        let id = Arc::new(std::sync::atomic::AtomicU32::new(1));
        let i = id.clone();
        let c = Capture::new(0, move || {
            Ok(Box::new(Moving { id: i.clone() }) as Box<dyn Source>)
        });
        c.want();
        assert!(until(|| c.latest().is_some()));
        // The private page comes on screen: hidden at once, the last frame gone.
        c.set_hidden(true);
        assert!(c.latest().is_none());
        id.store(2, Ordering::Release);
        std::thread::sleep(Duration::from_millis(700));
        assert!(c.latest().is_none(), "nothing read while hidden");
        // Back home: the framebuffer still shows the private page for a moment.
        c.set_hidden(false);
        std::thread::sleep(Duration::from_millis(200));
        assert!(c.latest().is_none(), "not before a new frame");
        let shown_at = Instant::now();
        id.store(3, Ordering::Release);
        assert!(until(|| c.latest().is_some()));
        assert!(shown_at.elapsed() >= REVEAL_AFTER - Duration::from_millis(200));
        // What it publishes was read from the new framebuffer.
        let f = c.latest().unwrap();
        let mut p = Pipeline::default();
        let data = buffer(8, 8, 32, |x, y| [x as u8 ^ 3, y as u8, 3]);
        let mine = p
            .process(
                &Raw {
                    data: &data,
                    width: 8,
                    height: 8,
                    stride: 32,
                    bgr: true,
                },
                0,
            )
            .unwrap()
            .unwrap();
        assert_eq!(f.jpeg, mine.jpeg);
        c.stop();
    }

    #[test]
    fn news_wakes_the_waiter() {
        let n = news();
        let t = Instant::now();
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            tell_news();
        });
        wait_news(n, Duration::from_secs(2));
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
