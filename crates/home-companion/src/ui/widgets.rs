// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The home's widget: one panel under the cards that takes the slots the cards leave free and
//! turns, page after page, through what is around the house (what comes next, the weather, the
//! shopping). Pure, the twin of `cards.rs`: [`pages`] builds the pages that have something to
//! say, [`fit`] cuts one to the free slots, [`Band`] decides when the screen changes.
//!
//! The rules (docs/ui.md, "Widget"):
//! - slots: 3 free (4 on the compact home) = a header and 4 rows (6), 2 = a header and 2 rows,
//!   1 = one row as tall as a card, without header; 0 = no widget. Rows that do not fit are cut,
//!   never shrunk;
//! - a page with nothing to say is not in the turn; at most [`MAX_PAGES`];
//! - the page on screen is a photograph: new data shows when the page comes back (or once a
//!   minute, [`Band::tick`] with `minute`), so a sensor that changes every second does not redraw
//!   the home every second;
//! - the turn is driven by the body's one-second tick, never by an animation of the screen, and
//!   it stands still while the home is not on screen; after a swipe or a tap it waits
//!   [`HOLD`] before turning again.

use std::time::{Duration, Instant};

use super::lists::{self, ShopList};
use super::next::NextItem;

/// Pages in the turn at most.
pub const MAX_PAGES: usize = 5;
/// After a swipe or a tap the turn waits this long (who chose a page wants to read it).
pub const HOLD: Duration = Duration::from_secs(60);
/// The turn's default interval.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(20);
/// The choices of Settings → Screen → Widgets, "Change every": seconds, 0 = by hand only.
pub const EVERY_CHOICES: [u32; 4] = [20, 30, 60, 0];

/// What a page is about. The order is the turn's (fixed in this first version).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Reminders and calendar events of today and tomorrow.
    Next,
    /// The next hours (or days) of the weather.
    Weather,
    /// The shopping list.
    Shopping,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Next, Kind::Weather, Kind::Shopping];

    /// The key in settings.toml and the screen's `widget-kind`.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Next => "next",
            Kind::Weather => "weather",
            Kind::Shopping => "shopping",
        }
    }

    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }
}

/// A row of a page. For the weather's columns (`WidgetView::columns`): `left` is the hour (or
/// the day), `note` the condition (the icon), `text` the temperature.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WidgetRow {
    /// The left column (a time, "tomorrow"), tabular, gold.
    pub left: String,
    pub text: String,
    /// The right column (a time, a count), tabular.
    pub right: String,
    /// A word for the screen that is not text (the weather's condition).
    pub note: String,
}

/// A page of the widget.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetView {
    pub kind: Kind,
    pub title: String,
    /// The header's right side: a count, or the time of data that is old ("" none).
    pub aside: String,
    pub rows: Vec<WidgetRow>,
    /// The rows are columns side by side (the weather).
    pub columns: bool,
    /// The one row shown when only one slot is free (None: the first row).
    pub line: Option<WidgetRow>,
}

/// The words of the pages.
pub struct Words<'a> {
    pub next: &'a str,
    pub weather: &'a str,
    pub shopping: &'a str,
    /// "today" and "tomorrow", for all-day events and tomorrow's rows.
    pub today: &'a str,
    pub tomorrow: &'a str,
    /// "{n} to buy".
    pub to_buy: &'a str,
}

/// What may become a page, already in the screen's words where the body formats them.
#[derive(Debug, Default)]
pub struct Sources<'a> {
    /// Reminders and events still to come, soonest first (`next::merge`).
    pub next: &'a [NextItem],
    /// The weather's columns (hour or day, condition, temperature), soonest first.
    pub weather: Vec<WidgetRow>,
    pub shopping: Option<&'a [ShopList]>,
}

/// The pages that have something to say, in the turn's order, among the `enabled` ones.
/// `now` and `local` (unix seconds → ("15:30", local day number)) date the rows of Next.
pub fn pages(
    s: &Sources,
    enabled: &[Kind],
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Vec<WidgetView> {
    let mut out = Vec::new();
    for kind in Kind::ALL {
        if !enabled.contains(&kind) {
            continue;
        }
        let page = match kind {
            Kind::Next => next_page(s.next, w, now, local),
            Kind::Weather => weather_page(&s.weather, w),
            Kind::Shopping => s.shopping.and_then(|l| shopping_page(l, w)),
        };
        out.extend(page);
    }
    out.truncate(MAX_PAGES);
    out
}

/// Today's and tomorrow's reminders and events: the time on the left (all-day: "today"),
/// tomorrow's with "tomorrow" on the left and the time on the right.
fn next_page(
    items: &[NextItem],
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Option<WidgetView> {
    let today = local(now).1;
    let rows: Vec<WidgetRow> = items
        .iter()
        .filter_map(|i| {
            let (hm, day) = local(i.at);
            // An event that began before today (a running all-day one) is today's.
            let offset = (day - today).max(0);
            if offset > 1 {
                return None;
            }
            let (left, right) = match (offset, i.all_day) {
                (0, true) => (w.today.to_string(), String::new()),
                (0, false) => (hm, String::new()),
                (_, true) => (w.tomorrow.to_string(), String::new()),
                (_, false) => (w.tomorrow.to_string(), hm),
            };
            Some(WidgetRow {
                left,
                text: i.text.clone(),
                right,
                note: String::new(),
            })
        })
        .collect();
    let first = rows.first()?.clone();
    let line = (rows.len() > 1).then(|| WidgetRow {
        right: if first.right.is_empty() {
            format!("+{}", rows.len() - 1)
        } else {
            format!("{} · +{}", first.right, rows.len() - 1)
        },
        ..first
    });
    Some(WidgetView {
        kind: Kind::Next,
        title: w.next.to_string(),
        aside: String::new(),
        rows,
        columns: false,
        line,
    })
}

fn weather_page(columns: &[WidgetRow], w: &Words) -> Option<WidgetView> {
    if columns.is_empty() {
        return None;
    }
    Some(WidgetView {
        kind: Kind::Weather,
        title: w.weather.to_string(),
        aside: String::new(),
        rows: columns.iter().take(5).cloned().collect(),
        columns: true,
        line: None,
    })
}

/// "4 to buy" in the header, one item per row; in one row: the first items on one line.
fn shopping_page(lists: &[ShopList], w: &Words) -> Option<WidgetView> {
    let n = lists::count(lists);
    if n == 0 {
        return None;
    }
    let count = w.to_buy.replace("{n}", &n.to_string());
    let rows = lists::first_texts(lists, 6)
        .into_iter()
        .map(|t| WidgetRow {
            text: t.to_string(),
            ..Default::default()
        })
        .collect();
    let more = if n > 3 { " …" } else { "" };
    Some(WidgetView {
        kind: Kind::Shopping,
        title: w.shopping.to_string(),
        aside: count.clone(),
        rows,
        columns: false,
        line: Some(WidgetRow {
            text: format!("{}{more}", lists::first_texts(lists, 3).join(" · ")),
            right: count,
            ..Default::default()
        }),
    })
}

/// Rows a page shows with `slots` free slots (`compact`: the band has four).
pub fn rows_for(slots: usize) -> usize {
    match slots {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => 4,
        _ => 6,
    }
}

/// A page cut to `slots` free slots; None with none free. One slot: one row, no header (the
/// screen draws it as a card).
pub fn fit(view: &WidgetView, slots: usize) -> Option<WidgetView> {
    if slots == 0 {
        return None;
    }
    let mut v = view.clone();
    if v.columns {
        // The columns are one row: they fit in every size.
        return Some(v);
    }
    if slots == 1 {
        v.rows = vec![v.line.clone().or_else(|| v.rows.first().cloned())?];
    } else {
        v.rows.truncate(rows_for(slots));
    }
    Some(v)
}

/// What the screen shows of the widget: the page cut to the slots, where it is in the turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    /// None: no widget (no free slot, or nothing to say).
    pub view: Option<WidgetView>,
    pub slots: usize,
    /// The page's place in the turn and how many there are (the dots).
    pub index: usize,
    pub count: usize,
    /// Grows by one at every change of page (the screen fades the new page in), not when only
    /// the data of the same page changes.
    pub seq: u32,
}

/// Why the page changed (for the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Tick,
    Swipe,
    Data,
}

impl Why {
    pub fn key(self) -> &'static str {
        match self {
            Why::Tick => "tick",
            Why::Swipe => "swipe",
            Why::Data => "data",
        }
    }
}

/// The widget between the data and the screen: what is on screen, the turn, the hold after a
/// gesture. Every method returns `Some` only when the screen has to change, so nothing is
/// written (and nothing redrawn) between two changes.
#[derive(Debug)]
pub struct Band {
    /// The latest pages, fresh data.
    latest: Vec<WidgetView>,
    /// The page on screen, as it was taken (not cut), and the slots it is cut to.
    current: Option<WidgetView>,
    slots: usize,
    since: Instant,
    hold_until: Option<Instant>,
    seq: u32,
    /// The last tick was paused or held: the next one starts a new interval.
    held: bool,
    /// What the screen was last told (to say nothing twice).
    told: Option<Shown>,
}

impl Band {
    pub fn new(now: Instant) -> Band {
        Band {
            latest: Vec::new(),
            current: None,
            slots: 0,
            since: now,
            hold_until: None,
            seq: 0,
            held: false,
            told: None,
        }
    }

    /// The kind of the page on screen.
    pub fn kind(&self) -> Option<Kind> {
        self.current.as_ref().map(|v| v.kind)
    }

    fn position(&self) -> Option<usize> {
        let k = self.kind()?;
        self.latest.iter().position(|v| v.kind == k)
    }

    fn take(&mut self, index: usize, now: Instant) {
        let changed = self.kind() != self.latest.get(index).map(|v| v.kind);
        self.current = self.latest.get(index).cloned();
        if changed {
            self.seq = self.seq.wrapping_add(1);
            self.since = now;
        }
    }

    fn tell(&mut self) -> Option<Shown> {
        let s = Shown {
            view: self.current.as_ref().and_then(|v| fit(v, self.slots)),
            slots: self.slots,
            index: self.position().unwrap_or(0),
            count: self.latest.len(),
            seq: self.seq,
        };
        // Without a widget the rest does not matter.
        let same = match (&self.told, &s.view) {
            (Some(t), None) => t.view.is_none(),
            (Some(t), Some(_)) => *t == s,
            (None, _) => false,
        };
        if same {
            return None;
        }
        self.told = Some(s.clone());
        Some(s)
    }

    /// New pages (any data changed) and the slots the cards leave free. The page on screen keeps
    /// its data; it changes only if it is gone, if there was none, or if the slots changed.
    pub fn offer(&mut self, pages: Vec<WidgetView>, slots: usize, now: Instant) -> Option<Shown> {
        let before = self.position();
        self.latest = pages;
        self.slots = slots;
        match (self.kind(), self.position()) {
            // Still there: the photograph stays, its place may have moved.
            (Some(_), Some(_)) => {}
            (Some(_), None) => {
                // Gone: the page that came after it (the first if it was the last).
                let next = before.unwrap_or(0).min(self.latest.len().saturating_sub(1));
                self.take(next, now);
            }
            (None, _) => self.take(0, now),
        }
        self.tell().map(|s| self.log(s, Why::Data))
    }

    /// The one-second tick. `paused`: the home is not on screen (another page, the Home
    /// Assistant view, a conversation, a ring, the night, the screen off...): nothing changes
    /// and the page, when the home is back, stays a whole interval. `every`: None = by hand
    /// only. `minute`: the clock's minute changed: the page on screen takes its fresh data.
    pub fn tick(
        &mut self,
        now: Instant,
        paused: bool,
        every: Option<Duration>,
        minute: bool,
    ) -> Option<Shown> {
        if paused || self.hold_until.is_some_and(|t| now < t) {
            // The interval starts again when the home (or the turn) is back.
            self.since = now;
            self.held = true;
            return None;
        }
        self.hold_until = None;
        if std::mem::take(&mut self.held) {
            self.since = now;
        }
        let n = self.latest.len();
        if let Some(every) = every
            && n > 1
            && now.duration_since(self.since) >= every
        {
            let i = self.position().map_or(0, |i| (i + 1) % n);
            self.take(i, now);
            return self.tell().map(|s| self.log(s, Why::Tick));
        }
        if minute && let Some(i) = self.position() {
            self.current = self.latest.get(i).cloned();
            return self.tell().map(|s| self.log(s, Why::Data));
        }
        None
    }

    /// A swipe on the widget: `dir` +1 the next page, -1 the previous one; then the hold.
    pub fn swipe(&mut self, dir: i32, now: Instant) -> Option<Shown> {
        self.hold_until = Some(now + HOLD);
        let n = self.latest.len() as i64;
        if n < 2 {
            return None;
        }
        let i = self.position().map_or(0, |i| i as i64);
        self.take((i + dir.signum() as i64).rem_euclid(n) as usize, now);
        self.tell().map(|s| self.log(s, Why::Swipe))
    }

    /// A tap on the widget (it opens the page's own page): the kind, and the hold.
    pub fn tap(&mut self, now: Instant) -> Option<Kind> {
        self.hold_until = Some(now + HOLD);
        self.kind()
    }

    fn log(&self, s: Shown, why: Why) -> Shown {
        if let Some(v) = &s.view {
            log::info!(event = "widget", page = v.kind.key(), why = why.key(), slots = s.slots;
                "widget: {} ({}/{}, {} slots, {})", v.kind.key(), s.index + 1, s.count, s.slots, why.key());
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86400;
    const NOW: i64 = 20 * DAY + 10 * 3600;

    fn local(t: i64) -> (String, i64) {
        let s = t.rem_euclid(DAY);
        (
            format!("{:02}:{:02}", s / 3600, s % 3600 / 60),
            t.div_euclid(DAY),
        )
    }

    fn words() -> Words<'static> {
        Words {
            next: "Next",
            weather: "Weather",
            shopping: "Shopping",
            today: "today",
            tomorrow: "tomorrow",
            to_buy: "{n} to buy",
        }
    }

    fn item(at: i64, all_day: bool, text: &str) -> NextItem {
        NextItem {
            at,
            all_day,
            text: text.into(),
            event: true,
            calendar: String::new(),
        }
    }

    fn view(kind: Kind, rows: usize) -> WidgetView {
        WidgetView {
            kind,
            title: kind.key().into(),
            aside: String::new(),
            rows: (0..rows)
                .map(|i| WidgetRow {
                    text: format!("{} {i}", kind.key()),
                    ..Default::default()
                })
                .collect(),
            columns: false,
            line: None,
        }
    }

    fn shop(items: &[&str]) -> Vec<ShopList> {
        vec![ShopList {
            entity: "todo.shopping".into(),
            name: "Shopping".into(),
            items: items
                .iter()
                .map(|t| (t.to_string(), t.to_string()))
                .collect(),
        }]
    }

    #[test]
    fn next_has_today_and_tomorrow_only() {
        let day = NOW - NOW % DAY;
        let items = vec![
            item(day, true, "Holiday"),
            item(day + 15 * 3600 + 1800, false, "Dentist"),
            item(day + DAY + 9 * 3600, false, "Car service"),
            item(day + DAY, true, "Birthday"),
            item(day + 2 * DAY + 3600, false, "Too far"),
        ];
        let s = Sources {
            next: &items,
            ..Default::default()
        };
        let p = pages(&s, &Kind::ALL, &words(), NOW, &local);
        assert_eq!(p.len(), 1);
        let r: Vec<(&str, &str, &str)> = p[0]
            .rows
            .iter()
            .map(|r| (r.left.as_str(), r.text.as_str(), r.right.as_str()))
            .collect();
        assert_eq!(
            r,
            [
                ("today", "Holiday", ""),
                ("15:30", "Dentist", ""),
                ("tomorrow", "Car service", "09:00"),
                ("tomorrow", "Birthday", ""),
            ]
        );
        // One slot: the first row, and how many more.
        let one = fit(&p[0], 1).unwrap();
        assert_eq!(one.rows.len(), 1);
        assert_eq!(one.rows[0].text, "Holiday");
        assert_eq!(one.rows[0].right, "+3");
    }

    #[test]
    fn empty_pages_are_left_out_and_disabled_ones_too() {
        let empty = shop(&[]);
        let s = Sources {
            shopping: Some(&empty),
            ..Default::default()
        };
        assert!(pages(&s, &Kind::ALL, &words(), NOW, &local).is_empty());
        let full = shop(&["Bread", "Milk", "Eggs", "Rice"]);
        let s = Sources {
            shopping: Some(&full),
            weather: vec![WidgetRow {
                left: "15:00".into(),
                text: "14°".into(),
                note: "sunny".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = pages(&s, &Kind::ALL, &words(), NOW, &local);
        let kinds: Vec<Kind> = p.iter().map(|v| v.kind).collect();
        assert_eq!(kinds, [Kind::Weather, Kind::Shopping]);
        assert_eq!(p[1].aside, "4 to buy");
        let one = fit(&p[1], 1).unwrap();
        assert_eq!(one.rows[0].text, "Bread · Milk · Eggs …");
        assert_eq!(one.rows[0].right, "4 to buy");
        let p = pages(&s, &[Kind::Shopping], &words(), NOW, &local);
        assert_eq!(p.len(), 1);
        assert!(pages(&s, &[], &words(), NOW, &local).is_empty());
    }

    #[test]
    fn slots_cut_rows_never_shrink_them() {
        let v = view(Kind::Next, 8);
        assert!(fit(&v, 0).is_none());
        assert_eq!(fit(&v, 1).unwrap().rows.len(), 1);
        assert_eq!(fit(&v, 2).unwrap().rows.len(), 2);
        assert_eq!(fit(&v, 3).unwrap().rows.len(), 4);
        assert_eq!(fit(&v, 4).unwrap().rows.len(), 6);
        let short = view(Kind::Next, 1);
        assert_eq!(fit(&short, 3).unwrap().rows.len(), 1);
        // The weather's columns are one row: the same in every size.
        let mut w = view(Kind::Weather, 5);
        w.columns = true;
        for slots in 1..=4 {
            assert_eq!(fit(&w, slots).unwrap().rows.len(), 5);
        }
    }

    fn three() -> Vec<WidgetView> {
        vec![
            view(Kind::Next, 2),
            view(Kind::Weather, 2),
            view(Kind::Shopping, 2),
        ]
    }

    const EVERY: Option<Duration> = Some(DEFAULT_EVERY);

    #[test]
    fn the_turn_changes_every_interval_and_says_nothing_between() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut b = Band::new(t0);
        let first = b.offer(three(), 3, t0).unwrap();
        assert_eq!(first.view.as_ref().unwrap().kind, Kind::Next);
        assert_eq!((first.index, first.count), (0, 3));
        // Nothing to write for 19 s: no property set, no frame drawn.
        for n in 1..20 {
            assert!(b.tick(s(n), false, EVERY, false).is_none(), "{n}");
        }
        let second = b.tick(s(20), false, EVERY, false).unwrap();
        assert_eq!(second.view.unwrap().kind, Kind::Weather);
        assert_eq!(second.seq, first.seq + 1);
        let third = b.tick(s(40), false, EVERY, false).unwrap();
        assert_eq!(third.view.unwrap().kind, Kind::Shopping);
        // Round again.
        assert_eq!(
            b.tick(s(60), false, EVERY, false)
                .unwrap()
                .view
                .unwrap()
                .kind,
            Kind::Next
        );
        // By hand only: never by itself.
        for n in 61..200 {
            assert!(b.tick(s(n), false, None, false).is_none());
        }
    }

    #[test]
    fn paused_nothing_changes_and_the_page_stays_a_whole_interval_after() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut b = Band::new(t0);
        b.offer(three(), 3, t0);
        for n in 1..100 {
            assert!(b.tick(s(n), true, EVERY, n % 60 == 0).is_none());
        }
        assert_eq!(b.kind(), Some(Kind::Next));
        // Back on the home at 100 s: the same page, for 20 s more.
        assert!(b.tick(s(100), false, EVERY, false).is_none());
        assert!(b.tick(s(119), false, EVERY, false).is_none());
        assert!(b.tick(s(120), false, EVERY, false).is_some());
    }

    #[test]
    fn one_page_never_turns() {
        let t0 = Instant::now();
        let mut b = Band::new(t0);
        assert!(b.offer(vec![view(Kind::Next, 2)], 3, t0).is_some());
        for n in 1..500 {
            assert!(
                b.tick(t0 + Duration::from_secs(n), false, EVERY, false)
                    .is_none()
            );
        }
        assert!(b.swipe(1, t0 + Duration::from_secs(501)).is_none());
    }

    #[test]
    fn a_swipe_turns_and_holds_the_turn_a_minute() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut b = Band::new(t0);
        b.offer(three(), 3, t0);
        let back = b.swipe(-1, s(5)).unwrap();
        assert_eq!(back.view.unwrap().kind, Kind::Shopping);
        assert_eq!(back.index, 2);
        for n in 6..65 {
            assert!(b.tick(s(n), false, EVERY, false).is_none(), "{n}");
        }
        // The hold is over at 65 s, then a whole interval.
        assert!(b.tick(s(65), false, EVERY, false).is_none());
        assert!(b.tick(s(84), false, EVERY, false).is_none());
        assert_eq!(
            b.tick(s(85), false, EVERY, false)
                .unwrap()
                .view
                .unwrap()
                .kind,
            Kind::Next
        );
        // A tap holds too.
        assert_eq!(b.tap(s(90)), Some(Kind::Next));
        assert!(b.tick(s(149), false, EVERY, false).is_none());
    }

    #[test]
    fn new_data_waits_for_the_page_to_come_back_or_the_minute() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let mut b = Band::new(t0);
        b.offer(three(), 3, t0);
        let mut fresh = three();
        fresh[0].rows[0].text = "changed".into();
        // The page on screen is a photograph.
        assert!(b.offer(fresh.clone(), 3, s(1)).is_none());
        // At the minute it takes the new data, without a fade.
        let m = b.tick(s(2), false, EVERY, true).unwrap();
        assert_eq!(m.view.unwrap().rows[0].text, "changed");
        assert_eq!(m.seq, 1);
        // The same data again: nothing.
        assert!(b.tick(s(3), false, EVERY, true).is_none());
    }

    #[test]
    fn slots_and_gone_pages_apply_at_once() {
        let t0 = Instant::now();
        let mut b = Band::new(t0);
        b.offer(three(), 3, t0);
        // A timer card arrives: two slots left.
        let medium = b.offer(three(), 2, t0).unwrap();
        assert_eq!(medium.slots, 2);
        // Three cards: no widget.
        let none = b.offer(three(), 0, t0).unwrap();
        assert!(none.view.is_none());
        assert!(b.offer(three(), 0, t0).is_none());
        b.offer(three(), 3, t0);
        b.swipe(1, t0);
        assert_eq!(b.kind(), Some(Kind::Weather));
        // The weather is gone: the page after it.
        let left = vec![view(Kind::Next, 2), view(Kind::Shopping, 2)];
        let s = b.offer(left, 3, t0).unwrap();
        assert_eq!(s.view.unwrap().kind, Kind::Shopping);
        // Nothing to say at all: no widget.
        assert!(b.offer(Vec::new(), 3, t0).unwrap().view.is_none());
        assert_eq!(b.kind(), None);
    }
}
