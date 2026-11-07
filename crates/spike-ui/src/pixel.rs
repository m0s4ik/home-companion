// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The panel's native pixel: 32 bpp, bytes B, G, R, X in memory
//! (little-endian XRGB8888, what msm's fbdev emulation exposes). Implementing
//! Slint's `TargetPixel` for it lets the software renderer draw straight into
//! the framebuffer layout, with no conversion pass.

use slint::platform::software_renderer::{PremultipliedRgbaColor, TargetPixel};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Bgrx {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub x: u8,
}

impl Bgrx {
    pub const BLACK: Bgrx = Bgrx { b: 0, g: 0, r: 0, x: 0xff };

    pub const fn rgb(&self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }
}

#[inline]
fn over(dst: u8, src: u8, inv_alpha: u16) -> u8 {
    // Same arithmetic as Slint's own Rgb8Pixel, so results match bit for bit.
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

/// Views raw framebuffer bytes as pixels. `bytes.len()` must be a multiple of 4
/// and 4-byte aligned (true for mmap'ed memory and for `Vec<Bgrx>`).
pub fn as_pixels(bytes: &mut [u8]) -> &mut [Bgrx] {
    assert!(bytes.len() % 4 == 0 && bytes.as_ptr() as usize % std::mem::align_of::<Bgrx>() == 0);
    // SAFETY: Bgrx is repr(C), 4 x u8, alignment 1, every bit pattern valid.
    unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<Bgrx>(), bytes.len() / 4) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_red_over_white() {
        let mut p = Bgrx::from_rgb(255, 255, 255);
        // 50 % red, premultiplied: 255 * 128 / 255 = 128.
        let red = PremultipliedRgbaColor { red: 128, green: 0, blue: 0, alpha: 128 };
        p.blend(red);
        let bytes: [u8; 4] = unsafe { std::mem::transmute(p) };
        assert_eq!(bytes, [127, 127, 255, 0xff], "memory order is B, G, R, X");
    }

    #[test]
    fn opaque_fill_and_layout() {
        let mut row = [Bgrx::default(); 4];
        Bgrx::blend_slice(&mut row, PremultipliedRgbaColor { red: 1, green: 2, blue: 3, alpha: 255 });
        assert!(row.iter().all(|p| *p == Bgrx { b: 3, g: 2, r: 1, x: 0xff }));
        assert_eq!(std::mem::size_of::<Bgrx>(), 4);
    }

    #[test]
    fn transparent_is_noop() {
        let mut p = Bgrx::from_rgb(10, 20, 30);
        p.blend(PremultipliedRgbaColor::default());
        assert_eq!(p.rgb(), [10, 20, 30]);
    }
}
