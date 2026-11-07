// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The software fallback: a minimal Slint platform whose software renderer draws into a RAM
//! back buffer; the dirty rows are copied to `/dev/fb0` (M0 decision: `RenderInto::Ram`).
//! With no framebuffer (`headless`) it renders into RAM only (CI, screenshots).
//!
//! Adapted from crates/spike-ui/src/platform_fb.rs (measured on the device in M0): the same
//! fbdev ioctls, BGRX pixel, console guard and event loop, plus the rotation of the landscape
//! screen onto the portrait panel.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use memmap2::{MmapMut, MmapOptions};
use slint::PhysicalSize;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RenderingRotation, RepaintBufferType,
    TargetPixel,
};
use slint::platform::{Platform, WindowAdapter};

// ---- the panel's pixel ----------------------------------------------------------------------

/// 32 bpp, bytes B, G, R, X in memory (little-endian XRGB8888, msm's fbdev emulation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Bgrx {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub x: u8,
}

impl Bgrx {
    pub const BLACK: Bgrx = Bgrx {
        b: 0,
        g: 0,
        r: 0,
        x: 0xff,
    };
}

#[inline]
fn over(dst: u8, src: u8, inv_alpha: u16) -> u8 {
    (dst as u16 * inv_alpha / 255) as u8 + src
}

impl TargetPixel for Bgrx {
    #[inline]
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let a = (u8::MAX - c.alpha) as u16;
        self.r = over(self.r, c.red, a);
        self.g = over(self.g, c.green, a);
        self.b = over(self.b, c.blue, a);
    }

    #[inline]
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { b, g, r, x: 0xff }
    }
}

fn as_pixels(bytes: &mut [u8]) -> &mut [Bgrx] {
    assert!(bytes.len() % 4 == 0 && bytes.as_ptr() as usize % std::mem::align_of::<Bgrx>() == 0);
    // SAFETY: Bgrx is repr(C), 4 x u8, alignment 1, every bit pattern valid.
    unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<Bgrx>(), bytes.len() / 4) }
}

// ---- Linux fbdev and console ioctls ---------------------------------------------------------

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
    rest: [u32; 20],
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

/// The mapped framebuffer.
pub struct Framebuffer {
    map: MmapMut,
    offset: usize,
    pub xres: u32,
    pub yres: u32,
    line_length: u32,
    pub info: String,
    _file: File,
}

impl Framebuffer {
    pub fn open(path: &Path) -> Result<Framebuffer, String> {
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
        if !ok {
            return Err(format!(
                "{}: FBIOGET_VSCREENINFO: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        if var.bits_per_pixel != 32
            || var.red.offset != 16
            || var.green.offset != 8
            || var.blue.offset != 0
        {
            return Err(format!(
                "{}: unsupported pixel format: {} bpp, red at {}, green at {}, blue at {} (need 32 bpp BGRX)",
                path.display(),
                var.bits_per_pixel,
                var.red.offset,
                var.green.offset,
                var.blue.offset
            ));
        }
        let id = String::from_utf8_lossy(&fix.id)
            .trim_end_matches('\0')
            .to_string();
        let offset = (var.yoffset * fix.line_length + var.xoffset * 4) as usize;
        let need = offset + (fix.line_length * var.yres) as usize;
        let len = (fix.smem_len as usize).max(need);
        let info = format!(
            "fbdev '{id}' {}x{} {} bpp, line {} bytes",
            var.xres, var.yres, var.bits_per_pixel, fix.line_length
        );
        // SAFETY: the mapping is shared with the device; we are its only writer.
        let map = unsafe { MmapOptions::new().len(len).map_mut(&file) }
            .map_err(|e| format!("cannot mmap {} ({len} bytes): {e}", path.display()))?;
        Ok(Framebuffer {
            map,
            offset,
            xres: var.xres,
            yres: var.yres,
            line_length: fix.line_length,
            info,
            _file: file,
        })
    }

    fn stride_px(&self) -> usize {
        self.line_length as usize / 4
    }

    fn pixels(&mut self) -> &mut [Bgrx] {
        let n = (self.line_length * self.yres) as usize;
        as_pixels(&mut self.map[self.offset..self.offset + n])
    }
}

/// Switches the text console to graphics mode (no cursor, no console output over the UI) and
/// restores the previous mode on drop.
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
                if libc::ioctl(fd, KDGETMODE as _, &mut old) == 0
                    && libc::ioctl(fd, KDSETMODE as _, KD_GRAPHICS) == 0
                {
                    return Ok(VtGuard { fd, old });
                }
                errors.push(format!(
                    "{path}: KDSETMODE: {}",
                    std::io::Error::last_os_error()
                ));
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

/// Sets the active VT back to text mode (`home-companion --reset-console`): the way out when a run killed
/// with SIGKILL left the console in graphics mode.
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

// ---- platform -------------------------------------------------------------------------------

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
pub fn install() -> Result<Rc<MinimalSoftwareWindow>, String> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(FbPlatform {
        window: window.clone(),
        start: Instant::now(),
    }))
    .map_err(|e| format!("cannot install the fbdev platform: {e}"))?;
    Ok(window)
}

/// The landscape screen's size.
pub const SCREEN_W: u32 = 1280;
pub const SCREEN_H: u32 = 800;

/// Where the renderer draws (a RAM back buffer) and where its dirty rows go (the framebuffer).
pub struct Output {
    fb: Option<Framebuffer>,
    back: Vec<Bgrx>,
    /// Buffer size (the panel's, rotated or not).
    width: u32,
    height: u32,
    pub rotation: u32,
}

impl Output {
    /// `rotation` in degrees clockwise (0, 90, 180, 270).
    pub fn new(fb: Option<Framebuffer>, rotation: u32) -> Output {
        let (width, height) = match &fb {
            Some(f) => (f.xres, f.yres),
            None if rotation % 180 == 90 => (SCREEN_H, SCREEN_W),
            None => (SCREEN_W, SCREEN_H),
        };
        Output {
            fb,
            back: vec![Bgrx::BLACK; (width * height) as usize],
            width,
            height,
            rotation,
        }
    }

    /// The window's (logical, landscape) size for this buffer.
    pub fn window_size(&self) -> PhysicalSize {
        if self.rotation % 180 == 90 {
            PhysicalSize::new(self.height, self.width)
        } else {
            PhysicalSize::new(self.width, self.height)
        }
    }

    pub fn rendering_rotation(&self) -> RenderingRotation {
        match self.rotation {
            90 => RenderingRotation::Rotate90,
            180 => RenderingRotation::Rotate180,
            270 => RenderingRotation::Rotate270,
            _ => RenderingRotation::NoRotation,
        }
    }

    /// Copies the dirty bounding box (buffer coordinates) to the framebuffer.
    fn present(&mut self, region: &slint::platform::software_renderer::PhysicalRegion) {
        let Some(fb) = self.fb.as_mut() else { return };
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for (p, s) in region.iter() {
            let (x, y) = (p.x.max(0) as u32, p.y.max(0) as u32);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + s.width);
            y1 = y1.max(y + s.height);
        }
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let (x1, y1) = (x1.min(self.width) as usize, y1.min(self.height) as usize);
        let (x0, y0) = (x0 as usize, y0 as usize);
        let stride = fb.stride_px();
        let w = self.width as usize;
        let dst = fb.pixels();
        for y in y0..y1 {
            dst[y * stride + x0..y * stride + x1]
                .copy_from_slice(&self.back[y * w + x0..y * w + x1]);
        }
    }
}

/// The event loop: timers, then draw if needed, then sleep until the next timer (or one frame
/// while an animation runs). Leaves when `quit` is set (by a Slint timer).
pub fn run(window: &MinimalSoftwareWindow, out: &mut Output, quit: &Rc<Cell<bool>>, fps: u32) {
    let tick = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let rotation = out.rendering_rotation();
    window.set_size(out.window_size());
    loop {
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|renderer| {
            renderer.set_rendering_rotation(rotation);
            let stride = out.width as usize;
            let region = renderer.render(&mut out.back, stride);
            out.present(&region);
        });
        if quit.get() {
            break;
        }
        let wait = if window.has_active_animations() {
            tick
        } else {
            slint::platform::duration_until_next_timer_update()
                .unwrap_or(tick)
                .min(tick)
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_red_over_white() {
        let mut p = Bgrx::from_rgb(255, 255, 255);
        p.blend(PremultipliedRgbaColor {
            red: 128,
            green: 0,
            blue: 0,
            alpha: 128,
        });
        assert_eq!((p.b, p.g, p.r, p.x), (127, 127, 255, 0xff));
    }

    #[test]
    fn rotated_sizes() {
        let o = Output::new(None, 90);
        assert_eq!(o.window_size(), PhysicalSize::new(SCREEN_W, SCREEN_H));
        assert_eq!((o.width, o.height), (SCREEN_H, SCREEN_W));
        let o = Output::new(None, 0);
        assert_eq!(o.window_size(), PhysicalSize::new(SCREEN_W, SCREEN_H));
    }
}
