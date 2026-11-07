// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A minimal Slint platform: the software renderer draws into BGRX pixels and
//! only the dirty region reaches the screen. Targets:
//! - `/dev/fb0` (or a plain file with `--fb-geometry`) mapped with mmap, drawn
//!   either directly (`--render-into fb`) or through a RAM back buffer whose
//!   dirty rows are copied (`--render-into ram`);
//! - a RAM buffer only (`--backend headless`), used for screenshots and CI.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use memmap2::{MmapMut, MmapOptions};
use slint::PhysicalSize;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};

use crate::args::{Args, Geometry, RenderInto};
use crate::pixel::{Bgrx, as_pixels};
use crate::scene::Scene;
use crate::shot;
use crate::stats::{Class, Dirty};

// ---- Linux fbdev and console ioctls -------------------------------------

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;
const KDSETMODE: libc::c_ulong = 0x4B3A;
const KDGETMODE: libc::c_ulong = 0x4B3B;
const KD_TEXT: libc::c_int = 0;
const KD_GRAPHICS: libc::c_int = 1;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Default)]
struct VarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    rest: [u32; 20], // nonstd .. reserved: not used here
}

#[repr(C)]
struct FixScreenInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

/// The mapped framebuffer (device or plain file).
pub struct Framebuffer {
    map: MmapMut,
    offset: usize,
    pub geometry: Geometry,
    pub info: String,
    _file: File,
}

impl Framebuffer {
    pub fn open(path: &Path, fallback: Option<Geometry>) -> Result<Framebuffer, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("cannot open framebuffer {}: {e}", path.display()))?;
        let fd = file.as_raw_fd();
        let mut var = VarScreenInfo::default();
        // SAFETY: FixScreenInfo is plain old data; the kernel fills it.
        let mut fix: FixScreenInfo = unsafe { std::mem::zeroed() };
        // SAFETY: both ioctls only write into the structs passed.
        let ok = unsafe {
            libc::ioctl(fd, FBIOGET_VSCREENINFO as _, &mut var) == 0
                && libc::ioctl(fd, FBIOGET_FSCREENINFO as _, &mut fix) == 0
        };
        let (geometry, offset, len, info) = if ok {
            if var.bits_per_pixel != 32 || var.red.offset != 16 || var.green.offset != 8 || var.blue.offset != 0 {
                return Err(format!(
                    "{}: unsupported pixel format: {} bpp, red at {}, green at {}, blue at {} (need 32 bpp BGRX)",
                    path.display(),
                    var.bits_per_pixel,
                    var.red.offset,
                    var.green.offset,
                    var.blue.offset
                ));
            }
            let id = String::from_utf8_lossy(&fix.id).trim_end_matches('\0').to_string();
            let g = Geometry { xres: var.xres, yres: var.yres, line_length: fix.line_length };
            let offset = (var.yoffset * fix.line_length + var.xoffset * 4) as usize;
            let info = format!(
                "fbdev '{id}' {}x{} (virtual {}x{}, offset {},{}) {} bpp, line {} bytes, smem {} bytes",
                var.xres,
                var.yres,
                var.xres_virtual,
                var.yres_virtual,
                var.xoffset,
                var.yoffset,
                var.bits_per_pixel,
                fix.line_length,
                fix.smem_len
            );
            (g, offset, fix.smem_len as usize, info)
        } else {
            let err = std::io::Error::last_os_error();
            let g = fallback.ok_or_else(|| {
                format!(
                    "{}: FBIOGET_VSCREENINFO failed ({err}); for a plain file pass --fb-geometry WxHxLINE",
                    path.display()
                )
            })?;
            let info = format!("plain file, geometry from --fb-geometry {}x{}x{}", g.xres, g.yres, g.line_length);
            (g, 0, (g.line_length * g.yres) as usize, info)
        };
        let need = offset + (geometry.line_length * geometry.yres) as usize;
        let len = len.max(need);
        if !ok {
            let size = file.metadata().map(|m| m.len()).unwrap_or(0) as usize;
            if size < need {
                return Err(format!("{}: file is {size} bytes, geometry needs {need}", path.display()));
            }
        }
        // SAFETY: the mapping is shared with the device/file; we are its only writer.
        let map = unsafe { MmapOptions::new().len(len).map_mut(&file) }
            .map_err(|e| format!("cannot mmap {} ({len} bytes): {e}", path.display()))?;
        Ok(Framebuffer { map, offset, geometry, info, _file: file })
    }

    pub fn stride_px(&self) -> usize {
        self.geometry.line_length as usize / 4
    }

    pub fn pixels(&mut self) -> &mut [Bgrx] {
        let n = (self.geometry.line_length * self.geometry.yres) as usize;
        as_pixels(&mut self.map[self.offset..self.offset + n])
    }

    pub fn flush(&self) {
        self.map.flush().ok();
    }
}

/// Switches the text console to graphics mode (no cursor, no console output
/// over the UI) and restores the previous mode on drop.
pub struct VtGuard {
    fd: libc::c_int,
    old: libc::c_int,
}

impl VtGuard {
    pub fn enter() -> Result<VtGuard, String> {
        let mut errors = Vec::new();
        for path in ["/dev/tty0", "/dev/tty"] {
            let c = std::ffi::CString::new(path).unwrap();
            // SAFETY: plain open/ioctl/close on a path we own the string of.
            unsafe {
                let fd = libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
                if fd < 0 {
                    errors.push(format!("{path}: {}", std::io::Error::last_os_error()));
                    continue;
                }
                let mut old: libc::c_int = 0;
                if libc::ioctl(fd, KDGETMODE as _, &mut old) == 0 && libc::ioctl(fd, KDSETMODE as _, KD_GRAPHICS) == 0 {
                    return Ok(VtGuard { fd, old });
                }
                errors.push(format!("{path}: KDSETMODE: {}", std::io::Error::last_os_error()));
                libc::close(fd);
            }
        }
        Err(errors.join("; "))
    }
}

impl Drop for VtGuard {
    fn drop(&mut self) {
        // SAFETY: fd is ours and still open.
        unsafe {
            libc::ioctl(self.fd, KDSETMODE as _, self.old);
            libc::close(self.fd);
        }
    }
}

/// Sets the active VT back to text mode: the way out when a run (ours killed
/// with SIGKILL, or Slint's own linuxfb path) left the console in graphics mode.
pub fn reset_console() -> Result<(), String> {
    let c = std::ffi::CString::new("/dev/tty0").unwrap();
    // SAFETY: plain open/ioctl/close on a path we own the string of.
    unsafe {
        let fd = libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(format!("/dev/tty0: {}", std::io::Error::last_os_error()));
        }
        let ok = libc::ioctl(fd, KDSETMODE as _, KD_TEXT) == 0;
        let err = std::io::Error::last_os_error();
        libc::close(fd);
        if !ok {
            return Err(format!("/dev/tty0: KDSETMODE KD_TEXT: {err}"));
        }
    }
    Ok(())
}

// ---- signals ---------------------------------------------------------------

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// SIGINT, SIGTERM and SIGHUP only set a flag; every backend polls it (our
/// event loop here, the scene timer for Slint's own backends) and leaves its
/// event loop normally, so console modes and other guards are restored by
/// their destructors (ours and Slint's linuxfb `KDSETMODE` alike).
pub fn install_signal_handlers() {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// True once one of the signals above arrived.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

// ---- platform ----------------------------------------------------------------

struct FbPlatform {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for FbPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Installs the platform; must run before the first component is created.
pub fn install(start: Instant) -> Result<Rc<MinimalSoftwareWindow>, String> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(FbPlatform { window: window.clone(), start }))
        .map_err(|e| format!("cannot install the fbdev platform: {e}"))?;
    Ok(window)
}

/// Where the renderer draws and how the result is presented.
pub struct Output {
    pub fb: Option<Framebuffer>,
    pub mode: RenderInto,
    back: Vec<Bgrx>,
    pub width: u32,
    pub height: u32,
}

impl Output {
    pub fn new(fb: Option<Framebuffer>, mode: RenderInto) -> Output {
        let (width, height) = fb.as_ref().map_or((800, 1280), |f| (f.geometry.xres, f.geometry.yres));
        let mode = if fb.is_none() { RenderInto::Ram } else { mode };
        let back = if mode == RenderInto::Ram {
            vec![Bgrx::BLACK; (width * height) as usize]
        } else {
            Vec::new()
        };
        Output { fb, mode, back, width, height }
    }

    /// The buffer the renderer draws into, with its stride in pixels.
    fn target(&mut self) -> (&mut [Bgrx], usize) {
        match (self.mode, self.fb.as_mut()) {
            (RenderInto::Fb, Some(fb)) => {
                let stride = fb.stride_px();
                (fb.pixels(), stride)
            }
            _ => (&mut self.back, self.width as usize),
        }
    }

    /// Copies the dirty bounding box from the back buffer to the framebuffer.
    fn present(&mut self, dirty: &Dirty) {
        let (RenderInto::Ram, Some(fb)) = (self.mode, self.fb.as_mut()) else { return };
        let bb = dirty.bbox();
        let stride = fb.stride_px();
        let dst = fb.pixels();
        let w = self.width as usize;
        for y in bb.y as usize..(bb.y + bb.h) as usize {
            let (x0, x1) = (bb.x as usize, (bb.x + bb.w) as usize);
            dst[y * stride + x0..y * stride + x1].copy_from_slice(&self.back[y * w + x0..y * w + x1]);
        }
    }

    pub fn screenshot(&mut self, path: &Path) -> Result<(), String> {
        let (w, h) = (self.width, self.height);
        let (px, stride) = self.target();
        shot::write_bgrx(path, w, h, stride, px)
    }
}

/// The event loop: timers, then draw if needed, then sleep until the next
/// timer (or one tick while a Slint animation runs).
pub fn run(window: &MinimalSoftwareWindow, out: &mut Output, scene: &Scene, args: &Args) -> Result<(), String> {
    let tick = Duration::from_secs_f64(1.0 / args.fps);
    let stats = Rc::clone(&scene.stats);
    let out = RefCell::new(out);
    let mut first = true;
    loop {
        if stop_requested() {
            eprintln!("spike-ui: signal received, exiting");
            break;
        }
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer| {
            let mut out = out.borrow_mut();
            let (w, h) = (out.width, out.height);
            let t0 = Instant::now();
            let region = {
                let (buf, stride) = out.target();
                renderer.render(buf, stride)
            };
            let t1 = Instant::now();
            let dirty = Dirty::from_region(&region).clipped(w, h);
            out.present(&dirty);
            let t2 = Instant::now();
            let class = Class::from_changes(scene.changes.replace(0), first);
            first = false;
            stats.borrow_mut().frame(
                class,
                (t1 - t0).as_secs_f64() * 1e3,
                (t2 - t1).as_secs_f64() * 1e3,
                dirty,
            );
        });
        if scene.quit.get() || args.frames.is_some_and(|n| stats.borrow().frame_count() >= n) {
            break;
        }
        let wait = if window.has_active_animations() {
            tick
        } else {
            slint::platform::duration_until_next_timer_update().unwrap_or(tick).min(tick)
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
    if let Some(fb) = out.borrow().fb.as_ref() {
        fb.flush();
    }
    Ok(())
}

pub fn set_size(window: &MinimalSoftwareWindow, out: &Output) {
    window.set_size(PhysicalSize::new(out.width, out.height));
}
