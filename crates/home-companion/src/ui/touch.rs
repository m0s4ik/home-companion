// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The touch screen without libinput: a thread reads the evdev device (multi-touch slot 0 or
//! the single-touch axes), turns it into press/move/release in the landscape screen's
//! coordinates (the same rotation as the picture) and the UI thread hands them to Slint with
//! `Window::dispatch_event`. Works with every renderer (our fbdev platform has no input of
//! its own, and Slint's linuxkms backend is built without libinput: the device image has no
//! libinput).

use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchKind {
    Down,
    Move,
    Up,
}

/// A touch in the landscape screen's logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchEvent {
    pub kind: TouchKind,
    pub x: f32,
    pub y: f32,
}

const fn ioc_read(nr: u64, size: u64) -> u64 {
    (2 << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

fn abs_info(fd: i32, code: u16) -> Option<AbsInfo> {
    let mut info = AbsInfo::default();
    let req = ioc_read(0x40 + code as u64, std::mem::size_of::<AbsInfo>() as u64);
    // SAFETY: EVIOCGABS writes one input_absinfo into the struct passed.
    let r = unsafe { libc::ioctl(fd, req as _, &mut info) };
    (r == 0 && info.maximum > info.minimum).then_some(info)
}

fn has_abs(fd: i32, code: u16) -> bool {
    let mut bits = [0u8; 8];
    let req = ioc_read(0x20 + EV_ABS as u64, bits.len() as u64);
    // SAFETY: EVIOCGBIT writes at most `bits.len()` bytes.
    let r = unsafe { libc::ioctl(fd, req as _, bits.as_mut_ptr()) };
    r >= 0 && bits[(code / 8) as usize] & (1 << (code % 8)) != 0
}

/// The first `/dev/input/event*` with multi-touch position axes.
pub fn find_device() -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir("/dev/input")
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("event"))
        })
        .collect();
    v.sort();
    v.into_iter()
        .find(|p| std::fs::File::open(p).is_ok_and(|f| has_abs(f.as_raw_fd(), ABS_MT_POSITION_X)))
}

/// Normalized panel coordinates (0..1) → landscape logical pixels, for a picture turned by
/// `rotation` degrees clockwise onto the panel.
pub fn map(u: f32, v: f32, rotation: u32, w: f32, h: f32) -> (f32, f32) {
    let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    match rotation {
        90 => (v * w, (1.0 - u) * h),
        180 => ((1.0 - u) * w, (1.0 - v) * h),
        270 => ((1.0 - v) * w, u * h),
        _ => (u * w, v * h),
    }
}

/// Starts the reader thread. Returns the device used.
pub fn spawn(
    path: &Path,
    rotation: u32,
    tx: Sender<TouchEvent>,
    wake: impl Fn() + Send + 'static,
) -> Result<PathBuf, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fd = file.as_raw_fd();
    let mt = has_abs(fd, ABS_MT_POSITION_X);
    let (cx, cy) = if mt {
        (ABS_MT_POSITION_X, ABS_MT_POSITION_Y)
    } else {
        (ABS_X, ABS_Y)
    };
    let (ix, iy) = match (abs_info(fd, cx), abs_info(fd, cy)) {
        (Some(x), Some(y)) => (x, y),
        _ => return Err(format!("{}: no absolute position axes", path.display())),
    };
    log::info!(
        "touch: {} ({}), x {}..{}, y {}..{}, rotation {rotation}",
        path.display(),
        if mt { "multi-touch" } else { "single-touch" },
        ix.minimum,
        ix.maximum,
        iy.minimum,
        iy.maximum
    );
    let p = path.to_path_buf();
    std::thread::Builder::new()
        .name("touch".into())
        .spawn(move || {
            let (mut x, mut y) = (ix.value, iy.value);
            let (mut down, mut was_down, mut slot) = (false, false, 0);
            let mut moved = false;
            let mut buf = [0u8; 24 * 64];
            loop {
                let n = match file.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for ev in buf[..n].chunks_exact(24) {
                    let kind = u16::from_ne_bytes([ev[16], ev[17]]);
                    let code = u16::from_ne_bytes([ev[18], ev[19]]);
                    let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                    match (kind, code) {
                        (EV_ABS, ABS_MT_SLOT) => slot = value,
                        (EV_ABS, c) if c == cx && slot == 0 => {
                            x = value;
                            moved = true;
                        }
                        (EV_ABS, c) if c == cy && slot == 0 => {
                            y = value;
                            moved = true;
                        }
                        (EV_ABS, ABS_MT_TRACKING_ID) if slot == 0 => down = value >= 0,
                        (EV_KEY, BTN_TOUCH) => down = value != 0,
                        (EV_SYN, SYN_REPORT) => {
                            let u = (x - ix.minimum) as f32 / (ix.maximum - ix.minimum) as f32;
                            let v = (y - iy.minimum) as f32 / (iy.maximum - iy.minimum) as f32;
                            let (lx, ly) = map(u, v, rotation, 1280.0, 800.0);
                            let kind = match (was_down, down) {
                                (false, true) => Some(TouchKind::Down),
                                (true, false) => Some(TouchKind::Up),
                                (true, true) if moved => Some(TouchKind::Move),
                                _ => None,
                            };
                            was_down = down;
                            moved = false;
                            if let Some(kind) = kind {
                                if tx.send(TouchEvent { kind, x: lx, y: ly }).is_err() {
                                    return;
                                }
                                wake();
                            }
                        }
                        _ => {}
                    }
                }
            }
            log::warn!("touch: {} closed", p.display());
        })
        .map_err(|e| format!("touch thread: {e}"))?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotations_map_the_corners() {
        // The panel's top-left corner, turned 90 degrees clockwise, is the screen's bottom-left.
        assert_eq!(map(0.0, 0.0, 90, 1280.0, 800.0), (0.0, 800.0));
        assert_eq!(map(1.0, 0.0, 90, 1280.0, 800.0), (0.0, 0.0));
        assert_eq!(map(0.0, 1.0, 270, 1280.0, 800.0), (0.0, 0.0));
        assert_eq!(map(0.5, 0.5, 180, 1280.0, 800.0), (640.0, 400.0));
        assert_eq!(map(0.25, 0.75, 0, 1280.0, 800.0), (320.0, 600.0));
    }

    #[test]
    fn ioctl_numbers() {
        // EVIOCGABS(ABS_MT_POSITION_X) and EVIOCGBIT(EV_ABS, 8) as in <linux/input.h>.
        assert_eq!(ioc_read(0x40 + 0x35, 24), 0x80184575);
        assert_eq!(ioc_read(0x20 + 3, 8), 0x80084523);
    }
}
