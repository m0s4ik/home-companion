// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Screenshots as RGB8 PNG.

use std::path::Path;

use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::pixel::Bgrx;

/// Writes tightly packed RGB8 pixels as a PNG.
pub fn write_rgb(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(rgb).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())
}

/// Screenshot of a BGRX buffer with `stride` pixels per line.
pub fn write_bgrx(path: &Path, width: u32, height: u32, stride: usize, px: &[Bgrx]) -> Result<(), String> {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height as usize {
        for p in &px[y * stride..y * stride + width as usize] {
            rgb.extend_from_slice(&p.rgb());
        }
    }
    write_rgb(path, width, height, &rgb)
}

/// Screenshot of a Slint snapshot (straight alpha); drawn over black.
#[cfg_attr(not(any(feature = "gpu", feature = "gpu-window")), allow(dead_code))]
pub fn write_rgba(path: &Path, buf: &SharedPixelBuffer<Rgba8Pixel>) -> Result<(), String> {
    let rgb: Vec<u8> = buf
        .as_slice()
        .iter()
        .flat_map(|p| {
            let a = p.a as u16;
            [p.r, p.g, p.b].map(|c| (c as u16 * a / 255) as u8)
        })
        .collect();
    write_rgb(path, buf.width(), buf.height(), &rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip() {
        let dir = std::env::temp_dir().join(format!("spike-gpu-shot-{}", std::process::id()));
        let path = dir.join("rt.png");
        let px: Vec<Bgrx> = (0..6 * 4)
            .map(|i| Bgrx { b: i as u8, g: (i * 7) as u8, r: (255 - i) as u8, x: 0xff })
            .collect();
        // 4 x 3 image inside a buffer with a stride of 6 pixels.
        write_bgrx(&path, 4, 3, 6, &px).unwrap();
        let back = crate::frames::decode_rgb(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!((back.w, back.h), (4, 3));
        for y in 0..3 {
            for x in 0..4 {
                let s = px[y * 6 + x];
                let i = (y * 4 + x) * 3;
                assert_eq!(&back.data[i..i + 3], &[s.r, s.g, s.b]);
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
