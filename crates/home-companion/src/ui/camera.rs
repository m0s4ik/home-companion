// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The camera, for now a photo from the control center shown as a preview and kept in
//! `<state>/photos` (Andrea, 2026-11-15). The picture comes from libcamera through GStreamer
//! (`gst-launch-1.0 libcamerasrc`, what the system's `CAMERA=1` installs): about 20 frames so the
//! exposure settles, all encoded to one file, and the last JPEG of it kept (thinksmart-view-debian
//! known issues: with libcamera 0.7.1 `multifilesink` writes nothing, `filesink` works). A
//! closed lens cover (`SW_CAMERA_LENS_COVER` on `gpio-keys`) is said instead of taking a black
//! picture.

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
            &format!("video/x-raw,width={PHOTO_W},height={PHOTO_H}"),
            "!",
            "identity",
            &format!("eos-after={FRAMES}"),
            "!",
            // Kept as the mirror showed it.
            "videoflip",
            "method=horizontal-flip",
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
        // Its messages to a file: a pipe read only at the end could fill and block it.
        .stderr(
            std::fs::File::create(dir.join(".capture.log"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        )
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
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("gst-launch-1.0: {e}"));
            }
        }
    };
    let err = std::fs::read_to_string(dir.join(".capture.log")).unwrap_or_default();
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

/// The live picture of the camera page (a mirror: flipped left to right), small and slow
/// enough to cost little: 640x480 RGBA at most 15 times a second.
pub const LIVE_W: u32 = 640;
pub const LIVE_H: u32 = 480;
/// The photo's size; the live picture is the same frame, scaled.
pub const PHOTO_W: u32 = 1280;
pub const PHOTO_H: u32 = 960;

/// The camera live, for the camera page: gst-launch writes raw RGBA frames to a pipe, a thread
/// keeps the newest. The sensor runs at ~75 frames a second by itself and libcamera debayers
/// each one (~70 % of a core even when 15 are kept): once streaming, its vertical blanking is
/// raised so it gives ~14; with the photo-size frames scaled down, ~40 % of a core while the page is open
/// (measured on the device). Stopped when dropped.
pub struct Live {
    child: std::process::Child,
    latest: std::sync::Arc<std::sync::Mutex<(u64, Vec<u8>)>>,
}

impl Live {
    pub fn start() -> Result<Live, String> {
        let mut child = Command::new("gst-launch-1.0")
            .args([
                "-q",
                "libcamerasrc",
                "!",
                // The photo's own size, scaled down here: libcamera crops a smaller request,
                // and the mirror must show exactly what the photo will be (Andrea, 2026-11-15).
                &format!("video/x-raw,width={PHOTO_W},height={PHOTO_H}"),
                "!",
                "videorate",
                "drop-only=true",
                "max-rate=15",
                "!",
                "videoscale",
                "method=nearest-neighbour",
                "!",
                &format!("video/x-raw,width={LIVE_W},height={LIVE_H}"),
                "!",
                "videoflip",
                "method=horizontal-flip",
                "!",
                "videoconvert",
                "!",
                "video/x-raw,format=RGBA",
                "!",
                "fdsink",
                "fd=1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("gst-launch-1.0: {e}"))?;
        let mut out = child.stdout.take().ok_or("no pipe")?;
        let latest = std::sync::Arc::new(std::sync::Mutex::new((0u64, Vec::new())));
        let keep = latest.clone();
        std::thread::Builder::new()
            .name("camera-live".into())
            .spawn(move || {
                use std::io::Read;
                let size = (LIVE_W * LIVE_H * 4) as usize;
                let mut n = 0u64;
                loop {
                    let mut frame = vec![0u8; size];
                    if out.read_exact(&mut frame).is_err() {
                        break;
                    }
                    n += 1;
                    if n == 10 {
                        slow_sensor();
                    }
                    if let Ok(mut l) = keep.lock() {
                        *l = (n, frame);
                    }
                }
            })
            .map_err(|e| format!("camera thread: {e}"))?;
        log::info!(event = "camera_live", on = true; "camera: live picture on");
        Ok(Live { child, latest })
    }

    /// The newest frame if it is newer than `after` (its number and the picture).
    pub fn frame(&self, after: u64) -> Option<(u64, slint::SharedPixelBuffer<slint::Rgba8Pixel>)> {
        let l = self.latest.lock().ok()?;
        (l.0 > after && !l.1.is_empty()).then(|| {
            (
                l.0,
                slint::SharedPixelBuffer::clone_from_slice(&l.1, LIVE_W, LIVE_H),
            )
        })
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        log::info!(event = "camera_live", on = false; "camera: live picture off");
    }
}

const V4L2_CID_EXPOSURE: u32 = 0x0098_0911;
const V4L2_CID_VBLANK: u32 = 0x009e_0901;
/// The vertical blanking that slows the ThinkSmart View's S5KC505A to ~14 frames a second
/// (from 70 lines, ~75 a second); a driver with a smaller range takes its maximum.
const SLOW_VBLANK: i32 = 4000;

#[repr(C)]
struct V4l2Control {
    id: u32,
    value: i32,
}

const fn ioc_rw_v(nr: u64) -> u64 {
    (3 << 30) | ((std::mem::size_of::<V4l2Control>() as u64) << 16) | ((b'V' as u64) << 8) | nr
}

/// Raises the vertical blanking of the camera's sensor: the sub-device that has both an
/// exposure and a vertical blanking control. Nothing when there is none or it refuses.
fn slow_sensor() {
    let Ok(dir) = std::fs::read_dir("/dev") else {
        return;
    };
    for e in dir.flatten() {
        if !e.file_name().to_string_lossy().starts_with("v4l-subdev") {
            continue;
        }
        let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(e.path())
        else {
            continue;
        };
        use std::os::fd::AsRawFd;
        let fd = f.as_raw_fd();
        let get = |id| {
            let mut c = V4l2Control { id, value: 0 };
            // SAFETY: VIDIOC_G_CTRL reads and writes one v4l2_control.
            let r = unsafe { libc::ioctl(fd, ioc_rw_v(27) as _, &mut c) };
            (r >= 0).then_some(c.value)
        };
        if get(V4L2_CID_EXPOSURE).is_none() || get(V4L2_CID_VBLANK).is_none() {
            continue;
        }
        let mut c = V4l2Control {
            id: V4L2_CID_VBLANK,
            value: SLOW_VBLANK,
        };
        // SAFETY: VIDIOC_S_CTRL reads and writes one v4l2_control.
        let r = unsafe { libc::ioctl(fd, ioc_rw_v(28) as _, &mut c) };
        log::info!(event = "camera_live", vblank = c.value; "camera: {} slowed (vertical blanking {}{})", e.path().display(), c.value, if r < 0 { ", refused" } else { "" });
        return;
    }
}

/// The photos kept, newest first.
pub fn photos(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .is_some_and(|n| n.starts_with("photo-") && n.ends_with(".jpg"))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}

/// The photo for the screen, decoded here (off the UI thread) and at most `w` x `h`: not with
/// `Image::load_from_path`, whose cache by path would keep 5 MB of RGBA after the preview.
pub fn preview(
    path: &Path,
    w: u32,
    h: u32,
) -> Result<slint::SharedPixelBuffer<slint::Rgba8Pixel>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let rgba = img.thumbnail(w, h).to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(slint::SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        w,
        h,
    ))
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
    fn v4l2_ioctl_numbers() {
        // VIDIOC_G_CTRL and VIDIOC_S_CTRL of linux/videodev2.h.
        assert_eq!(ioc_rw_v(27), 0xc008_561b);
        assert_eq!(ioc_rw_v(28), 0xc008_561c);
    }

    #[test]
    fn evdev_bits() {
        let bits = [0x10u8, 0x02, 0, 0];
        assert!(bit(&bits, 4) && bit(&bits, 9));
        assert!(!bit(&bits, 8) && !bit(&bits, 40));
    }
}
