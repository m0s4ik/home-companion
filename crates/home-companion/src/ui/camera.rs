// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The camera, for now a photo from the control center shown as a preview and kept in
//! `<state>/photos` (Andrea, 2026-11-15). The picture comes from libcamera through GStreamer
//! (`gst-launch-1.0 libcamerasrc`, what the system's `CAMERA=1` installs): about 20 frames so the
//! exposure settles, all encoded to one file, and the last JPEG of it kept (thinksmart-view-debian
//! known issues: with libcamera 0.7.1 `multifilesink` writes nothing, `filesink` works). A
//! closed lens cover (`SW_CAMERA_LENS_COVER` on `gpio-keys`) is said instead of taking a black
//! picture.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Photos kept (the oldest go).
pub const KEEP: usize = 20;
/// Frames taken before the one kept (the exposure settles in about a second).
const FRAMES: u32 = 20;
/// A capture that takes longer than this is stopped.
const TIMEOUT: Duration = Duration::from_secs(12);
const EV_SW: u64 = 5;
const SW_CAMERA_LENS_COVER: usize = 9;

/// The GStreamer plugin of libcamera, where Debian puts it.
fn plugin_present() -> bool {
    [
        "aarch64-linux-gnu",
        "x86_64-linux-gnu",
        "arm-linux-gnueabihf",
    ]
    .iter()
    .any(|t| Path::new(&format!("/usr/lib/{t}/gstreamer-1.0/libgstlibcamera.so")).exists())
}

/// This device has a camera the body can use: a media device, GStreamer and libcamera's plugin.
pub fn present() -> bool {
    Path::new("/dev/media0").exists()
        && Path::new("/usr/bin/gst-launch-1.0").exists()
        && plugin_present()
}

const fn ioc_read(nr: u64, size: u64) -> u64 {
    (2 << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}

/// Bit `n` of an evdev bit array.
fn bit(bits: &[u8], n: usize) -> bool {
    bits.get(n / 8).is_some_and(|b| b & (1 << (n % 8)) != 0)
}

/// The lens cover: Some(true) closed, Some(false) open, None no device reports it.
pub fn lens_covered() -> Option<bool> {
    let dir = std::fs::read_dir("/dev/input").ok()?;
    for e in dir.flatten() {
        let p = e.path();
        if !p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("event"))
        {
            continue;
        }
        let Ok(f) = std::fs::File::open(&p) else {
            continue;
        };
        use std::os::fd::AsRawFd;
        let fd = f.as_raw_fd();
        let mut bits = [0u8; 4];
        // SAFETY: EVIOCGBIT(EV_SW) writes at most `bits.len()` bytes.
        let r = unsafe {
            libc::ioctl(
                fd,
                ioc_read(0x20 + EV_SW, bits.len() as u64) as _,
                bits.as_mut_ptr(),
            )
        };
        if r < 0 || !bit(&bits, SW_CAMERA_LENS_COVER) {
            continue;
        }
        let mut state = [0u8; 4];
        // SAFETY: EVIOCGSW writes at most `state.len()` bytes.
        let r = unsafe {
            libc::ioctl(
                fd,
                ioc_read(0x1b, state.len() as u64) as _,
                state.as_mut_ptr(),
            )
        };
        if r >= 0 {
            return Some(bit(&state, SW_CAMERA_LENS_COVER));
        }
    }
    None
}

/// The last whole JPEG of a run of them (from its last start-of-image marker to the end).
pub fn last_jpeg(data: &[u8]) -> Option<&[u8]> {
    let start = data.windows(3).rposition(|w| w == [0xff, 0xd8, 0xff])?;
    let jpeg = &data[start..];
    // A whole one ends with the end-of-image marker.
    (jpeg.len() > 4 && jpeg.ends_with(&[0xff, 0xd9])).then_some(jpeg)
}

/// The photos to delete so that `keep` stay: names sort by time ("photo-YYYYMMDD-HHMMSS.jpg").
pub fn to_prune(mut names: Vec<String>, keep: usize) -> Vec<String> {
    names.retain(|n| n.starts_with("photo-") && n.ends_with(".jpg"));
    names.sort();
    let extra = names.len().saturating_sub(keep);
    names.truncate(extra);
    names
}

/// "photo-20261115-124700.jpg" for a local time (year, month, day, hour, minute, second).
pub fn photo_name(t: (i32, u32, u32, u32, u32, u32)) -> String {
    format!(
        "photo-{:04}{:02}{:02}-{:02}{:02}{:02}.jpg",
        t.0, t.1, t.2, t.3, t.4, t.5
    )
}

fn local_now() -> (i32, u32, u32, u32, u32, u32) {
    // SAFETY: localtime_r only writes into the struct we pass.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = libc::time(std::ptr::null_mut());
        libc::localtime_r(&tt, &mut tm);
        tm
    };
    (
        tm.tm_year + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    )
}

/// Takes a photo into `dir` (created if needed) and returns its path. Blocks for about 2 s: call
/// it from a thread.
pub fn capture(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(".capture.mjpeg");
    let _ = std::fs::remove_file(&tmp);
    let started = Instant::now();
    let mut child = Command::new("gst-launch-1.0")
        .args([
            "-q",
            "libcamerasrc",
            "!",
            "video/x-raw,width=1280,height=960",
            "!",
            "identity",
            &format!("eos-after={FRAMES}"),
            "!",
            "videoconvert",
            "!",
            "jpegenc",
            "quality=88",
            "!",
            "filesink",
        ])
        .arg(format!("location={}", tmp.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("gst-launch-1.0: {e}"))?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() < TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("no picture in {} s", TIMEOUT.as_secs()));
            }
            Err(e) => return Err(format!("gst-launch-1.0: {e}")),
        }
    };
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let data = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    if !status.success() && data.is_empty() {
        let line = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed");
        return Err(format!("gst-launch-1.0: {}", line.trim()));
    }
    let jpeg = last_jpeg(&data).ok_or_else(|| "no whole picture in the capture".to_string())?;
    let path = dir.join(photo_name(local_now()));
    std::fs::write(&path, jpeg).map_err(|e| format!("{}: {e}", path.display()))?;
    log::info!(
        event = "camera_photo", ms = started.elapsed().as_millis() as u64, bytes = jpeg.len() as u64;
        "camera: photo {} ({} KB in {} ms)", path.display(), jpeg.len() / 1024, started.elapsed().as_millis()
    );
    if let Ok(rd) = std::fs::read_dir(dir) {
        let names = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        for n in to_prune(names, KEEP) {
            let _ = std::fs::remove_file(dir.join(n));
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_whole_jpeg_of_a_run() {
        let a = [0xff, 0xd8, 0xff, 0xe0, 1, 2, 0xff, 0xd9];
        let b = [0xff, 0xd8, 0xff, 0xdb, 9, 9, 9, 0xff, 0xd9];
        let run: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
        assert_eq!(last_jpeg(&run), Some(&b[..]));
        // Cut short: not whole.
        assert_eq!(last_jpeg(&run[..run.len() - 1]), None);
        assert_eq!(last_jpeg(&[]), None);
    }

    #[test]
    fn prune_keeps_the_newest() {
        let names = vec![
            "photo-20261115-120000.jpg".to_string(),
            "photo-20261114-090000.jpg".to_string(),
            "photo-20261115-130000.jpg".to_string(),
            ".capture.mjpeg".to_string(),
            "notes.txt".to_string(),
        ];
        assert_eq!(to_prune(names.clone(), 2), ["photo-20261114-090000.jpg"]);
        assert!(to_prune(names, 3).is_empty());
    }

    #[test]
    fn names_sort_by_time() {
        assert_eq!(
            photo_name((2026, 11, 5, 9, 7, 3)),
            "photo-20261105-090703.jpg"
        );
        assert!(photo_name((2026, 11, 5, 9, 7, 3)) < photo_name((2026, 11, 15, 0, 0, 0)));
    }

    #[test]
    fn evdev_bits() {
        let bits = [0x10u8, 0x02, 0, 0];
        assert!(bit(&bits, 4) && bit(&bits, 9));
        assert!(!bit(&bits, 8) && !bit(&bits, 40));
    }
}
