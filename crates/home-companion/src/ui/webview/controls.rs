// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! When the body's controls over "Casa" (the round home button and the home indicator bar)
//! are shown (Andrea, 2026-11-04): a full-screen dashboard has nothing on top of it at rest.
//! They come up when the view opens and with every touch, and go away `seconds` after the
//! last one; 0 = always shown. The button can be dragged anywhere and snaps to the nearest
//! edge on release, like a phone's floating assistive button (Andrea, 2026-11-04); a touch on
//! it that does not move goes home. Pure logic, the caller passes the clock; positions are
//! the view's logical pixels (1280x800, independent of the panel's rotation).

use std::time::{Duration, Instant};

/// The view's size (logical pixels).
pub const VIEW_W: f32 = 1280.0;
pub const VIEW_H: f32 = 800.0;
/// The button's diameter: Parla's (Grid.primary-size), gold like it (Andrea, 2026-11-06).
pub const SIZE: f32 = 96.0;
/// Its distance from the edge it rests on.
pub const MARGIN: f32 = 16.0;
/// The bottom strip of the swipe up (gesture.rs): the button never covers it, so on the
/// bottom edge it rests 4 px above the strip's top (24 px from the edge).
pub const STRIP: f32 = 20.0;
const BOTTOM_MARGIN: f32 = STRIP + 4.0;
/// A touch on the button that moves farther than this drags it (and does not go home).
pub const DRAG_SLOP: f32 = 12.0;
/// A touch this close to the button's circle counts as on it.
pub const HIT_SLACK: f32 = 8.0;

#[derive(Debug, Clone)]
pub struct Controls {
    /// How long they stay after the last touch (`None` = always shown).
    hold: Option<Duration>,
    until: Option<Instant>,
    /// When they last appeared (hidden before).
    since: Option<Instant>,
}

/// The button takes a tap only once it has been on screen this long: a second quick tap on the
/// page where the button just appeared stays the page's (review of 2026-11-04, issue 7).
pub const ARM: Duration = Duration::from_millis(250);

impl Controls {
    pub fn new(seconds: f32) -> Controls {
        Controls {
            hold: (seconds > 0.0).then(|| Duration::from_secs_f32(seconds)),
            until: None,
            since: None,
        }
    }

    /// The view opened, or a touch: shown for `seconds` from `now`.
    pub fn poke(&mut self, now: Instant) {
        if !self.shown(now) || self.since.is_none() {
            self.since = Some(now);
        }
        if let Some(h) = self.hold {
            self.until = Some(now + h);
        }
    }

    /// Shown for at least [`ARM`]: the button may take a tap.
    pub fn armed(&self, now: Instant) -> bool {
        self.shown(now)
            && (self.hold.is_none()
                || self
                    .since
                    .is_some_and(|s| now.saturating_duration_since(s) >= ARM))
    }

    /// The view closed: hidden at once (the next open shows them again).
    pub fn reset(&mut self) {
        self.until = None;
        self.since = None;
    }

    pub fn shown(&self, now: Instant) -> bool {
        match self.hold {
            None => true,
            Some(_) => self.until.is_some_and(|u| now < u),
        }
    }
}

/// The edge the button rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn name(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
        }
    }

    pub fn from_name(s: &str) -> Option<Edge> {
        Some(match s {
            "left" => Edge::Left,
            "right" => Edge::Right,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            _ => return None,
        })
    }
}

/// Where the button rests: an edge and the offset along it (the button's left for the top
/// and bottom edges, its top for the left and right ones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub edge: Edge,
    pub offset: f32,
}

const MAX_X: f32 = VIEW_W - MARGIN - SIZE;
const MAX_Y: f32 = VIEW_H - BOTTOM_MARGIN - SIZE;

impl Default for Place {
    /// Bottom right (clear of Home Assistant's sidebar entries at the bottom left).
    fn default() -> Self {
        Place {
            edge: Edge::Right,
            offset: MAX_Y,
        }
    }
}

impl Place {
    /// The same place, kept inside the view (a saved one may come from another layout).
    pub fn clamped(self) -> Place {
        let offset = if !self.offset.is_finite() {
            MARGIN
        } else {
            match self.edge {
                Edge::Left | Edge::Right => self.offset.clamp(MARGIN, MAX_Y),
                Edge::Top | Edge::Bottom => self.offset.clamp(MARGIN, MAX_X),
            }
        };
        Place { offset, ..self }
    }

    /// The button's top left.
    pub fn xy(self) -> (f32, f32) {
        let p = self.clamped();
        match p.edge {
            Edge::Left => (MARGIN, p.offset),
            Edge::Right => (MAX_X, p.offset),
            Edge::Top => (p.offset, MARGIN),
            Edge::Bottom => (p.offset, MAX_Y),
        }
    }

    /// The nearest edge to a button whose top left is at (x, y), and the place on it.
    pub fn snap(x: f32, y: f32) -> Place {
        let (cx, cy) = (x + SIZE / 2.0, y + SIZE / 2.0);
        let d = [
            (cx, Edge::Left),
            (VIEW_W - cx, Edge::Right),
            (cy, Edge::Top),
            (VIEW_H - cy, Edge::Bottom),
        ];
        let edge = d
            .iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(Edge::Right, |e| e.1);
        let offset = match edge {
            Edge::Left | Edge::Right => y,
            Edge::Top | Edge::Bottom => x,
        };
        Place { edge, offset }.clamped()
    }

    /// The touch at (x, y) is on the button.
    pub fn hit(self, x: f32, y: f32) -> bool {
        let (bx, by) = self.xy();
        let (cx, cy, r) = (bx + SIZE / 2.0, by + SIZE / 2.0, SIZE / 2.0 + HIT_SLACK);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    }
}

/// One touch that started on the button: a tap (home) or a drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    finger: (f32, f32),
    from: (f32, f32),
    moving: bool,
}

/// How a touch on the button ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragEnd {
    /// It did not move: go home.
    Tap,
    /// It was dragged: the button rests here now.
    Moved(Place),
}

impl Drag {
    /// A touch down at `finger` on the button resting at `place`.
    pub fn start(place: Place, finger: (f32, f32)) -> Drag {
        Drag {
            finger,
            from: place.xy(),
            moving: false,
        }
    }

    pub fn moving(&self) -> bool {
        self.moving
    }

    /// Where the button's top left is while the finger is at (x, y).
    fn follow(&self, x: f32, y: f32) -> (f32, f32) {
        let bx = self.from.0 + x - self.finger.0;
        let by = self.from.1 + y - self.finger.1;
        (
            bx.clamp(0.0, VIEW_W - SIZE),
            by.clamp(0.0, VIEW_H - STRIP - SIZE),
        )
    }

    /// The finger moved: the button's new top left once it is a drag (`None`: still a tap).
    pub fn motion(&mut self, x: f32, y: f32) -> Option<(f32, f32)> {
        if !self.moving {
            let (dx, dy) = (x - self.finger.0, y - self.finger.1);
            if dx * dx + dy * dy <= DRAG_SLOP * DRAG_SLOP {
                return None;
            }
            self.moving = true;
        }
        Some(self.follow(x, y))
    }

    /// The finger lifted at (x, y).
    pub fn end(mut self, x: f32, y: f32) -> DragEnd {
        match self.motion(x, y) {
            None => DragEnd::Tap,
            Some((bx, by)) => DragEnd::Moved(Place::snap(bx, by)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_place_is_bottom_right_clear_of_the_strip() {
        let (x, y) = Place::default().xy();
        assert_eq!(x, VIEW_W - MARGIN - SIZE);
        assert!(y + SIZE <= VIEW_H - STRIP);
        assert!(Place::default().hit(x + SIZE / 2.0, y + SIZE / 2.0));
        assert!(!Place::default().hit(x - 20.0, y + SIZE / 2.0));
    }

    #[test]
    fn tap_or_drag() {
        let p = Place::default();
        let (x, y) = p.xy();
        let f = (x + 28.0, y + 28.0);
        // A wobble within the slop is still a tap.
        let mut d = Drag::start(p, f);
        assert_eq!(d.motion(f.0 + 8.0, f.1 - 8.0), None);
        assert!(!d.moving());
        assert_eq!(d.end(f.0 + 5.0, f.1), DragEnd::Tap);
        // Past the slop it is a drag, and stays one even back at the start.
        let mut d = Drag::start(p, f);
        assert_eq!(d.motion(f.0 - 30.0, f.1), Some((x - 30.0, y)));
        assert!(d.moving());
        assert_eq!(d.motion(f.0, f.1), Some((x, y)));
        assert_eq!(d.end(f.0, f.1), DragEnd::Moved(p));
        // Up without a motion event, far away: a drag too.
        let d = Drag::start(p, f);
        assert!(matches!(d.end(100.0, 300.0), DragEnd::Moved(_)));
    }

    #[test]
    fn drag_stays_inside_and_above_the_strip() {
        let p = Place::default();
        let (x, y) = p.xy();
        let mut d = Drag::start(p, (x, y));
        assert_eq!(
            d.motion(5000.0, 5000.0),
            Some((VIEW_W - SIZE, VIEW_H - STRIP - SIZE))
        );
        assert_eq!(d.motion(-500.0, -500.0), Some((0.0, 0.0)));
    }

    #[test]
    fn snaps_to_the_nearest_edge() {
        // Near the left edge, mid height.
        assert_eq!(
            Place::snap(40.0, 300.0),
            Place {
                edge: Edge::Left,
                offset: 300.0
            }
        );
        assert_eq!(Place::snap(40.0, 300.0).xy(), (MARGIN, 300.0));
        // Near the top, in the middle.
        assert_eq!(Place::snap(600.0, 30.0).xy(), (600.0, MARGIN));
        // Near the bottom: above the strip.
        let p = Place::snap(600.0, VIEW_H - STRIP - SIZE);
        assert_eq!(p.edge, Edge::Bottom);
        assert_eq!(p.xy(), (600.0, VIEW_H - STRIP - 4.0 - SIZE));
        // Near the right, in a corner: kept inside.
        assert_eq!(
            Place::snap(VIEW_W - SIZE, 0.0).xy(),
            (VIEW_W - MARGIN - SIZE, MARGIN)
        );
        // A saved place out of range comes back inside.
        let far = Place {
            edge: Edge::Top,
            offset: 9000.0,
        };
        assert_eq!(far.xy(), (VIEW_W - MARGIN - SIZE, MARGIN));
        let nan = Place {
            edge: Edge::Left,
            offset: f32::NAN,
        };
        assert_eq!(nan.xy(), (MARGIN, MARGIN));
        for e in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            assert_eq!(Edge::from_name(e.name()), Some(e));
        }
        assert_eq!(Edge::from_name("middle"), None);
    }

    #[test]
    fn shown_after_a_poke_then_hidden() {
        let t0 = Instant::now();
        let s = |x: f32| t0 + Duration::from_secs_f32(x);
        let mut c = Controls::new(4.0);
        assert!(!c.shown(t0));
        c.poke(t0);
        assert!(c.shown(t0));
        assert!(c.shown(s(3.9)));
        assert!(!c.shown(s(4.0)));
        // A later touch extends it: 4 s after the last one.
        c.poke(s(3.0));
        assert!(c.shown(s(6.9)));
        assert!(!c.shown(s(7.0)));
        c.poke(s(8.0));
        assert!(c.shown(s(8.0)));
        c.reset();
        assert!(!c.shown(s(8.0)));
    }

    #[test]
    fn zero_is_always_shown() {
        let t0 = Instant::now();
        let mut c = Controls::new(0.0);
        assert!(c.shown(t0));
        c.reset();
        assert!(c.shown(t0 + Duration::from_secs(3600)));
        c.poke(t0);
        assert!(c.shown(t0 + Duration::from_secs(3600)));
    }

    #[test]
    fn the_button_takes_a_tap_only_after_a_moment() {
        let t0 = Instant::now();
        let mut c = Controls::new(4.0);
        c.poke(t0);
        assert!(c.shown(t0) && !c.armed(t0 + Duration::from_millis(100)));
        assert!(c.armed(t0 + ARM));
        // Pokes while shown do not re-arm it.
        c.poke(t0 + Duration::from_secs(1));
        assert!(c.armed(t0 + Duration::from_secs(1)));
        // Hidden, then shown again: a new wait.
        let later = t0 + Duration::from_secs(10);
        c.poke(later);
        assert!(!c.armed(later));
    }
}
