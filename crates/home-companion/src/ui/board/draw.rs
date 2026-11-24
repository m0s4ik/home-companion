// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The tiles as the screen's `BoardTile` draws them (design/components.slint): the model of
//! [`super::view`] with the icon's path data, the curve's path in its box and the words of the
//! state, for a canvas's cell size. Pure: the screen only copies it into Slint's structs, and
//! the browser panel can draw its preview from the same values.

use super::icons::Icons;
use super::{TileKind, TileState, TileView};

/// A canvas's cell and the gap between cells, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cells {
    pub width: f32,
    pub height: f32,
    pub gap: f32,
}

/// The page's cells (6 x 4 over the whole screen at x 25, y 96: 1230 x 664).
pub const PAGE: Cells = Cells {
    width: 195.0,
    height: 157.0,
    gap: 12.0,
};
/// The custom widget's (4 x 2 in its 288 px slot, the dots in the last 16).
pub const WIDGET: Cells = Cells {
    width: 193.0,
    height: 130.0,
    gap: 12.0,
};
/// The widget's one line (row 0 only, as tall as a card).
pub const LINE: Cells = Cells {
    width: 193.0,
    height: 88.0,
    gap: 12.0,
};

impl Cells {
    /// A tile's size in pixels.
    pub fn size(&self, w: u8, h: u8) -> (f32, f32) {
        let span = |n: u8, cell: f32| n as f32 * cell + n.saturating_sub(1) as f32 * self.gap;
        (span(w, self.width), span(h, self.height))
    }
}

/// One tile, ready for the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct TileDraw {
    /// Its place in its canvas's tiles (what a tap names).
    pub index: usize,
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
    /// "button", "sensor", "level".
    pub kind: &'static str,
    /// "off", "on", "pending", "unavailable", "failed".
    pub state: &'static str,
    /// The icon's SVG path data (24 x 24).
    pub icon: String,
    pub name: String,
    pub value: String,
    pub status: String,
    pub range: String,
    /// 0-100, -1 none.
    pub level: i32,
    /// The day's curve as path commands in [`curve_box`] ("" none).
    pub curve: String,
    pub tappable: bool,
}

pub fn kind_key(k: TileKind) -> &'static str {
    match k {
        TileKind::Button => "button",
        TileKind::Sensor => "sensor",
        TileKind::Level => "level",
    }
}

pub fn state_key(s: TileState) -> &'static str {
    match s {
        TileState::Off => "off",
        TileState::On => "on",
        TileState::Pending => "pending",
        TileState::Unavailable => "unavailable",
        TileState::Failed => "failed",
    }
}

/// Where a sensor's curve is drawn in its tile (width, height), None: no curve at this size.
/// 2 x 1: 150 x 44 at the bottom right; 4 x 1: 440 x 44; 2 x 2: the tile's width less its
/// padding, 84 tall, over the range.
pub fn curve_box(w: u8, h: u8, cells: Cells) -> Option<(f32, f32)> {
    let (tw, _) = cells.size(w, h);
    match (w, h) {
        (2, 1) => Some((150.0, 44.0)),
        (4, 1) => Some((440.0, 44.0)),
        (2, 2) => Some((tw - 40.0, 84.0)),
        _ => None,
    }
}

/// The tiles of a canvas for its cells; `line`: the widget's one line (row 0 only).
pub fn draws(views: &[TileView], cells: Cells, icons: &Icons, line: bool) -> Vec<TileDraw> {
    views
        .iter()
        .enumerate()
        .filter(|(_, v)| !line || v.y == 0)
        .map(|(index, v)| {
            let curve = if line {
                String::new()
            } else {
                v.curve
                    .as_ref()
                    .zip(curve_box(v.w, v.h, cells))
                    .map(|(c, (w, h))| crate::ui::widgets::curve_path_in(&c.points, w, h))
                    .unwrap_or_default()
            };
            TileDraw {
                index,
                x: v.x,
                y: v.y,
                w: v.w,
                h: if line { 1 } else { v.h },
                kind: kind_key(v.kind),
                state: state_key(v.state),
                icon: icons.path(&v.icon).to_string(),
                name: v.name.clone(),
                value: v.value.clone(),
                status: v.status.clone(),
                range: v.range.clone(),
                level: v.level.map_or(-1, i32::from),
                curve,
                tappable: v.tappable,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::widgets::Curve;

    fn view(x: u8, y: u8, w: u8, h: u8, kind: TileKind) -> TileView {
        TileView {
            x,
            y,
            w,
            h,
            kind,
            entity_id: "sensor.t".into(),
            state: TileState::On,
            icon: "thermometer".into(),
            name: "Outdoor".into(),
            value: "12.5°C".into(),
            status: String::new(),
            range: String::new(),
            level: Some(40),
            curve: Some(Curve {
                points: vec![0.0, 1.0],
                first: 0.0,
                low: 0.0,
                high: 1.0,
            }),
            tappable: false,
            confirm: false,
        }
    }

    #[test]
    fn sizes_of_the_canvases() {
        assert_eq!(PAGE.size(6, 4), (1230.0, 664.0));
        assert_eq!(WIDGET.size(4, 2), (808.0, 272.0));
        assert_eq!(PAGE.size(2, 2), (402.0, 326.0));
        assert_eq!(LINE.size(1, 1), (193.0, 88.0));
    }

    #[test]
    fn a_tile_for_the_screen() {
        let icons = Icons::default();
        let v = [
            view(0, 0, 2, 1, TileKind::Sensor),
            view(0, 1, 2, 2, TileKind::Sensor),
            view(2, 1, 1, 1, TileKind::Sensor),
        ];
        let d = draws(&v, PAGE, &icons, false);
        assert_eq!((d[0].kind, d[0].state, d[0].level), ("sensor", "on", 40));
        assert_eq!(d[0].curve, "M 0.0 42.0 L 150.0 2.0");
        assert_eq!(d[1].curve, "M 0.0 82.0 L 362.0 2.0");
        assert_eq!(d[2].curve, "", "a small sensor has no curve");
        assert_eq!(
            d[0].icon,
            super::super::icons::builtin("thermometer").unwrap()
        );
        // The one line: row 0 only, one cell tall, no curve; the index stays the canvas's.
        let l = draws(&v[..], LINE, &icons, true);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].index, l[0].h, l[0].curve.as_str()), (0, 1, ""));
        let mut later = v.to_vec();
        later.swap(0, 2);
        assert_eq!(draws(&later, LINE, &icons, true)[0].index, 2);
    }
}
