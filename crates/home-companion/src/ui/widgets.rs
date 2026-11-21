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

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::lists::{self, ShopList};
use super::next::NextItem;

/// Pages in the turn at most.
pub const MAX_PAGES: usize = 10;
/// Pages of one kind at most (the kinds that [`Kind::repeats`]): a second markets' page holds
/// more markets, each page with its own list.
pub const MAX_SAME: u8 = 4;
/// A page's name (given from the browser) at most, in characters: it is a title.
pub const MAX_PAGE_NAME: usize = 24;
/// After a swipe or a tap the turn waits this long (who chose a page wants to read it).
pub const HOLD: Duration = Duration::from_secs(60);
/// The turn's default interval.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(20);
/// The choices of Settings → Screen → Widgets, "Change every": seconds, 0 = by hand only.
pub const EVERY_CHOICES: [u32; 4] = [20, 30, 60, 0];
/// Points of a row's curve at most.
pub const CURVE_POINTS: usize = 32;

/// What a page is about. [`Kind::ALL`] is the default order of the turn; the user's order is
/// `[widgets] order` in settings.toml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Reminders and calendar events of today and tomorrow.
    Next,
    /// Stocks, indices, currencies and coins chosen from the browser (feeds/).
    Quotes,
    /// The next hours (or days) of the weather.
    Weather,
    /// The weather of other cities chosen from the browser (feeds/).
    Cities,
    /// The shopping list.
    Shopping,
    /// The notes on the board (the brain).
    Notes,
    /// Home Assistant's sensors chosen from the browser: value and the last 24 hours.
    Sensors,
    /// Home Assistant: doors and windows open, batteries low; only when there is something.
    Check,
    /// Home Assistant: what a chosen media player plays; only while it plays.
    Playing,
    /// Home Assistant: who is home. Off by default (anyone in the room reads it).
    Home,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Next,
        Kind::Quotes,
        Kind::Weather,
        Kind::Cities,
        Kind::Shopping,
        Kind::Notes,
        Kind::Sensors,
        Kind::Check,
        Kind::Playing,
        Kind::Home,
    ];

    /// The pages the first version knew: a `[widgets]` section without `seen` was written by it.
    pub const FIRST: [Kind; 3] = [Kind::Next, Kind::Weather, Kind::Shopping];

    /// The key in settings.toml and the screen's `widget-kind`.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Next => "next",
            Kind::Quotes => "quotes",
            Kind::Weather => "weather",
            Kind::Cities => "cities",
            Kind::Shopping => "shopping",
            Kind::Notes => "notes",
            Kind::Sensors => "sensors",
            Kind::Check => "check",
            Kind::Playing => "playing",
            Kind::Home => "home",
        }
    }

    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }

    /// On unless the user turned it off: every page but who is home.
    pub fn default_on(self) -> bool {
        self != Kind::Home
    }

    /// A tap on the page opens a page of its own (what to check, now playing and who is home
    /// have none: the widget's window already goes through every row).
    pub fn tappable(self) -> bool {
        !matches!(self, Kind::Check | Kind::Playing | Kind::Home)
    }

    /// Its content is a list the user chooses: it can be on the home more than once, each page
    /// with its own list ([`Page`]).
    pub fn repeats(self) -> bool {
        matches!(self, Kind::Quotes | Kind::Cities | Kind::Sensors)
    }

    /// Its rows can be more than the slots: the page shows a window that moves on each time
    /// the page comes back.
    pub fn windowed(self) -> bool {
        matches!(
            self,
            Kind::Quotes | Kind::Sensors | Kind::Notes | Kind::Check | Kind::Cities | Kind::Home
        )
    }
}

/// A page of the turn: its kind and, for the kinds that repeat, which one of them (1 the first,
/// up to [`MAX_SAME`]). Its key is the key in settings.toml of its place in the turn (`order`,
/// `on`) and of its list: "quotes" for the first, "quotes-2" for the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Page {
    pub kind: Kind,
    pub n: u8,
}

/// A kind is its first page.
impl PartialEq<Kind> for Page {
    fn eq(&self, k: &Kind) -> bool {
        self.kind == *k && self.n == 1
    }
}

impl From<Kind> for Page {
    fn from(kind: Kind) -> Page {
        Page { kind, n: 1 }
    }
}

impl Page {
    pub fn new(kind: Kind, n: u8) -> Page {
        Page { kind, n }
    }

    /// The first page of every kind, in the default order.
    pub fn all() -> Vec<Page> {
        Kind::ALL.into_iter().map(Page::from).collect()
    }

    pub fn key(self) -> String {
        if self.n <= 1 {
            self.kind.key().to_string()
        } else {
            format!("{}-{}", self.kind.key(), self.n)
        }
    }

    /// "quotes", "quotes-2"... ("next-2", "quotes-9", "quotes-1": None).
    pub fn from_key(key: &str) -> Option<Page> {
        if let Some(kind) = Kind::from_key(key) {
            return Some(kind.into());
        }
        let (k, n) = key.rsplit_once('-')?;
        let kind = Kind::from_key(k).filter(|k| k.repeats())?;
        let n: u8 = n.parse().ok().filter(|n| (2..=MAX_SAME).contains(n))?;
        (n.to_string() == key[k.len() + 1..]).then_some(Page { kind, n })
    }

    /// Its name for people: the one given, else the kind's word, with its number from the
    /// second on ("Markets", "Markets 2").
    pub fn label(self, word: &str, name: &str) -> String {
        if !name.trim().is_empty() {
            name.trim().to_string()
        } else if self.n <= 1 {
            word.to_string()
        } else {
            format!("{word} {}", self.n)
        }
    }

    /// The first number from 2 that no page of `kind` in `order` has (None: [`MAX_SAME`] there).
    pub fn next_free(kind: Kind, order: &[Page]) -> Option<Page> {
        (2..=MAX_SAME)
            .map(|n| Page { kind, n })
            .find(|p| !order.contains(p))
    }
}

/// The lists of the pages of a kind: page number → its items, in order. The first page's is
/// there even when empty.
#[derive(Debug, Clone, PartialEq)]
pub struct Lists<T>(pub Vec<(u8, Vec<T>)>);

impl<T> Default for Lists<T> {
    fn default() -> Self {
        Lists(vec![(1, Vec::new())])
    }
}

impl<T: Clone> Lists<T> {
    /// One list: the first page's.
    pub fn one(items: Vec<T>) -> Lists<T> {
        Lists(vec![(1, items)])
    }

    pub fn get(&self, n: u8) -> &[T] {
        self.0
            .iter()
            .find(|(m, _)| *m == n)
            .map_or(&[], |(_, v)| v.as_slice())
    }

    pub fn set(&mut self, n: u8, items: Vec<T>) {
        match self.0.iter_mut().find(|(m, _)| *m == n) {
            Some((_, v)) => *v = items,
            None => {
                self.0.push((n, items));
                self.0.sort_by_key(|(m, _)| *m);
            }
        }
    }

    /// A page gone (the first stays, emptied never: it is only turned off).
    pub fn remove(&mut self, n: u8) {
        if n > 1 {
            self.0.retain(|(m, _)| *m != n);
        }
    }

    /// Every page's items once, in the pages' order then the lists' (what the feeds fetch).
    pub fn union<K: PartialEq>(&self, key: impl Fn(&T) -> K) -> Vec<T> {
        let mut out: Vec<T> = Vec::new();
        for x in self.0.iter().flat_map(|(_, v)| v) {
            if !out.iter().any(|o| key(o) == key(x)) {
                out.push(x.clone());
            }
        }
        out
    }

    /// No page has an item.
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|(_, v)| v.is_empty())
    }
}

/// The lists of every page of the kinds that repeat, as settings.toml holds them.
pub struct AllLists<'a> {
    pub quotes: &'a Lists<crate::feeds::QuoteChoice>,
    pub cities: &'a Lists<crate::feeds::CityChoice>,
    pub sensors: &'a Lists<String>,
    /// The names given to pages.
    pub names: &'a [(Page, String)],
}

/// What each page of `order` of the kinds that repeat shows: its items (`sensor_name` names an
/// entity) and its title (`word` gives the kinds' words: `widget_quotes`...).
pub fn picks_of(
    order: &[Page],
    lists: &AllLists,
    sensor_name: &dyn Fn(&str) -> String,
    word: &dyn Fn(&str) -> String,
) -> Vec<Picks> {
    order
        .iter()
        .filter(|p| p.kind.repeats())
        .map(|&p| {
            let pick = |key: String, name: String| Pick { key, name };
            let items: Vec<Pick> = match p.kind {
                Kind::Quotes => lists
                    .quotes
                    .get(p.n)
                    .iter()
                    .map(|q| pick(q.key(), q.name.clone()))
                    .collect(),
                Kind::Cities => lists
                    .cities
                    .get(p.n)
                    .iter()
                    .map(|c| pick(c.key(), c.name.clone()))
                    .collect(),
                _ => lists
                    .sensors
                    .get(p.n)
                    .iter()
                    .map(|id| pick(id.clone(), sensor_name(id)))
                    .collect(),
            };
            let name = lists
                .names
                .iter()
                .find(|(x, _)| *x == p)
                .map_or("", |(_, n)| n.as_str());
            Picks {
                page: p,
                title: p.label(&word(&format!("widget_{}", p.kind.key())), name),
                items,
            }
        })
        .collect()
}

/// What a page of a kind that repeats shows: its items' keys (and names, for the rows still
/// waiting for their data), in order, and its title.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pick {
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Picks {
    pub page: Page,
    pub title: String,
    pub items: Vec<Pick>,
}

/// A row of a page. For the weather's columns (`WidgetView::columns`): `left` is the hour (or
/// the day), `note` the condition (the icon), `text` the temperature.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WidgetRow {
    /// The left column (a time, "tomorrow"), tabular, gold.
    pub left: String,
    pub text: String,
    /// The right column (a time, a count, a value), tabular.
    pub right: String,
    /// A word for the screen that is not text: a weather condition (its icon).
    pub note: String,
    /// After the value: "▲ 1.2%" (empty: none).
    pub change: String,
    /// -1, 0, +1: the change's direction (its colour, on the markets' page only).
    pub trend: i8,
    /// The last hours, normalised 0-1, at most [`CURVE_POINTS`] (empty: no curve).
    pub curve: Vec<f32>,
    /// Chosen but its data not here yet (the browser panel draws "…"; never on the screen).
    pub wait: bool,
}

/// A page of the widget.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetView {
    pub kind: Kind,
    /// Which page of its kind (1 the first; see [`Page`]).
    pub n: u8,
    pub title: String,
    /// The header's right side: a count, or the time of data that is old ("" none).
    pub aside: String,
    pub rows: Vec<WidgetRow>,
    /// The rows are columns side by side (the weather).
    pub columns: bool,
    /// The one row shown when only one slot is free (None: the first row).
    pub line: Option<WidgetRow>,
    /// With more rows than fit, the words of the window shown ("{a}-{b} of {n}"); "" = cut.
    pub window: String,
    /// The weather's big block (now or tomorrow) above its columns; None on other pages.
    pub hero: Option<super::weather::WeatherHero>,
}

impl WidgetView {
    pub fn page(&self) -> Page {
        Page::new(self.kind, self.n)
    }
}

/// The words of the pages.
pub struct Words<'a> {
    pub next: &'a str,
    pub weather: &'a str,
    pub shopping: &'a str,
    pub quotes: &'a str,
    pub cities: &'a str,
    pub notes: &'a str,
    pub sensors: &'a str,
    pub check: &'a str,
    pub playing: &'a str,
    pub home: &'a str,
    /// "today" and "tomorrow", for all-day events and tomorrow's rows.
    pub today: &'a str,
    pub tomorrow: &'a str,
    /// "{n} to buy".
    pub to_buy: &'a str,
    /// "data from {t}": the header of a page whose data is old.
    pub data_from: &'a str,
    /// "{a}-{b} of {n}": the rows shown of more.
    pub window: &'a str,
    /// "open", "{n} open · {b} batteries" (to check).
    pub open: &'a str,
    pub check_line: &'a str,
    /// "home", "away" (who is home; a zone is said by its name).
    pub at_home: &'a str,
    pub away: &'a str,
    /// The decimal separator and the thousands' one ("." and "," in English).
    pub decimal: char,
    pub group: char,
}

/// A number with a curve: a stock, a coin, a sensor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Trend {
    /// What it is chosen by (a market's "yahoo:AAPL", a sensor's entity id).
    pub key: String,
    pub name: String,
    pub value: f64,
    /// Digits after the separator.
    pub decimals: u8,
    /// A currency code ("USD") or a unit ("°C"); "" none.
    pub unit: String,
    /// What the change is against (yesterday's close, 24 hours ago); None: no change.
    pub prev: Option<f64>,
    /// The change in percent (markets) or in the value's unit (sensors).
    pub pct: bool,
    /// The last hours, at most [`CURVE_POINTS`] (empty: no curve), and their true low and high.
    pub points: Vec<f32>,
    pub low: f64,
    pub high: f64,
    /// When the value was read (unix seconds).
    pub at: i64,
    /// Older than this (seconds) the header says when it is from; older than `max_age` the row
    /// is not shown.
    pub fresh: i64,
    pub max_age: i64,
}

/// Another city's weather, already in the screen's words.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CityRow {
    /// What it is chosen by (the city's id).
    pub key: String,
    pub name: String,
    /// The time there now ("15:02").
    pub time: String,
    /// The temperature now ("14°").
    pub temp: String,
    /// The condition (Home Assistant's word, the icon).
    pub cond: String,
    pub at: i64,
    pub fresh: i64,
    pub max_age: i64,
}

/// Something to check in the house.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRow {
    pub name: String,
    /// An opening that is open (None: a battery, with its percent).
    pub battery: Option<u8>,
}

/// What a media player plays.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playing {
    pub title: String,
    pub artist: String,
    pub player: String,
}

/// Where a person is.
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Home,
    Away,
    /// A zone, by its name.
    Zone(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub name: String,
    pub place: Place,
}

/// The data of the pages that come from the browser's choices and Home Assistant, as the screen
/// holds it between two refreshes.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub quotes: Vec<Trend>,
    pub cities: Vec<CityRow>,
    pub notes: Vec<String>,
    pub sensors: Vec<Trend>,
    pub check: Vec<CheckRow>,
    pub playing: Option<Playing>,
    pub people: Vec<Person>,
}

/// What may become a page, already in the screen's words where the body formats them.
#[derive(Debug, Default)]
pub struct Sources<'a> {
    /// Reminders and events still to come, soonest first (`next::merge`).
    pub next: &'a [NextItem],
    /// The weather's columns (hour or day, condition, temperature), soonest first.
    pub weather: Vec<WidgetRow>,
    /// The weather's big block (now, or tomorrow in the evening).
    pub weather_hero: Option<super::weather::WeatherHero>,
    pub shopping: Option<&'a [ShopList]>,
    pub quotes: &'a [Trend],
    pub cities: &'a [CityRow],
    pub notes: &'a [String],
    pub sensors: &'a [Trend],
    pub check: &'a [CheckRow],
    pub playing: Option<&'a Playing>,
    pub people: &'a [Person],
    /// What each page of the kinds that repeat shows (`quotes`, `cities` and `sensors` hold
    /// every page's items); a page without its picks shows every item of its kind.
    pub picks: &'a [Picks],
}

impl Sources<'_> {
    fn picks(&self, page: Page) -> Option<&Picks> {
        self.picks.iter().find(|p| p.page == page)
    }
}

/// The items of `all` a page shows, in its order.
fn picked<T: Clone>(all: &[T], key: impl Fn(&T) -> &str, picks: Option<&Picks>) -> Vec<T> {
    match picks {
        None => all.to_vec(),
        Some(p) => p
            .items
            .iter()
            .filter_map(|i| all.iter().find(|x| key(x) == i.key).cloned())
            .collect(),
    }
}

/// The pages that have something to say, in the order of `enabled` (the user's).
/// `now` and `local` (unix seconds → ("15:30", local day number)) date the rows of Next and
/// the time of data that is old.
pub fn pages<P: Into<Page> + Copy>(
    s: &Sources,
    enabled: &[P],
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Vec<WidgetView> {
    let mut out = Vec::new();
    let mut seen: Vec<Page> = Vec::new();
    for p in enabled.iter().map(|&p| p.into()) {
        if seen.contains(&p) {
            continue;
        }
        seen.push(p);
        out.extend(page(s, p, w, now, local));
    }
    out.truncate(MAX_PAGES);
    out
}

/// One page, if it has something to say: of the kinds that repeat, the items of its picks,
/// under its picks' title.
pub fn page(
    s: &Sources,
    p: impl Into<Page>,
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Option<WidgetView> {
    let p: Page = p.into();
    let picks = s.picks(p);
    let mut v = kind_page(s, p.kind, picks, w, now, local)?;
    v.n = p.n;
    if let Some(t) = picks.map(|x| &x.title).filter(|t| !t.is_empty()) {
        v.title = t.clone();
    }
    Some(v)
}

fn kind_page(
    s: &Sources,
    kind: Kind,
    picks: Option<&Picks>,
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Option<WidgetView> {
    let trends = |all: &[Trend]| picked(all, |t: &Trend| t.key.as_str(), picks);
    match kind {
        Kind::Next => next_page(s.next, w, now, local),
        Kind::Weather => weather_page(&s.weather, s.weather_hero.as_ref(), w),
        Kind::Shopping => s.shopping.and_then(|l| shopping_page(l, w)),
        Kind::Quotes => trend_page(Kind::Quotes, w.quotes, &trends(s.quotes), w, now, local),
        Kind::Sensors => trend_page(Kind::Sensors, w.sensors, &trends(s.sensors), w, now, local),
        Kind::Cities => cities_page(
            &picked(s.cities, |c: &CityRow| c.key.as_str(), picks),
            w,
            now,
            local,
        ),
        Kind::Notes => notes_page(s.notes, w),
        Kind::Check => check_page(s.check, w),
        Kind::Playing => s.playing.and_then(|p| playing_page(p, w)),
        Kind::Home => home_page(s.people, w),
    }
}

/// What the browser panel shows of the choices that are not lists (the lists are the sources'
/// picks).
#[derive(Debug, Default)]
pub struct Chosen<'a> {
    /// What to check has at least one kind on.
    pub check: bool,
    /// The media player chosen ("" none).
    pub player: &'a str,
}

/// Every page of `order` for the browser panel: the page as the screen would draw it now
/// (`real`), with a "…" row for each chosen item whose data has not come yet (the picks of
/// `real`); the pages that show only at times (what to check, now playing) and the weather fall
/// back to an example (`sample`, drawn at `sample_now`) and say so. None: nothing to show.
#[allow(clippy::too_many_arguments)]
pub fn panel_pages(
    real: &Sources,
    sample: &Sources,
    chosen: &Chosen,
    order: &[Page],
    w: &Words,
    now: i64,
    sample_now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Vec<(Page, Option<WidgetView>, bool)> {
    order
        .iter()
        .map(|&p| {
            let kind = p.kind;
            let real_page = page(real, p, w, now, local);
            if let Some(picks) = real.picks(p).filter(|x| !x.items.is_empty()) {
                return (p, Some(waiting(p, real_page, picks, w)), false);
            }
            let fallback = match kind {
                Kind::Check => chosen.check,
                Kind::Playing => !chosen.player.is_empty(),
                Kind::Weather => true,
                _ => false,
            };
            match real_page {
                Some(v) => (p, Some(v), false),
                None if fallback => {
                    let mut v = page(sample, p, w, sample_now, local);
                    if kind == Kind::Playing
                        && let Some(v) = v.as_mut()
                    {
                        v.aside = chosen.player.to_string();
                    }
                    let sample = v.is_some();
                    (p, v, sample)
                }
                None => (p, None, false),
            }
        })
        .collect()
}

/// The page with its rows in the order of its picks, a waiting row for each item without data.
pub fn waiting(p: Page, page: Option<WidgetView>, picks: &Picks, w: &Words) -> WidgetView {
    let names: Vec<&String> = picks.items.iter().map(|i| &i.name).collect();
    let mut v = page.unwrap_or_else(|| WidgetView {
        kind: p.kind,
        n: p.n,
        title: if picks.title.is_empty() {
            match p.kind {
                Kind::Quotes => w.quotes,
                Kind::Cities => w.cities,
                _ => w.sensors,
            }
            .to_string()
        } else {
            picks.title.clone()
        },
        aside: String::new(),
        rows: Vec::new(),
        columns: false,
        line: None,
        window: w.window.to_string(),
        hero: None,
    });
    let mut have = std::mem::take(&mut v.rows);
    for n in names {
        match have.iter().position(|r| r.text == **n) {
            Some(i) => v.rows.push(have.remove(i)),
            None => v.rows.push(WidgetRow {
                text: n.clone(),
                right: "…".into(),
                wait: true,
                ..Default::default()
            }),
        }
    }
    v.rows.extend(have);
    v
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
                ..Default::default()
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
        n: 1,
        title: w.next.to_string(),
        aside: String::new(),
        rows,
        columns: false,
        line,
        window: String::new(),
        hero: None,
    })
}

/// The big block (now or tomorrow) and, under it, the next hours (or days) as columns.
fn weather_page(
    columns: &[WidgetRow],
    hero: Option<&super::weather::WeatherHero>,
    w: &Words,
) -> Option<WidgetView> {
    if columns.is_empty() && hero.is_none() {
        return None;
    }
    Some(WidgetView {
        kind: Kind::Weather,
        n: 1,
        title: w.weather.to_string(),
        aside: String::new(),
        rows: columns.iter().take(5).cloned().collect(),
        columns: true,
        line: None,
        window: String::new(),
        hero: hero.cloned(),
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
        n: 1,
        title: w.shopping.to_string(),
        aside: count.clone(),
        rows,
        columns: false,
        line: Some(WidgetRow {
            text: format!("{}{more}", lists::first_texts(lists, 3).join(" · ")),
            right: count,
            ..Default::default()
        }),
        window: String::new(),
        hero: None,
    })
}

/// "data from 14:02" when the oldest of `ats` shown is older than its `fresh`.
fn data_from(
    old: impl Iterator<Item = (i64, i64)>,
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> String {
    old.filter(|(at, fresh)| now - at > *fresh)
        .map(|(at, _)| at)
        .min()
        .map(|at| w.data_from.replace("{t}", &local(at).0))
        .unwrap_or_default()
}

/// A number in the screen's words: `decimals` digits, the thousands grouped.
pub fn format_number(v: f64, decimals: u8, w: &Words) -> String {
    // Never a string of a hundred digits: past a trillion (or not a number) a dash.
    if !v.is_finite() || v.abs() >= 1e15 {
        return "—".into();
    }
    let s = format!("{:.*}", decimals.min(6) as usize, v.abs());
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(w.group);
        }
        grouped.push(c);
    }
    let neg = v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0');
    let mut out = String::new();
    if neg {
        out.push('−');
    }
    out.push_str(&grouped);
    if !frac.is_empty() {
        out.push(w.decimal);
        out.push_str(frac);
    }
    out
}

/// A currency's sign where it has a common one, else the code; a unit as it is.
pub fn unit_sign(unit: &str) -> &str {
    match unit {
        "USD" | "usd" => "$",
        "EUR" | "eur" => "€",
        "GBP" | "gbp" => "£",
        "JPY" | "jpy" => "¥",
        _ => unit,
    }
}

/// The value with its unit: "189.20 $", "21.5 °C", "45 %".
pub fn value_text(t: &Trend, w: &Words) -> String {
    let v = format_number(t.value, t.decimals, w);
    match unit_sign(&t.unit) {
        "" => v,
        u => format!("{v} {u}"),
    }
}

/// The change against `prev`: ("▲ 1.2%", +1); ("", 0) without one. Under half a tenth it is
/// no change ("0.0%", 0).
pub fn change_text(t: &Trend, w: &Words) -> (String, i8) {
    let Some(prev) = t.prev.filter(|p| p.is_finite()) else {
        return (String::new(), 0);
    };
    let (d, decimals, suffix) = if t.pct {
        if prev == 0.0 {
            return (String::new(), 0);
        }
        ((t.value - prev) / prev.abs() * 100.0, 1u8, "%")
    } else {
        (t.value - prev, t.decimals.min(2), "")
    };
    let step = 0.5 * 10f64.powi(-(decimals as i32));
    if !d.is_finite() {
        return (String::new(), 0);
    }
    if d.abs() < step {
        return (format!("{}{suffix}", format_number(0.0, decimals, w)), 0);
    }
    let (arrow, trend) = if d > 0.0 { ("▲", 1) } else { ("▼", -1) };
    (
        format!("{arrow} {}{suffix}", format_number(d.abs(), decimals, w)),
        trend,
    )
}

/// Points between 0 and 1 (the lowest at 0); a flat line in the middle.
pub fn normalise(points: &[f32]) -> Vec<f32> {
    let pts: Vec<f32> = points.iter().copied().filter(|p| p.is_finite()).collect();
    if pts.len() < 2 {
        return Vec::new();
    }
    let lo = pts.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = pts.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if hi - lo <= f32::EPSILON * hi.abs().max(1.0) {
        return vec![0.5; pts.len()];
    }
    pts.iter().map(|p| (p - lo) / (hi - lo)).collect()
}

/// At most `n` points from many: the mean of each bucket, gaps (NaN, ∞) left out; a bucket
/// with no point is skipped.
pub fn downsample(points: &[f64], n: usize) -> Vec<f32> {
    if n == 0 || points.is_empty() {
        return Vec::new();
    }
    let len = points.len();
    let buckets = n.min(len);
    let mut out = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let (from, to) = (b * len / buckets, (b + 1) * len / buckets);
        let good: Vec<f64> = points[from..to]
            .iter()
            .copied()
            .filter(|p| p.is_finite())
            .collect();
        if !good.is_empty() {
            out.push((good.iter().sum::<f64>() / good.len() as f64) as f32);
        }
    }
    out
}

/// A curve as SVG path commands in the screen's 160 x 32 box (2 px off the top and bottom for
/// the stroke); "" without one.
pub fn curve_path(points: &[f32]) -> String {
    curve_path_in(points, 160.0, 32.0)
}

/// A curve in a `w` x `h` box, 2 px off its top and bottom.
pub fn curve_path_in(points: &[f32], w: f32, h: f32) -> String {
    if points.len() < 2 {
        return String::new();
    }
    let last = (points.len() - 1) as f32;
    let mut out = String::new();
    for (i, p) in points.iter().enumerate() {
        let x = i as f32 / last * w;
        let y = 2.0 + (1.0 - p.clamp(0.0, 1.0)) * (h - 4.0);
        out.push_str(&format!(
            "{}{x:.1} {y:.1}",
            if i == 0 { "M " } else { " L " }
        ));
    }
    out
}

/// A card of the markets' or the sensors' page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrendCard {
    pub name: String,
    /// "data from 14:02" when old, else `detail` as given (a symbol, a unit).
    pub detail: String,
    pub value: String,
    pub change: String,
    pub trend: i8,
    pub curve: Vec<f32>,
    /// "max 190.10", "min 186.20" ("" without a curve).
    pub high: String,
    pub low: String,
}

/// The cards of the markets' or the sensors' page: every item with a value not too old, in
/// order. `details` gives each item's second line (its symbol); `high`/`low` are "max {v}" and
/// "min {v}".
pub fn trend_cards(
    items: &[Trend],
    details: &dyn Fn(&Trend) -> String,
    w: &Words,
    high: &str,
    low: &str,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Vec<TrendCard> {
    items
        .iter()
        .filter(|t| t.value.is_finite() && now - t.at <= t.max_age)
        .map(|t| {
            let row = trend_row(t, w);
            let num = |v: f64| {
                let v = format_number(v, t.decimals, w);
                match unit_sign(&t.unit) {
                    "" => v,
                    u => format!("{v} {u}"),
                }
            };
            let curve = !row.curve.is_empty();
            TrendCard {
                name: t.name.clone(),
                detail: if now - t.at > t.fresh {
                    w.data_from.replace("{t}", &local(t.at).0)
                } else {
                    details(t)
                },
                value: row.right,
                change: row.change,
                trend: row.trend,
                curve: row.curve,
                high: if curve {
                    high.replace("{v}", &num(t.high))
                } else {
                    String::new()
                },
                low: if curve {
                    low.replace("{v}", &num(t.low))
                } else {
                    String::new()
                },
            }
        })
        .collect()
}

/// A page in Settings → Screen → Widgets.
#[derive(Debug, Clone, PartialEq)]
pub struct PageSetting {
    pub page: Page,
    pub label: String,
    pub on: bool,
    /// What it shows, chosen from the browser ("3 quotes · chosen from the browser"); "" none.
    pub hint: String,
}

/// One row of a number with a curve.
pub fn trend_row(t: &Trend, w: &Words) -> WidgetRow {
    let (change, trend) = change_text(t, w);
    WidgetRow {
        text: t.name.clone(),
        right: value_text(t, w),
        change,
        trend,
        curve: normalise(&t.points),
        ..Default::default()
    }
}

/// The markets or the sensors: one row each (name, curve, value, change); rows older than their
/// `max_age` left out; "data from 14:02" when one shown is older than its `fresh`. One slot:
/// the first row without the curve.
fn trend_page(
    kind: Kind,
    title: &str,
    items: &[Trend],
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Option<WidgetView> {
    let shown: Vec<&Trend> = items
        .iter()
        .filter(|t| t.value.is_finite() && now - t.at <= t.max_age)
        .collect();
    let first = shown.first()?;
    let rows: Vec<WidgetRow> = shown.iter().map(|t| trend_row(t, w)).collect();
    Some(WidgetView {
        kind,
        n: 1,
        title: title.to_string(),
        aside: data_from(shown.iter().map(|t| (t.at, t.fresh)), w, now, local),
        line: Some(WidgetRow {
            curve: Vec::new(),
            ..trend_row(first, w)
        }),
        rows,
        columns: false,
        window: w.window.to_string(),
        hero: None,
    })
}

/// Other cities: the time there on the left, the name, the sky, the temperature.
fn cities_page(
    cities: &[CityRow],
    w: &Words,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> Option<WidgetView> {
    let shown: Vec<&CityRow> = cities.iter().filter(|c| now - c.at <= c.max_age).collect();
    shown.first()?;
    let rows: Vec<WidgetRow> = shown
        .iter()
        .map(|c| WidgetRow {
            left: c.time.clone(),
            text: c.name.clone(),
            right: c.temp.clone(),
            note: c.cond.clone(),
            ..Default::default()
        })
        .collect();
    Some(WidgetView {
        kind: Kind::Cities,
        n: 1,
        title: w.cities.to_string(),
        aside: data_from(shown.iter().map(|c| (c.at, c.fresh)), w, now, local),
        line: None,
        rows,
        columns: false,
        window: w.window.to_string(),
        hero: None,
    })
}

/// The board: a note per row; in one row the first and how many more.
fn notes_page(notes: &[String], w: &Words) -> Option<WidgetView> {
    let first = notes.first()?;
    let rows = notes
        .iter()
        .map(|t| WidgetRow {
            text: t.clone(),
            ..Default::default()
        })
        .collect();
    Some(WidgetView {
        kind: Kind::Notes,
        n: 1,
        title: w.notes.to_string(),
        aside: String::new(),
        rows,
        columns: false,
        line: Some(WidgetRow {
            text: first.clone(),
            right: if notes.len() > 1 {
                format!("+{}", notes.len() - 1)
            } else {
                String::new()
            },
            ..Default::default()
        }),
        window: w.window.to_string(),
        hero: None,
    })
}

/// Openings first, then batteries from the emptiest; in one row how many of each.
fn check_page(items: &[CheckRow], w: &Words) -> Option<WidgetView> {
    if items.is_empty() {
        return None;
    }
    let mut sorted: Vec<&CheckRow> = items.iter().collect();
    sorted.sort_by_key(|c| (c.battery.is_some(), c.battery.unwrap_or(0)));
    let rows: Vec<WidgetRow> = sorted
        .iter()
        .map(|c| WidgetRow {
            text: c.name.clone(),
            right: match c.battery {
                Some(p) => format!("{p} %"),
                None => w.open.to_string(),
            },
            ..Default::default()
        })
        .collect();
    let open = items.iter().filter(|c| c.battery.is_none()).count();
    let batteries = items.len() - open;
    Some(WidgetView {
        kind: Kind::Check,
        n: 1,
        title: w.check.to_string(),
        aside: String::new(),
        line: Some(WidgetRow {
            text: w
                .check_line
                .replace("{n}", &open.to_string())
                .replace("{b}", &batteries.to_string()),
            ..Default::default()
        }),
        rows,
        columns: false,
        window: w.window.to_string(),
        hero: None,
    })
}

/// The title, the artist; the player in the header.
fn playing_page(p: &Playing, w: &Words) -> Option<WidgetView> {
    if p.title.trim().is_empty() {
        return None;
    }
    let mut rows = vec![WidgetRow {
        text: p.title.clone(),
        ..Default::default()
    }];
    if !p.artist.trim().is_empty() {
        rows.push(WidgetRow {
            text: p.artist.clone(),
            ..Default::default()
        });
    }
    let line = WidgetRow {
        text: if p.artist.trim().is_empty() {
            p.title.clone()
        } else {
            format!("{} · {}", p.title, p.artist)
        },
        ..Default::default()
    };
    Some(WidgetView {
        kind: Kind::Playing,
        n: 1,
        title: w.playing.to_string(),
        aside: p.player.clone(),
        rows,
        columns: false,
        line: Some(line),
        window: String::new(),
        hero: None,
    })
}

/// Who is home: a name per row and where.
fn home_page(people: &[Person], w: &Words) -> Option<WidgetView> {
    if people.is_empty() {
        return None;
    }
    let place = |p: &Place| match p {
        Place::Home => w.at_home.to_string(),
        Place::Away => w.away.to_string(),
        Place::Zone(z) => z.clone(),
    };
    let rows: Vec<WidgetRow> = people
        .iter()
        .map(|p| WidgetRow {
            text: p.name.clone(),
            right: place(&p.place),
            ..Default::default()
        })
        .collect();
    let home: Vec<&str> = people
        .iter()
        .filter(|p| p.place == Place::Home)
        .map(|p| p.name.as_str())
        .collect();
    Some(WidgetView {
        kind: Kind::Home,
        n: 1,
        title: w.home.to_string(),
        aside: String::new(),
        line: Some(WidgetRow {
            text: if home.is_empty() {
                people
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ")
            } else {
                home.join(" · ")
            },
            right: if home.is_empty() {
                w.away.to_string()
            } else {
                w.at_home.to_string()
            },
            ..Default::default()
        }),
        rows,
        columns: false,
        window: w.window.to_string(),
        hero: None,
    })
}

/// The slots of the widget's full size: a header and four rows, or the weather's big block.
pub const FULL_SLOTS: usize = 3;

/// The slots the widget takes of the `free` ones the cards leave: two sizes only, the same for
/// every page, so that the pages of the turn never change height: the full size when there
/// is room for it, else one line as a card (none without a free slot). The compact home's four
/// free slots take the full size too.
pub fn size(free: usize) -> usize {
    match free {
        0 => 0,
        f if f >= FULL_SLOTS => FULL_SLOTS,
        _ => 1,
    }
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
/// screen draws it as a card). A windowed page with more rows than fit shows the `turn`-th
/// window of them (0, 1, 2… round), and says which in its header.
pub fn fit(view: &WidgetView, slots: usize, turn: u32) -> Option<WidgetView> {
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
        return Some(v);
    }
    let per = rows_for(slots);
    let n = v.rows.len();
    if n > per && !v.window.is_empty() {
        let windows = n.div_ceil(per);
        let from = (turn as usize % windows) * per;
        let to = (from + per).min(n);
        v.rows = v.rows[from..to].to_vec();
        let range = v
            .window
            .replace("{a}", &(from + 1).to_string())
            .replace("{b}", &to.to_string())
            .replace("{n}", &n.to_string());
        v.aside = if v.aside.is_empty() {
            range
        } else {
            format!("{range} · {}", v.aside)
        };
    } else {
        v.rows.truncate(per);
    }
    Some(v)
}

/// The windows of rows a page has with `slots` free (1: it all fits).
pub fn windows(view: &WidgetView, slots: usize) -> usize {
    if view.columns || view.window.is_empty() || slots < 2 {
        return 1;
    }
    view.rows.len().div_ceil(rows_for(slots)).max(1)
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
    /// How many times each page came on screen: its window of rows (`fit`).
    turns: HashMap<Page, u32>,
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
            turns: HashMap::new(),
            told: None,
        }
    }

    /// The kind of the page on screen.
    pub fn kind(&self) -> Option<Kind> {
        self.current.as_ref().map(|v| v.kind)
    }

    /// The page on screen.
    pub fn page(&self) -> Option<Page> {
        self.current.as_ref().map(|v| v.page())
    }

    fn position(&self) -> Option<usize> {
        let k = self.page()?;
        self.latest.iter().position(|v| v.page() == k)
    }

    fn take(&mut self, index: usize, now: Instant) {
        let changed = self.page() != self.latest.get(index).map(|v| v.page());
        self.current = self.latest.get(index).cloned();
        if changed {
            self.seq = self.seq.wrapping_add(1);
            self.since = now;
            if let Some(k) = self.page() {
                *self.turns.entry(k).or_insert(0) += 1;
            }
        }
    }

    fn tell(&mut self) -> Option<Shown> {
        let s = Shown {
            view: self.current.as_ref().and_then(|v| {
                let turn = self
                    .turns
                    .get(&v.page())
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1);
                fit(v, self.slots, turn)
            }),
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
        match (self.page(), self.position()) {
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
        // The only page, with more rows than fit: its next window, with the fade.
        if let Some(every) = every
            && n == 1
            && now.duration_since(self.since) >= every
            && let Some(v) = self.latest.first().cloned()
            && windows(&v, self.slots) > 1
        {
            self.current = Some(v.clone());
            *self.turns.entry(v.page()).or_insert(0) += 1;
            self.seq = self.seq.wrapping_add(1);
            self.since = now;
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

    /// A tap on the widget (it opens the page's own page): the page, and the hold.
    pub fn tap(&mut self, now: Instant) -> Option<Page> {
        self.hold_until = Some(now + HOLD);
        self.page()
    }

    fn log(&self, s: Shown, why: Why) -> Shown {
        if let Some(v) = &s.view {
            let page = v.page().key();
            log::info!(event = "widget", page = page.as_str(), why = why.key(), slots = s.slots;
                "widget: {page} ({}/{}, {} slots, {})", s.index + 1, s.count, s.slots, why.key());
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
            quotes: "Markets",
            cities: "Cities",
            notes: "Notes",
            sensors: "Sensors",
            check: "To check",
            playing: "Now playing",
            home: "At home",
            data_from: "data from {t}",
            window: "{a}-{b} of {n}",
            open: "open",
            check_line: "{n} open · {b} batteries",
            at_home: "home",
            away: "away",
            decimal: '.',
            group: ',',
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
            n: 1,
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
            window: String::new(),
            hero: None,
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
        let one = fit(&p[0], 1, 0).unwrap();
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
        let one = fit(&p[1], 1, 0).unwrap();
        assert_eq!(one.rows[0].text, "Bread · Milk · Eggs …");
        assert_eq!(one.rows[0].right, "4 to buy");
        let p = pages(&s, &[Kind::Shopping], &words(), NOW, &local);
        assert_eq!(p.len(), 1);
        assert!(pages::<Page>(&s, &[], &words(), NOW, &local).is_empty());
    }

    #[test]
    fn pages_of_a_kind_have_their_keys() {
        let q2 = Page::new(Kind::Quotes, 2);
        assert_eq!(Page::from(Kind::Quotes).key(), "quotes");
        assert_eq!(q2.key(), "quotes-2");
        for (key, page) in [
            ("quotes", Some(Kind::Quotes.into())),
            ("quotes-2", Some(q2)),
            ("sensors-4", Some(Page::new(Kind::Sensors, 4))),
            ("cities-3", Some(Page::new(Kind::Cities, 3))),
        ] {
            assert_eq!(Page::from_key(key), page, "{key}");
        }
        for bad in [
            "quotes-1",
            "quotes-5",
            "quotes-02",
            "next-2",
            "weather-2",
            "quotes-",
            "-2",
            "stocks",
        ] {
            assert_eq!(Page::from_key(bad), None, "{bad}");
        }
        assert_eq!(q2.label("Markets", ""), "Markets 2");
        assert_eq!(Page::from(Kind::Quotes).label("Markets", ""), "Markets");
        assert_eq!(q2.label("Markets", " Crypto "), "Crypto");
        let order = vec![Kind::Quotes.into(), Page::new(Kind::Quotes, 3)];
        assert_eq!(Page::next_free(Kind::Quotes, &order), Some(q2));
        let full: Vec<Page> = (1..=MAX_SAME).map(|n| Page::new(Kind::Quotes, n)).collect();
        assert_eq!(Page::next_free(Kind::Quotes, &full), None);
        assert!(Kind::Quotes.repeats() && Kind::Cities.repeats() && Kind::Sensors.repeats());
        assert!(!Kind::Next.repeats() && !Kind::Weather.repeats() && !Kind::Home.repeats());
    }

    #[test]
    fn the_feeds_fetch_every_pages_items_once() {
        let mut l: Lists<&str> = Lists::default();
        assert!(l.is_empty() && l.get(1).is_empty());
        l.set(1, vec!["AAPL", "BTC"]);
        l.set(3, vec!["GOLD", "AAPL"]);
        l.set(2, vec!["EUR"]);
        assert_eq!(l.union(|x| *x), ["AAPL", "BTC", "EUR", "GOLD"]);
        assert_eq!(l.get(3), ["GOLD", "AAPL"]);
        l.remove(2);
        l.remove(1);
        assert_eq!(
            l.union(|x| *x),
            ["AAPL", "BTC", "GOLD"],
            "the first page is never removed"
        );
    }

    #[test]
    fn each_page_shows_its_own_items() {
        let quotes = vec![
            trend("AAPL", 189.2, 187.0, NOW - 60),
            trend("BTC", 60000.0, 61000.0, NOW - 60),
            trend("GOLD", 2400.0, 2390.0, NOW - 60),
        ];
        let q2 = Page::new(Kind::Quotes, 2);
        let picks = |page: Page, title: &str, keys: &[&str]| Picks {
            page,
            title: title.into(),
            items: keys
                .iter()
                .map(|k| Pick {
                    key: format!("yahoo:{k}"),
                    name: k.to_string(),
                })
                .collect(),
        };
        let all = vec![
            picks(Kind::Quotes.into(), "Markets", &["GOLD", "AAPL"]),
            picks(q2, "Crypto", &["BTC", "MISSING"]),
        ];
        let s = Sources {
            quotes: &quotes,
            picks: &all,
            ..Default::default()
        };
        let p = pages(&s, &[Kind::Quotes.into(), q2, q2], &words(), NOW, &local);
        assert_eq!(p.len(), 2, "a page once");
        assert_eq!(
            (p[0].page(), p[0].title.as_str()),
            (Kind::Quotes.into(), "Markets")
        );
        let texts = |v: &WidgetView| v.rows.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&p[0]), ["GOLD", "AAPL"]);
        assert_eq!((p[1].page(), p[1].title.as_str()), (q2, "Crypto"));
        assert_eq!(texts(&p[1]), ["BTC"]);
        // Waiting: the item without data shows "…" in its place.
        let w = waiting(q2, Some(p[1].clone()), &all[1], &words());
        assert_eq!(texts(&w), ["BTC", "MISSING"]);
        assert!(w.rows[1].wait);
        // A page with an empty list shows nothing; without picks, every item (the demos).
        let empty = vec![picks(q2, "", &[])];
        let s2 = Sources {
            quotes: &quotes,
            picks: &empty,
            ..Default::default()
        };
        assert!(page(&s2, q2, &words(), NOW, &local).is_none());
        let s3 = Sources {
            quotes: &quotes,
            ..Default::default()
        };
        assert_eq!(page(&s3, q2, &words(), NOW, &local).unwrap().rows.len(), 3);
        // The turn tells the two pages apart: each its own place, its own window.
        let t0 = Instant::now();
        let mut b = Band::new(t0);
        b.offer(p.clone(), 3, t0);
        assert_eq!(b.page(), Some(Kind::Quotes.into()));
        let s = b.swipe(1, t0).unwrap();
        assert_eq!((s.index, s.count), (1, 2));
        assert_eq!(b.tap(t0), Some(q2));
    }

    #[test]
    fn the_panel_shows_every_page_in_the_turns_order() {
        let q2 = Page::new(Kind::Quotes, 2);
        let all = vec![Picks {
            page: q2,
            title: "Markets 2".into(),
            items: vec![Pick {
                key: "yahoo:X".into(),
                name: "X".into(),
            }],
        }];
        let real = Sources {
            picks: &all,
            ..Default::default()
        };
        let sample = Sources::default();
        let order = [q2, Kind::Next.into(), Kind::Quotes.into()];
        let v = panel_pages(
            &real,
            &sample,
            &Chosen::default(),
            &order,
            &words(),
            NOW,
            NOW,
            &local,
        );
        let ids: Vec<Page> = v.iter().map(|x| x.0).collect();
        assert_eq!(ids, order);
        let p2 = v[0].1.as_ref().unwrap();
        assert_eq!((p2.title.as_str(), p2.n), ("Markets 2", 2));
        assert!(p2.rows[0].wait);
        assert!(v[2].1.is_none(), "the first page has nothing chosen");
    }

    #[test]
    fn two_sizes_only() {
        assert_eq!(size(0), 0);
        assert_eq!(size(1), 1);
        // Two free slots: one line, never a shorter full widget.
        assert_eq!(size(2), 1);
        assert_eq!(size(3), FULL_SLOTS);
        // The compact home with no card: the same full size.
        assert_eq!(size(4), FULL_SLOTS);
        assert_eq!(rows_for(size(4)), 4);
    }

    #[test]
    fn slots_cut_rows_never_shrink_them() {
        let v = view(Kind::Next, 8);
        assert!(fit(&v, 0, 0).is_none());
        assert_eq!(fit(&v, 1, 0).unwrap().rows.len(), 1);
        assert_eq!(fit(&v, 2, 0).unwrap().rows.len(), 2);
        assert_eq!(fit(&v, 3, 0).unwrap().rows.len(), 4);
        assert_eq!(fit(&v, 4, 0).unwrap().rows.len(), 6);
        let short = view(Kind::Next, 1);
        assert_eq!(fit(&short, 3, 0).unwrap().rows.len(), 1);
        // The weather's columns are one row: the same in every size.
        let mut w = view(Kind::Weather, 5);
        w.columns = true;
        for slots in 1..=4 {
            assert_eq!(fit(&w, slots, 0).unwrap().rows.len(), 5);
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
        assert_eq!(b.tap(s(90)), Some(Kind::Next.into()));
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

    fn trend(name: &str, value: f64, prev: f64, at: i64) -> Trend {
        Trend {
            key: format!("yahoo:{name}"),
            name: name.into(),
            value,
            decimals: 2,
            unit: "USD".into(),
            prev: Some(prev),
            pct: true,
            points: vec![1.0, 2.0, 1.5, 3.0],
            low: 1.0,
            high: 3.0,
            at,
            fresh: 1800,
            max_age: 48 * 3600,
        }
    }

    #[test]
    fn numbers_in_the_screens_words() {
        let w = words();
        assert_eq!(format_number(4512.304, 2, &w), "4,512.30");
        assert_eq!(format_number(-0.004, 2, &w), "0.00");
        assert_eq!(format_number(-12.5, 1, &w), "−12.5");
        assert_eq!(format_number(1234567.0, 0, &w), "1,234,567");
        assert_eq!(format_number(1e300, 2, &w), "—");
        assert_eq!(format_number(f64::NAN, 2, &w), "—");
        assert_eq!(format_number(1.5, 255, &w), "1.500000");
        let it = Words {
            decimal: ',',
            group: '.',
            ..words()
        };
        assert_eq!(format_number(64250.5, 2, &it), "64.250,50");
        let t = trend("Apple", 189.2, 186.95, NOW);
        assert_eq!(value_text(&t, &w), "189.20 $");
        assert_eq!(change_text(&t, &w), ("▲ 1.2%".into(), 1));
        let down = trend("X", 99.0, 100.0, NOW);
        assert_eq!(change_text(&down, &it), ("▼ 1,0%".into(), -1));
        let flat = trend("X", 100.01, 100.0, NOW);
        assert_eq!(change_text(&flat, &w), ("0.0%".into(), 0));
        let zero = trend("X", 1.0, 0.0, NOW);
        assert_eq!(change_text(&zero, &w), (String::new(), 0));
        let sensor = Trend {
            unit: "°C".into(),
            pct: false,
            decimals: 1,
            value: 21.5,
            prev: Some(19.0),
            ..trend("Room", 0.0, 0.0, NOW)
        };
        assert_eq!(value_text(&sensor, &w), "21.5 °C");
        assert_eq!(change_text(&sensor, &w), ("▲ 2.5".into(), 1));
    }

    #[test]
    fn curves_are_cut_and_normalised() {
        let many: Vec<f64> = (0..288).map(|i| i as f64).collect();
        let d = downsample(&many, CURVE_POINTS);
        assert_eq!(d.len(), 32);
        assert_eq!(d[0], 4.0);
        let gaps = [1.0, f64::NAN, 3.0, f64::NAN, f64::INFINITY, 5.0];
        assert_eq!(downsample(&gaps, 3), vec![1.0, 3.0, 5.0]);
        assert_eq!(downsample(&[f64::NAN, f64::NAN], 2), Vec::<f32>::new());
        assert_eq!(downsample(&[2.0, 4.0], 32), vec![2.0, 4.0]);
        assert_eq!(normalise(&[2.0, 4.0, 3.0]), vec![0.0, 1.0, 0.5]);
        assert_eq!(normalise(&[7.0, 7.0]), vec![0.5, 0.5]);
        assert!(normalise(&[1.0]).is_empty());
    }

    #[test]
    fn the_order_is_the_users() {
        let full = shop(&["Bread"]);
        let notes = vec!["Call the plumber".to_string()];
        let s = Sources {
            shopping: Some(&full),
            notes: &notes,
            ..Default::default()
        };
        let p = pages(
            &s,
            &[Kind::Notes, Kind::Shopping, Kind::Notes],
            &words(),
            NOW,
            &local,
        );
        let kinds: Vec<Kind> = p.iter().map(|v| v.kind).collect();
        assert_eq!(kinds, [Kind::Notes, Kind::Shopping]);
    }

    #[test]
    fn markets_rows_old_data_and_one_slot() {
        let q = vec![
            trend("Apple", 189.2, 186.95, NOW - 60),
            trend("Gold", 2400.0, 2410.0, NOW - 3 * 3600),
            trend("Gone", 1.0, 1.0, NOW - 3 * 86400),
        ];
        let s = Sources {
            quotes: &q,
            ..Default::default()
        };
        let p = pages(&s, &[Kind::Quotes], &words(), NOW, &local);
        assert_eq!(p.len(), 1);
        assert_eq!(
            p[0].rows.len(),
            2,
            "the row older than two days is not shown"
        );
        assert_eq!(p[0].aside, "data from 07:00");
        assert_eq!(p[0].rows[0].change, "▲ 1.2%");
        assert_eq!(p[0].rows[0].curve.len(), 4);
        assert_eq!(p[0].rows[1].trend, -1);
        let one = fit(&p[0], 1, 0).unwrap();
        assert_eq!(one.rows.len(), 1);
        assert!(one.rows[0].curve.is_empty());
        assert_eq!(one.rows[0].right, "189.20 $");
        // Nothing to show: not in the turn.
        let none = Sources {
            quotes: &q[2..],
            ..Default::default()
        };
        assert!(pages(&none, &[Kind::Quotes], &words(), NOW, &local).is_empty());
    }

    #[test]
    fn the_window_moves_on_each_turn() {
        let q: Vec<Trend> = (0..8)
            .map(|i| trend(&format!("Q{i}"), 1.0, 1.0, NOW))
            .collect();
        let s = Sources {
            quotes: &q,
            ..Default::default()
        };
        let p = pages(&s, &[Kind::Quotes], &words(), NOW, &local);
        let names = |v: &WidgetView| v.rows.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        let a = fit(&p[0], 3, 0).unwrap();
        assert_eq!(names(&a), ["Q0", "Q1", "Q2", "Q3"]);
        assert_eq!(a.aside, "1-4 of 8");
        let b = fit(&p[0], 3, 1).unwrap();
        assert_eq!(names(&b), ["Q4", "Q5", "Q6", "Q7"]);
        assert_eq!(b.aside, "5-8 of 8");
        assert_eq!(names(&fit(&p[0], 3, 2).unwrap()), names(&a));
        // Two slots: four windows of two; the last of six rows (compact) is short.
        assert_eq!(fit(&p[0], 2, 3).unwrap().aside, "7-8 of 8");
        assert_eq!(fit(&p[0], 4, 1).unwrap().aside, "7-8 of 8");
        // All of it fits: no range.
        let few = fit(
            &pages(
                &Sources {
                    quotes: &q[..3],
                    ..Default::default()
                },
                &[Kind::Quotes],
                &words(),
                NOW,
                &local,
            )[0],
            3,
            1,
        )
        .unwrap();
        assert_eq!(few.aside, "");
        // The band: the page's window moves on when it comes back...
        let t0 = Instant::now();
        let sec = |n: u64| t0 + Duration::from_secs(n);
        let mut band = Band::new(t0);
        let mut both = p.clone();
        both.push(view(Kind::Next, 1));
        let first = band.offer(both, 3, t0).unwrap();
        assert_eq!(first.view.unwrap().aside, "1-4 of 8");
        band.tick(sec(20), false, EVERY, false).unwrap();
        let back = band.tick(sec(40), false, EVERY, false).unwrap();
        assert_eq!(back.view.unwrap().aside, "5-8 of 8");
        // ...and alone, at every interval.
        let mut alone = Band::new(t0);
        alone.offer(p.clone(), 3, t0);
        assert!(alone.tick(sec(19), false, EVERY, false).is_none());
        let next = alone.tick(sec(20), false, EVERY, false).unwrap();
        assert_eq!(next.view.unwrap().aside, "5-8 of 8");
        assert_eq!(next.seq, 2);
        // Alone and all fits: still nothing.
        let mut short = Band::new(t0);
        short.offer(vec![view(Kind::Notes, 2)], 3, t0);
        assert!(short.tick(sec(60), false, EVERY, false).is_none());
    }

    #[test]
    fn notes_check_playing_home_cities() {
        let notes: Vec<String> = vec!["Water the plants".into(), "Call the plumber".into()];
        let check = vec![
            CheckRow {
                name: "Hall sensor".into(),
                battery: Some(12),
            },
            CheckRow {
                name: "Kitchen window".into(),
                battery: None,
            },
            CheckRow {
                name: "Remote".into(),
                battery: Some(5),
            },
        ];
        let playing = Playing {
            title: "Song".into(),
            artist: "Band".into(),
            player: "Living room speaker".into(),
        };
        let people = vec![
            Person {
                name: "Ada".into(),
                place: Place::Home,
            },
            Person {
                name: "Bob".into(),
                place: Place::Zone("Work".into()),
            },
        ];
        let cities = vec![CityRow {
            key: "city:1".into(),
            name: "Berlin".into(),
            time: "11:00".into(),
            temp: "14°".into(),
            cond: "rainy".into(),
            at: NOW - 600,
            fresh: 3600,
            max_age: 86400,
        }];
        let s = Sources {
            notes: &notes,
            check: &check,
            playing: Some(&playing),
            people: &people,
            cities: &cities,
            ..Default::default()
        };
        let p = pages(&s, &Kind::ALL, &words(), NOW, &local);
        let kinds: Vec<Kind> = p.iter().map(|v| v.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Cities,
                Kind::Notes,
                Kind::Check,
                Kind::Playing,
                Kind::Home
            ]
        );
        assert_eq!(fit(&p[1], 1, 0).unwrap().rows[0].right, "+1");
        let c: Vec<(&str, &str)> = p[2]
            .rows
            .iter()
            .map(|r| (r.text.as_str(), r.right.as_str()))
            .collect();
        assert_eq!(
            c,
            [
                ("Kitchen window", "open"),
                ("Remote", "5 %"),
                ("Hall sensor", "12 %")
            ]
        );
        assert_eq!(
            fit(&p[2], 1, 0).unwrap().rows[0].text,
            "1 open · 2 batteries"
        );
        assert_eq!(p[3].aside, "Living room speaker");
        assert_eq!(fit(&p[3], 1, 0).unwrap().rows[0].text, "Song · Band");
        assert_eq!(p[4].rows[1].right, "Work");
        assert_eq!(fit(&p[4], 1, 0).unwrap().rows[0].text, "Ada");
        assert_eq!(p[0].rows[0].note, "rainy");
        assert_eq!(p[0].aside, "");
        // Nothing playing (no title): no page.
        let silent = Playing::default();
        let s = Sources {
            playing: Some(&silent),
            ..Default::default()
        };
        assert!(pages(&s, &Kind::ALL, &words(), NOW, &local).is_empty());
        assert!(!Kind::Home.default_on() && Kind::Quotes.default_on());
        assert!(!Kind::Playing.tappable() && Kind::Notes.tappable());
    }

    #[test]
    fn trend_cards_for_the_page() {
        let q = vec![
            trend("Apple", 189.2, 186.95, NOW - 60),
            trend("Old", 10.0, 10.0, NOW - 7200),
            trend("Gone", 1.0, 1.0, NOW - 3 * 86400),
        ];
        let cards = trend_cards(
            &q,
            &|t| format!("{} sym", t.name),
            &words(),
            "max {v}",
            "min {v}",
            NOW,
            &local,
        );
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].detail, "Apple sym");
        assert_eq!(cards[0].value, "189.20 $");
        assert_eq!(
            (cards[0].high.as_str(), cards[0].low.as_str()),
            ("max 3.00 $", "min 1.00 $")
        );
        assert_eq!(cards[1].detail, "data from 08:00");
        assert!(curve_path_in(&cards[0].curve, 520.0, 96.0).starts_with("M 0.0 94.0 L"));
    }
}
