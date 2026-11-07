// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The screen-mirroring page (AirPlay from a phone or a tablet): when a session starts the page opens over whatever was on screen, the user can go
//! home while it goes on (a home card brings it back), and when it ends the page closes back to
//! where it was. The layout (video centred and as large as it fits, controls in the side bands
//! or on a tap) is companion.slint's; this is the page's navigation and the demos' picture.

use crate::core::MirrorInfo;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// The page's name in companion.slint.
pub const PAGE: &str = "mirror";

/// The page to show after the session changed from `was` (a session was on) to `now`, with
/// `page` on screen and `back` the page the mirroring opened over; None leaves the page as it
/// is. A new session opens the page; a new size of the same session (the sender rotated) does
/// not drag the user back from the home; the end closes the page only if it is on screen.
pub fn page_on_change(
    was: bool,
    now: Option<&MirrorInfo>,
    page: &str,
    back: &str,
) -> Option<String> {
    match now {
        Some(_) if !was && page != PAGE => Some(PAGE.into()),
        Some(_) => None,
        None if page == PAGE => Some(if back.is_empty() || back == PAGE {
            "home".into()
        } else {
            back.into()
        }),
        None => None,
    }
}

/// The page to come back to when a session that opened over `page` ends: that page, except the
/// pages that make no sense to land on later (the link page opens a port, a page left alone
/// would go home anyway after a minute).
pub fn back_page(page: &str) -> String {
    match page {
        PAGE | "setup" => "home".into(),
        p => p.into(),
    }
}

/// The home card's (title, text): "Duplicazione schermo", "iPhone di Andrea · Tocca per
/// tornare"; the sender's name alone is enough when the hint is empty.
pub fn card(title: &str, hint: &str, info: &MirrorInfo) -> (String, String) {
    let text = match (info.source.is_empty(), hint.is_empty()) {
        (true, _) => hint.to_string(),
        (false, true) => info.source.clone(),
        (false, false) => format!("{} · {hint}", info.source),
    };
    (title.to_string(), text)
}

/// A neutral picture of a phone's or a tablet's home screen at `w` x `h` (the demos and the
/// screenshots, in place of the video): a dark gradient, a status bar, rows of rounded app
/// squares and a dock. Generated, nothing real on it.
pub fn placeholder(w: u32, h: u32) -> Image {
    let (w, h) = (w.clamp(128, 1280), h.clamp(128, 800));
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w, h);
    let px = buf.make_mut_slice();
    // The wallpaper: deep blue at the top to plum at the bottom.
    let (top, bottom) = ([22.0f32, 38.0, 64.0], [58.0f32, 30.0, 70.0]);
    for y in 0..h {
        let t = y as f32 / (h - 1) as f32;
        let c: [u8; 3] = std::array::from_fn(|i| (top[i] + (bottom[i] - top[i]) * t) as u8);
        for x in 0..w {
            px[(y * w + x) as usize] = Rgba8Pixel::new(c[0], c[1], c[2], 255);
        }
    }
    let mut rect = |x0: u32, y0: u32, rw: u32, rh: u32, r: u32, c: [u8; 3]| {
        for y in y0..(y0 + rh).min(h) {
            for x in x0..(x0 + rw).min(w) {
                // Rounded corners: outside the corner circle stays wallpaper.
                let dx = (x0 + r)
                    .saturating_sub(x)
                    .max(x.saturating_sub(x0 + rw - 1 - r));
                let dy = (y0 + r)
                    .saturating_sub(y)
                    .max(y.saturating_sub(y0 + rh - 1 - r));
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                px[(y * w + x) as usize] = Rgba8Pixel::new(c[0], c[1], c[2], 255);
            }
        }
    };
    let unit = w.min(h);
    // The status bar: the time on the left, signal and battery on the right.
    let bar = (unit / 40).max(3);
    let m = unit / 16;
    rect(m, bar * 2, bar * 5, bar * 2, bar / 2, [235, 235, 240]);
    rect(
        w - m - bar * 5,
        bar * 2,
        bar * 5,
        bar * 2,
        bar / 2,
        [235, 235, 240],
    );
    rect(
        w - m - bar * 9,
        bar * 2,
        bar * 3,
        bar * 2,
        bar / 2,
        [235, 235, 240],
    );
    // The apps: 4 columns on a portrait screen, 7 or 10 on a landscape one.
    let cols = if w > h {
        if w > 1100 { 10 } else { 7 }
    } else {
        4
    };
    let cell = w / cols;
    let side = (cell * 3 / 5).min(h / 6);
    let palette = [
        [92u8, 156, 230],
        [120, 190, 120],
        [235, 170, 80],
        [214, 100, 110],
        [160, 120, 220],
        [90, 190, 190],
        [230, 210, 110],
        [200, 200, 205],
    ];
    let dock_h = cell;
    let rows = ((h.saturating_sub(bar * 8 + dock_h + m)) / cell).min(6);
    for r in 0..rows {
        for c in 0..cols {
            let col = palette[((r * cols + c) as usize * 5 + r as usize) % palette.len()];
            let x = c * cell + (cell - side) / 2;
            let y = bar * 8 + r * cell;
            rect(x, y, side, side, side / 4, col);
        }
    }
    // The dock: a lighter strip with four apps.
    let dock_h = dock_h.min(h / 4);
    let dy = h - dock_h - m / 2;
    let dw = (cell * 4).min(w - 2 * m);
    let dx = (w - dw) / 2;
    rect(dx, dy, dw, dock_h, dock_h / 3, [70, 66, 96]);
    for c in 0..4 {
        let x = dx + c * (dw / 4) + (dw / 4 - side) / 2;
        rect(
            x,
            dy + (dock_h - side) / 2,
            side,
            side,
            side / 4,
            palette[c as usize * 2],
        );
    }
    Image::from_rgba8(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(w: u32, h: u32) -> MirrorInfo {
        MirrorInfo {
            source: "iPhone".into(),
            width: w,
            height: h,
        }
    }

    #[test]
    fn a_new_session_opens_the_page() {
        let i = info(368, 800);
        assert_eq!(
            page_on_change(false, Some(&i), "home", "").as_deref(),
            Some(PAGE)
        );
        assert_eq!(
            page_on_change(false, Some(&i), "music", "").as_deref(),
            Some(PAGE)
        );
    }

    #[test]
    fn a_rotation_does_not_drag_the_user_back() {
        let i = info(1280, 592);
        assert_eq!(page_on_change(true, Some(&i), "home", "music"), None);
        assert_eq!(page_on_change(true, Some(&i), PAGE, "music"), None);
    }

    #[test]
    fn the_end_closes_the_page_back_where_it_was() {
        assert_eq!(
            page_on_change(true, None, PAGE, "music").as_deref(),
            Some("music")
        );
        assert_eq!(
            page_on_change(true, None, PAGE, "").as_deref(),
            Some("home")
        );
        assert_eq!(
            page_on_change(true, None, PAGE, PAGE).as_deref(),
            Some("home")
        );
        // The user already went elsewhere: nothing moves.
        assert_eq!(page_on_change(true, None, "timers", "music"), None);
    }

    #[test]
    fn back_page_skips_the_link_page() {
        assert_eq!(back_page("settings"), "settings");
        assert_eq!(back_page("setup"), "home");
        assert_eq!(back_page(PAGE), "home");
    }

    #[test]
    fn the_card_names_the_sender() {
        let (t, x) = card("Duplicazione schermo", "Tocca per tornare", &info(1, 1));
        assert_eq!(t, "Duplicazione schermo");
        assert_eq!(x, "iPhone · Tocca per tornare");
        let nameless = MirrorInfo::default();
        assert_eq!(card("D", "Tocca", &nameless).1, "Tocca");
        assert_eq!(card("D", "", &info(1, 1)).1, "iPhone");
    }

    #[test]
    fn placeholder_has_the_frame_size() {
        for (w, h) in [(368, 800), (1280, 592), (1066, 800), (600, 800)] {
            let i = placeholder(w, h);
            assert_eq!((i.size().width, i.size().height), (w, h));
        }
    }
}
