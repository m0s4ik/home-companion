// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Test viewer for the web view protocol: takes the panel with
//! KMS, connects to the helper's socket, imports every frame's dma-buf once per buffer id as
//! a GL texture (zero copy), draws it rotated onto the panel, page-flips, and sends
//! `presented` / `release` back; forwards evdev touch in view pixels. No Slint: the body
//! (P2) does the same inside its own renderer.
//!
//! Control on standard input, one command per line: `show`, `hide`, `load URL`, `reload`,
//! `back`, `tap X Y` (a touch in view pixels, through the protocol), `swipe X Y DY` (a drag
//! of DY pixels down, 6 steps 16 ms apart in the events' times), `png FILE` (the
//! scanned-out buffer, turned back to the view's orientation), `quit`.
//!
//! Release rule: a buffer is released when a flipped frame no longer shows it (or at once if
//! a newer frame replaced it before it was drawn); `presented` is sent on the flip that shows
//! a frame (the helper paces rendering on it).

mod egl;
mod kms;
mod png;
mod touch;

use std::collections::HashMap;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egl::{Api, EGLImage, GLuint};
use webview_proto::{self as proto, Message, Recv};

static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

struct Args {
    socket: PathBuf,
    card: String,
    touch: Option<PathBuf>,
    rotation: u32,
    check: bool,
    exit_after: Option<f64>,
    stats_every: f64,
    info: bool,
    hidden: bool,
}

const USAGE: &str = "webview-viewer [--socket PATH] [--card /dev/dri/card0] [--touch PATH|none] \
[--rotation 0|90|180|270] [--check] [--stats SECS] [--exit-after SECS] [--info] [--hidden]";

fn args() -> Result<Args, String> {
    let mut a = Args {
        socket: PathBuf::from("/run/home-companion-web/home-companion-web.sock"),
        card: "/dev/dri/card0".into(),
        touch: touch::find_device(),
        rotation: 270,
        check: false,
        exit_after: None,
        stats_every: 5.0,
        info: false,
        hidden: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or(format!("{k}: missing value"));
        match k.as_str() {
            "--socket" => a.socket = val()?.into(),
            "--card" => a.card = val()?,
            "--touch" => {
                let v = val()?;
                a.touch = (v != "none").then(|| v.into());
            }
            "--rotation" => a.rotation = val()?.parse().map_err(|_| "--rotation: 0|90|180|270")?,
            "--check" => a.check = true,
            "--stats" => a.stats_every = val()?.parse().map_err(|_| "--stats: seconds")?,
            "--exit-after" => {
                a.exit_after = Some(val()?.parse().map_err(|_| "--exit-after: seconds")?)
            }
            "--info" => a.info = true,
            "--hidden" => a.hidden = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument {k}\n{USAGE}")),
        }
    }
    Ok(a)
}

/// View (normalized, origin top left) → panel (normalized), inverse of `touch::map`.
fn view_to_panel(s: f32, t: f32, rotation: u32) -> (f32, f32) {
    match rotation {
        90 => (1.0 - t, s),
        180 => (1.0 - s, 1.0 - t),
        270 => (t, 1.0 - s),
        _ => (s, t),
    }
}

/// A frame's dma-buf imported once: the EGLImage and the texture on it.
struct Imported {
    image: EGLImage,
    tex: GLuint,
}

#[derive(Clone, Copy)]
struct Shown {
    buffer: u32,
    seq: u64,
    produced_ns: u64,
    received_ns: u64,
    /// Connection it came from (frames in flight across a reconnect are not answered).
    conn_gen: u64,
    /// Its buffer was dropped by the helper after it was drawn.
    dropped: bool,
    /// Its `presented` was already sent (skipped while hidden, or a redraw of a shown frame).
    answered: bool,
}

#[derive(Default)]
struct Stats {
    frames: u64,
    skipped: u64,
    imports: u64,
    recv_to_flip: Vec<u64>,
    prod_to_flip: Vec<u64>,
    since: Option<Instant>,
    cpu_since: Duration,
    checks_ok: u64,
    checks_bad: u64,
    gl_errors: u64,
}

fn cpu_time() -> Duration {
    // SAFETY: getrusage writes one struct.
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(r.ru_utime) + tv(r.ru_stime)
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            let l = s.lines().find(|l| l.starts_with("VmRSS:"))?;
            l.split_whitespace().nth(1)?.parse().ok()
        })
        .unwrap_or(0)
}

fn pct(v: &mut [u64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable();
    let i = ((v.len() - 1) as f64 * p).round() as usize;
    v[i] as f64 / 1e6
}

impl Stats {
    fn report(&mut self) {
        let now = Instant::now();
        let cpu = cpu_time();
        if let Some(since) = self.since {
            let wall = now.duration_since(since).as_secs_f64();
            let cpu_pct = (cpu - self.cpu_since).as_secs_f64() / wall * 100.0;
            eprintln!(
                "stats: {:.1} fps shown, {} skipped, {} imports | receive→flip ms p50 {:.1} p95 {:.1} max {:.1} | produce→flip ms p50 {:.1} p95 {:.1} | cpu {:.1}% rss {} kB | gl errors {} | checks ok {} bad {}",
                self.frames as f64 / wall,
                self.skipped,
                self.imports,
                pct(&mut self.recv_to_flip, 0.5),
                pct(&mut self.recv_to_flip, 0.95),
                pct(&mut self.recv_to_flip, 1.0),
                pct(&mut self.prod_to_flip, 0.5),
                pct(&mut self.prod_to_flip, 0.95),
                cpu_pct,
                rss_kb(),
                self.gl_errors,
                self.checks_ok,
                self.checks_bad,
            );
        }
        *self = Stats {
            since: Some(now),
            cpu_since: cpu,
            ..Stats::default()
        };
    }
}

struct Viewer {
    api: Api,
    out: kms::Output,
    program: GLuint,
    rotation: u32,
    check: bool,
    conn: Option<OwnedFd>,
    retry_at: Instant,
    conn_gen: u64,
    textures: HashMap<u32, Imported>,
    /// Received, not drawn yet.
    pending: Option<Shown>,
    /// Drawn, flip queued.
    in_flight: Option<Shown>,
    /// The buffer the screen shows (sampled by the last flipped frame).
    on_screen: Option<u32>,
    stats: Stats,
    checks_left: u32,
    /// The view's size from the helper's hello.
    view: (u32, u32),
    /// The frame on screen (redrawn when the view is shown again).
    last_shown: Option<Shown>,
    /// The body's "Casa" screen is hidden: placeholder on the panel, frames not drawn.
    hidden: bool,
    /// Redraw once the queued flip is done (hide/show while a flip was pending).
    want_redraw: bool,
    /// Latency marks (CLOCK_MONOTONIC ns): last touch down / tap, last show.
    tap_at: Option<u64>,
    show_at: Option<u64>,
}

const VS: &str = "attribute vec2 pos; attribute vec2 uv; varying vec2 v_uv;
void main() { v_uv = uv; gl_Position = vec4(pos, 0.0, 1.0); }";
const FS: &str = "precision mediump float; uniform sampler2D tex; varying vec2 v_uv;
void main() { gl_FragColor = vec4(texture2D(tex, v_uv).rgb, 1.0); }";

impl Viewer {
    fn send(&mut self, m: &Message) {
        if let Some(c) = &self.conn
            && let Err(e) = proto::send(c.as_fd(), m, &[])
        {
            eprintln!("socket: send {:?}: {e}", m.kind());
            self.disconnect();
        }
    }

    /// A frame that will never reach the screen still gets its `presented` (skipped), so the
    /// helper's pacing never waits for it.
    fn skip(&mut self, f: Shown) {
        if f.answered {
            return;
        }
        self.stats.skipped += 1;
        self.send(&Message::Presented(proto::Presented {
            buffer: f.buffer,
            skipped: true,
            seq: f.seq,
            time_ns: proto::monotonic_ns(),
        }));
    }

    fn release(&mut self, buffer: u32) {
        let used = self.pending.is_some_and(|p| p.buffer == buffer)
            || self.in_flight.is_some_and(|p| p.buffer == buffer)
            || self.on_screen == Some(buffer);
        if !used && self.textures.contains_key(&buffer) {
            self.send(&Message::Release { buffer });
        }
    }

    fn forget(&mut self, buffer: u32) {
        if let Some(t) = self.textures.remove(&buffer) {
            // SAFETY: our texture and image; GL keeps the storage until pending draws finish.
            unsafe {
                (self.api.gl.glDeleteTextures)(1, &t.tex);
                (self.api.ext.eglDestroyImageKHR)(self.out.display.dpy, t.image);
            }
        }
    }

    fn disconnect(&mut self) {
        if self.conn.take().is_none() {
            return;
        }
        eprintln!(
            "socket: disconnected, {} textures dropped",
            self.textures.len()
        );
        for id in self.textures.keys().copied().collect::<Vec<_>>() {
            self.forget(id);
        }
        self.pending = None;
        self.on_screen = None;
        self.last_shown = None;
        self.retry_at = Instant::now() + Duration::from_millis(500);
        // Placeholder: dark grey (the body shows its card here).
        if !self.out.flip_pending() {
            let _ = self.draw(None);
        }
    }

    fn try_connect(&mut self, path: &std::path::Path) {
        if self.conn.is_some() || Instant::now() < self.retry_at {
            return;
        }
        self.retry_at = Instant::now() + Duration::from_millis(500);
        let Ok(c) = proto::connect(path) else { return };
        let _ = proto::set_nonblocking(c.as_fd(), true);
        eprintln!("socket: connected to {}", path.display());
        self.conn = Some(c);
        self.conn_gen += 1;
        self.checks_left = 3;
        self.send(&Message::Hello(proto::Hello {
            version: proto::VERSION,
            width: 1280,
            height: 800,
            scale_milli: 1250,
        }));
        if !self.hidden {
            self.send(&Message::Show);
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        if hidden {
            self.send(&Message::Hide);
            eprintln!("hide: placeholder on the panel");
        } else {
            self.show_at = Some(proto::monotonic_ns());
            self.send(&Message::Show);
            eprintln!("show");
        }
        if self.out.flip_pending() {
            self.want_redraw = true;
        } else {
            self.redraw();
        }
    }

    /// Draws the current state: placeholder while hidden, else the waiting frame or the one
    /// last shown (a stale frame at once after `show`, without a second `presented`).
    fn redraw(&mut self) {
        self.want_redraw = false;
        let r = if self.hidden {
            self.draw(None)
        } else if self.pending.is_some() {
            self.draw_pending();
            Ok(())
        } else {
            let last = self.last_shown.map(|f| Shown {
                answered: true,
                ..f
            });
            self.draw(last)
        };
        if let Err(e) = r {
            eprintln!("draw: {e}");
        }
    }

    /// Draws `buffer`'s texture (None = placeholder) and queues the flip.
    fn draw(&mut self, frame: Option<Shown>) -> Result<(), String> {
        let gl = &self.api.gl;
        let tex = frame
            .and_then(|f| self.textures.get(&f.buffer))
            .map(|t| t.tex);
        // Panel corners (u, v) → view texture coordinates; strip TL, BL, TR, BR.
        let corners = [(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0)];
        let mut v = [0f32; 16];
        for (i, &(u, w)) in corners.iter().enumerate() {
            let (s, t) = touch::map(u, w, self.rotation, 1.0, 1.0);
            v[i * 4..i * 4 + 4].copy_from_slice(&[u * 2.0 - 1.0, 1.0 - w * 2.0, s, t]);
        }
        // SAFETY: GL calls on our current context; client-side vertex arrays live until
        // glDrawArrays returns.
        unsafe {
            (gl.glViewport)(0, 0, self.out.width as i32, self.out.height as i32);
            match tex {
                Some(tex) => {
                    (gl.glUseProgram)(self.program);
                    (gl.glActiveTexture)(egl::GL_TEXTURE0);
                    (gl.glBindTexture)(egl::GL_TEXTURE_2D, tex);
                    (gl.glVertexAttribPointer)(0, 2, egl::GL_FLOAT, 0, 16, v.as_ptr().cast());
                    (gl.glVertexAttribPointer)(1, 2, egl::GL_FLOAT, 0, 16, v[2..].as_ptr().cast());
                    (gl.glEnableVertexAttribArray)(0);
                    (gl.glEnableVertexAttribArray)(1);
                    (gl.glDrawArrays)(egl::GL_TRIANGLE_STRIP, 0, 4);
                }
                None => {
                    (gl.glClearColor)(0.12, 0.12, 0.12, 1.0);
                    (gl.glClear)(egl::GL_COLOR_BUFFER_BIT);
                }
            }
        }
        // SAFETY: glGetError on the current context.
        let err = unsafe { (gl.glGetError)() };
        if err != 0 {
            self.stats.gl_errors += 1;
            eprintln!("draw: GL error 0x{err:x}");
        }
        let on_screen_now = self.out.present(&self.api)?;
        if on_screen_now {
            // First modeset: no event.
            self.flipped(frame, proto::monotonic_ns());
        } else {
            self.in_flight = frame;
        }
        Ok(())
    }

    fn flipped(&mut self, frame: Option<Shown>, time_ns: u64) {
        let Some(f) = frame.filter(|f| f.conn_gen == self.conn_gen) else {
            return;
        };
        let ms = |t: u64| time_ns.saturating_sub(t) as f64 / 1e6;
        if let Some(t) = self.show_at {
            if f.answered {
                eprintln!(
                    "show → stale frame (seq {}) on screen: {:.1} ms",
                    f.seq,
                    ms(t)
                );
            } else {
                eprintln!(
                    "show → fresh frame (seq {}) on screen: {:.1} ms",
                    f.seq,
                    ms(t)
                );
                self.show_at = None;
            }
        }
        if !f.answered {
            if let Some(t) = self.tap_at.take() {
                eprintln!(
                    "touch → next frame (seq {}) on screen: {:.1} ms",
                    f.seq,
                    ms(t)
                );
            }
            self.send(&Message::Presented(proto::Presented {
                buffer: f.buffer,
                skipped: false,
                seq: f.seq,
                time_ns,
            }));
            self.stats.frames += 1;
            self.stats
                .recv_to_flip
                .push(time_ns.saturating_sub(f.received_ns));
            self.stats
                .prod_to_flip
                .push(time_ns.saturating_sub(f.produced_ns));
        }
        self.last_shown = (!f.dropped).then_some(f);
        if f.dropped {
            // Shown, but the helper has taken the buffer back: nothing to release for it.
            if let Some(old) = self.on_screen.take() {
                self.release(old);
            }
            return;
        }
        let old = self.on_screen.replace(f.buffer);
        if let Some(old) = old.filter(|&o| o != f.buffer) {
            self.release(old);
        }
        if self.check && self.checks_left > 0 {
            self.checks_left -= 1;
            self.readback(f);
        }
    }

    /// Reads the scanned-out buffer at the view's corners and centre and compares with the
    /// fake producer's pattern: red top left, green top right, blue bottom left, white
    /// bottom right, centre red channel = seq % 256.
    fn readback(&mut self, f: Shown) {
        let (pw, ph) = (self.out.width as f32, self.out.height as f32);
        let (vw, vh) = (self.view.0 as f32, self.view.1 as f32);
        let views = [
            (40.0, 40.0),
            (vw - 40.0, 40.0),
            (40.0, vh - 40.0),
            (vw - 40.0, vh - 40.0),
            (vw / 2.0, vh / 2.0),
        ];
        let pts: Vec<(u32, u32)> = views
            .iter()
            .map(|&(x, y)| {
                let (u, v) = view_to_panel(x / vw, y / vh, self.rotation);
                ((u * pw).min(pw - 1.0) as u32, (v * ph).min(ph - 1.0) as u32)
            })
            .collect();
        let want = [
            (255, 0, 0),
            (0, 255, 0),
            (0, 0, 255),
            (255, 255, 255),
            ((f.seq % 256) as i32, 128, 0),
        ];
        match self.out.read_front(&pts) {
            Ok(got) => {
                let near = |a: (u8, u8, u8), b: (i32, i32, i32)| {
                    (a.0 as i32 - b.0).abs() <= 8
                        && (a.1 as i32 - b.1).abs() <= 8
                        && (a.2 as i32 - b.2).abs() <= 8
                };
                let ok = got.iter().zip(want).all(|(&g, w)| near(g, w));
                if ok {
                    self.stats.checks_ok += 1;
                } else {
                    self.stats.checks_bad += 1;
                }
                eprintln!(
                    "check seq {}: {} | view TL {:?} TR {:?} BL {:?} BR {:?} centre {:?} at panel {:?}",
                    f.seq,
                    if ok {
                        "ok (upright, landscape, right frame)"
                    } else {
                        "MISMATCH"
                    },
                    got[0],
                    got[1],
                    got[2],
                    got[3],
                    got[4],
                    pts
                );
            }
            Err(e) => eprintln!("check: {e}"),
        }
    }

    fn on_message(&mut self, m: Message, fds: Vec<OwnedFd>) {
        let now = proto::monotonic_ns();
        match m {
            Message::Hello(h) => {
                eprintln!(
                    "helper hello: version {} view {}x{} scale {:.2}",
                    h.version,
                    h.width,
                    h.height,
                    h.scale_milli as f64 / 1000.0
                );
                if h.width > 0 && h.height > 0 {
                    self.view = (h.width, h.height);
                }
            }
            Message::Frame(f) => {
                if f.has_fds {
                    self.forget(f.buffer);
                    let planes: Vec<egl::PlaneFd> = f
                        .planes
                        .iter()
                        .zip(&fds)
                        .map(|(p, fd)| egl::PlaneFd {
                            fd: fd.as_raw_fd(),
                            offset: p.offset,
                            stride: p.stride,
                        })
                        .collect();
                    let imported = egl::import_dmabuf(
                        &self.api,
                        self.out.display.dpy,
                        f.width,
                        f.height,
                        f.fourcc,
                        f.modifier,
                        &planes,
                    )
                    .and_then(|image| {
                        match egl::texture_from_image(&self.api, image) {
                            Ok(tex) => Ok(Imported { image, tex }),
                            Err(e) => {
                                // SAFETY: the image we just created.
                                unsafe {
                                    (self.api.ext.eglDestroyImageKHR)(self.out.display.dpy, image)
                                };
                                Err(e)
                            }
                        }
                    });
                    match imported {
                        Ok(t) => {
                            eprintln!(
                                "import buffer {}: {}x{} {} modifier 0x{:x} planes {:?}",
                                f.buffer,
                                f.width,
                                f.height,
                                egl::fourcc_name(f.fourcc),
                                f.modifier,
                                f.planes
                            );
                            self.stats.imports += 1;
                            self.textures.insert(f.buffer, t);
                        }
                        Err(e) => {
                            eprintln!("import buffer {} failed: {e}", f.buffer);
                            self.send(&Message::Release { buffer: f.buffer });
                            return;
                        }
                    }
                    // fds closed here: EGL holds its own references.
                    drop(fds);
                } else if !self.textures.contains_key(&f.buffer) {
                    eprintln!("frame for unknown buffer {} without fds: ignored", f.buffer);
                    self.send(&Message::Release { buffer: f.buffer });
                    return;
                }
                let shown = Shown {
                    buffer: f.buffer,
                    seq: f.seq,
                    produced_ns: f.time_ns,
                    received_ns: now,
                    conn_gen: self.conn_gen,
                    dropped: false,
                    answered: false,
                };
                if self.hidden {
                    // Not drawn now: answered at once so the helper never waits on a hidden
                    // view, kept for `show`.
                    let shown = Shown {
                        answered: true,
                        ..shown
                    };
                    self.skip(Shown {
                        answered: false,
                        ..shown
                    });
                    if let Some(old) = self.pending.replace(shown) {
                        self.skip(old);
                        if old.buffer != shown.buffer {
                            self.release(old.buffer);
                        }
                    }
                    return;
                }
                if let Some(old) = self.pending.replace(shown) {
                    // Replaced before it was drawn: answered at once, its buffer released.
                    self.skip(old);
                    if old.buffer != shown.buffer {
                        self.release(old.buffer);
                    }
                }
                if !self.out.flip_pending() {
                    self.draw_pending();
                }
            }
            Message::Drop { buffer } => {
                if let Some(p) = self.pending.take_if(|p| p.buffer == buffer) {
                    self.skip(p);
                }
                if self.on_screen == Some(buffer) {
                    self.on_screen = None;
                }
                self.last_shown.take_if(|f| f.buffer == buffer);
                if let Some(f) = self.in_flight.as_mut().filter(|f| f.buffer == buffer) {
                    f.dropped = true;
                }
                self.forget(buffer);
                eprintln!("drop buffer {buffer}");
            }
            Message::State(s) => eprintln!(
                "state: {:?} {} {:?} {:?}",
                s.state, s.progress_milli, s.title, s.message
            ),
            Message::Menu(m) => {
                eprintln!("menu {}: {} items, answering cancel", m.menu, m.items.len());
                self.send(&Message::MenuResult {
                    menu: m.menu,
                    index: -1,
                });
            }
            other => eprintln!("unexpected from helper: {other:?}"),
        }
    }

    fn draw_pending(&mut self) {
        if self.hidden {
            return;
        }
        if let Some(f) = self.pending.take()
            && let Err(e) = self.draw(Some(f))
        {
            eprintln!("draw: {e}");
        }
    }

    fn read_socket(&mut self) {
        loop {
            let Some(c) = &self.conn else { return };
            match proto::recv(c.as_fd()) {
                Ok(Recv::Message(m, fds)) => self.on_message(m, fds),
                Ok(Recv::Eof) => return self.disconnect(),
                Err(proto::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) if e.is_unknown() => eprintln!("socket: {e} (skipped)"),
                Err(e) => {
                    eprintln!("socket: {e}");
                    return self.disconnect();
                }
            }
        }
    }
}

/// Line-based control on standard input (non-blocking reads from the main loop's poll).
#[derive(Default)]
struct Control {
    buf: Vec<u8>,
    closed: bool,
}

impl Control {
    fn read(&mut self) -> Vec<String> {
        let mut chunk = [0u8; 1024];
        // SAFETY: reading into our own buffer; poll said fd 0 is readable.
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            self.closed =
                n == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted;
            return Vec::new();
        }
        self.buf.extend_from_slice(&chunk[..n as usize]);
        let mut lines = Vec::new();
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line).trim().to_string();
            if !line.is_empty() {
                lines.push(line);
            }
        }
        lines
    }
}

impl Viewer {
    fn command(&mut self, line: &str) {
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        let arg = arg.trim();
        match cmd {
            "show" => self.set_hidden(false),
            "hide" => self.set_hidden(true),
            "load" if !arg.is_empty() => self.send(&Message::Load {
                url: arg.to_string(),
            }),
            "reload" => self.send(&Message::Reload),
            "back" => self.send(&Message::Back),
            "tap" => {
                let mut it = arg.split_whitespace().map(str::parse::<f32>);
                let (Some(Ok(x)), Some(Ok(y))) = (it.next(), it.next()) else {
                    eprintln!("tap X Y: view pixels");
                    return;
                };
                let now = proto::monotonic_ns();
                self.tap_at = Some(now);
                for kind in [proto::TouchKind::Down, proto::TouchKind::Up] {
                    self.send(&Message::Touch(proto::Touch {
                        kind,
                        id: 900,
                        x,
                        y,
                        time_ns: now,
                    }));
                }
                eprintln!("tap at view ({x:.0}, {y:.0})");
            }
            "swipe" => {
                let v: Vec<f32> = arg
                    .split_whitespace()
                    .filter_map(|a| a.parse().ok())
                    .collect();
                let &[x, y, dy] = v.as_slice() else {
                    eprintln!("swipe X Y DY: view pixels");
                    return;
                };
                let now = proto::monotonic_ns();
                self.tap_at = Some(now);
                let steps = 6;
                for i in 0..=steps + 1 {
                    let kind = match i {
                        0 => proto::TouchKind::Down,
                        i if i <= steps => proto::TouchKind::Motion,
                        _ => proto::TouchKind::Up,
                    };
                    let k = i.min(steps) as f32 / steps as f32;
                    self.send(&Message::Touch(proto::Touch {
                        kind,
                        id: 901,
                        x,
                        y: y + dy * k,
                        time_ns: now + i as u64 * 16_000_000,
                    }));
                }
                eprintln!("swipe at view ({x:.0}, {y:.0}) by {dy:.0}");
            }
            "png" if !arg.is_empty() => match self.save_png(arg) {
                Ok(()) => eprintln!("png: panel saved to {arg} (view orientation)"),
                Err(e) => eprintln!("png: {e}"),
            },
            "quit" => QUIT.store(true, Ordering::Relaxed),
            _ => eprintln!("unknown command: {line}"),
        }
    }

    /// The scanned-out buffer, turned back to the view's orientation (what a person sees
    /// with the device in landscape), as PNG.
    fn save_png(&self, path: &str) -> Result<(), String> {
        let (vw, vh) = self.view;
        let (pw, ph) = (self.out.width, self.out.height);
        let rotation = self.rotation;
        let rgb = self.out.read_front_all(|panel, stride| {
            let mut rgb = Vec::with_capacity((vw * vh * 3) as usize);
            for y in 0..vh {
                for x in 0..vw {
                    let (u, v) = view_to_panel(
                        (x as f32 + 0.5) / vw as f32,
                        (y as f32 + 0.5) / vh as f32,
                        rotation,
                    );
                    let px = ((u * pw as f32) as usize).min(pw as usize - 1);
                    let py = ((v * ph as f32) as usize).min(ph as usize - 1);
                    let i = py * stride + px * 4;
                    rgb.extend_from_slice(&[panel[i + 2], panel[i + 1], panel[i]]);
                }
            }
            rgb
        })?;
        std::fs::write(path, png::encode_rgb(vw, vh, &rgb)).map_err(|e| format!("{path}: {e}"))
    }
}

fn run() -> Result<(), String> {
    let a = args()?;
    // SAFETY: async-signal-safe handler (one atomic store).
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let api = Api::load()?;
    let out = kms::Output::open(&api, &a.card)?;
    eprintln!(
        "egl: {} {} | gl: {} {}",
        api.query_string(out.display.dpy, egl::EGL_VENDOR),
        api.query_string(out.display.dpy, egl::EGL_VERSION),
        api.gl_string(egl::GL_RENDERER),
        api.gl_string(egl::GL_VERSION)
    );
    if a.info {
        for (f, mods) in egl::dmabuf_formats(&api, out.display.dpy) {
            let m: Vec<String> = mods
                .iter()
                .map(|(m, ext)| format!("0x{m:x}{}", if *ext { " (external only)" } else { "" }))
                .collect();
            eprintln!("dma-buf import {}: {}", egl::fourcc_name(f), m.join(", "));
        }
    }
    let program = egl::program(&api, VS, FS, &["pos", "uv"])?;
    // SAFETY: GL calls on the current context.
    unsafe {
        (api.gl.glUseProgram)(program);
        (api.gl.glUniform1i)((api.gl.glGetUniformLocation)(program, c"tex".as_ptr()), 0);
        (api.gl.glDisable)(egl::GL_BLEND);
    }
    let mut touch = match &a.touch {
        Some(p) => match touch::TouchReader::open(p, a.rotation) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("touch: {e}");
                None
            }
        },
        None => None,
    };
    let mut v = Viewer {
        api,
        out,
        program,
        rotation: a.rotation,
        check: a.check,
        conn: None,
        retry_at: Instant::now(),
        textures: HashMap::new(),
        pending: None,
        in_flight: None,
        on_screen: None,
        stats: Stats::default(),
        checks_left: 0,
        conn_gen: 0,
        view: (1280, 800),
        last_shown: None,
        hidden: a.hidden,
        want_redraw: false,
        tap_at: None,
        show_at: None,
    };
    v.draw(None)?;
    v.stats.report();
    let mut control = Control::default();
    let mut stdin_open = true;
    let start = Instant::now();
    let mut next_stats = start + Duration::from_secs_f64(a.stats_every);
    while !QUIT.load(Ordering::Relaxed) {
        if a.exit_after
            .is_some_and(|s| start.elapsed().as_secs_f64() >= s)
        {
            break;
        }
        v.try_connect(&a.socket);
        let mut fds = vec![libc::pollfd {
            fd: v.out.fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        if let Some(t) = &touch {
            fds.push(libc::pollfd {
                fd: t.fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        if let Some(c) = &v.conn {
            fds.push(libc::pollfd {
                fd: c.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let stdin_slot = stdin_open.then(|| {
            fds.push(libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            });
            fds.len() - 1
        });
        // SAFETY: fds is a valid array of pollfd.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 100) };
        if r < 0 {
            continue;
        }
        if fds[0].revents != 0
            && let Some(t) = v.out.dispatch()
        {
            let f = v.in_flight.take();
            v.flipped(f, t);
            if v.want_redraw {
                v.redraw();
            } else {
                v.draw_pending();
            }
        }
        if let Some(i) = stdin_slot
            && fds[i].revents != 0
        {
            for line in control.read() {
                v.command(&line);
            }
            if control.closed {
                stdin_open = false;
            }
        }
        if let Some(t) = touch.as_mut() {
            t.view = (v.view.0 as f32, v.view.1 as f32);
            for ev in t.read() {
                eprintln!(
                    "touch {:?} id {} at view ({:.0}, {:.0})",
                    ev.kind, ev.id, ev.x, ev.y
                );
                if ev.kind == proto::TouchKind::Down {
                    v.tap_at = Some(proto::monotonic_ns());
                }
                if !v.hidden {
                    v.send(&Message::Touch(ev));
                }
            }
        }
        if v.conn.is_some() {
            v.read_socket();
        }
        if Instant::now() >= next_stats {
            v.stats.report();
            next_stats += Duration::from_secs_f64(a.stats_every);
        }
    }
    v.stats.report();
    drop(touch);
    let ids: Vec<u32> = v.textures.keys().copied().collect();
    for id in ids {
        v.forget(id);
    }
    v.out.destroy(&v.api);
    eprintln!("viewer: exit");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("webview-viewer: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_to_panel_inverts_the_touch_map() {
        for rot in [0, 90, 180, 270] {
            for (s, t) in [(0.1, 0.2), (0.9, 0.3), (0.5, 0.5)] {
                let (u, v) = view_to_panel(s, t, rot);
                let (s2, t2) = touch::map(u, v, rot, 1.0, 1.0);
                assert!((s - s2).abs() < 1e-6 && (t - t2).abs() < 1e-6, "rot {rot}");
            }
        }
    }
}
