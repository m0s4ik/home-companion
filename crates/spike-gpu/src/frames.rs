// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The character animation: one folder per pose of `frame_NNN.png`. Files
//! are read once (compressed, ~100 KB each); each mode then decodes them into
//! the form it draws from (GL textures, Slint images, pre-scaled BGRX).

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::pixel::Bgrx;

/// The background colour of the Alfred frames (#03050D).
pub const FRAME_BG: [u8; 3] = [0x03, 0x05, 0x0d];

pub struct Pose {
    pub name: String,
    /// The PNG files as read from disk.
    pub pngs: Vec<Vec<u8>>,
}

pub struct Animation {
    pub dir: PathBuf,
    pub poses: Vec<Pose>,
    /// Size of every frame (checked on the first frame of each pose).
    pub width: u32,
    pub height: u32,
    pub read_ms: f64,
    pub png_bytes: usize,
}

impl Animation {
    pub fn frame_count(&self) -> usize {
        self.poses.iter().map(|p| p.pngs.len()).sum()
    }
}

/// Tightly packed RGB8 pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgb {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// Lists the pose folders of `dir` that hold at least one `frame_*.png`, sorted.
pub fn list_poses(dir: &Path) -> Result<Vec<String>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| {
        format!("{}: {e} (extract the animations with tools/fetch-frames.sh, or pass --frames-dir)", dir.display())
    })?;
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| !frame_files(&e.path()).unwrap_or_default().is_empty())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    Ok(names)
}

fn frame_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("frame_") && n.ends_with(".png"))
        })
        .collect();
    files.sort();
    Ok(files)
}

/// PNG width and height from the IHDR chunk.
pub fn png_size(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err("not a PNG file".into());
    }
    let be = |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Ok((be(16), be(20)))
}

/// Reads the frames of `names` (empty = every pose folder) from `dir`.
pub fn load(dir: &Path, names: &[String]) -> Result<Animation, String> {
    let t0 = Instant::now();
    let names = if names.is_empty() { list_poses(dir)? } else { names.to_vec() };
    if names.is_empty() {
        return Err(format!("{}: no pose folders with frame_*.png", dir.display()));
    }
    let mut poses = Vec::new();
    let mut size = None;
    let mut png_bytes = 0;
    for name in names {
        let pdir = dir.join(&name);
        let files = frame_files(&pdir)?;
        if files.is_empty() {
            return Err(format!("{}: no frame_*.png", pdir.display()));
        }
        let mut pngs = Vec::with_capacity(files.len());
        for f in &files {
            let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
            let s = png_size(&bytes).map_err(|e| format!("{}: {e}", f.display()))?;
            match size {
                None => size = Some(s),
                Some(s0) if s0 != s => {
                    return Err(format!("{}: {}x{}, other frames are {}x{}", f.display(), s.0, s.1, s0.0, s0.1));
                }
                _ => {}
            }
            png_bytes += bytes.len();
            pngs.push(bytes);
        }
        poses.push(Pose { name, pngs });
    }
    let (width, height) = size.unwrap();
    Ok(Animation {
        dir: dir.to_path_buf(),
        poses,
        width,
        height,
        read_ms: t0.elapsed().as_secs_f64() * 1e3,
        png_bytes,
    })
}

/// Decodes any 8-bit PNG to RGB8; alpha, if any, is composited over the
/// frames' background colour.
pub fn decode_rgb(bytes: &[u8]) -> Result<Rgb, String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::EXPAND);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width, info.height);
    let n = (w * h) as usize;
    let over = |c: u8, a: u8, bg: u8| ((c as u16 * a as u16 + bg as u16 * (255 - a as u16) + 127) / 255) as u8;
    let data = match info.color_type {
        png::ColorType::Rgb => {
            buf.truncate(n * 3);
            buf
        }
        png::ColorType::Rgba => buf[..n * 4]
            .chunks_exact(4)
            .flat_map(|p| [over(p[0], p[3], FRAME_BG[0]), over(p[1], p[3], FRAME_BG[1]), over(p[2], p[3], FRAME_BG[2])])
            .collect(),
        png::ColorType::Grayscale => buf[..n].iter().flat_map(|&g| [g, g, g]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..n * 2]
            .chunks_exact(2)
            .flat_map(|p| [over(p[0], p[1], FRAME_BG[0]), over(p[0], p[1], FRAME_BG[1]), over(p[0], p[1], FRAME_BG[2])])
            .collect(),
        other => return Err(format!("unexpected PNG color type after expansion: {other:?}")),
    };
    Ok(Rgb { w, h, data })
}

/// RGB8 to RGBA8 (opaque), for GL textures.
#[cfg_attr(not(any(feature = "gpu", feature = "gpu-window")), allow(dead_code))]
pub fn rgb_to_rgba(src: &Rgb) -> Vec<u8> {
    src.data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 0xff]).collect()
}

/// RGB8 to the panel's BGRX.
pub fn rgb_to_bgrx(src: &Rgb) -> Vec<Bgrx> {
    src.data.chunks_exact(3).map(|p| Bgrx { b: p[2], g: p[1], r: p[0], x: 0xff }).collect()
}

/// Resizes with bilinear interpolation (pixel centres aligned); downscaling by
/// more than 2x first averages 2x2 blocks so that no source row is skipped.
pub fn scale_rgb(src: Rgb, w: u32, h: u32) -> Rgb {
    if (src.w, src.h) == (w, h) {
        return src;
    }
    if w * 2 <= src.w && h * 2 <= src.h && src.w >= 2 && src.h >= 2 {
        return scale_rgb(halve(&src), w, h);
    }
    let (sw, sh) = (src.w as usize, src.h as usize);
    let fx = src.w as f32 / w as f32;
    let fy = src.h as f32 / h as f32;
    // Source index and 8-bit weight of the next sample, per destination
    // coordinate (computed once; the inner loop is integer only).
    let taps = |n: usize, f: f32, len: usize| -> Vec<(usize, usize, u32)> {
        (0..n)
            .map(|i| {
                let s = ((i as f32 + 0.5) * f - 0.5).clamp(0.0, (len - 1) as f32);
                let i0 = s.floor() as usize;
                (i0, (i0 + 1).min(len - 1), ((s - i0 as f32) * 256.0).round() as u32)
            })
            .collect()
    };
    let cols = taps(w as usize, fx, sw);
    let rows = taps(h as usize, fy, sh);
    let mut data = Vec::with_capacity(w as usize * h as usize * 3);
    for &(y0, y1, wy) in &rows {
        let (r0, r1) = (&src.data[y0 * sw * 3..(y0 + 1) * sw * 3], &src.data[y1 * sw * 3..(y1 + 1) * sw * 3]);
        for &(x0, x1, wx) in &cols {
            for c in 0..3 {
                let a = r0[x0 * 3 + c] as u32 * (256 - wx) + r0[x1 * 3 + c] as u32 * wx;
                let b = r1[x0 * 3 + c] as u32 * (256 - wx) + r1[x1 * 3 + c] as u32 * wx;
                data.push(((a * (256 - wy) + b * wy + 32768) >> 16) as u8);
            }
        }
    }
    Rgb { w, h, data }
}

fn halve(src: &Rgb) -> Rgb {
    let (w, h) = (src.w / 2, src.h / 2);
    let sw = src.w as usize;
    let mut data = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h as usize {
        for x in 0..w as usize {
            for c in 0..3 {
                let p = |yy: usize, xx: usize| src.data[(yy * sw + xx) * 3 + c] as u16;
                let s = p(2 * y, 2 * x) + p(2 * y, 2 * x + 1) + p(2 * y + 1, 2 * x) + p(2 * y + 1, 2 * x + 1);
                data.push(((s + 2) / 4) as u8);
            }
        }
    }
    Rgb { w, h, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8]) {
        crate::shot::write_rgb(path, w, h, rgb).unwrap();
    }

    #[test]
    fn scale_keeps_flat_colour_and_size() {
        let src = Rgb { w: 4, h: 6, data: [10u8, 20, 30].repeat(24) };
        for (w, h) in [(4, 6), (5, 8), (2, 3), (1, 1), (9, 13)] {
            let d = scale_rgb(src.clone(), w, h);
            assert_eq!((d.w, d.h, d.data.len()), (w, h, (w * h * 3) as usize));
            assert!(d.data.chunks_exact(3).all(|p| p == [10, 20, 30]), "{w}x{h}");
        }
    }

    #[test]
    fn scale_interpolates() {
        // Two columns, black and white: doubling the width gives a ramp.
        let src = Rgb { w: 2, h: 1, data: vec![0, 0, 0, 255, 255, 255] };
        let d = scale_rgb(src, 4, 1);
        let r: Vec<u8> = d.data.chunks_exact(3).map(|p| p[0]).collect();
        assert_eq!(r, vec![0, 64, 191, 255]);
    }

    #[test]
    fn conversions() {
        let src = Rgb { w: 1, h: 1, data: vec![1, 2, 3] };
        assert_eq!(rgb_to_rgba(&src), vec![1, 2, 3, 255]);
        assert_eq!(rgb_to_bgrx(&src), vec![Bgrx { b: 3, g: 2, r: 1, x: 255 }]);
    }

    #[test]
    fn load_and_decode_a_fake_animation() {
        let dir = std::env::temp_dir().join(format!("spike-gpu-frames-{}", std::process::id()));
        for (pose, n) in [("idle", 3), ("error", 2)] {
            std::fs::create_dir_all(dir.join(pose)).unwrap();
            for i in 0..n {
                let rgb: Vec<u8> = (0..6 * 4).flat_map(|p| [p as u8, i as u8, 7]).collect();
                write_png(&dir.join(pose).join(format!("frame_{i:03}.png")), 6, 4, &rgb);
            }
        }
        std::fs::write(dir.join("README.txt"), "not a pose").unwrap();
        assert_eq!(list_poses(&dir).unwrap(), vec!["error", "idle"]);
        let a = load(&dir, &[]).unwrap();
        assert_eq!((a.width, a.height, a.frame_count()), (6, 4, 5));
        let a = load(&dir, &["idle".into()]).unwrap();
        assert_eq!(a.poses.len(), 1);
        let f = decode_rgb(&a.poses[0].pngs[2]).unwrap();
        assert_eq!((f.w, f.h), (6, 4));
        assert_eq!(&f.data[3..6], &[1, 2, 7]);
        assert!(load(&dir, &["missing".into()]).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
