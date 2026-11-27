// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The real finger and the remote's touches (crate::panel::remote) on one window: the renderer's
//! loop asks [`Arbiter`] what to dispatch. Pure, so the rules are tested:
//!
//! - the real finger wins: while it is down, and [`REAL_WINS_FOR`] after it lifts, the remote's
//!   touches are dropped; a remote press held when the finger comes down is released first;
//! - a real touch that starts on the sign «Controlled by …» never reaches the screen under it
//!   (Home Assistant's view included): it ends the session, and the rest of that touch is eaten;
//! - each source keeps its own pressed state: an Up is dispatched only after that source's Down;
//!   a remote Down while the remote's press is held is dropped;
//! - a remote touch is dispatched only if the session that sent it is still on and the screen
//!   may be touched at that moment ([`Arbiter::remote`]'s `allowed`, checked at dispatch time);
//! - when the session ends with a remote press held, an Up is made for it ([`Arbiter::idle`]).

use std::time::{Duration, Instant};

use super::touch::{TouchEvent, TouchKind};

/// The remote waits this long after the real finger lifts.
pub const REAL_WINS_FOR: Duration = Duration::from_millis(500);
/// A remote press with nothing from the remote for this long is released where it is: its Up
/// was lost (a batch the panel refused, the network), and the next click's Down would be dropped.
pub const REMOTE_LONGEST_PRESS: Duration = Duration::from_secs(10);

/// The remote's touches waiting for the renderer's loop: two full requests of the panel. The
/// loop takes one tap per round, the browser may send faster: beyond this a request is refused
/// whole (the panel answers `slow_down`), never cut in the middle of a press.
pub const REMOTE_QUEUE: usize = 2 * crate::panel::remote::MAX_EVENTS;

/// The queue between the panel's requests and the renderer's loop (both on the screen's
/// thread), each touch with its session's number.
#[derive(Clone, Default)]
pub struct TouchQueue(
    std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<(u64, TouchEvent)>>>,
);

impl TouchQueue {
    /// Queues all of `events`, or none when they do not fit in [`REMOTE_QUEUE`].
    pub fn push_all(&self, events: &[(u64, TouchEvent)]) -> bool {
        let mut q = self.0.borrow_mut();
        if q.len() + events.len() > REMOTE_QUEUE {
            return false;
        }
        q.extend(events.iter().copied());
        true
    }

    /// Drops what is queued (the session it belonged to ended).
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    pub fn pop(&self) -> Option<(u64, TouchEvent)> {
        self.0.borrow_mut().pop_front()
    }

    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
}

/// What the loop does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Out {
    /// Dispatch this touch (Home Assistant's view first, then the window), as before.
    Touch(TouchEvent),
    /// The real finger touched Stop on the sign: end the session now.
    EndSession,
}

#[derive(Debug, Default)]
pub struct Arbiter {
    real_down: bool,
    real_up_at: Option<Instant>,
    /// The real touch began on the sign: the rest of it goes nowhere.
    on_sign: bool,
    /// Where the remote's press is, while held.
    remote_held: Option<TouchEvent>,
    /// The remote's last touch of that press.
    remote_seen: Option<Instant>,
}

impl Arbiter {
    /// A touch of the real finger; `on_sign`: its Down falls on the sign.
    pub fn real(&mut self, ev: TouchEvent, on_sign: bool, now: Instant) -> Vec<Out> {
        let mut out = Vec::new();
        match ev.kind {
            TouchKind::Down => {
                if self.real_down {
                    // A Down without the Up before it: as a new touch.
                    return vec![Out::Touch(ev)];
                }
                self.real_down = true;
                if let Some(held) = self.remote_held.take() {
                    out.push(Out::Touch(TouchEvent {
                        kind: TouchKind::Up,
                        ..held
                    }));
                }
                if on_sign {
                    self.on_sign = true;
                    out.push(Out::EndSession);
                } else {
                    out.push(Out::Touch(ev));
                }
            }
            TouchKind::Move => {
                if self.real_down && !self.on_sign {
                    out.push(Out::Touch(ev));
                }
            }
            TouchKind::Up => {
                if self.real_down && !self.on_sign {
                    out.push(Out::Touch(ev));
                }
                self.real_down = false;
                self.on_sign = false;
                self.real_up_at = Some(now);
            }
        }
        out
    }

    /// A touch of the remote; `allowed`: its session is the one on now and the screen may be
    /// touched (checked when it is dispatched, not when it was sent).
    pub fn remote(&mut self, ev: TouchEvent, allowed: bool, now: Instant) -> Vec<Out> {
        let real_wins = self.real_down
            || self
                .real_up_at
                .is_some_and(|t| now.saturating_duration_since(t) < REAL_WINS_FOR);
        if !allowed || real_wins {
            // Dropped; a press it left held is released (once the finger is gone).
            return if !allowed { self.idle() } else { Vec::new() };
        }
        match ev.kind {
            // A Down while the remote's press is held: dropped (a browser never sends it). Made
            // into Up + Down it would be a whole tap, and a batch of them could answer a
            // question its own first tap opened.
            TouchKind::Down if self.remote_held.is_some() => Vec::new(),
            TouchKind::Down => {
                self.remote_held = Some(ev);
                self.remote_seen = Some(now);
                vec![Out::Touch(ev)]
            }
            TouchKind::Move => match &mut self.remote_held {
                Some(h) => {
                    h.x = ev.x;
                    h.y = ev.y;
                    self.remote_seen = Some(now);
                    vec![Out::Touch(ev)]
                }
                None => Vec::new(),
            },
            TouchKind::Up => match self.remote_held.take() {
                Some(_) => vec![Out::Touch(ev)],
                None => Vec::new(),
            },
        }
    }

    /// No session (any more): a remote press still held is released where it was.
    pub fn idle(&mut self) -> Vec<Out> {
        match self.remote_held.take() {
            Some(h) if !self.real_down => vec![Out::Touch(TouchEvent {
                kind: TouchKind::Up,
                ..h
            })],
            _ => Vec::new(),
        }
    }

    /// A remote press with no touch of the remote for [`REMOTE_LONGEST_PRESS`]: released where
    /// it is (as [`Arbiter::idle`]).
    pub fn release_stale(&mut self, now: Instant) -> Vec<Out> {
        let stale = self
            .remote_seen
            .is_some_and(|t| now.saturating_duration_since(t) >= REMOTE_LONGEST_PRESS);
        if self.remote_held.is_some() && stale {
            self.idle()
        } else {
            Vec::new()
        }
    }

    pub fn remote_held(&self) -> bool {
        self.remote_held.is_some()
    }

    /// One round of the remote's touches: takes them from `next` (each with its `allowed`,
    /// as for [`Arbiter::remote`]) until what was taken lifts a press. Returns what to
    /// dispatch and whether a press was lifted (a tap: the loop then waits before the next
    /// round). What `next` still holds waits for a later round.
    pub fn remote_round(
        &mut self,
        mut next: impl FnMut() -> Option<(TouchEvent, bool)>,
        now: Instant,
    ) -> (Vec<Out>, bool) {
        let mut outs = Vec::new();
        while let Some((ev, allowed)) = next() {
            let got = self.remote(ev, allowed, now);
            let lifted = got
                .iter()
                .any(|o| matches!(o, Out::Touch(e) if e.kind == TouchKind::Up));
            outs.extend(got);
            if lifted {
                return (outs, true);
            }
        }
        (outs, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(kind: TouchKind, x: f32) -> TouchEvent {
        TouchEvent { kind, x, y: 10.0 }
    }

    fn touches(o: &[Out]) -> Vec<(TouchKind, f32)> {
        o.iter()
            .filter_map(|o| match o {
                Out::Touch(e) => Some((e.kind, e.x)),
                Out::EndSession => None,
            })
            .collect()
    }

    #[test]
    fn the_real_finger_wins() {
        let mut a = Arbiter::default();
        let now = Instant::now();
        // The remote presses; the real finger comes down: the remote's press is released first.
        assert_eq!(
            touches(&a.remote(t(TouchKind::Down, 1.0), true, now)),
            [(TouchKind::Down, 1.0)]
        );
        let o = a.real(t(TouchKind::Down, 5.0), false, now);
        assert_eq!(touches(&o), [(TouchKind::Up, 1.0), (TouchKind::Down, 5.0)]);
        // A flood from the remote meanwhile: nothing of it.
        for k in [
            TouchKind::Down,
            TouchKind::Move,
            TouchKind::Up,
            TouchKind::Down,
        ] {
            assert!(a.remote(t(k, 2.0), true, now).is_empty());
        }
        // The real Up is the real one's.
        assert_eq!(
            touches(&a.real(t(TouchKind::Up, 6.0), false, now)),
            [(TouchKind::Up, 6.0)]
        );
        // Half a second more for the finger.
        assert!(
            a.remote(
                t(TouchKind::Down, 2.0),
                true,
                now + Duration::from_millis(100)
            )
            .is_empty()
        );
        let later = now + REAL_WINS_FOR;
        assert_eq!(
            touches(&a.remote(t(TouchKind::Down, 3.0), true, later)),
            [(TouchKind::Down, 3.0)]
        );
    }

    #[test]
    fn a_remote_up_never_lifts_a_real_press() {
        let mut a = Arbiter::default();
        let now = Instant::now();
        a.real(t(TouchKind::Down, 5.0), false, now);
        assert!(
            a.remote(t(TouchKind::Up, 1.0), true, now + Duration::from_secs(2))
                .is_empty()
        );
        // A remote Up without its Down: nothing either, finger or not.
        a.real(t(TouchKind::Up, 5.0), false, now);
        assert!(
            a.remote(t(TouchKind::Up, 1.0), true, now + Duration::from_secs(2))
                .is_empty()
        );
        assert!(
            a.remote(t(TouchKind::Move, 1.0), true, now + Duration::from_secs(2))
                .is_empty()
        );
    }

    #[test]
    fn stop_on_the_sign_goes_nowhere_else() {
        let mut a = Arbiter::default();
        let now = Instant::now();
        let o = a.real(t(TouchKind::Down, 5.0), true, now);
        assert_eq!(o, [Out::EndSession]);
        assert!(a.real(t(TouchKind::Move, 6.0), false, now).is_empty());
        assert!(a.real(t(TouchKind::Up, 6.0), false, now).is_empty());
        // The next touch is an ordinary one.
        assert_eq!(
            touches(&a.real(t(TouchKind::Down, 7.0), false, now)),
            [(TouchKind::Down, 7.0)]
        );
    }

    #[test]
    fn downs_in_a_row_are_one_press() {
        let mut a = Arbiter::default();
        let now = Instant::now();
        let mut o = Vec::new();
        for x in [1.0, 2.0, 3.0] {
            o.extend(a.remote(t(TouchKind::Down, x), true, now));
        }
        assert_eq!(touches(&o), [(TouchKind::Down, 1.0)]);
        // Its own Up lifts it where it was pressed.
        assert_eq!(
            touches(&a.remote(t(TouchKind::Up, 1.0), true, now)),
            [(TouchKind::Up, 1.0)]
        );
    }

    #[test]
    fn a_lost_up_does_not_eat_the_next_click() {
        let mut a = Arbiter::default();
        let now = Instant::now();
        a.remote(t(TouchKind::Down, 1.0), true, now);
        // A drag keeps its press however long it lasts, as long as it moves.
        let later = now + REMOTE_LONGEST_PRESS - Duration::from_secs(1);
        a.remote(t(TouchKind::Move, 2.0), true, later);
        assert!(a.release_stale(now + REMOTE_LONGEST_PRESS).is_empty());
        // Then nothing more from the remote (its Up was lost): released where it was.
        let stale = later + REMOTE_LONGEST_PRESS;
        assert_eq!(touches(&a.release_stale(stale)), [(TouchKind::Up, 2.0)]);
        assert!(!a.remote_held());
        assert!(a.release_stale(stale).is_empty());
        // The next click is a whole click.
        assert_eq!(
            touches(&a.remote(t(TouchKind::Down, 5.0), true, stale)),
            [(TouchKind::Down, 5.0)]
        );
        assert_eq!(
            touches(&a.remote(t(TouchKind::Up, 5.0), true, stale)),
            [(TouchKind::Up, 5.0)]
        );
    }

    #[test]
    fn a_round_ends_at_the_first_tap() {
        let mut a = Arbiter::default();
        let now = Instant::now();
        // A tile, then twice where «Yes» would appear, in one batch.
        let mut queue: std::collections::VecDeque<_> = [
            t(TouchKind::Down, 1.0),
            t(TouchKind::Up, 1.0),
            t(TouchKind::Down, 2.0),
            t(TouchKind::Up, 2.0),
            t(TouchKind::Down, 2.0),
            t(TouchKind::Up, 2.0),
        ]
        .into();
        let (o, tapped) = a.remote_round(|| queue.pop_front().map(|e| (e, true)), now);
        assert!(tapped);
        assert_eq!(touches(&o), [(TouchKind::Down, 1.0), (TouchKind::Up, 1.0)]);
        assert_eq!(queue.len(), 4, "the rest waits for a later round");
        // Downs only: one press, no tap, nothing left behind it.
        let mut a = Arbiter::default();
        let mut queue: std::collections::VecDeque<_> = [
            t(TouchKind::Down, 1.0),
            t(TouchKind::Down, 2.0),
            t(TouchKind::Down, 2.0),
        ]
        .into();
        let (o, tapped) = a.remote_round(|| queue.pop_front().map(|e| (e, true)), now);
        assert!(!tapped);
        assert_eq!(touches(&o), [(TouchKind::Down, 1.0)]);
        // A session that ended mid-press: the release made for it ends the round too.
        let mut queue: std::collections::VecDeque<_> =
            [t(TouchKind::Move, 4.0), t(TouchKind::Down, 5.0)].into();
        let (o, tapped) = a.remote_round(|| queue.pop_front().map(|e| (e, false)), now);
        assert!(tapped);
        assert_eq!(touches(&o), [(TouchKind::Up, 1.0)]);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn a_session_that_ends_releases_its_press() {
        let mut a = Arbiter::default();
        let now = Instant::now();
        a.remote(t(TouchKind::Down, 1.0), true, now);
        a.remote(t(TouchKind::Move, 4.0), true, now);
        // Queued touches of a session that ended: dropped, and the press released where it was.
        let o = a.remote(t(TouchKind::Move, 9.0), false, now);
        assert_eq!(touches(&o), [(TouchKind::Up, 4.0)]);
        assert!(!a.remote_held());
        assert!(a.idle().is_empty());
        // Ended with nothing queued: idle releases it.
        a.remote(t(TouchKind::Down, 2.0), true, now);
        assert_eq!(touches(&a.idle()), [(TouchKind::Up, 2.0)]);
    }

    #[test]
    fn the_queue_takes_whole_requests_up_to_its_size() {
        let q = TouchQueue::default();
        let req = |n: usize| vec![(1u64, t(TouchKind::Move, 0.0)); n];
        assert!(q.push_all(&req(32)));
        assert!(q.push_all(&req(30)));
        // Three more do not fit: none of them is queued.
        assert!(!q.push_all(&req(3)));
        assert_eq!(q.len(), 62);
        assert!(q.push_all(&req(2)));
        assert_eq!(q.len(), REMOTE_QUEUE);
        assert!(!q.push_all(&req(1)));
        // The loop takes them in order; room again.
        assert!(q.pop().is_some());
        assert!(q.push_all(&req(1)));
        while q.pop().is_some() {}
        assert!(q.is_empty());
        assert!(q.push_all(&req(5)));
        q.clear();
        assert!(q.is_empty());
    }
}
