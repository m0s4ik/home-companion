// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Fake web view helper: listens on the protocol's socket, draws a
//! moving test pattern with GLES 2 into GBM buffers on the render node (EGLImage + FBO) and
//! sends them as dma-buf frames at ~30 fps, honouring `release`; prints the touches it gets
//! and draws a dot where the last one is. Stands in for `home-companion-web` (WPE) while testing the
//! viewer and the body.
//!
//! Pattern (view coordinates, origin top left): red square top left, green top right, blue
//! bottom left, white bottom right, a yellow bar moving left to right, a centre patch whose
//! red channel is `seq % 256` (the viewer's `--check` reads it back).

#[path = "../../webview-viewer/src/egl.rs"]
mod egl;

use std::collections::HashSet;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egl::{Api, GLuint};
use gbm::{AsRaw, BufferObject, BufferObjectFlags, Format, Modifier};
use webview_proto::{self as proto, Message, Recv};

static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

struct Args {
    socket: PathBuf,
    render: String,
    fps: f64,
    width: u32,
    height: u32,
    buffers: usize,
    format: Format,
    alloc: Alloc,
    drop_every: Option<f64>,
    paced: bool,
    finish: bool,
    exit_after: Option<f64>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Alloc {
    /// `gbm_bo_create(RENDERING)`: the driver picks the layout.
    Driver,
    /// `GBM_BO_USE_LINEAR`.
    Linear,
    /// `gbm_bo_create_with_modifiers2` with the modifiers EGL can import (not external-only).
    Modifiers,
}

const USAGE: &str = "webview-fake [--socket PATH] [--render /dev/dri/renderD128] [--fps 30] \
[--size 1280x800] [--buffers 3] [--format argb|xrgb] [--alloc driver|linear|modifiers] \
[--drop-every SECS] [--paced] [--finish] [--exit-after SECS]";

fn args() -> Result<Args, String> {
    let mut a = Args {
        socket: PathBuf::from("/run/home-companion-web/home-companion-web.sock"),
        render: "/dev/dri/renderD128".into(),
        fps: 30.0,
        width: 1280,
        height: 800,
        buffers: 3,
        format: Format::Argb8888,
        alloc: Alloc::Driver,
        drop_every: None,
        paced: false,
        finish: false,
        exit_after: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().ok_or(format!("{k}: missing value"));
        match k.as_str() {
            "--socket" => a.socket = val()?.into(),
            "--render" => a.render = val()?,
            "--fps" => a.fps = val()?.parse().map_err(|_| "--fps: number")?,
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--size: WxH")?;
                a.width = w.parse().map_err(|_| "--size: WxH")?;
                a.height = h.parse().map_err(|_| "--size: WxH")?;
            }
            "--buffers" => a.buffers = val()?.parse().map_err(|_| "--buffers: number")?,
            "--format" => {
                a.format = match val()?.as_str() {
                    "argb" => Format::Argb8888,
                    "xrgb" => Format::Xrgb8888,
                    _ => return Err("--format: argb|xrgb".into()),
                }
            }
            "--alloc" => {
                a.alloc = match val()?.as_str() {
                    "driver" => Alloc::Driver,
                    "linear" => Alloc::Linear,
                    "modifiers" => Alloc::Modifiers,
                    _ => return Err("--alloc: driver|linear|modifiers".into()),
                }
            }
            "--drop-every" => {
                a.drop_every = Some(val()?.parse().map_err(|_| "--drop-every: seconds")?)
            }
            "--paced" => a.paced = true,
            "--finish" => a.finish = true,
            "--exit-after" => {
                a.exit_after = Some(val()?.parse().map_err(|_| "--exit-after: seconds")?)
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument {k}\n{USAGE}")),
        }
    }
    Ok(a)
}

/// One buffer of the ring: the GBM bo and the producer's own view of it (EGLImage, texture,
/// FBO to render into).
struct Buffer {
    id: u32,
    bo: BufferObject<()>,
    image: egl::EGLImage,
    tex: GLuint,
    fbo: GLuint,
    /// Sent and not released yet.
    held: bool,
}

struct Producer {
    api: Api,
    gbm: gbm::Device<std::fs::File>,
    dpy: egl::EGLDisplay,
    program: GLuint,
    u_size: i32,
    u_bar: i32,
    u_seq: i32,
    u_touch: i32,
    width: u32,
    height: u32,
    format: Format,
    alloc: Alloc,
    buffers: Vec<Buffer>,
    next_id: u32,
    /// Ids whose fds the current client has.
    sent_fds: HashSet<u32>,
    seq: u64,
    touch: Option<(f32, f32)>,
    finish: bool,
}

const VS: &str = "attribute vec2 pos; void main() { gl_Position = vec4(pos, 0.0, 1.0); }";
// gl_FragCoord.y = 0.5 is the buffer's first row in memory, the top of the view for whoever
// imports the dma-buf: no flip needed.
const FS: &str = "#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
uniform vec2 size; uniform float bar; uniform float seq; uniform vec2 touch;
void main() {
  vec2 p = gl_FragCoord.xy;
  vec3 c = vec3(0.1 + 0.4 * p.x / size.x, 0.15, 0.1 + 0.4 * p.y / size.y);
  if (mod(floor(p.x / 80.0) + floor(p.y / 80.0), 2.0) < 1.0) c *= 0.8;
  if (abs(p.x - bar) < 24.0) c = vec3(1.0, 1.0, 0.0);
  float m = 120.0;
  if (p.x < m && p.y < m) c = vec3(1.0, 0.0, 0.0);
  else if (p.x > size.x - m && p.y < m) c = vec3(0.0, 1.0, 0.0);
  else if (p.x < m && p.y > size.y - m) c = vec3(0.0, 0.0, 1.0);
  else if (p.x > size.x - m && p.y > size.y - m) c = vec3(1.0);
  if (abs(p.x - size.x * 0.5) < 32.0 && abs(p.y - size.y * 0.5) < 32.0) c = vec3(seq, 128.0 / 255.0, 0.0);
  if (touch.x >= 0.0 && distance(p, touch) < 30.0) c = vec3(1.0);
  gl_FragColor = vec4(c, 1.0);
}";

impl Producer {
    fn allocate(&mut self) -> Result<Buffer, String> {
        let (w, h) = (self.width, self.height);
        let usage = BufferObjectFlags::RENDERING;
        let bo = match self.alloc {
            Alloc::Driver => self
                .gbm
                .create_buffer_object::<()>(w, h, self.format, usage),
            Alloc::Linear => self.gbm.create_buffer_object::<()>(
                w,
                h,
                self.format,
                usage | BufferObjectFlags::LINEAR,
            ),
            Alloc::Modifiers => {
                let mods: Vec<Modifier> = egl::dmabuf_formats(&self.api, self.dpy)
                    .into_iter()
                    .filter(|(f, _)| *f == self.format as u32)
                    .flat_map(|(_, m)| m)
                    .filter(|(_, external)| !external)
                    .map(|(m, _)| Modifier::from(m))
                    .collect();
                self.gbm.create_buffer_object_with_modifiers2::<()>(
                    w,
                    h,
                    self.format,
                    mods.into_iter(),
                    usage,
                )
            }
        }
        .map_err(|e| format!("gbm_bo_create: {e}"))?;
        let fds = plane_fds(&bo)?;
        let planes = planes(&bo);
        let pfds: Vec<egl::PlaneFd> = planes
            .iter()
            .zip(&fds)
            .map(|(p, fd)| egl::PlaneFd {
                fd: fd.as_raw_fd(),
                offset: p.offset,
                stride: p.stride,
            })
            .collect();
        let modifier: u64 = bo.modifier().into();
        let image = egl::import_dmabuf(
            &self.api,
            self.dpy,
            w,
            h,
            self.format as u32,
            modifier,
            &pfds,
        )?;
        let tex = egl::texture_from_image(&self.api, image)?;
        let gl = &self.api.gl;
        let mut fbo = 0;
        // SAFETY: GL calls on our current context.
        unsafe {
            (gl.glGenFramebuffers)(1, &mut fbo);
            (gl.glBindFramebuffer)(egl::GL_FRAMEBUFFER, fbo);
            (gl.glFramebufferTexture2D)(
                egl::GL_FRAMEBUFFER,
                egl::GL_COLOR_ATTACHMENT0,
                egl::GL_TEXTURE_2D,
                tex,
                0,
            );
            let st = (gl.glCheckFramebufferStatus)(egl::GL_FRAMEBUFFER);
            if st != egl::GL_FRAMEBUFFER_COMPLETE {
                return Err(format!("framebuffer incomplete: 0x{st:x}"));
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        eprintln!(
            "buffer {id}: {}x{} {} modifier 0x{modifier:x} planes {:?} ({:?})",
            w,
            h,
            egl::fourcc_name(self.format as u32),
            planes,
            self.alloc
        );
        Ok(Buffer {
            id,
            bo,
            image,
            tex,
            fbo,
            held: false,
        })
    }

    fn free(&self, b: Buffer) {
        // SAFETY: our own GL/EGL objects.
        unsafe {
            (self.api.gl.glDeleteFramebuffers)(1, &b.fbo);
            (self.api.gl.glDeleteTextures)(1, &b.tex);
            (self.api.ext.eglDestroyImageKHR)(self.dpy, b.image);
        }
    }

    fn render(&mut self, i: usize, t: f64) {
        let gl = &self.api.gl;
        let verts: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let (w, h) = (self.width as f32, self.height as f32);
        let (tx, ty) = self.touch.unwrap_or((-1.0, -1.0));
        // SAFETY: GL calls on our current context; the vertex array lives across the draw.
        unsafe {
            (gl.glBindFramebuffer)(egl::GL_FRAMEBUFFER, self.buffers[i].fbo);
            (gl.glViewport)(0, 0, w as i32, h as i32);
            (gl.glUseProgram)(self.program);
            (gl.glUniform2f)(self.u_size, w, h);
            (gl.glUniform1f)(self.u_bar, ((t * 400.0) % w as f64) as f32);
            (gl.glUniform1f)(self.u_seq, (self.seq % 256) as f32 / 255.0);
            (gl.glUniform2f)(self.u_touch, tx, ty);
            (gl.glVertexAttribPointer)(0, 2, egl::GL_FLOAT, 0, 0, verts.as_ptr().cast());
            (gl.glEnableVertexAttribArray)(0);
            (gl.glDrawArrays)(egl::GL_TRIANGLE_STRIP, 0, 4);
            if self.finish {
                (gl.glFinish)();
            } else {
                // Submitted; the viewer's GPU reads wait on the buffer's implicit fence.
                (gl.glFlush)();
            }
        }
    }

    /// Renders into a free buffer and sends it. Returns false if every buffer is held.
    fn produce(&mut self, sock: BorrowedFd<'_>, t: f64) -> Result<bool, String> {
        let Some(i) = self.buffers.iter().position(|b| !b.held) else {
            return Ok(false);
        };
        self.seq += 1;
        self.render(i, t);
        let b = &self.buffers[i];
        let first = !self.sent_fds.contains(&b.id);
        let fds = if first { plane_fds(&b.bo)? } else { Vec::new() };
        let msg = Message::Frame(proto::Frame {
            buffer: b.id,
            width: self.width,
            height: self.height,
            fourcc: self.format as u32,
            modifier: b.bo.modifier().into(),
            has_fds: first,
            seq: self.seq,
            time_ns: proto::monotonic_ns(),
            planes: planes(&b.bo),
        });
        let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|f| f.as_fd()).collect();
        proto::send(sock, &msg, &borrowed).map_err(|e| format!("send frame: {e}"))?;
        // Our fds close here, after sendmsg (the viewer has its own).
        drop(fds);
        let id = self.buffers[i].id;
        self.buffers[i].held = true;
        self.sent_fds.insert(id);
        Ok(true)
    }

    /// Replaces one buffer (as WebKit does on resize or a web process restart): `drop` for the
    /// old id, the new one gets a new id. Takes a held one (likely on screen) when there is
    /// one: the harder case for the viewer.
    fn replace_one(&mut self, sock: Option<BorrowedFd<'_>>) -> Result<(), String> {
        let i = self.buffers.iter().position(|b| b.held).unwrap_or(0);
        let fresh = self.allocate()?;
        let old = std::mem::replace(&mut self.buffers[i], fresh);
        if let Some(s) = sock {
            Message::Drop { buffer: old.id }
                .send_to(s)
                .map_err(|e| format!("send drop: {e}"))?;
        }
        eprintln!(
            "replaced buffer {} (held {}) with {}",
            old.id, old.held, self.buffers[i].id
        );
        self.sent_fds.remove(&old.id);
        self.free(old);
        Ok(())
    }
}

fn plane_fds(bo: &BufferObject<()>) -> Result<Vec<OwnedFd>, String> {
    (0..bo.plane_count() as i32)
        .map(|p| {
            bo.fd_for_plane(p)
                .map_err(|e| format!("gbm_bo_get_fd_for_plane: {e}"))
        })
        .collect()
}

fn planes(bo: &BufferObject<()>) -> Vec<proto::Plane> {
    (0..bo.plane_count() as i32)
        .map(|p| proto::Plane {
            offset: bo.offset(p),
            stride: bo.stride_for_plane(p),
        })
        .collect()
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
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&a.render)
        .map_err(|e| format!("{}: {e}", a.render))?;
    let gbm = gbm::Device::new(file).map_err(|e| format!("gbm: {e}"))?;
    let display = egl::Display::new(&api, gbm.as_raw() as *mut _, None)?;
    // SAFETY: surfaceless context (EGL_KHR_surfaceless_context): we only render to FBOs.
    if unsafe {
        (api.egl.eglMakeCurrent)(
            display.dpy,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            display.ctx,
        )
    } == 0
    {
        return Err(format!("eglMakeCurrent (surfaceless): {}", api.error()));
    }
    eprintln!(
        "fake: {} on {} | {} {}",
        gbm.backend_name(),
        a.render,
        api.gl_string(egl::GL_RENDERER),
        api.gl_string(egl::GL_VERSION)
    );
    let program = egl::program(&api, VS, FS, &["pos"])?;
    let uni = |n: &std::ffi::CStr| {
        // SAFETY: valid program and C string.
        unsafe { (api.gl.glGetUniformLocation)(program, n.as_ptr()) }
    };
    let (u_size, u_bar, u_seq, u_touch) = (uni(c"size"), uni(c"bar"), uni(c"seq"), uni(c"touch"));
    let mut p = Producer {
        api,
        gbm,
        dpy: display.dpy,
        program,
        u_size,
        u_bar,
        u_seq,
        u_touch,
        width: a.width,
        height: a.height,
        format: a.format,
        alloc: a.alloc,
        buffers: Vec::new(),
        next_id: 1,
        sent_fds: HashSet::new(),
        seq: 0,
        touch: None,
        finish: a.finish,
    };
    for _ in 0..a.buffers.max(1) {
        let b = p.allocate()?;
        p.buffers.push(b);
    }
    if let Some(dir) = a.socket.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = proto::listen(&a.socket).map_err(|e| format!("{}: {e}", a.socket.display()))?;
    proto::set_nonblocking(listener.as_fd(), true).map_err(|e| e.to_string())?;
    eprintln!("fake: listening on {}", a.socket.display());

    let start = Instant::now();
    let period = Duration::from_secs_f64(1.0 / a.fps.max(1.0));
    let mut next_frame = Instant::now();
    let mut next_drop = a
        .drop_every
        .map(|s| Instant::now() + Duration::from_secs_f64(s));
    let mut client: Option<OwnedFd> = None;
    // --paced: wait for `presented` of the last frame before rendering the next one.
    let mut awaiting_present = false;
    let (mut sent, mut stalls, mut presented, mut skipped) = (0u64, 0u64, 0u64, 0u64);
    let mut last_report = Instant::now();
    while !QUIT.load(Ordering::Relaxed) {
        if a.exit_after
            .is_some_and(|s| start.elapsed().as_secs_f64() >= s)
        {
            break;
        }
        let now = Instant::now();
        let wait = next_frame
            .saturating_duration_since(now)
            .min(Duration::from_millis(100));
        let mut fds = vec![libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        if let Some(c) = &client {
            fds.push(libc::pollfd {
                fd: c.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        // SAFETY: valid pollfd array.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, wait.as_millis() as i32) };
        if fds[0].revents != 0
            && let Ok(c) = proto::accept(listener.as_fd())
        {
            if client.is_some() {
                eprintln!("fake: second client refused");
            } else {
                let _ = proto::set_nonblocking(c.as_fd(), true);
                eprintln!("fake: client connected");
                let hello = Message::Hello(proto::Hello {
                    version: proto::VERSION,
                    width: a.width,
                    height: a.height,
                    scale_milli: 1250,
                });
                if hello.send_to(&c).is_ok() {
                    client = Some(c);
                    p.sent_fds.clear();
                    for b in &mut p.buffers {
                        b.held = false;
                    }
                    awaiting_present = false;
                }
            }
        }
        // Messages from the body.
        let mut lost = false;
        while let Some(c) = &client {
            match proto::recv(c.as_fd()) {
                Ok(Recv::Message(m, _)) => match m {
                    Message::Release { buffer } => {
                        if let Some(b) = p.buffers.iter_mut().find(|b| b.id == buffer) {
                            b.held = false;
                        }
                    }
                    Message::Presented(pr) => {
                        if pr.skipped {
                            skipped += 1;
                        } else {
                            presented += 1;
                        }
                        if p.seq == pr.seq {
                            awaiting_present = false;
                        }
                    }
                    Message::Touch(t) => {
                        println!(
                            "touch {:?} id {} at ({:.1}, {:.1}) t {}",
                            t.kind, t.id, t.x, t.y, t.time_ns
                        );
                        p.touch = match t.kind {
                            proto::TouchKind::Up | proto::TouchKind::Cancel => None,
                            _ => Some((t.x, t.y)),
                        };
                    }
                    other => eprintln!("fake: from body: {other:?}"),
                },
                Ok(Recv::Eof) => {
                    lost = true;
                    break;
                }
                Err(proto::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.is_unknown() => eprintln!("fake: {e} (skipped)"),
                Err(e) => {
                    eprintln!("fake: {e}");
                    lost = true;
                    break;
                }
            }
        }
        if lost {
            eprintln!("fake: client gone");
            client = None;
        }
        if let Some(t) = next_drop
            && Instant::now() >= t
        {
            p.replace_one(client.as_ref().map(|c| c.as_fd()))?;
            next_drop = a
                .drop_every
                .map(|s| Instant::now() + Duration::from_secs_f64(s));
        }
        if Instant::now() >= next_frame {
            next_frame += period;
            if next_frame < Instant::now() {
                next_frame = Instant::now() + period;
            }
            if let Some(c) = &client
                && !(a.paced && awaiting_present)
            {
                match p.produce(c.as_fd(), start.elapsed().as_secs_f64()) {
                    Ok(true) => {
                        sent += 1;
                        awaiting_present = true;
                    }
                    Ok(false) => stalls += 1,
                    Err(e) => {
                        eprintln!("fake: {e}");
                        client = None;
                    }
                }
            }
        }
        if last_report.elapsed() >= Duration::from_secs(5) {
            let held = p.buffers.iter().filter(|b| b.held).count();
            eprintln!(
                "fake: sent {sent} presented {presented} skipped {skipped} stalls {stalls} held now {held}"
            );
            (sent, stalls, presented, skipped) = (0, 0, 0, 0);
            last_report = Instant::now();
        }
    }
    for b in std::mem::take(&mut p.buffers) {
        p.free(b);
    }
    let _ = std::fs::remove_file(&a.socket);
    eprintln!("fake: exit");
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("webview-fake: {e}");
        std::process::exit(1);
    }
}
