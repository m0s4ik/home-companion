// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Where things go on the 800x1280 portrait panel: the character on top,
//! centred, the clock below it (as in the previous Python screen program,
//! which shows the 440x760 frames at their own size).

pub const SCREEN_W: u32 = 800;
pub const SCREEN_H: u32 = 1280;
/// Top of the character box.
pub const CHAR_TOP: u32 = 40;
/// Top of the clock's 200 px text box (it ends 30 px above the bottom edge).
pub const CLOCK_Y: u32 = 1050;
/// The largest scale of a 440x760 frame that still ends above the clock.
pub const MAX_CHAR_SCALE: f64 = 1.31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CharBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// The character's on-screen box for frames of `fw` x `fh` scaled by `scale`,
/// centred horizontally and clamped to fit between the top and the clock.
pub fn char_box(fw: u32, fh: u32, scale: f64) -> CharBox {
    let max_h = CLOCK_Y - CHAR_TOP - 10;
    let mut w = (fw as f64 * scale).round().max(1.0) as u32;
    let mut h = (fh as f64 * scale).round().max(1.0) as u32;
    if h > max_h {
        w = (w as u64 * max_h as u64 / h as u64) as u32;
        h = max_h;
    }
    if w > SCREEN_W {
        h = (h as u64 * SCREEN_W as u64 / w as u64) as u32;
        w = SCREEN_W;
    }
    CharBox { x: (SCREEN_W - w) / 2, y: CHAR_TOP, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boxes_fit() {
        assert_eq!(char_box(440, 760, 1.0), CharBox { x: 180, y: 40, w: 440, h: 760 });
        let b = char_box(440, 760, MAX_CHAR_SCALE);
        assert!(b.y + b.h <= CLOCK_Y && b.x + b.w <= SCREEN_W, "{b:?}");
        // A wide frame is clamped to the screen width.
        let b = char_box(1600, 100, 1.0);
        assert_eq!((b.w, b.x), (800, 0));
    }
}
