// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The panel through KMS: DRM master on the card, a GBM surface as EGL window, legacy
//! modeset for the first frame and page flips with events after it.

use std::collections::HashMap;
use std::os::fd::{AsFd, BorrowedFd};

use drm::control::{self, Device as _, PageFlipFlags, connector, crtc, framebuffer};
use gbm::{AsRaw, BufferObject, BufferObjectFlags, Format};

use crate::egl::{Api, Display, EGLSurface};

pub struct Card(std::fs::File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl control::Device for Card {}

pub struct Output {
    pub gbm: gbm::Device<Card>,
    surface: gbm::Surface<()>,
    pub display: Display,
    pub egl_surface: EGLSurface,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: control::Mode,
    /// Panel size in pixels (portrait on the ThinkSmart View: 800x1280).
    pub width: u32,
    pub height: u32,
    /// Scanned out now.
    front: Option<BufferObject<()>>,
    /// Waiting for its flip event.
    next: Option<BufferObject<()>>,
    fbs: HashMap<u32, framebuffer::Handle>,
    modeset_done: bool,
}

impl Output {
    pub fn open(api: &Api, path: &str) -> Result<Output, String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("{path}: {e}"))?;
        let card = Card(file);
        drm::Device::acquire_master_lock(&card)
            .map_err(|e| format!("{path}: DRM master: {e} (is home-companion running?)"))?;
        let res = card
            .resource_handles()
            .map_err(|e| format!("resources: {e}"))?;
        let (conn, info) = res
            .connectors()
            .iter()
            .filter_map(|&c| card.get_connector(c, false).ok().map(|i| (c, i)))
            .find(|(_, i)| i.state() == connector::State::Connected && !i.modes().is_empty())
            .ok_or("no connected connector")?;
        let mode = info.modes()[0];
        let crtc = info
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| {
                let enc = card.get_encoder(*info.encoders().first()?).ok()?;
                res.filter_crtcs(enc.possible_crtcs()).first().copied()
            })
            .ok_or("no CRTC for the connector")?;
        let (w, h) = mode.size();
        let (width, height) = (w as u32, h as u32);
        let gbm = gbm::Device::new(card).map_err(|e| format!("gbm: {e}"))?;
        let surface = gbm
            .create_surface::<()>(
                width,
                height,
                Format::Xrgb8888,
                BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING,
            )
            .map_err(|e| format!("gbm surface: {e}"))?;
        let display = Display::new(api, gbm.as_raw() as *mut _, Some(Format::Xrgb8888 as u32))?;
        // SAFETY: the gbm surface outlives the EGL surface (both owned by Output, the EGL
        // surface is destroyed first in Drop).
        let egl_surface = unsafe {
            (api.egl.eglCreateWindowSurface)(
                display.dpy,
                display.config,
                surface.as_raw() as *mut _,
                std::ptr::null(),
            )
        };
        if egl_surface.is_null() {
            return Err(format!("eglCreateWindowSurface: {}", api.error()));
        }
        // SAFETY: valid display, surface and context.
        if unsafe { (api.egl.eglMakeCurrent)(display.dpy, egl_surface, egl_surface, display.ctx) }
            == 0
        {
            return Err(format!("eglMakeCurrent: {}", api.error()));
        }
        eprintln!(
            "output: {path} {}x{}@{} connector {:?} crtc {:?}",
            width,
            height,
            mode.vrefresh(),
            conn,
            crtc
        );
        Ok(Output {
            gbm,
            surface,
            display,
            egl_surface,
            crtc,
            connector: conn,
            mode,
            width,
            height,
            front: None,
            next: None,
            fbs: HashMap::new(),
            modeset_done: false,
        })
    }

    pub fn flip_pending(&self) -> bool {
        self.next.is_some()
    }

    /// After drawing: swap, then modeset (first time, returns true = already on screen) or queue
    /// a flip (false = wait for the event).
    pub fn present(&mut self, api: &Api) -> Result<bool, String> {
        // SAFETY: current display and surface.
        if unsafe { (api.egl.eglSwapBuffers)(self.display.dpy, self.egl_surface) } == 0 {
            return Err(format!("eglSwapBuffers: {}", api.error()));
        }
        // SAFETY: called exactly once after eglSwapBuffers.
        let bo =
            unsafe { self.surface.lock_front_buffer() }.map_err(|_| "lock_front_buffer failed")?;
        // SAFETY: reading the handle union as u32 (GEM handle).
        let key = unsafe { bo.handle().u32_ };
        let fb = match self.fbs.get(&key) {
            Some(fb) => *fb,
            None => {
                let fb = self
                    .gbm
                    .add_framebuffer(&bo, 24, 32)
                    .map_err(|e| format!("add_framebuffer: {e}"))?;
                self.fbs.insert(key, fb);
                fb
            }
        };
        if !self.modeset_done {
            self.gbm
                .set_crtc(
                    self.crtc,
                    Some(fb),
                    (0, 0),
                    &[self.connector],
                    Some(self.mode),
                )
                .map_err(|e| format!("set_crtc: {e}"))?;
            self.modeset_done = true;
            self.front = Some(bo);
            return Ok(true);
        }
        self.gbm
            .page_flip(self.crtc, fb, PageFlipFlags::EVENT, None)
            .map_err(|e| format!("page_flip: {e}"))?;
        self.next = Some(bo);
        Ok(false)
    }

    /// Reads the DRM events; returns the flip time (CLOCK_MONOTONIC ns) if our flip completed.
    pub fn dispatch(&mut self) -> Option<u64> {
        let mut done = None;
        if let Ok(events) = self.gbm.receive_events() {
            for ev in events {
                if let control::Event::PageFlip(f) = ev {
                    done = Some(f.duration.as_nanos() as u64);
                }
            }
        }
        if done.is_some() {
            // The old front buffer goes back to the surface.
            self.front = self.next.take();
        }
        done
    }

    /// Reads pixels (x, y) of the buffer on screen as (r, g, b): what the panel scans out,
    /// row 0 at the top.
    pub fn read_front(&self, points: &[(u32, u32)]) -> Result<Vec<(u8, u8, u8)>, String> {
        let bo = self.front.as_ref().ok_or("nothing on screen")?;
        bo.map(0, 0, self.width, self.height, |m| {
            let s = m.stride() as usize;
            let b = m.buffer();
            points
                .iter()
                .map(|&(x, y)| {
                    let i = y as usize * s + x as usize * 4;
                    (b[i + 2], b[i + 1], b[i]) // XRGB8888 little-endian: B G R X
                })
                .collect()
        })
        .map_err(|e| format!("gbm_bo_map: {e}"))
    }

    /// Maps the whole buffer on screen (XRGB8888: B G R X per pixel) and hands it with its
    /// stride to `f`.
    pub fn read_front_all<T>(&self, f: impl FnOnce(&[u8], usize) -> T) -> Result<T, String> {
        let bo = self.front.as_ref().ok_or("nothing on screen")?;
        bo.map(0, 0, self.width, self.height, |m| {
            f(m.buffer(), m.stride() as usize)
        })
        .map_err(|e| format!("gbm_bo_map: {e}"))
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.gbm.as_fd()
    }

    pub fn destroy(&mut self, api: &Api) {
        // SAFETY: tearing down our own EGL objects once.
        unsafe {
            (api.egl.eglMakeCurrent)(
                self.display.dpy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            (api.egl.eglDestroySurface)(self.display.dpy, self.egl_surface);
            (api.egl.eglDestroyContext)(self.display.dpy, self.display.ctx);
            (api.egl.eglTerminate)(self.display.dpy);
        }
        for fb in self.fbs.values() {
            let _ = self.gbm.destroy_framebuffer(*fb);
        }
        self.front = None;
        self.next = None;
    }
}
