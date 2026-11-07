// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The swipe up from the bottom edge that leaves "Casa", like a phone's home gesture (Andrea,
//! 2026-11-04): a touch that starts in the bottom strip of the view is held back from the page
//! until it is clear what it is:
//!
//! - it goes up: the pill follows the finger; let go at least `threshold` above the start →
//!   home; let go earlier (or come back down) → nothing, and the page never saw it;
//! - it is a tap (up before it moved `slop`) → the page gets it late, as down + up at the
//!   start point;
//! - it goes sideways or down first → the page gets the down at the start point and the touch
//!   from then on (a drag of the page that happened to start at the edge).
//!
//! Touches that start above the strip (Home Assistant's sidebar entries at the bottom, its
//! profile and notifications, start well above it) go to the page at once. Pure logic, no
//! clock. Inside, distances are measured from the edge, so "in" is positive.
//!
//! The edge is the one the home button rests on (Andrea, 2026-11-04: the indicator bar and the
//! swipe follow the button; bottom by default): from the top edge the swipe goes down, from the
//! left and right edges sideways.

use super::controls::Edge;

/// What the caller does with one touch event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Route {
    /// Pass it to the page as it is.
    Page,
    /// Nothing for the page now (held, or part of the swipe).
    Hold,
    /// Pass a down at (x, y) to the page first, then this event.
    PageFrom { x: f32, y: f32 },
    /// Pass a down and an up at (x, y) to the page (a late tap); the event itself is consumed.
    Tap { x: f32, y: f32 },
    /// The swipe is complete: go home (the page saw nothing).
    Home,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// Started in the strip, not decided yet.
    Held {
        x: f32,
        y: f32,
    },
    /// Going up: the pill is shown.
    Pulling {
        y0: f32,
    },
    /// Handed to the page.
    Page,
}

/// The touch kinds the gesture needs (the body's own touch reader has one point).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Down,
    Move,
    Up,
}

#[derive(Debug, Clone)]
pub struct EdgeSwipe {
    /// The edge the swipe starts from.
    side: Edge,
    /// Width and height of the view (view pixels).
    pub width: f32,
    pub height: f32,
    /// Height of the strip (view pixels).
    pub edge: f32,
    /// How far up a swipe must go.
    pub threshold: f32,
    /// Movement below which a touch is still a tap.
    pub slop: f32,
    phase: Phase,
    /// How far up the finger is (0 when not pulling), for the pill.
    pull: f32,
}

impl Default for EdgeSwipe {
    fn default() -> Self {
        EdgeSwipe::new(800.0, 20.0, 120.0, 16.0)
    }
}

impl EdgeSwipe {
    /// The edge the swipe starts from (the view is 1280 x `height`).
    pub fn set_side(&mut self, side: Edge) {
        if self.side != side {
            self.side = side;
            self.reset();
        }
    }

    pub fn side(&self) -> Edge {
        self.side
    }

    /// Screen → (along the edge, in from the edge).
    fn edge_uv(&self, x: f32, y: f32) -> (f32, f32) {
        match self.side {
            Edge::Bottom => (x, self.height - y),
            Edge::Top => (x, y),
            Edge::Left => (y, x),
            Edge::Right => (y, self.width - x),
        }
    }

    /// (along, in) → screen.
    fn screen_xy(&self, u: f32, v: f32) -> (f32, f32) {
        match self.side {
            Edge::Bottom => (u, self.height - v),
            Edge::Top => (u, v),
            Edge::Left => (v, u),
            Edge::Right => (self.width - v, u),
        }
    }

    pub fn new(height: f32, edge: f32, threshold: f32, slop: f32) -> EdgeSwipe {
        EdgeSwipe {
            side: Edge::Bottom,
            width: 1280.0,
            height,
            edge,
            threshold,
            slop,
            phase: Phase::Idle,
            pull: 0.0,
        }
    }

    /// How far the finger has come up while pulling (0 = no pill).
    pub fn pull(&self) -> f32 {
        self.pull
    }

    /// Letting go now would go home.
    pub fn armed(&self) -> bool {
        self.pull >= self.threshold
    }

    /// Forget any touch in progress (the view was hidden).
    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
        self.pull = 0.0;
    }

    pub fn event(&mut self, kind: Kind, x: f32, y: f32) -> Route {
        // From here on `y` is the distance from the bottom edge (up = more).
        let (x, y) = self.edge_uv(x, y);
        match (self.phase, kind) {
            (_, Kind::Down) => {
                self.pull = 0.0;
                if y < self.edge {
                    self.phase = Phase::Held { x, y };
                    Route::Hold
                } else {
                    self.phase = Phase::Page;
                    Route::Page
                }
            }
            (Phase::Held { x: x0, y: y0 }, Kind::Move) => {
                let (dx, dy) = (x - x0, y - y0);
                if dy >= self.slop && dy >= dx.abs() {
                    self.phase = Phase::Pulling { y0 };
                    self.pull = dy;
                    Route::Hold
                } else if dx.abs() >= self.slop || -dy >= self.slop {
                    self.phase = Phase::Page;
                    let (x, y) = self.screen_xy(x0, y0);
                    Route::PageFrom { x, y }
                } else {
                    Route::Hold
                }
            }
            (Phase::Held { x: x0, y: y0 }, Kind::Up) => {
                self.phase = Phase::Idle;
                let (x, y) = self.screen_xy(x0, y0);
                Route::Tap { x, y }
            }
            (Phase::Pulling { y0 }, Kind::Move) => {
                self.pull = (y - y0).max(0.0);
                Route::Hold
            }
            (Phase::Pulling { y0 }, Kind::Up) => {
                let home = y - y0 >= self.threshold;
                self.reset();
                if home { Route::Home } else { Route::Hold }
            }
            (Phase::Page, Kind::Up) => {
                self.phase = Phase::Idle;
                Route::Page
            }
            (Phase::Page, Kind::Move) => Route::Page,
            // A move or up without a down we saw (the view was shown mid-touch): not ours.
            (Phase::Idle, _) => Route::Hold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swipe_up_from_the_bottom_edge_goes_home() {
        let mut g = EdgeSwipe::default();
        assert_eq!(g.event(Kind::Down, 600.0, 795.0), Route::Hold);
        assert_eq!(g.event(Kind::Move, 602.0, 760.0), Route::Hold);
        assert!(g.pull() > 30.0 && !g.armed());
        assert_eq!(g.event(Kind::Move, 605.0, 660.0), Route::Hold);
        assert!(g.armed());
        assert_eq!(g.event(Kind::Up, 605.0, 660.0), Route::Home);
        assert_eq!(g.pull(), 0.0);
    }

    #[test]
    fn short_pull_does_nothing() {
        let mut g = EdgeSwipe::default();
        g.event(Kind::Down, 600.0, 795.0);
        g.event(Kind::Move, 600.0, 720.0);
        assert_eq!(g.event(Kind::Up, 600.0, 720.0), Route::Hold);
        assert_eq!(g.pull(), 0.0);
    }

    #[test]
    fn tap_in_the_strip_reaches_the_page_late() {
        let mut g = EdgeSwipe::default();
        assert_eq!(g.event(Kind::Down, 30.0, 790.0), Route::Hold);
        assert_eq!(g.event(Kind::Move, 33.0, 786.0), Route::Hold);
        assert_eq!(
            g.event(Kind::Up, 33.0, 786.0),
            Route::Tap { x: 30.0, y: 790.0 }
        );
    }

    #[test]
    fn sideways_drag_from_the_strip_goes_to_the_page() {
        let mut g = EdgeSwipe::default();
        g.event(Kind::Down, 300.0, 790.0);
        assert_eq!(
            g.event(Kind::Move, 330.0, 788.0),
            Route::PageFrom { x: 300.0, y: 790.0 }
        );
        assert_eq!(g.event(Kind::Move, 360.0, 788.0), Route::Page);
        assert_eq!(g.event(Kind::Up, 360.0, 788.0), Route::Page);
    }

    #[test]
    fn touches_above_the_strip_are_the_page_s() {
        // Home Assistant's sidebar bottom entries (profile, notifications) and anything else.
        let mut g = EdgeSwipe::default();
        assert_eq!(g.event(Kind::Down, 40.0, 760.0), Route::Page);
        assert_eq!(g.event(Kind::Up, 40.0, 760.0), Route::Page);
        assert_eq!(g.event(Kind::Down, 300.0, 400.0), Route::Page);
        assert_eq!(g.event(Kind::Move, 300.0, 100.0), Route::Page);
        assert_eq!(g.event(Kind::Up, 300.0, 100.0), Route::Page);
        assert_eq!(g.pull(), 0.0);
        // A move without a down (shown mid-touch) is dropped.
        assert_eq!(g.event(Kind::Move, 300.0, 300.0), Route::Hold);
        // The top edge is the page's now.
        assert_eq!(g.event(Kind::Down, 600.0, 5.0), Route::Page);
    }

    #[test]
    fn off_takes_no_touch() {
        let mut g = EdgeSwipe::new(800.0, -1.0, 120.0, 16.0);
        assert_eq!(g.event(Kind::Down, 600.0, 799.0), Route::Page);
    }

    #[test]
    fn the_swipe_follows_the_button_to_its_edge() {
        let mut g = EdgeSwipe::default();
        g.set_side(Edge::Right);
        // From the right edge leftwards.
        assert_eq!(g.event(Kind::Down, 1275.0, 400.0), Route::Hold);
        g.event(Kind::Move, 1200.0, 402.0);
        g.event(Kind::Move, 1100.0, 405.0);
        assert!(g.armed());
        assert_eq!(g.event(Kind::Up, 1100.0, 405.0), Route::Home);
        // The bottom strip is the page's now.
        assert_eq!(g.event(Kind::Down, 600.0, 795.0), Route::Page);
        g.event(Kind::Up, 600.0, 795.0);
        // A tap in the left strip reaches the page late, where it was.
        g.set_side(Edge::Left);
        g.event(Kind::Down, 5.0, 300.0);
        assert_eq!(
            g.event(Kind::Up, 5.0, 300.0),
            Route::Tap { x: 5.0, y: 300.0 }
        );
        g.set_side(Edge::Top);
        g.event(Kind::Down, 640.0, 4.0);
        g.event(Kind::Move, 641.0, 80.0);
        assert_eq!(g.event(Kind::Up, 641.0, 140.0), Route::Home);
    }
}
