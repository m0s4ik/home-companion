// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! evdev touch (non-blocking, read from the main loop's poll): multi-touch protocol B with
//! every slot, or the single-touch axes, mapped to view pixels with the body's rotation
//! (`crates/home-companion/src/ui/touch.rs`, `map`).

use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use webview_proto::{Touch, TouchKind};

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const SLOTS: usize = 10;

const fn ioc(dir: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
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
    // SAFETY: EVIOCGABS writes one input_absinfo.
    let r = unsafe { libc::ioctl(fd, ioc(2, 0x40 + code as u64, 24) as _, &mut info) };
    (r == 0 && info.maximum > info.minimum).then_some(info)
}

fn has_abs(fd: i32, code: u16) -> bool {
    let mut bits = [0u8; 8];
    // SAFETY: EVIOCGBIT(EV_ABS) writes at most 8 bytes.
    let r = unsafe { libc::ioctl(fd, ioc(2, 0x20 + EV_ABS as u64, 8) as _, bits.as_mut_ptr()) };
    r >= 0 && bits[(code / 8) as usize] & (1 << (code % 8)) != 0
}

/// The first `/dev/input/event*` with multi-touch axes.
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

/// Normalized panel coordinates (0..1) → view pixels, for a picture turned by `rotation`
/// degrees clockwise onto the panel. Same as the body's `ui::touch::map`.
pub fn map(u: f32, v: f32, rotation: u32, w: f32, h: f32) -> (f32, f32) {
    let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    match rotation {
        90 => (v * w, (1.0 - u) * h),
        180 => ((1.0 - u) * w, (1.0 - v) * h),
        270 => ((1.0 - v) * w, u * h),
        _ => (u * w, v * h),
    }
}

#[derive(Clone, Copy, Default)]
struct Slot {
    /// evdev tracking id, -1 = up.
    id: i32,
    was_down: bool,
    x: i32,
    y: i32,
    moved: bool,
}

pub struct TouchReader {
    file: std::fs::File,
    mt: bool,
    ix: AbsInfo,
    iy: AbsInfo,
    slots: [Slot; SLOTS],
    slot: usize,
    /// Single-touch: BTN_TOUCH.
    pressed: bool,
    rotation: u32,
    /// View size (from the helper's hello).
    pub view: (f32, f32),
    dropped: bool,
}

impl TouchReader {
    pub fn open(path: &Path, rotation: u32) -> Result<TouchReader, String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
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
        // Event times on CLOCK_MONOTONIC, the protocol's clock (EVIOCSCLOCKID).
        let clock: libc::c_int = libc::CLOCK_MONOTONIC;
        // SAFETY: EVIOCSCLOCKID reads one int.
        unsafe { libc::ioctl(fd, ioc(1, 0xa0, 4) as _, &clock) };
        eprintln!(
            "touch: {} ({}), x {}..{}, y {}..{}, rotation {rotation}",
            path.display(),
            if mt { "multi-touch" } else { "single-touch" },
            ix.minimum,
            ix.maximum,
            iy.minimum,
            iy.maximum
        );
        Ok(TouchReader {
            file,
            mt,
            ix,
            iy,
            slots: [Slot {
                id: -1,
                ..Slot::default()
            }; SLOTS],
            slot: 0,
            pressed: false,
            rotation,
            view: (1280.0, 800.0),
            dropped: false,
        })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }

    fn to_view(&self, x: i32, y: i32) -> (f32, f32) {
        let u = (x - self.ix.minimum) as f32 / (self.ix.maximum - self.ix.minimum) as f32;
        let v = (y - self.iy.minimum) as f32 / (self.iy.maximum - self.iy.minimum) as f32;
        map(u, v, self.rotation, self.view.0, self.view.1)
    }

    /// Reads what is available; returns the touches completed by SYN_REPORTs.
    pub fn read(&mut self) -> Vec<Touch> {
        let mut out = Vec::new();
        let mut buf = [0u8; 24 * 64];
        loop {
            let n = match self.file.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for ev in buf[..n].chunks_exact(24) {
                let sec = i64::from_ne_bytes(ev[0..8].try_into().unwrap());
                let usec = i64::from_ne_bytes(ev[8..16].try_into().unwrap());
                let kind = u16::from_ne_bytes([ev[16], ev[17]]);
                let code = u16::from_ne_bytes([ev[18], ev[19]]);
                let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                let time_ns = sec as u64 * 1_000_000_000 + usec as u64 * 1000;
                self.event(kind, code, value, time_ns, &mut out);
            }
        }
        out
    }

    fn event(&mut self, kind: u16, code: u16, value: i32, time_ns: u64, out: &mut Vec<Touch>) {
        let s = self.slot;
        match (kind, code) {
            (EV_SYN, SYN_DROPPED) => self.dropped = true,
            (EV_SYN, SYN_REPORT) if self.dropped => {
                // Lost events: cancel every active point and start over.
                self.dropped = false;
                for (i, sl) in self.slots.iter_mut().enumerate() {
                    if sl.was_down {
                        sl.was_down = false;
                        sl.id = -1;
                        out.push(Touch {
                            kind: TouchKind::Cancel,
                            id: i as i32,
                            x: 0.0,
                            y: 0.0,
                            time_ns,
                        });
                    }
                }
            }
            (EV_SYN, SYN_REPORT) => {
                for i in 0..SLOTS {
                    let sl = self.slots[i];
                    let down = if self.mt { sl.id >= 0 } else { self.pressed };
                    let k = match (sl.was_down, down) {
                        (false, true) => Some(TouchKind::Down),
                        (true, false) => Some(TouchKind::Up),
                        (true, true) if sl.moved => Some(TouchKind::Motion),
                        _ => None,
                    };
                    self.slots[i].was_down = down;
                    self.slots[i].moved = false;
                    if let Some(kind) = k {
                        let (x, y) = self.to_view(sl.x, sl.y);
                        out.push(Touch {
                            kind,
                            id: i as i32,
                            x,
                            y,
                            time_ns,
                        });
                    }
                    if !self.mt {
                        break;
                    }
                }
            }
            (EV_ABS, ABS_MT_SLOT) => self.slot = (value.max(0) as usize).min(SLOTS - 1),
            (EV_ABS, ABS_MT_TRACKING_ID) => self.slots[s].id = value,
            (EV_ABS, ABS_MT_POSITION_X) if self.mt => {
                self.slots[s].x = value;
                self.slots[s].moved = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) if self.mt => {
                self.slots[s].y = value;
                self.slots[s].moved = true;
            }
            (EV_ABS, ABS_X) if !self.mt => {
                self.slots[0].x = value;
                self.slots[0].moved = true;
            }
            (EV_ABS, ABS_Y) if !self.mt => {
                self.slots[0].y = value;
                self.slots[0].moved = true;
            }
            (EV_KEY, BTN_TOUCH) => self.pressed = value != 0,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_270_like_the_body() {
        // Panel raw (x 0..799, y 0..1279): the panel's bottom-left corner is the view's
        // top-left (the device stands landscape, panel turned 270 degrees).
        assert_eq!(map(0.0, 1.0, 270, 1280.0, 800.0), (0.0, 0.0));
        assert_eq!(map(1.0, 1.0, 270, 1280.0, 800.0), (0.0, 800.0));
        assert_eq!(map(0.0, 0.0, 270, 1280.0, 800.0), (1280.0, 0.0));
    }
}
