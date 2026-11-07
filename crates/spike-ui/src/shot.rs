// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! PNG in and out: screenshots (RGB8) and poses (any PNG, decoded to RGBA8).

use std::path::Path;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

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

/// Decodes any 8-bit PNG (gray, RGB, palette, with or without alpha) to RGBA8.
pub fn decode_rgba(bytes: &[u8]) -> Result<SharedPixelBuffer<Rgba8Pixel>, String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let mut data = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut data).map_err(|e| e.to_string())?;
    let (w, h) = (info.width, info.height);
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let dst = buf.make_mut_bytes();
    match info.color_type {
        png::ColorType::Rgba => dst.copy_from_slice(&data[..dst.len()]),
        png::ColorType::GrayscaleAlpha => {
            for (d, s) in dst.chunks_exact_mut(4).zip(data.chunks_exact(2)) {
                d.copy_from_slice(&[s[0], s[0], s[0], s[1]]);
            }
        }
        other => return Err(format!("unexpected PNG color type after expansion: {other:?}")),
    }
    Ok(buf)
}

/// Loads a pose: our own PNG decoder, so the fbdev-only build needs no image
/// crate (Slint's `Image::load_from_path` is only compiled in with a backend
/// that enables `image-decoders`, i.e. winit or linuxkms).
pub fn load_pose(path: &Path) -> Result<Image, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode_rgba(&bytes).map(Image::from_rgba8).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn pose_from_bytes(bytes: &[u8]) -> Image {
    Image::from_rgba8(decode_rgba(bytes).expect("embedded pose PNG is valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip() {
        let dir = std::env::temp_dir().join(format!("spike-ui-test-{}", std::process::id()));
        let path = dir.join("rt.png");
        let px: Vec<Bgrx> = (0..6 * 4)
            .map(|i| Bgrx { b: i as u8, g: (i * 7) as u8, r: (255 - i) as u8, x: 0xff })
            .collect();
        // 4 x 3 image inside a buffer with a stride of 6 pixels.
        write_bgrx(&path, 4, 3, 6, &px).unwrap();
        let back = decode_rgba(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!((back.width(), back.height()), (4, 3));
        for y in 0..3 {
            for x in 0..4 {
                let s = px[y * 6 + x];
                let d = back.as_slice()[y * 4 + x];
                assert_eq!([d.r, d.g, d.b, d.a], [s.r, s.g, s.b, 255]);
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn embedded_poses_decode() {
        for bytes in [&include_bytes!("../fixtures/pose-a.png")[..], include_bytes!("../fixtures/pose-b.png")] {
            let b = decode_rgba(bytes).unwrap();
            assert_eq!((b.width(), b.height()), (600, 600));
        }
    }
}
