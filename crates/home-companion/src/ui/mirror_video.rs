// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! AirPlay screen mirroring on the screen. UxPlay decodes the video
//! with the hardware decoder (venus) and hands the frames on through GStreamer's `unixfdsink`:
//! a Unix socket carrying each frame's dma-buf, its layout (`GstVideoMeta`) and nothing else,
//! no copy. Here, on the UI thread, a frame is imported into Slint's GL context (NV12 as an
//! external texture), drawn once into an RGBA texture (the colour conversion is the GPU's) and
//! shown as a `slint::Image`; the frame goes back to the decoder at once.
//!
//! The socket protocol (gst-plugins-bad 1.26, gst/unixfd): every message is a header
//! `{type: u32, size: u32}` (little endian) and `size` bytes; NEW_BUFFER carries the fds as
//! SCM_RIGHTS with its first byte, and must be answered with RELEASE_BUFFER `{id: u64}`.

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const CMD_NEW_BUFFER: u32 = 0;
pub const CMD_RELEASE_BUFFER: u32 = 1;
pub const CMD_CAPS: u32 = 2;
pub const CMD_EOS: u32 = 3;
/// `MEMORY_TYPE_DMABUF`: the fds are dma-bufs (anything else would be a copy in shared memory).
pub const MEMORY_DMABUF: u8 = 1;
/// `GST_VIDEO_FORMAT_NV12`.
pub const GST_FORMAT_NV12: i32 = 23;
pub const FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");

/// One frame's layout, from NEW_BUFFER and its serialized `GstVideoMeta`.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub id: u64,
    pub dmabuf: bool,
    /// (offset, size) of each memory, one fd each.
    pub memories: Vec<(u64, u64)>,
    pub format: i32,
    pub width: u32,
    pub height: u32,
    /// Per plane: offset in the buffer and stride.
    pub planes: Vec<(u64, i32)>,
}

/// NEW_BUFFER's payload (56 bytes, the memories, then the serialized metas).
pub fn parse_new_buffer(p: &[u8]) -> Option<Layout> {
    let u64_at =
        |o: usize| -> Option<u64> { Some(u64::from_le_bytes(p.get(o..o + 8)?.try_into().ok()?)) };
    let u32_at =
        |o: usize| -> Option<u32> { Some(u32::from_le_bytes(p.get(o..o + 4)?.try_into().ok()?)) };
    let id = u64_at(0)?;
    let kind = *p.get(52)?;
    let n_memory = *p.get(53)? as usize;
    let n_meta = u16::from_le_bytes(p.get(54..56)?.try_into().ok()?) as usize;
    let mut memories = Vec::with_capacity(n_memory);
    for i in 0..n_memory {
        let o = 56 + 16 * i;
        memories.push((u64_at(o + 8)?, u64_at(o)?));
    }
    let mut o = 56 + 16 * n_memory;
    let mut layout = None;
    for _ in 0..n_meta {
        // [total size][name length][name][\0][version][payload]
        let total = u32_at(o)? as usize;
        let name_len = u32_at(o + 4)? as usize;
        let name = std::str::from_utf8(p.get(o + 8..o + 8 + name_len)?).ok()?;
        let body = p.get(o + 8 + name_len + 2..o + total)?;
        if name == "GstVideoMeta" {
            layout = parse_video_meta(body);
        }
        o += total.max(1);
    }
    let (format, width, height, planes) = layout?;
    Some(Layout {
        id,
        dmabuf: kind == MEMORY_DMABUF,
        memories,
        format,
        width,
        height,
        planes,
    })
}

/// `GstVideoMeta` as gst-plugins-base serializes it: flags, format, width, height, n_planes,
/// the offsets (u64), the strides (i32), then the alignment (not needed here).
pub type VideoMeta = (i32, u32, u32, Vec<(u64, i32)>);

/// See [`VideoMeta`]: format, width, height and (offset, stride) per plane.
pub fn parse_video_meta(b: &[u8]) -> Option<VideoMeta> {
    let i32_at =
        |o: usize| -> Option<i32> { Some(i32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?)) };
    let format = i32_at(4)?;
    let width = i32_at(8)? as u32;
    let height = i32_at(12)? as u32;
    let n = i32_at(16)? as usize;
    if n == 0 || n > 4 {
        return None;
    }
    let mut planes = Vec::with_capacity(n);
    for i in 0..n {
        let off = u64::from_le_bytes(b.get(20 + 8 * i..28 + 8 * i)?.try_into().ok()?);
        let stride = i32_at(20 + 8 * n + 4 * i)?;
        planes.push((off, stride));
    }
    Some((format, width, height, planes))
}

pub fn release_message(id: u64) -> [u8; 16] {
    let mut m = [0u8; 16];
    m[0..4].copy_from_slice(&CMD_RELEASE_BUFFER.to_le_bytes());
    m[4..8].copy_from_slice(&8u32.to_le_bytes());
    m[8..16].copy_from_slice(&id.to_le_bytes());
    m
}

/// One message from the socket: its type, payload and fds.
struct Message {
    kind: u32,
    payload: Vec<u8>,
    fds: Vec<OwnedFd>,
}

/// Reads one message if one is waiting (never blocks for the first byte).
fn receive(sock: &mut UnixStream) -> std::io::Result<Option<Message>> {
    let fd = sock.as_raw_fd();
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, no wait.
    if unsafe { libc::poll(&mut pfd, 1, 0) } <= 0 {
        return Ok(None);
    }
    let mut head = [0u8; 8];
    let mut fds = Vec::new();
    // The header with the fds (if any) attached to its first byte.
    let mut cmsg = [0u64; 16]; // room for CMSG_SPACE(16 fds), aligned
    let mut iov = libc::iovec {
        iov_base: head.as_mut_ptr().cast(),
        iov_len: 8,
    };
    // SAFETY: msghdr pointing at our buffers; recvmsg fills them.
    let n = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&cmsg) as _;
        let n = libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC);
        if n > 0 {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(c) as *const RawFd;
                    let count = ((*c).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / 4;
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(*data.add(i)));
                    }
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        n
    };
    if n == 0 {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if (n as usize) < 8 {
        sock.read_exact(&mut head[n as usize..])?;
    }
    let kind = u32::from_le_bytes(head[0..4].try_into().unwrap());
    let size = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
    if size > 1 << 20 {
        return Err(std::io::Error::other(format!("message of {size} bytes")));
    }
    let mut payload = vec![0u8; size];
    sock.read_exact(&mut payload)?;
    Ok(Some(Message { kind, payload, fds }))
}

/// The client side of UxPlay's video socket, polled on the UI thread.
pub struct Mirror {
    path: PathBuf,
    sock: Option<UnixStream>,
    last_try: Option<Instant>,
    #[cfg(feature = "gpu")]
    gpu: Option<gpu::Converter>,
    frames: u64,
    started: Option<Instant>,
    /// The sender's picture's aspect ratio (None: the frame as it is).
    aspect: Option<f32>,
}

/// The part of a `w` x `h` frame that is picture, for the sender's aspect ratio: the decoder
/// pads to whole macroblocks on the right and at the bottom (an iPad upright: 556 of 560).
pub fn visible(w: u32, h: u32, aspect: Option<f32>) -> (u32, u32) {
    let Some(a) = aspect.filter(|a| *a > 0.0) else {
        return (w, h);
    };
    let vw = (h as f32 * a).round() as u32;
    let vh = (w as f32 / a).round() as u32;
    // Only small paddings: anything bigger is not padding (a stale ratio after a rotation).
    if vw < w && w - vw <= 16 {
        (vw, h)
    } else if vh < h && h - vh <= 16 {
        (w, vh)
    } else {
        (w, h)
    }
}

impl Mirror {
    pub fn new(path: PathBuf) -> Mirror {
        Mirror {
            path,
            sock: None,
            last_try: None,
            #[cfg(feature = "gpu")]
            gpu: None,
            frames: 0,
            started: None,
            aspect: None,
        }
    }

    pub fn set_aspect(&mut self, a: f32) {
        if self.aspect != Some(a) {
            log::info!(event = "mirror", aspect = a; "mirroring: picture aspect {a:.4}");
            self.aspect = Some(a);
        }
    }

    pub fn connected(&self) -> bool {
        self.sock.is_some()
    }

    /// Connected this long and not one frame: the decoder is stuck (seen when a sender starts
    /// right after another one's session); the session has to be started again.
    pub fn stuck(&self, after: Duration) -> bool {
        self.frames == 0 && self.started.is_some_and(|t| t.elapsed() > after)
    }

    /// Mirroring is on: connect (again every 200 ms until UxPlay's sink listens), take the
    /// waiting frames; the newest one converted (its image, width and height), if any. Call it
    /// often (every UI tick).
    pub fn poll(&mut self) -> Option<(slint::Image, u32, u32)> {
        if self.sock.is_none() {
            if self
                .last_try
                .is_some_and(|t| t.elapsed() < Duration::from_millis(200))
            {
                return None;
            }
            self.last_try = Some(Instant::now());
            match UnixStream::connect(&self.path) {
                Ok(s) => {
                    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                    log::info!(event = "mirror", state = "connected"; "mirroring: video socket connected");
                    self.sock = Some(s);
                    self.frames = 0;
                    self.started = Some(Instant::now());
                }
                Err(_) => return None,
            }
        }
        // Everything waiting; only the newest frame is drawn, the others go back at once.
        let mut newest: Option<(Layout, Vec<OwnedFd>)> = None;
        loop {
            let sock = self.sock.as_mut()?;
            match receive(sock) {
                Ok(None) => break,
                Ok(Some(m)) => match m.kind {
                    CMD_NEW_BUFFER => match parse_new_buffer(&m.payload) {
                        Some(l) => {
                            if let Some((old, _)) = newest.replace((l, m.fds)) {
                                self.release(old.id);
                            }
                        }
                        None => {
                            // Without a layout we cannot draw it; give it back.
                            if m.payload.len() >= 8 {
                                let id = u64::from_le_bytes(m.payload[0..8].try_into().unwrap());
                                self.release(id);
                            }
                            log::warn!("mirroring: a frame without GstVideoMeta (dropped)");
                        }
                    },
                    CMD_CAPS => {
                        log::info!(event = "mirror", caps = String::from_utf8_lossy(&m.payload).trim_end_matches('\0'); "mirroring: caps")
                    }
                    CMD_EOS => log::info!("mirroring: end of stream"),
                    _ => {}
                },
                Err(e) => {
                    self.disconnect(&format!("{e}"));
                    return None;
                }
            }
        }
        let (l, fds) = newest?;
        let (vw, vh) = visible(l.width, l.height, self.aspect);
        let img = self.draw(&l, &fds, vw, vh);
        drop(fds);
        self.release(l.id);
        img.map(|i| (i, vw, vh))
    }

    #[cfg_attr(not(feature = "gpu"), allow(unused_variables))]
    fn draw(&mut self, l: &Layout, fds: &[OwnedFd], vw: u32, vh: u32) -> Option<slint::Image> {
        if !l.dmabuf || l.format != GST_FORMAT_NV12 || fds.is_empty() {
            log::warn!(
                "mirroring: frame not drawn (dma-buf {}, format {})",
                l.dmabuf,
                l.format
            );
            return None;
        }
        self.frames += 1;
        if self.frames == 1 {
            log::info!(event = "mirror", width = l.width, height = l.height; "mirroring: first frame {}x{}", l.width, l.height);
        }
        #[cfg(feature = "gpu")]
        {
            if self.gpu.is_none() {
                match gpu::Converter::new() {
                    Ok(c) => self.gpu = Some(c),
                    Err(e) => {
                        log::warn!("mirroring: {e}");
                        return None;
                    }
                }
            }
            match self.gpu.as_mut()?.convert(l, fds, vw, vh) {
                Ok(img) => Some(img),
                Err(e) => {
                    log::warn!("mirroring: {e}");
                    None
                }
            }
        }
        #[cfg(not(feature = "gpu"))]
        None
    }

    fn release(&mut self, id: u64) {
        if let Some(s) = self.sock.as_mut() {
            use std::io::Write;
            if let Err(e) = s.write_all(&release_message(id)) {
                self.disconnect(&format!("release: {e}"));
            }
        }
    }

    /// Mirroring is over (or the socket broke).
    pub fn disconnect(&mut self, why: &str) {
        if self.sock.take().is_some() {
            let secs = self.started.map_or(0.0, |t| t.elapsed().as_secs_f32());
            log::info!(event = "mirror", state = "closed", frames = self.frames; "mirroring: video socket closed ({why}), {} frames in {secs:.0} s", self.frames);
        }
    }
}

#[cfg(feature = "gpu")]
mod gpu {
    use super::Layout;
    use crate::ui::webview::gl::egl::{self, Api, EGLDisplay, GLuint};
    use std::ffi::c_void;
    use std::os::fd::{AsRawFd, OwnedFd};

    const VS: &str = "attribute vec2 pos;\nuniform vec2 scale;\nvarying vec2 uv;\nvoid main() {\n  uv = (pos * 0.5 + 0.5) * scale;\n  gl_Position = vec4(pos, 0.0, 1.0);\n}\n";
    const FS: &str = "#extension GL_OES_EGL_image_external : require\nprecision mediump float;\nuniform samplerExternalOES tex;\nvarying vec2 uv;\nvoid main() {\n  gl_FragColor = vec4(texture2D(tex, uv).rgb, 1.0);\n}\n";

    /// NV12 dma-buf → RGBA texture, in Slint's own context (current on the UI thread).
    pub struct Converter {
        api: Api,
        dpy: EGLDisplay,
        program: GLuint,
        fbo: GLuint,
        /// Two targets in turn: the image changes every frame, so Slint draws it again.
        targets: [GLuint; 2],
        next: usize,
        size: (u32, u32),
    }

    type GetCurrent = unsafe extern "C" fn() -> *mut c_void;

    impl Converter {
        pub fn new() -> Result<Converter, String> {
            let api = Api::load()?;
            // SAFETY: dlsym of eglGetCurrentDisplay (EGL 1.4, this signature) in the libEGL
            // Slint already loaded.
            let dpy = unsafe {
                let h = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
                if h.is_null() {
                    return Err("libEGL.so.1 not loaded (not the GPU renderer?)".into());
                }
                let p = libc::dlsym(h, c"eglGetCurrentDisplay".as_ptr());
                if p.is_null() {
                    return Err("eglGetCurrentDisplay missing".into());
                }
                std::mem::transmute::<*mut c_void, GetCurrent>(p)()
            };
            if dpy.is_null() {
                return Err(
                    "no current EGL display on the UI thread (not the GPU renderer?)".into(),
                );
            }
            let program = egl::program(&api, VS, FS, &["pos"])?;
            let gl = &api.gl;
            let (mut fbo, mut targets) = (0, [0; 2]);
            // SAFETY: GL calls on the current context.
            unsafe {
                (gl.glGenFramebuffers)(1, &mut fbo);
                (gl.glGenTextures)(2, targets.as_mut_ptr());
                for t in targets {
                    (gl.glBindTexture)(egl::GL_TEXTURE_2D, t);
                    (gl.glTexParameteri)(
                        egl::GL_TEXTURE_2D,
                        egl::GL_TEXTURE_MIN_FILTER,
                        egl::GL_LINEAR,
                    );
                    (gl.glTexParameteri)(
                        egl::GL_TEXTURE_2D,
                        egl::GL_TEXTURE_MAG_FILTER,
                        egl::GL_LINEAR,
                    );
                    (gl.glTexParameteri)(
                        egl::GL_TEXTURE_2D,
                        egl::GL_TEXTURE_WRAP_S,
                        egl::GL_CLAMP_TO_EDGE,
                    );
                    (gl.glTexParameteri)(
                        egl::GL_TEXTURE_2D,
                        egl::GL_TEXTURE_WRAP_T,
                        egl::GL_CLAMP_TO_EDGE,
                    );
                }
                (gl.glBindTexture)(egl::GL_TEXTURE_2D, 0);
            }
            log::info!(
                "mirroring: converting frames on the GPU ({})",
                api.gl_string(egl::GL_RENDERER)
            );
            Ok(Converter {
                api,
                dpy,
                program,
                fbo,
                targets,
                next: 0,
                size: (0, 0),
            })
        }

        /// Converts the frame's top left `vw` x `vh` (the picture without the padding).
        pub fn convert(
            &mut self,
            l: &Layout,
            fds: &[OwnedFd],
            vw: u32,
            vh: u32,
        ) -> Result<slint::Image, String> {
            let (w, h) = (l.width, l.height);
            let gl = &self.api.gl;
            // The image: both planes in the first memory (venus), or one memory per plane.
            let mut a = vec![
                egl::EGL_WIDTH,
                w as i32,
                egl::EGL_HEIGHT,
                h as i32,
                egl::EGL_LINUX_DRM_FOURCC_EXT,
                super::FOURCC_NV12 as i32,
            ];
            const PLANES: [[i32; 3]; 2] = [[0x3272, 0x3273, 0x3274], [0x3275, 0x3276, 0x3277]];
            for (i, (off, stride)) in l.planes.iter().take(2).enumerate() {
                let fd = fds.get(i).or(fds.first()).ok_or("no fd")?;
                let base = l.memories.get(i).or(l.memories.first()).map_or(0, |m| m.0);
                a.extend_from_slice(&[
                    PLANES[i][0],
                    fd.as_raw_fd(),
                    PLANES[i][1],
                    (base + off) as i32,
                    PLANES[i][2],
                    *stride,
                ]);
            }
            // The iPhone's stream: BT.709, full range (its caps say colorimetry 1:3:5:1).
            a.extend_from_slice(&[
                egl::EGL_YUV_COLOR_SPACE_HINT_EXT,
                egl::EGL_ITU_REC709_EXT,
                egl::EGL_SAMPLE_RANGE_HINT_EXT,
                egl::EGL_YUV_FULL_RANGE_EXT,
                egl::EGL_NONE,
            ]);
            // SAFETY: attribute list terminated by EGL_NONE; EGL dups the fds.
            let image = unsafe {
                (self.api.ext.eglCreateImageKHR)(
                    self.dpy,
                    std::ptr::null_mut(),
                    egl::EGL_LINUX_DMA_BUF_EXT,
                    std::ptr::null_mut(),
                    a.as_ptr(),
                )
            };
            if image.is_null() {
                return Err(format!("eglCreateImageKHR(NV12): {}", self.api.error()));
            }
            let target = self.targets[self.next];
            self.next = 1 - self.next;
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            // SAFETY: GL calls on the current context; Slint/femtovg sets up its own state at
            // every render, the framebuffer and viewport are put back as they were.
            let res = unsafe {
                let (mut old_fbo, mut vp) = (0, [0i32; 4]);
                (gl.glGetIntegerv)(egl::GL_FRAMEBUFFER_BINDING, &mut old_fbo);
                (gl.glGetIntegerv)(egl::GL_VIEWPORT, vp.as_mut_ptr());
                let mut ext = 0;
                (gl.glGenTextures)(1, &mut ext);
                (gl.glActiveTexture)(egl::GL_TEXTURE0);
                (gl.glBindTexture)(egl::GL_TEXTURE_EXTERNAL_OES, ext);
                (gl.glTexParameteri)(
                    egl::GL_TEXTURE_EXTERNAL_OES,
                    egl::GL_TEXTURE_MIN_FILTER,
                    egl::GL_LINEAR,
                );
                (gl.glTexParameteri)(
                    egl::GL_TEXTURE_EXTERNAL_OES,
                    egl::GL_TEXTURE_MAG_FILTER,
                    egl::GL_LINEAR,
                );
                (gl.glGetError)();
                (gl.glEGLImageTargetTexture2DOES)(egl::GL_TEXTURE_EXTERNAL_OES, image);
                let mut err = (gl.glGetError)();
                if err == 0 {
                    if self.size != (vw, vh) {
                        for t in self.targets {
                            (gl.glBindTexture)(egl::GL_TEXTURE_2D, t);
                            (gl.glTexImage2D)(
                                egl::GL_TEXTURE_2D,
                                0,
                                egl::GL_RGBA as i32,
                                vw as i32,
                                vh as i32,
                                0,
                                egl::GL_RGBA,
                                egl::GL_UNSIGNED_BYTE,
                                std::ptr::null(),
                            );
                        }
                        (gl.glBindTexture)(egl::GL_TEXTURE_2D, 0);
                        self.size = (vw, vh);
                    }
                    (gl.glBindFramebuffer)(egl::GL_FRAMEBUFFER, self.fbo);
                    (gl.glFramebufferTexture2D)(
                        egl::GL_FRAMEBUFFER,
                        egl::GL_COLOR_ATTACHMENT0,
                        egl::GL_TEXTURE_2D,
                        target,
                        0,
                    );
                    if (gl.glCheckFramebufferStatus)(egl::GL_FRAMEBUFFER)
                        == egl::GL_FRAMEBUFFER_COMPLETE
                    {
                        (gl.glViewport)(0, 0, vw as i32, vh as i32);
                        (gl.glDisable)(egl::GL_BLEND);
                        (gl.glDisable)(egl::GL_SCISSOR_TEST);
                        (gl.glDisable)(egl::GL_STENCIL_TEST);
                        (gl.glDisable)(egl::GL_CULL_FACE);
                        (gl.glUseProgram)(self.program);
                        let loc = (gl.glGetUniformLocation)(self.program, c"tex".as_ptr());
                        (gl.glUniform1i)(loc, 0);
                        let sc = (gl.glGetUniformLocation)(self.program, c"scale".as_ptr());
                        (gl.glUniform2f)(sc, vw as f32 / w as f32, vh as f32 / h as f32);
                        (gl.glBindBuffer)(egl::GL_ARRAY_BUFFER, 0);
                        (gl.glVertexAttribPointer)(0, 2, egl::GL_FLOAT, 0, 8, quad.as_ptr().cast());
                        (gl.glEnableVertexAttribArray)(0);
                        (gl.glDrawArrays)(egl::GL_TRIANGLE_STRIP, 0, 4);
                        (gl.glDisableVertexAttribArray)(0);
                        (gl.glUseProgram)(0);
                        (gl.glEnable)(egl::GL_BLEND);
                    } else {
                        err = 1;
                    }
                    (gl.glBindFramebuffer)(egl::GL_FRAMEBUFFER, old_fbo as GLuint);
                    (gl.glViewport)(vp[0], vp[1], vp[2], vp[3]);
                }
                (gl.glBindTexture)(egl::GL_TEXTURE_EXTERNAL_OES, 0);
                (gl.glDeleteTextures)(1, &ext);
                // The draw must have read the frame before it goes back to the decoder.
                (gl.glFlush)();
                (self.api.ext.eglDestroyImageKHR)(self.dpy, image);
                err
            };
            if res != 0 {
                return Err(format!("NV12 → RGBA failed (GL 0x{res:x})"));
            }
            let id = std::num::NonZeroU32::new(target).ok_or("no texture")?;
            // SAFETY: a GL_TEXTURE_2D of the context Slint renders with, alive with the converter.
            Ok(unsafe {
                slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(id, (vw, vh).into())
            }
            .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
            .build())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NEW_BUFFER as unixfdsink sent it for a 1280x592 frame from venus (captured on the device).
    fn sample() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0x1122u64.to_le_bytes()); // id
        p.extend_from_slice(&[0u8; 40]); // pts, dts, duration, offset, offset_end
        p.extend_from_slice(&0u32.to_le_bytes()); // flags
        p.push(MEMORY_DMABUF);
        p.push(1); // n_memory
        p.extend_from_slice(&1u16.to_le_bytes()); // n_meta
        p.extend_from_slice(&1_167_360u64.to_le_bytes()); // size
        p.extend_from_slice(&0u64.to_le_bytes()); // offset
        let body = hex(
            "0000000017000000000500005002000002000000000000000000000000e00b00000000000005000000050000000000001000000000000000000000000000000000000000",
        );
        let name = b"GstVideoMeta";
        let total = 8 + name.len() + 2 + body.len();
        p.extend_from_slice(&(total as u32).to_le_bytes());
        p.extend_from_slice(&(name.len() as u32).to_le_bytes());
        p.extend_from_slice(name);
        p.push(0);
        p.push(0); // version
        p.extend_from_slice(&body);
        p
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn a_venus_frame() {
        let l = parse_new_buffer(&sample()).unwrap();
        assert_eq!(l.id, 0x1122);
        assert!(l.dmabuf);
        assert_eq!(l.memories, vec![(0, 1_167_360)]);
        assert_eq!((l.format, l.width, l.height), (GST_FORMAT_NV12, 1280, 592));
        // The chroma plane after 608 padded rows.
        assert_eq!(l.planes, vec![(0, 1280), (1280 * 608, 1280)]);
    }

    #[test]
    fn no_meta_no_layout() {
        let mut p = sample();
        p[54] = 0; // n_meta
        assert_eq!(parse_new_buffer(&p), None);
        assert_eq!(parse_new_buffer(&[0u8; 20]), None);
    }

    #[test]
    fn padding_cropped() {
        // An iPad upright: 556 of a 560 frame.
        assert_eq!(visible(560, 800, Some(1112.0 / 1600.0)), (556, 800));
        assert_eq!(visible(1280, 592, Some(1280.0 / 590.0)), (1280, 590));
        assert_eq!(visible(368, 800, None), (368, 800));
        // A ratio from before a rotation: left alone.
        assert_eq!(visible(1280, 592, Some(368.0 / 800.0)), (1280, 592));
    }

    #[test]
    fn release() {
        let m = release_message(7);
        assert_eq!(&m[0..8], &[1, 0, 0, 0, 8, 0, 0, 0]);
        assert_eq!(u64::from_le_bytes(m[8..].try_into().unwrap()), 7);
    }
}
