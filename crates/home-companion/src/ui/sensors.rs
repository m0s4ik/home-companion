// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What wakes the screen besides touch: the volume keys (evdev; on the ThinkSmart View
//! `gpio-keys` has Vol+ and `pm8941_resin` Vol−) and moving the device (the accelerometer, IIO
//! `in_accel_{x,y,z}_raw`, read every half second). Each source is a small thread that sends a
//! [`WakeSource`] to the UI thread. The volume keys also send a volume step to the core, repeated
//! in software while held (the kernel does not repeat these keys), and both together go home
//! (keycombo.rs).

use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use super::backlight::WakeSource;
use super::keycombo::{self, Key, Output};
use crate::core::CoreEvent;
use crate::volume::VolumeRequest;

const EV_KEY: u16 = 1;
const EV_SW: u16 = 5;
/// The microphone's switch: 1 = the microphone is there (on), 0 = muted. On the ThinkSmart View
/// it is the slider on top (`gpio-keys`, "Mic Mute"), which also cuts the microphones.
const SW_MICROPHONE_INSERT: u16 = 4;
pub const KEY_VOLUMEDOWN: u16 = 114;
pub const KEY_VOLUMEUP: u16 = 115;

const fn ioc_read(nr: u64, size: u64) -> u64 {
    (2 << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}

fn has_key(fd: i32, code: u16) -> bool {
    let mut bits = [0u8; 96];
    let req = ioc_read(0x20 + EV_KEY as u64, bits.len() as u64);
    // SAFETY: EVIOCGBIT writes at most `bits.len()` bytes.
    let r = unsafe { libc::ioctl(fd, req as _, bits.as_mut_ptr()) };
    r >= 0 && bits[(code / 8) as usize] & (1 << (code % 8)) != 0
}

/// The microphone's switch on `fd`, if the device has one: Some(true) = on.
fn mic_switch(fd: i32) -> Option<bool> {
    let mut bits = [0u8; 4];
    // EVIOCGBIT(EV_SW, 4).
    let req = ioc_read(0x20 + EV_SW as u64, bits.len() as u64);
    // SAFETY: EVIOCGBIT writes at most `bits.len()` bytes.
    let r = unsafe { libc::ioctl(fd, req as _, bits.as_mut_ptr()) };
    if r < 0 || bits[0] & (1 << SW_MICROPHONE_INSERT) == 0 {
        return None;
    }
    let mut state = [0u8; 4];
    // EVIOCGSW(4).
    let req = ioc_read(0x1b, state.len() as u64);
    // SAFETY: EVIOCGSW writes at most `state.len()` bytes.
    let r = unsafe { libc::ioctl(fd, req as _, state.as_mut_ptr()) };
    (r >= 0).then_some(state[0] & (1 << SW_MICROPHONE_INSERT) != 0)
}

/// The `/dev/input/event*` devices with a volume key.
pub fn find_key_devices() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir("/dev/input") else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = dir
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("event"))
        })
        .filter(|p| {
            std::fs::File::open(p).is_ok_and(|f| {
                has_key(f.as_raw_fd(), KEY_VOLUMEUP) || has_key(f.as_raw_fd(), KEY_VOLUMEDOWN)
            })
        })
        .collect();
    v.sort();
    v
}

/// The longest wait between two polls of the key tracker while a key is down.
const POLL: Duration = Duration::from_millis(50);

/// One input event: (type, code, value).
fn parse_event(ev: &[u8]) -> (u16, u16, i32) {
    (
        u16::from_ne_bytes([ev[16], ev[17]]),
        u16::from_ne_bytes([ev[18], ev[19]]),
        i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]),
    )
}

/// Waits until `fd` is readable or `timeout` passed (None = forever); false on timeout.
fn wait_readable(fd: i32, timeout: Option<Duration>) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.map_or(-1, |t| t.as_millis().min(60_000) as i32);
    // SAFETY: one valid pollfd.
    let r = unsafe { libc::poll(&mut p, 1, ms) };
    r != 0
}

/// One thread per device: a press of a volume key wakes the screen; the shared tracker
/// (keycombo.rs) turns the presses into volume steps sent to the core with `volume` (held: the
/// step repeats), and Vol+ with Vol− into "go home" (no volume change).
pub fn spawn_keys(devices: &[PathBuf], tx: &Sender<WakeSource>, volume: Option<Sender<CoreEvent>>) {
    for path in devices {
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("keys {}: {e}", path.display());
                continue;
            }
        };
        let (tx, p, volume) = (tx.clone(), path.clone(), volume.clone());
        let what = if volume.is_some() {
            "volume, wakes the screen"
        } else {
            "volume keys wake the screen"
        };
        let r = std::thread::Builder::new()
            .name("keys".into())
            .spawn(move || {
                let fd = file.as_raw_fd();
                let mut buf = [0u8; 24 * 16];
                // The microphone's switch, if this device has it: how it is now, then each move.
                let has_switch = match (mic_switch(fd), &volume) {
                    (Some(on), Some(v)) => {
                        log::info!(
                            "keys: {} has the microphone's switch ({})",
                            p.display(),
                            if on { "on" } else { "muted" }
                        );
                        let _ = v.send(CoreEvent::MicSwitch { on, initial: true });
                        true
                    }
                    _ => false,
                };
                // A step: wake the screen, change the volume. False when the receivers are gone.
                let step = |key: Key, repeated: bool| -> bool {
                    let (code, dir) = match key {
                        Key::Up => (KEY_VOLUMEUP, 1),
                        Key::Down => (KEY_VOLUMEDOWN, -1),
                    };
                    if tx.send(WakeSource::Key(code)).is_err() {
                        return false;
                    }
                    volume.as_ref().is_none_or(|v| {
                        v.send(CoreEvent::Volume(VolumeRequest::Step {
                            dir,
                            repeat: repeated,
                        }))
                        .is_ok()
                    })
                };
                // Volume steps come only from the tracker's outputs (the pair goes home, inside).
                let act = |out: Vec<Output>| -> bool {
                    out.into_iter().all(|o| match o {
                        Output::Volume(key) => step(key, false),
                        Output::Repeat(key) => step(key, true),
                        Output::Home => true,
                    })
                };
                loop {
                    // Poll the shared tracker at least every 50 ms while a key is down.
                    let timeout = keycombo::wait_shared().map(|w| w.min(POLL));
                    if wait_readable(fd, timeout) {
                        let n = match file.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        for ev in buf[..n].chunks_exact(24) {
                            let (kind, code, value) = parse_event(ev);
                            if has_switch && kind == EV_SW && code == SW_MICROPHONE_INSERT {
                                let on = value != 0;
                                if let Some(v) = &volume {
                                    if v.send(CoreEvent::MicSwitch { on, initial: false }).is_err()
                                    {
                                        return;
                                    }
                                }
                                continue;
                            }
                            if kind != EV_KEY || (code != KEY_VOLUMEUP && code != KEY_VOLUMEDOWN) {
                                continue;
                            }
                            // A press wakes the screen at once (the volume waits for the
                            // tracker), except the press that completes the pair: that one
                            // may put the screen to sleep (keycombo::pair_action).
                            let out = keycombo::feed_shared(code == KEY_VOLUMEUP, value);
                            let pair = out.contains(&Output::Home);
                            if value == 1 && !pair && tx.send(WakeSource::Key(code)).is_err() {
                                return;
                            }
                            if !act(out) {
                                return;
                            }
                        }
                    }
                    if !act(keycombo::poll_shared()) {
                        return;
                    }
                }
                log::warn!("keys: {} closed", p.display());
            });
        match r {
            Ok(_) => log::info!("keys: {} ({what})", path.display()),
            Err(e) => log::warn!("keys thread: {e}"),
        }
    }
}

/// The IIO device of the accelerometer: `setting` "auto" = the first with `in_accel_x_raw`.
pub fn find_accelerometer(setting: &str) -> Option<PathBuf> {
    match setting {
        "" => None,
        "auto" => {
            let mut v: Vec<PathBuf> = std::fs::read_dir("/sys/bus/iio/devices")
                .ok()?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.join("in_accel_x_raw").exists())
                .collect();
            v.sort();
            v.into_iter().next()
        }
        d => Some(PathBuf::from(d)),
    }
}

fn read_accel(dir: &Path, scale: f32) -> Option<[f32; 3]> {
    let mut g = [0.0; 3];
    for (i, axis) in ["x", "y", "z"].iter().enumerate() {
        let s = std::fs::read_to_string(dir.join(format!("in_accel_{axis}_raw"))).ok()?;
        g[i] = s.trim().parse::<i32>().ok()? as f32 * scale;
    }
    Some(g)
}

/// Movement between two readings (m/s²) above `threshold` wakes the screen.
pub fn moved(prev: [f32; 3], now: [f32; 3], threshold: f32) -> bool {
    let d: f32 = prev.iter().zip(now).map(|(a, b)| (a - b) * (a - b)).sum();
    d.sqrt() > threshold
}

/// The light sensor is read every this many accelerometer readings (every 2 s at 0.5 s).
pub const LIGHT_EVERY: u32 = 4;

/// Reads the accelerometer every `period`; sends a wake when it moved more than `threshold`.
/// Reads the light sensor too, every [`LIGHT_EVERY`] periods (light.rs).
pub fn spawn_motion(
    dir: PathBuf,
    threshold: f32,
    period: Duration,
    tx: Sender<WakeSource>,
    light: Option<super::light::Als>,
) {
    let scale = std::fs::read_to_string(dir.join("in_accel_scale"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1.0);
    let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
    let Some(first) = read_accel(&dir, scale) else {
        log::warn!("accelerometer {}: cannot read it", dir.display());
        return;
    };
    log::info!(
        "accelerometer {} ({}): {:.1} {:.1} {:.1} m/s², movement over {threshold} m/s² wakes the screen",
        dir.display(),
        name.trim(),
        first[0],
        first[1],
        first[2]
    );
    if let Some(a) = &light {
        log::info!(
            "light sensor {} (VCNL4200 light part over I2C): {:.1} lux, automatic brightness; proximity (IR LED) {}",
            a.path().display(),
            a.read_lux().unwrap_or(-1.0),
            match a.proximity_off() {
                Some(true) => "off",
                Some(false) => "ON",
                None => "unknown",
            }
        );
    }
    let _ = std::thread::Builder::new()
        .name("motion".into())
        .spawn(move || {
            let mut prev = first;
            let mut n = 0u32;
            loop {
                if n % LIGHT_EVERY == 0
                    && let Some(lux) = light.as_ref().and_then(super::light::Als::read_lux)
                {
                    super::light::set_lux(lux);
                }
                n = n.wrapping_add(1);
                std::thread::sleep(period);
                let Some(g) = read_accel(&dir, scale) else {
                    continue;
                };
                if moved(prev, g, threshold) && tx.send(WakeSource::Motion).is_err() {
                    return;
                }
                prev = g;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_threshold() {
        let g = [0.0, -9.2, 3.0];
        assert!(!moved(g, [0.3, -9.0, 3.2], 1.5));
        assert!(moved(g, [1.2, -9.2, 4.2], 1.5));
    }

    #[test]
    fn reads_iio_axes_with_scale() {
        let d = std::env::temp_dir().join(format!("home-companion-iio-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for (a, v) in [("x", "10"), ("y", "-920"), ("z", "300")] {
            std::fs::write(d.join(format!("in_accel_{a}_raw")), format!("{v}\n")).unwrap();
        }
        assert_eq!(read_accel(&d, 0.5), Some([5.0, -460.0, 150.0]));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn ioctl_number() {
        // EVIOCGBIT(EV_KEY, 96) as in <linux/input.h>.
        assert_eq!(ioc_read(0x20 + 1, 96), 0x80604521);
    }
}
