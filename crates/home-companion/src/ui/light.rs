// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Automatic brightness from the ambient light sensor (Andrea, 2026-11-04: at night in a dark
//! room the screen was far too bright).
//!
//! The ThinkSmart View has a Vishay VCNL4200 (light and proximity) at 0x51 on the sensor I²C bus,
//! with the kernel's `vcnl4000` driver bound to it (thinksmart-view-debian, driver/vcnl4000).
//! Reading `in_illuminance_raw` through the driver powers the proximity part too (its runtime PM
//! switches both on for 2 s), and the proximity IR LED glows faintly red: polling it every 2 s
//! would keep the LED on. So the body reads only the light part straight from the chip over
//! `/dev/i2c-N` (as rickx's daemon does): it switches the ALS on in `ALS_CONF` (the proximity
//! part stays shut down) and reads `ALS_DATA`. The driver touches the chip only when someone
//! reads it through sysfs, which nobody does. packaging/home-companion.service lets group `home-companion` use the bus.
//!
//! Lux become the two backlight levels (`idle`, `active`) on a curve through three points
//! (dark, a lit room, daylight), interpolated on log(lux). The reading is smoothed (a moving
//! average of log lux) and the levels change only when the light moved by more than
//! [`HYSTERESIS`] decades from the last change, so a passing shadow does nothing; the backlight
//! fades to the new levels (backlight.rs, `Fade`).

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::backlight::Levels;

/// VCNL4200 registers (16-bit, little endian).
const ALS_CONF: u8 = 0x00;
const ALS_DATA: u8 = 0x09;
const PS_CONF1: u8 = 0x03;
const ADDRESS: u16 = 0x51;
const I2C_SLAVE_FORCE: u64 = 0x0706;

/// The light changes the levels only past this many decades (about ×1.6) from the last change.
pub const HYSTERESIS: f32 = 0.2;
/// Weight of a new reading in the moving average of log lux (one reading every 2 s: about 10 s).
pub const SMOOTHING: f32 = 0.2;

/// The newest lux, as f32 bits (`u32::MAX`: none yet); written by the sensor thread.
static LUX: AtomicU32 = AtomicU32::new(u32::MAX);
/// Readings so far (the UI feeds each one once).
static SEQ: AtomicU32 = AtomicU32::new(0);

pub fn set_lux(lux: f32) {
    LUX.store(lux.to_bits(), Ordering::Relaxed);
    SEQ.fetch_add(1, Ordering::Release);
}

/// The newest reading and its number.
pub fn lux() -> Option<(u32, f32)> {
    let seq = SEQ.load(Ordering::Acquire);
    match LUX.load(Ordering::Relaxed) {
        u32::MAX => None,
        b => Some((seq, f32::from_bits(b))),
    }
}

/// The curve: three points (lux, levels), from config `[screen]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Curve {
    pub dark: (f32, Levels),
    pub room: (f32, Levels),
    pub bright: (f32, Levels),
}

impl Default for Curve {
    fn default() -> Self {
        Curve {
            dark: (
                2.0,
                Levels {
                    idle: 40,
                    active: 400,
                },
            ),
            room: (
                200.0,
                Levels {
                    idle: 600,
                    active: 2200,
                },
            ),
            bright: (
                1000.0,
                Levels {
                    idle: 1200,
                    active: 4095,
                },
            ),
        }
    }
}

impl Curve {
    /// The levels at `lux`: log-linear between the points, flat outside them.
    pub fn levels(&self, lux: f32) -> Levels {
        let l = lux.max(0.01).log10();
        let (a, b) = if lux <= self.room.0 {
            (self.dark, self.room)
        } else {
            (self.room, self.bright)
        };
        let (la, lb) = (a.0.max(0.01).log10(), b.0.max(0.01).log10());
        let t = if lb > la {
            ((l - la) / (lb - la)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let mix = |x: u32, y: u32| (x as f32 + (y as f32 - x as f32) * t).round() as u32;
        Levels {
            idle: mix(a.1.idle, b.1.idle),
            active: mix(a.1.active, b.1.active),
        }
    }

    /// The darkest levels (night mode never goes above them).
    pub fn dark_levels(&self) -> Levels {
        self.dark.1
    }
}

/// Smoothing and hysteresis on top of the curve.
#[derive(Debug, Clone)]
pub struct Auto {
    pub curve: Curve,
    /// The last three readings (log10 lux): their median goes into the average, so one odd
    /// reading (a hand over the sensor) does nothing.
    last: Vec<f32>,
    /// Moving average of log10(lux).
    avg: Option<f32>,
    /// log10(lux) of the levels in use.
    used: Option<f32>,
}

impl Auto {
    pub fn new(curve: Curve) -> Auto {
        Auto {
            curve,
            last: Vec::new(),
            avg: None,
            used: None,
        }
    }

    /// A new reading; returns the new levels when they change.
    pub fn feed(&mut self, lux: f32) -> Option<Levels> {
        self.last.push(lux.max(0.01).log10());
        if self.last.len() > 3 {
            self.last.remove(0);
        }
        let mut m = self.last.clone();
        m.sort_by(f32::total_cmp);
        let l = m[m.len() / 2];
        let avg = match self.avg {
            None => l,
            Some(a) => a + (l - a) * SMOOTHING,
        };
        self.avg = Some(avg);
        match self.used {
            Some(u) if (avg - u).abs() < HYSTERESIS => None,
            _ => {
                self.used = Some(avg);
                Some(self.curve.levels(10f32.powf(avg)))
            }
        }
    }

    /// The levels in use (None before the first reading).
    pub fn levels(&self) -> Option<Levels> {
        self.used.map(|u| self.curve.levels(10f32.powf(u)))
    }
}

/// The light part of the VCNL4200, read over I²C.
pub struct Als {
    bus: File,
    path: PathBuf,
    /// Lux per count for the integration time set in ALS_CONF.
    scale: f32,
}

/// `/dev/i2c-N` of the light sensor: the bus of the IIO device named `vcnl4000`.
pub fn find_bus() -> Option<PathBuf> {
    for e in std::fs::read_dir("/sys/bus/iio/devices").ok()?.flatten() {
        let name = std::fs::read_to_string(e.path().join("name")).unwrap_or_default();
        if name.trim() != "vcnl4000" {
            continue;
        }
        // …/i2c-0/0-0051/iio:device0
        let real = std::fs::canonicalize(e.path()).ok()?;
        let bus = real.parent()?.parent()?.file_name()?.to_str()?.to_string();
        if bus.starts_with("i2c-") {
            return Some(Path::new("/dev").join(bus));
        }
    }
    None
}

fn smbus(fd: i32, read: bool, reg: u8, data: &mut [u8; 4]) -> std::io::Result<()> {
    // struct i2c_smbus_ioctl_data { u8 read_write; u8 command; u32 size; union i2c_smbus_data *data; }
    #[repr(C)]
    struct Args {
        read_write: u8,
        command: u8,
        size: u32,
        data: *mut u8,
    }
    const I2C_SMBUS: u64 = 0x0720;
    const WORD_DATA: u32 = 3;
    let mut a = Args {
        read_write: read as u8,
        command: reg,
        size: WORD_DATA,
        data: data.as_mut_ptr(),
    };
    // SAFETY: the ioctl reads/writes a word through `data` (34 bytes at most for block
    // transfers; a word transfer uses 2, and the buffer has 4).
    let r = unsafe { libc::ioctl(fd, I2C_SMBUS as _, &mut a as *mut Args) };
    if r < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Als {
    /// Opens the bus, switches the light part on (the proximity part is left as it is: off).
    pub fn open(path: &Path) -> Result<Als, String> {
        let bus = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let fd = bus.as_raw_fd();
        // SAFETY: plain ioctl on our fd; the driver is bound, hence FORCE.
        if unsafe { libc::ioctl(fd, I2C_SLAVE_FORCE as _, ADDRESS as libc::c_ulong) } < 0 {
            return Err(format!(
                "{}: address 0x51: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let mut d = [0u8; 4];
        smbus(fd, true, ALS_CONF, &mut d).map_err(|e| format!("ALS_CONF: {e}"))?;
        let conf = u16::from_le_bytes([d[0], d[1]]);
        let it = (conf >> 6) & 3;
        if conf & 1 != 0 {
            // ALS_SD set: switch the light part on.
            let on = (conf & !1).to_le_bytes();
            let mut w = [on[0], on[1], 0, 0];
            smbus(fd, false, ALS_CONF, &mut w).map_err(|e| format!("ALS_CONF: {e}"))?;
        }
        Ok(Als {
            bus,
            path: path.to_path_buf(),
            scale: 0.024 / (1 << it) as f32,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The proximity part is shut down (`PS_SD`): its IR LED is off.
    pub fn proximity_off(&self) -> Option<bool> {
        let mut d = [0u8; 4];
        smbus(self.bus.as_raw_fd(), true, PS_CONF1, &mut d).ok()?;
        Some(d[0] & 1 != 0)
    }

    pub fn read_lux(&self) -> Option<f32> {
        let fd = self.bus.as_raw_fd();
        let mut d = [0u8; 4];
        // Switched off by the driver meanwhile (someone read it through sysfs): on again.
        smbus(fd, true, ALS_CONF, &mut d).ok()?;
        if d[0] & 1 != 0 {
            let mut w = [d[0] & !1, d[1], 0, 0];
            smbus(fd, false, ALS_CONF, &mut w).ok()?;
            return None;
        }
        smbus(fd, true, ALS_DATA, &mut d).ok()?;
        Some(u16::from_le_bytes([d[0], d[1]]) as f32 * self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_goes_through_its_points_and_stays_flat_outside() {
        let c = Curve::default();
        assert_eq!(c.levels(0.0), c.dark.1);
        assert_eq!(c.levels(2.0), c.dark.1);
        assert_eq!(c.levels(200.0), c.room.1);
        assert_eq!(c.levels(1000.0), c.bright.1);
        assert_eq!(c.levels(50_000.0), c.bright.1);
        // 20 lux: half way between 2 and 200 on a log scale.
        let m = c.levels(20.0);
        assert_eq!(m.active, 1300);
        assert_eq!(m.idle, 320);
        // Monotonic.
        let mut last = 0;
        for lux in [0.5, 1.0, 3.0, 10.0, 30.0, 100.0, 300.0, 600.0, 2000.0] {
            let a = c.levels(lux).active;
            assert!(a >= last, "{lux} lux: {a} < {last}");
            last = a;
        }
    }

    #[test]
    fn small_changes_and_flickers_do_not_move_the_levels() {
        let mut a = Auto::new(Curve::default());
        let first = a.feed(150.0).unwrap();
        // ±20 % around: nothing.
        for lux in [180.0, 120.0, 160.0, 140.0, 175.0] {
            assert_eq!(a.feed(lux), None);
        }
        // One dark reading (a hand over the sensor) is smoothed away.
        assert_eq!(a.feed(1.0), None);
        // The light goes off for good: after a few readings the levels go down.
        let mut changed = None;
        for _ in 0..20 {
            if let Some(l) = a.feed(0.5) {
                changed = Some(l);
            }
        }
        let dark = changed.expect("levels change in the dark");
        assert!(dark.active < first.active);
        assert_eq!(a.levels(), Some(dark));
    }
}
