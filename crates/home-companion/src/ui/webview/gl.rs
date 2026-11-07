// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The helper's frames as GL textures inside Slint's own GL context (GPU renderer only).
//!
//! Slint 1.18's linuxkms + femtovg backend has one EGL context, made current on the UI thread
//! when the window is created and never released (`ensure_current` only re-makes it current
//! if something else took it). So the UI thread can import a dma-buf with
//! `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` on the current display and bind it to a
//! `GL_TEXTURE_2D` of the current context (no copy), and Slint draws it as a
//! `BorrowedOpenGLTexture`. A rendering notifier would be the documented way, but in Slint
//! 1.18 it breaks the rotated output (renderer.rs); the current context is checked before
//! every import instead.
//!
//! The EGL/GLES code is the test viewer's (crates/webview-viewer/src/egl.rs, shared by path).

#[path = "../../../../webview-viewer/src/egl.rs"]
pub(crate) mod egl;

use std::ffi::{CStr, c_void};
use std::os::fd::{AsRawFd, OwnedFd};

use egl::{Api, EGLDisplay, EGLImage, GLuint};
use webview_proto::Frame;

/// A frame's dma-buf imported once: the EGLImage and the texture on it.
pub struct Imported {
    image: EGLImage,
    pub tex: GLuint,
    pub width: u32,
    pub height: u32,
}

type GetCurrent = unsafe extern "C" fn() -> *mut c_void;

pub struct Importer {
    api: Api,
    get_display: GetCurrent,
    get_context: GetCurrent,
    /// The display and context the textures belong to.
    dpy: EGLDisplay,
    ctx: *mut c_void,
}

fn sym(lib: &CStr, name: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: dlopen of a library already loaded by Slint (same handle), dlsym by name.
    unsafe {
        let h = libc::dlopen(lib.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
        let h = if h.is_null() {
            libc::dlopen(lib.as_ptr(), libc::RTLD_NOW)
        } else {
            h
        };
        if h.is_null() {
            return Err(format!("{} not loaded", lib.to_string_lossy()));
        }
        let p = libc::dlsym(h, name.as_ptr());
        if p.is_null() {
            return Err(format!("{} missing", name.to_string_lossy()));
        }
        Ok(p)
    }
}

impl Importer {
    /// Needs Slint's context current on this thread (after the window was created).
    pub fn new() -> Result<Importer, String> {
        let api = Api::load()?;
        // SAFETY: the EGL 1.4 entry points with these signatures.
        let (get_display, get_context) = unsafe {
            (
                std::mem::transmute::<*mut c_void, GetCurrent>(sym(
                    c"libEGL.so.1",
                    c"eglGetCurrentDisplay",
                )?),
                std::mem::transmute::<*mut c_void, GetCurrent>(sym(
                    c"libEGL.so.1",
                    c"eglGetCurrentContext",
                )?),
            )
        };
        // SAFETY: plain queries of the calling thread's EGL state.
        let (dpy, ctx) = unsafe { (get_display(), get_context()) };
        if dpy.is_null() || ctx.is_null() {
            return Err("no current EGL context on the UI thread (not the GPU renderer?)".into());
        }
        log::info!(
            "web view: importing frames into the UI's GL context ({})",
            api.gl_string(egl::GL_RENDERER)
        );
        Ok(Importer {
            api,
            get_display,
            get_context,
            dpy,
            ctx,
        })
    }

    /// Slint's context is the current one (it always is on the UI thread; checked anyway).
    fn current(&self) -> bool {
        // SAFETY: plain queries of the calling thread's EGL state.
        unsafe { (self.get_display)() == self.dpy && (self.get_context)() == self.ctx }
    }

    /// Imports a frame's planes (zero copy). The fds may be closed afterwards.
    pub fn import(&self, f: &Frame, fds: &[OwnedFd]) -> Result<Imported, String> {
        if !self.current() {
            return Err("the UI's GL context is not current".into());
        }
        let planes: Vec<egl::PlaneFd> = f
            .planes
            .iter()
            .zip(fds)
            .map(|(p, fd)| egl::PlaneFd {
                fd: fd.as_raw_fd(),
                offset: p.offset,
                stride: p.stride,
            })
            .collect();
        let image = egl::import_dmabuf(
            &self.api, self.dpy, f.width, f.height, f.fourcc, f.modifier, &planes,
        )?;
        match egl::texture_from_image(&self.api, image) {
            Ok(tex) => {
                // SAFETY: GL call on the current context; femtovg binds its own textures per draw.
                unsafe { (self.api.gl.glBindTexture)(egl::GL_TEXTURE_2D, 0) };
                Ok(Imported {
                    image,
                    tex,
                    width: f.width,
                    height: f.height,
                })
            }
            Err(e) => {
                // SAFETY: the image we just created.
                unsafe { (self.api.ext.eglDestroyImageKHR)(self.dpy, image) };
                Err(e)
            }
        }
    }

    /// Frees a texture and its image (nothing may draw it any more).
    pub fn forget(&self, t: Imported) {
        if !self.current() {
            log::warn!("web view: GL context not current, texture {} leaked", t.tex);
            return;
        }
        // SAFETY: our texture and image; GL keeps the storage until pending draws finish.
        unsafe {
            (self.api.gl.glDeleteTextures)(1, &t.tex);
            (self.api.ext.eglDestroyImageKHR)(self.dpy, t.image);
        }
    }

    /// The texture as a Slint image (origin top left: row 0 of the dma-buf is the top).
    pub fn image(t: &Imported) -> slint::Image {
        let Some(id) = std::num::NonZeroU32::new(t.tex) else {
            return slint::Image::default();
        };
        // SAFETY: a GL_TEXTURE_2D of the context Slint renders with, alive until `forget`,
        // which the caller only does after the image is no longer shown.
        unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                id,
                (t.width, t.height).into(),
            )
        }
        .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
        .build()
    }

    pub fn fourcc_name(f: u32) -> String {
        egl::fourcc_name(f)
    }
}
