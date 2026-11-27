// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The screen's side of the board (crate::ui::board): its page (a swipe in from the home's right
//! edge), the custom widgets of the turn, the taps and the levels sent to Home Assistant through
//! the hub (`HubCmd::Call`), the tiles' states (`HubMsg::Tiles`) and answers (`HubMsg::Called`).
//!
//! - **Drawn only when seen**: new states are kept, and the page or the widget is redrawn only
//!   while it is on screen (the board's page, or the custom widget on the home) with the screen
//!   on; what changed meanwhile is drawn when it comes back.
//! - **After a tap** the tile waits for its new state ([`board::Pending`]): a single-shot timer
//!   runs only while something waits, to put it back when nothing came.
//! - **A level** is sent when the finger lifts, and at most every [`LEVEL_EVERY`] while it drags.
//! - **The question** (a cover, a valve, or a tile with "Ask first"): the body's own Yes/No, in
//!   the brain's question window (`ask-*` of companion.slint, one state on screen: the brain's
//!   question if there is one, else the tile's). The two never overlap: a tile's question is
//!   refused (the error sound) while the brain's is open, and a brain's question that comes
//!   while a tile's is open closes the tile's as a No. 30 s without an answer is a No. A touch
//!   of the remote screen never answers it (remote_ui: `remote_hidden_now`).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::TimerMode;

use super::{Screen, Ui, WakeSource};
use crate::core::UiEvent;
use crate::ha::widgets::States;
use crate::ui::board::{self, Board, BoardWords, Canvas, Pending, TileView, draw, icons::Icons};
use crate::ui::webview::controls::Place as Place2;
use crate::ui::widgets::{self, CustomPage, Kind as WidgetKind, WidgetRow};

/// How long the board's home button stays after the last touch (Home Assistant's view's).
const BOARD_HOME_HOLD: Duration = Duration::from_secs(4);

/// A level dragged is sent at most this often.
pub const LEVEL_EVERY: Duration = Duration::from_millis(300);
/// A tile's question is a No after this.
pub const ASK_FOR: Duration = Duration::from_secs(30);

/// Which canvas a tile is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui::screen) enum Place {
    /// The board's page (its index in `pages`).
    Page(usize),
    /// A custom widget (its index in `widgets`).
    Widget(usize),
}

/// A tile's question, open.
#[derive(Debug, Clone)]
pub(in crate::ui::screen) struct TileAsk {
    pub entity: String,
    pub call: board::Call,
    pub title: String,
    pub line: String,
    /// The tile's icon (path data).
    pub icon: String,
    pub since: Instant,
}

/// What the screen keeps of the board.
#[derive(Default)]
pub(in crate::ui::screen) struct BoardUi {
    pub board: Board,
    /// The tiles' entities, as the hub last told.
    pub states: States,
    pub pending: Pending,
    pub icons: Icons,
    /// The page shown (an index of the pages that have tiles; 0 when none has).
    pub page: usize,
    pub next_id: u64,
    /// Calls sent: their id → the entity.
    pub calls: HashMap<u64, String>,
    pub ask: Option<TileAsk>,
    /// A level: when its entity was last sent, and a value waiting to be sent.
    pub level_sent: HashMap<String, Instant>,
    pub level_wait: Option<(Place, usize, u8)>,
    /// What is on screen changed while it was not seen: drawn when it comes back.
    pub dirty: bool,
    /// The hub lost Home Assistant (`HubMsg::Link`): the tiles are what was last known,
    /// dimmed and not tappable ([`board::away`]).
    pub away: bool,
}

impl BoardUi {
    /// The pages that have tiles (the others are not shown).
    pub fn pages(&self) -> Vec<usize> {
        (0..self.board.pages.len())
            .filter(|&i| !self.board.pages[i].tiles.is_empty())
            .collect()
    }

    /// The custom widgets that have tiles (the turn's custom pages, in this order).
    pub fn widgets(&self) -> Vec<usize> {
        (0..self.board.widgets.len())
            .filter(|&i| !self.board.widgets[i].tiles.is_empty())
            .collect()
    }

    pub fn canvas(&self, place: Place) -> Option<&Canvas> {
        match place {
            Place::Page(i) => self.board.pages.get(i),
            Place::Widget(i) => self.board.widgets.get(i),
        }
    }
}

/// A canvas's tiles as the screen draws them.
fn views(
    b: &BoardUi,
    canvas: &Canvas,
    curves: &HashMap<String, crate::ha::widgets::Curve>,
    w: &BoardWords,
    now: Instant,
) -> Vec<TileView> {
    let mut v = board::view(canvas, &b.states, curves, &b.pending, w, now);
    if b.away {
        board::away(&mut v, w);
    }
    v
}

/// The rows of a custom widget for the turn (and the browser panel's preview): a tile each, its
/// name and its value or state.
pub(in crate::ui::screen) fn custom_rows(views: &[TileView]) -> Vec<WidgetRow> {
    let mut v: Vec<&TileView> = views.iter().collect();
    v.sort_by_key(|t| (t.y, t.x));
    v.iter()
        .map(|t| WidgetRow {
            text: t.name.clone(),
            right: if t.value.is_empty() {
                t.status.clone()
            } else {
                t.value.clone()
            },
            ..Default::default()
        })
        .collect()
}

impl<S: Screen> Ui<S> {
    pub(super) fn wire_board(self: &Rc<Self>) {
        let ui = Rc::downgrade(self);
        self.screen.on_board_tapped(move |place, id| {
            if let Some(ui) = ui.upgrade()
                && let Some(p) = ui.board_place(&place)
            {
                ui.board_tap(p, id.max(0) as usize);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_board_level(move |place, id, v, done| {
            if let Some(ui) = ui.upgrade()
                && let Some(p) = ui.board_place(&place)
            {
                ui.board_level(p, id.max(0) as usize, v.clamp(0, 100) as u8, done);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_board_swiped(move |dir| {
            if let Some(ui) = ui.upgrade() {
                ui.board_swipe(dir);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_board_peek(move || {
            if let Some(ui) = ui.upgrade() {
                // Its page as it is now: it comes in under the finger before it is "seen".
                ui.render_board_page();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_board_home_moved(move |x, y| {
            if let Some(ui) = ui.upgrade() {
                ui.board_home_moved(x, y);
            }
        });
        self.screen.on_board_edge(move |outcome, pulled| {
            log::info!(event = "edge_swipe", outcome = outcome.as_str(), max_in = pulled as f64,
                edge = "right", target = "board";
                "home: touch from the right edge: {outcome} ({pulled:.0} px)");
        });
    }

    /// The board's home button where it rests: the place the user gave Home Assistant's view's.
    fn board_home_place(&self) -> Place2 {
        self.opts
            .state_dir
            .as_deref()
            .map(crate::ui::webview::saved_place)
            .unwrap_or_default()
    }

    /// A touch on the board (or the board opened): its home button shows, where it rests.
    pub(super) fn board_home_show(&self) {
        // The demos are the board at rest (nothing would hide it: no clock).
        if self.opts.demo.is_some() {
            return;
        }
        let (x, y) = self.board_home_place().xy();
        self.screen.set_board_home(x, y);
        self.screen.set_board_home_shown(true);
    }

    /// Every second: the home button goes [`BOARD_HOME_HOLD`] after the last touch.
    pub(super) fn board_home_tick(&self, now: Instant) {
        if !self.screen.get_board_home_shown() {
            return;
        }
        let touched = self.desk.borrow().touched;
        if self.screen.get_page() != "board"
            || touched.is_none_or(|t| now.saturating_duration_since(t) >= BOARD_HOME_HOLD)
        {
            self.screen.set_board_home_shown(false);
        }
    }

    /// The home button dragged and let go with its top left at (x, y): it rests on the
    /// nearest edge, saved for Home Assistant's view too.
    pub(super) fn board_home_moved(&self, x: f32, y: f32) {
        let p = Place2::snap(x, y);
        log::info!(
            event = "board_home_button", edge = p.edge.name(), offset = p.offset as f64;
            "board: home button moved to the {} edge at {:.0}", p.edge.name(), p.offset
        );
        if let Some(dir) = self.opts.state_dir.as_deref() {
            crate::settings::Settings::in_dir(dir).save_casa_home_button(p.edge.name(), p.offset);
        }
        let (bx, by) = p.xy();
        self.screen.set_board_home(bx, by);
    }

    /// The board at the start (`board.json`; the demos set their own).
    pub(super) fn load_board(&self) {
        // The portrait in the board's header.
        self.screen.set_face(self.face());
        self.screen.set_portrait(self.portrait());
        let b = match (&self.settings, &self.opts.demo) {
            (Some(s), None) => s.board(),
            _ => Board::default(),
        };
        self.apply_board(b);
    }

    /// Replaces the board (the browser panel's editor): validated against Home Assistant's
    /// catalogue, saved to `board.json`, followed by the hub and drawn. The reasons of what was
    /// left out, one line each.
    pub fn replace_board(&self, b: Board) -> Vec<String> {
        let (b, why) = board::validate(b, &self.ha_catalog.borrow());
        for w in &why {
            log::warn!(event = "board"; "board: {w}");
        }
        if let Some(s) = &self.settings
            && let Err(e) = s.save_board(&b)
        {
            log::warn!(event = "board"; "board.json: {e}");
        }
        self.apply_board(b);
        why
    }

    /// The board in use: its icons read, the hub told what to follow, everything redrawn.
    pub(super) fn apply_board(&self, b: Board) {
        {
            let mut t = self.tile_board.borrow_mut();
            let names: Vec<String> = b
                .pages
                .iter()
                .chain(&b.widgets)
                .flat_map(|c| &c.tiles)
                .map(|tile| {
                    if tile.icon.is_empty() {
                        // The default comes with the state's class: every class's is built in.
                        String::new()
                    } else {
                        tile.icon.clone()
                    }
                })
                .filter(|n| !n.is_empty())
                .collect();
            let missing = t.icons.prepare(names.iter().map(String::as_str));
            if !missing.is_empty() {
                log::warn!(event = "board_icons"; "board: icons not found: {}", missing.join(", "));
            }
            t.board = b;
            t.page = t.page.min(t.pages().len().saturating_sub(1));
            t.ask = None;
            t.level_wait = None;
        }
        self.send_widget_want();
        let handle = !self.tile_board.borrow().pages().is_empty();
        self.screen.set_board_handle(handle);
        self.refresh_widget();
        self.render_board(true);
    }

    /// "page" or "widget" → the canvas on screen there.
    fn board_place(&self, place: &str) -> Option<Place> {
        let t = self.tile_board.borrow();
        match place {
            "page" => t.pages().get(t.page).copied().map(Place::Page),
            "widget" => {
                let p = self.widget.borrow().page()?;
                if p.kind != WidgetKind::Custom {
                    return None;
                }
                t.board
                    .widgets
                    .iter()
                    .position(|c| board::widget_number(&c.id) == Some(p.n))
                    .map(Place::Widget)
            }
            _ => None,
        }
    }

    /// The custom widgets' pages for the turn: each widget canvas with tiles, by its page's
    /// number (`w2` is "custom-2"), named as on the board ("Custom 2" without a name).
    pub(super) fn custom_pages(&self) -> Vec<CustomPage> {
        let t = self.tile_board.borrow();
        let curves = self.ha_curves.borrow();
        let l = self.locale.borrow();
        let w = BoardWords::from_locale(&l);
        let now = Instant::now();
        t.widgets()
            .into_iter()
            .filter_map(|i| {
                let c = &t.board.widgets[i];
                let n = board::widget_number(&c.id)?;
                Some(CustomPage {
                    n,
                    title: widgets::Page::new(WidgetKind::Custom, n)
                        .label(l.text("widget_custom"), &c.name),
                    rows: custom_rows(&views(&t, c, &curves, &w, now)),
                })
            })
            .collect()
    }

    /// A custom widget's page named as its canvas on the board (None: another page, or no name).
    pub(super) fn custom_name(&self, p: widgets::Page) -> Option<String> {
        if p.kind != WidgetKind::Custom {
            return None;
        }
        let t = self.tile_board.borrow();
        t.board
            .widget(p.n)
            .map(|c| c.name.clone())
            .filter(|n| !n.is_empty())
    }

    /// The board's page is on screen, the screen on.
    fn board_page_seen(&self) -> bool {
        self.screen.get_page() == "board" && !crate::ui::backlight::screen_off()
    }

    /// The custom widget is on the home, seen.
    fn board_widget_seen(&self) -> bool {
        self.widget.borrow().kind() == Some(WidgetKind::Custom) && !self.widget_paused()
    }

    /// Draws what is seen of the board (`force`: whatever was drawn before); what is not seen
    /// waits (`dirty`).
    pub(super) fn render_board(&self, force: bool) {
        let page = self.board_page_seen();
        let widget = self.board_widget_seen();
        {
            let mut t = self.tile_board.borrow_mut();
            if !page && !widget {
                t.dirty = true;
                if !force {
                    return;
                }
            } else {
                t.dirty = false;
            }
        }
        if page || force {
            self.render_board_page();
        }
        if widget || force {
            self.render_board_widget();
        }
    }

    /// The board's page as it is.
    fn render_board_page(&self) {
        let t = self.tile_board.borrow();
        let l = self.locale.borrow();
        let w = BoardWords::from_locale(&l);
        let pages = t.pages();
        let Some(&i) = pages.get(t.page) else {
            self.screen.set_board(
                l.text("board_title"),
                &[],
                0,
                0,
                true,
                l.text("board_empty"),
            );
            self.screen.set_board_sides(&[], &[]);
            return;
        };
        let c = &t.board.pages[i];
        let curves = self.ha_curves.borrow();
        let now = Instant::now();
        let v = views(&t, c, &curves, &w, now);
        let tiles = draw::draws(&v, draw::PAGE, &t.icons, false);
        // The neighbours, drawn beside it while a finger drags it across.
        let side = |n: Option<usize>| match n.and_then(|n| pages.get(n)) {
            Some(&j) => draw::draws(
                &views(&t, &t.board.pages[j], &curves, &w, now),
                draw::PAGE,
                &t.icons,
                false,
            ),
            None => Vec::new(),
        };
        self.screen
            .set_board_sides(&side(t.page.checked_sub(1)), &side(Some(t.page + 1)));
        let title = if c.name.is_empty() {
            l.text("board_title")
        } else {
            c.name.as_str()
        };
        self.screen
            .set_board(title, &tiles, t.page, pages.len(), false, "");
    }

    /// The custom widget's tiles, when its page is the one of the turn on screen.
    pub(super) fn render_board_widget(&self) {
        let Some(Place::Widget(i)) = self.board_place("widget") else {
            return;
        };
        let line = self.widget.borrow().slots() <= 1;
        let t = self.tile_board.borrow();
        let l = self.locale.borrow();
        let w = BoardWords::from_locale(&l);
        let curves = self.ha_curves.borrow();
        let v = views(&t, &t.board.widgets[i], &curves, &w, Instant::now());
        let cells = if line { draw::LINE } else { draw::WIDGET };
        self.screen
            .set_widget_tiles(&draw::draws(&v, cells, &t.icons, line));
    }

    /// The widget changed page (or size): its tiles if it is a custom one.
    pub(super) fn show_widget(&self, s: &widgets::Shown) {
        self.screen.set_widget(s);
        if s.view
            .as_ref()
            .is_some_and(|v| v.kind == WidgetKind::Custom)
        {
            self.render_board_widget();
        }
    }

    /// The board's page came on screen, or the home: draw what changed meanwhile.
    pub(super) fn board_seen_again(&self) {
        if self.tile_board.borrow().dirty {
            self.render_board(false);
        }
    }

    /// A swipe across the page: the next page or the previous one (the first's "previous" is
    /// the home: companion.slint).
    fn board_swipe(&self, dir: i32) {
        {
            let mut t = self.tile_board.borrow_mut();
            let n = t.pages().len();
            if n < 2 {
                return;
            }
            t.page = (t.page as i64 + dir.signum() as i64).clamp(0, n as i64 - 1) as usize;
        }
        self.render_board_page();
    }

    /// New states of the tiles' entities.
    pub(super) fn board_states(&self, s: States) {
        {
            let mut t = self.tile_board.borrow_mut();
            t.states = s;
            let now = Instant::now();
            let states = t.states.clone();
            t.pending.prune(&states, now);
        }
        self.board_timer_arm();
        self.render_board(false);
        // The panel's preview and the one-line rows: when the widget's turn takes them.
    }

    /// The hub's link to Home Assistant came up or went: the tiles show it.
    pub(super) fn board_link(&self, up: bool) {
        {
            let mut t = self.tile_board.borrow_mut();
            if t.away == !up {
                return;
            }
            t.away = !up;
        }
        log::info!(event = "board_link", up = up; "board: Home Assistant {}", if up { "back" } else { "away" });
        self.render_board(false);
    }

    /// Home Assistant is away: a touch on a tile does nothing but say so (the error sound).
    fn board_away(&self) -> bool {
        let away = self.tile_board.borrow().away;
        if away {
            self.send(UiEvent::Failed);
        }
        away
    }

    /// Home Assistant's answer to a call.
    pub(super) fn board_called(&self, id: u64, ok: bool, error: Option<String>) {
        let entity = self.tile_board.borrow_mut().calls.remove(&id);
        let Some(entity) = entity else { return };
        if ok {
            log::info!(event = "board_call", entity = entity.as_str(), ok = true; "board: {entity} done");
            return;
        }
        let why = error.unwrap_or_default();
        log::warn!(event = "board_call", entity = entity.as_str(), ok = false; "board: {entity} failed: {why}");
        self.tile_board
            .borrow_mut()
            .pending
            .failed(&entity, Instant::now());
        self.send(UiEvent::Failed);
        self.board_timer_arm();
        self.render_board(false);
    }

    /// The timer that puts a waiting tile back when nothing came: only while one waits.
    fn board_timer_arm(&self) {
        let next = self.tile_board.borrow().pending.next_change();
        let Some(at) = next else {
            self.board_timer.stop();
            return;
        };
        let ui = self.this.borrow().clone();
        let wait = at.saturating_duration_since(Instant::now()) + Duration::from_millis(20);
        self.board_timer
            .start(TimerMode::SingleShot, wait, move || {
                if let Some(ui) = ui.upgrade() {
                    {
                        let mut t = ui.tile_board.borrow_mut();
                        let states = t.states.clone();
                        t.pending.prune(&states, Instant::now());
                    }
                    ui.board_timer_arm();
                    ui.render_board(false);
                }
            });
    }

    /// Sends a tile's call; it waits for its new state.
    fn board_send(&self, call: board::Call) {
        let entity = call.entity_id.clone();
        let id = {
            let mut t = self.tile_board.borrow_mut();
            t.next_id += 1;
            let id = t.next_id;
            t.calls.insert(id, entity.clone());
            let states = t.states.clone();
            // A level call waits at its level (the bar does not jump back meanwhile).
            let level = board::ask_percent(&call, states.by_id.get(&entity))
                .map(|p| p.round().clamp(0.0, 100.0) as u8);
            t.pending.sent(&entity, &states, level, Instant::now());
            id
        };
        log::info!(event = "board_call", entity = entity.as_str(), service = call.service.as_str();
            "board: {}.{} on {entity}", call.domain, call.service);
        let sent = self
            .hub_tx
            .borrow()
            .as_ref()
            .is_some_and(|tx| tx.send(call.into_cmd(id)).is_ok());
        if !sent {
            self.board_called(id, false, Some("no Home Assistant".into()));
            return;
        }
        self.board_timer_arm();
        self.render_board(false);
    }

    /// A tap on tile `index` of `place`: its action, or its question first.
    pub(super) fn board_tap(&self, place: Place, index: usize) {
        if self.board_away() {
            return;
        }
        let (tile, st) = {
            let t = self.tile_board.borrow();
            let Some(tile) = t.canvas(place).and_then(|c| c.tiles.get(index)).cloned() else {
                return;
            };
            let st = t.states.by_id.get(&tile.entity_id).cloned();
            (tile, st)
        };
        let Some(call) = board::action_for(&tile, st.as_ref()) else {
            return;
        };
        let class = st.as_ref().map_or("", |s| s.device_class.as_str());
        let ha_name = st.as_ref().map_or("", |s| s.name.as_str());
        if board::needs_confirm(&tile, class, ha_name) {
            self.board_ask(place, index, call);
        } else {
            self.board_send(call);
        }
    }

    /// A level dragged on tile `index` of `place` (`done`: the finger lifted).
    pub(super) fn board_level(&self, place: Place, index: usize, value: u8, done: bool) {
        if self.tile_board.borrow().away {
            if done {
                self.send(UiEvent::Failed);
            }
            return;
        }
        let (tile, st) = {
            let t = self.tile_board.borrow();
            let Some(tile) = t.canvas(place).and_then(|c| c.tiles.get(index)).cloned() else {
                return;
            };
            let st = t.states.by_id.get(&tile.entity_id).cloned();
            (tile, st)
        };
        let Some(call) = board::set_level(&tile, st.as_ref(), value) else {
            return;
        };
        let class = st.as_ref().map_or("", |s| s.device_class.as_str());
        let ha_name = st.as_ref().map_or("", |s| s.name.as_str());
        if board::needs_confirm(&tile, class, ha_name) {
            // Asked once, for where the finger lifted; nothing moves while it drags.
            if done {
                self.board_ask(place, index, call);
            }
            return;
        }
        let now = Instant::now();
        if !done {
            let last = self
                .tile_board
                .borrow()
                .level_sent
                .get(&tile.entity_id)
                .copied();
            if let Some(last) = last.filter(|l| now.duration_since(*l) < LEVEL_EVERY) {
                // Too soon: the latest value goes when the interval is over.
                self.tile_board.borrow_mut().level_wait = Some((place, index, value));
                let ui = self.this.borrow().clone();
                self.level_timer.start(
                    TimerMode::SingleShot,
                    LEVEL_EVERY.saturating_sub(now.duration_since(last)),
                    move || {
                        if let Some(ui) = ui.upgrade() {
                            let w = ui.tile_board.borrow_mut().level_wait.take();
                            if let Some((p, i, v)) = w {
                                ui.board_level(p, i, v, false);
                            }
                        }
                    },
                );
                return;
            }
        } else {
            self.tile_board.borrow_mut().level_wait = None;
            self.level_timer.stop();
        }
        self.tile_board
            .borrow_mut()
            .level_sent
            .insert(tile.entity_id.clone(), now);
        self.board_send(call);
    }

    /// The question before a tile's call: refused while the brain's is open (one question on
    /// screen at a time).
    fn board_ask(&self, place: Place, index: usize, call: board::Call) {
        if self.house.borrow().ask.is_some() || self.tile_board.borrow().ask.is_some() {
            log::info!(event = "board_ask", entity = call.entity_id.as_str(), shown = false;
                "board: no question for {}: {}", call.entity_id,
                self.locale.borrow().text("board_ask_busy"));
            self.send(UiEvent::Failed);
            return;
        }
        // Bound to what is asked: a Yes later closes what the question said it closes.
        let call = {
            let t = self.tile_board.borrow();
            let state = t
                .states
                .by_id
                .get(&call.entity_id)
                .map_or("", |s| s.state.as_str());
            board::bound_to_state(call, state)
        };
        let ask = {
            let t = self.tile_board.borrow();
            let l = self.locale.borrow();
            let w = BoardWords::from_locale(&l);
            let Some(c) = t.canvas(place) else { return };
            let curves = self.ha_curves.borrow();
            let v = views(&t, c, &curves, &w, Instant::now());
            let Some(tv) = v.get(index) else { return };
            let state = t
                .states
                .by_id
                .get(&call.entity_id)
                .map_or("", |s| s.state.as_str());
            let pct = board::ask_percent(&call, t.states.by_id.get(&call.entity_id))
                .map_or_else(String::new, |p| format!("{p:.0}"));
            let title = l
                .text(board::ask_key(&call, state))
                .replace("{name}", &tv.name)
                .replace("{pct}", &pct);
            let line = match place {
                Place::Page(_) if !c.name.is_empty() => c.name.clone(),
                Place::Page(_) => l.text("board_title").to_string(),
                Place::Widget(_) => c.name.clone(),
            };
            TileAsk {
                entity: call.entity_id.clone(),
                title,
                line,
                icon: t.icons.path(&tv.icon).to_string(),
                call,
                since: Instant::now(),
            }
        };
        log::info!(event = "board_ask", entity = ask.entity.as_str(), shown = true; "board: asks {:?}", ask.title);
        self.tile_board.borrow_mut().ask = Some(ask);
        self.wake(WakeSource::Touch);
        self.render_house();
    }

    /// The tile's question answered (Yes or No touched, or its time over: No).
    pub(super) fn board_answer(&self, yes: bool, via: &str) {
        let Some(ask) = self.tile_board.borrow_mut().ask.take() else {
            return;
        };
        log::info!(event = "board_answer", entity = ask.entity.as_str(), yes = yes, via = via;
            "board: {} to {:?} ({via})", if yes { "yes" } else { "no" }, ask.title);
        if yes {
            self.board_send(ask.call);
        }
        self.render_house();
    }

    /// The tile's question in the question's window (render_house, when the brain has none):
    /// (title, line, icon, time left 1..0).
    pub(super) fn board_question(&self, now: Instant) -> Option<(String, String, String, f32)> {
        let t = self.tile_board.borrow();
        let a = t.ask.as_ref()?;
        let left =
            1.0 - now.saturating_duration_since(a.since).as_secs_f32() / ASK_FOR.as_secs_f32();
        Some((
            a.title.clone(),
            a.line.clone(),
            a.icon.clone(),
            left.clamp(0.0, 1.0),
        ))
    }

    /// The tile's question's time (poll_house): over → a No. True when it closed.
    pub(super) fn board_question_tick(&self, now: Instant) -> bool {
        let over = self
            .tile_board
            .borrow()
            .ask
            .as_ref()
            .is_some_and(|a| now.saturating_duration_since(a.since) >= ASK_FOR);
        if over {
            self.board_answer(false, "time");
        }
        over
    }

    /// The brain's question came: a tile's question open is a No (one question at a time).
    pub(super) fn board_question_yields(&self) {
        if self.tile_board.borrow().ask.is_some() {
            let a = self.tile_board.borrow_mut().ask.take();
            if let Some(a) = a {
                log::info!(event = "board_answer", entity = a.entity.as_str(), yes = false, via = "brain";
                    "board: no to {:?}: the brain's question came", a.title);
            }
        }
    }

    /// The hub's want: the board's entities, its wide sensors' days, its Advanced services.
    pub(super) fn board_want(&self, w: &mut crate::ha::widgets::Want) {
        let t = self.tile_board.borrow();
        w.tiles = t.board.entities();
        w.tile_curves = t.board.curve_entities();
        w.advanced = t.board.advanced();
    }
}

impl<S: Screen> Ui<S> {
    /// `--state board` (1), `board-confirm` (2: the shutter's question), `board-empty` (3),
    /// `widget-custom*` (4): the demo board on its sample states.
    pub(super) fn show_demo_board(&self, which: u8) {
        if which == 0 {
            return;
        }
        self.screen.set_face(self.face());
        self.screen.set_portrait(self.portrait());
        let (b, states, curves) = demo_board(which == 3);
        self.ha_curves.borrow_mut().extend(curves);
        self.tile_board.borrow_mut().states = states;
        self.apply_board(b);
        match which {
            2 => self.board_tap(Place::Page(0), 6),
            6 => self.screen.set_board_held(0.0, -420.0),
            7 => self.screen.set_board_held(520.0, 0.0),
            _ => {}
        }
    }
}

/// The demos' board (`--state board*`, `widget-custom*`): generic names, sample states.
pub(super) fn demo_board(
    empty: bool,
) -> (Board, States, HashMap<String, crate::ha::widgets::Curve>) {
    use crate::ha::widgets::{Curve, State};
    use crate::ui::board::{Tile, TileKind};
    if empty {
        return (Board::default(), States::default(), HashMap::new());
    }
    let tile =
        |x: u8, y: u8, w: u8, h: u8, kind: TileKind, entity: &str, name: &str, icon: &str| Tile {
            x,
            y,
            w,
            h,
            kind,
            entity_id: entity.into(),
            name: name.into(),
            icon: icon.into(),
            confirm: None,
            service: None,
            service_data: None,
        };
    use TileKind::{Button, Level, Sensor};
    let page = Canvas {
        id: "p1".into(),
        name: "Living room".into(),
        tiles: vec![
            tile(
                0,
                0,
                4,
                1,
                Level,
                "light.living_room",
                "Living room lights",
                "lightbulb-group",
            ),
            tile(
                0,
                1,
                1,
                1,
                Button,
                "light.floor_lamp",
                "Floor lamp",
                "floor-lamp",
            ),
            tile(
                1,
                1,
                1,
                1,
                Button,
                "light.ceiling",
                "Ceiling",
                "ceiling-light",
            ),
            tile(
                2,
                1,
                1,
                1,
                Button,
                "light.led_strip",
                "LED strip",
                "led-strip-variant",
            ),
            tile(3, 1, 1, 1, Button, "scene.movie", "Movie", "movie-open"),
            tile(
                0,
                2,
                2,
                2,
                Sensor,
                "sensor.outdoor_temperature",
                "Outdoor",
                "",
            ),
            tile(
                2,
                2,
                1,
                1,
                Button,
                "cover.living_room",
                "Shutter",
                "window-shutter-open",
            ),
            tile(3, 2, 1, 1, Button, "media_player.tv", "TV", "television"),
            tile(2, 3, 2, 1, Sensor, "sensor.power", "Power", ""),
            tile(
                4,
                0,
                2,
                1,
                Sensor,
                "sensor.indoor_humidity",
                "Humidity",
                "water-percent",
            ),
            tile(
                4,
                1,
                1,
                1,
                Button,
                "switch.coffee",
                "Coffee",
                "coffee-maker",
            ),
            tile(5, 1, 1, 1, Button, "fan.bedroom", "Fan", "fan"),
            tile(4, 2, 2, 1, Level, "light.kitchen", "Kitchen", "lightbulb"),
            tile(4, 3, 1, 1, Button, "switch.plug", "Plug", "power-socket-eu"),
            tile(
                5,
                3,
                1,
                1,
                Sensor,
                "sensor.phone_battery",
                "Phone",
                "battery",
            ),
        ],
    };
    let second = Canvas {
        id: "p2".into(),
        name: "Night".into(),
        tiles: vec![
            tile(
                0,
                0,
                1,
                1,
                Button,
                "scene.good_night",
                "Good night",
                "weather-night",
            ),
            tile(1, 0, 1, 1, Button, "light.bedroom", "Bedroom", "lightbulb"),
            tile(
                2,
                0,
                2,
                1,
                Button,
                "cover.bedroom",
                "Bedroom blinds",
                "blinds",
            ),
        ],
    };
    let widget = Canvas {
        id: "w1".into(),
        name: "Lights".into(),
        tiles: vec![
            tile(
                0,
                0,
                1,
                1,
                Button,
                "light.floor_lamp",
                "Floor lamp",
                "floor-lamp",
            ),
            tile(
                1,
                0,
                1,
                1,
                Button,
                "light.ceiling",
                "Ceiling",
                "ceiling-light",
            ),
            tile(
                2,
                0,
                1,
                1,
                Button,
                "cover.living_room",
                "Shutter",
                "window-shutter-open",
            ),
            tile(3, 0, 1, 1, Button, "scene.movie", "Movie", "movie-open"),
            tile(
                0,
                1,
                2,
                1,
                Sensor,
                "sensor.outdoor_temperature",
                "Outdoor",
                "",
            ),
            tile(
                2,
                1,
                2,
                1,
                Level,
                "media_player.tv",
                "TV volume",
                "volume-high",
            ),
        ],
    };
    let board = Board {
        pages: vec![page, second],
        widgets: vec![widget],
        ..Board::default()
    };
    let st = |state: &str| State {
        state: state.into(),
        ..Default::default()
    };
    let mut s = States::default();
    for (id, v) in [
        (
            "light.living_room",
            State {
                brightness: Some(153.0),
                ..st("on")
            },
        ),
        (
            "light.floor_lamp",
            State {
                brightness: Some(153.0),
                ..st("on")
            },
        ),
        ("light.ceiling", st("off")),
        (
            "light.led_strip",
            State {
                brightness: Some(255.0),
                ..st("on")
            },
        ),
        ("scene.movie", st("2026-11-03T10:00:00+00:00")),
        ("scene.good_night", st("2026-11-02T22:30:00+00:00")),
        (
            "sensor.outdoor_temperature",
            State {
                unit: "°C".into(),
                device_class: "temperature".into(),
                ..st("12.5")
            },
        ),
        (
            "cover.living_room",
            State {
                device_class: "shutter".into(),
                position: Some(100.0),
                ..st("open")
            },
        ),
        (
            "media_player.tv",
            State {
                device_class: "tv".into(),
                volume: Some(0.35),
                ..st("off")
            },
        ),
        (
            "sensor.indoor_humidity",
            State {
                unit: "%".into(),
                device_class: "humidity".into(),
                ..st("48")
            },
        ),
        ("switch.coffee", st("off")),
        ("fan.bedroom", st("on")),
        (
            "light.kitchen",
            State {
                brightness: Some(102.0),
                ..st("on")
            },
        ),
        ("switch.plug", st("off")),
        (
            "sensor.phone_battery",
            State {
                unit: "%".into(),
                device_class: "battery".into(),
                ..st("82")
            },
        ),
        ("light.bedroom", st("off")),
        (
            "cover.bedroom",
            State {
                device_class: "blind".into(),
                position: Some(0.0),
                ..st("closed")
            },
        ),
        (
            "sensor.power",
            State {
                unit: "W".into(),
                device_class: "power".into(),
                ..st("412")
            },
        ),
    ] {
        s.by_id.insert(id.into(), v);
    }
    // A day of the temperature and of the power (24 hours, in the widget's 32 points).
    let wave = |seed: f32| -> Vec<f32> {
        (0..32)
            .map(|i| {
                let t = i as f32 / 31.0;
                (0.5 + 0.32 * (t * 6.3 + seed).sin() + 0.12 * (t * 15.0 + seed * 2.0).sin())
                    .clamp(0.0, 1.0)
            })
            .collect()
    };
    let curves = [
        (
            "sensor.outdoor_temperature".to_string(),
            Curve {
                points: wave(0.4),
                first: 9.0,
                low: 8.1,
                high: 14.9,
            },
        ),
        (
            "sensor.power".to_string(),
            Curve {
                points: wave(2.2),
                first: 380.0,
                low: 120.0,
                high: 1450.0,
            },
        ),
    ]
    .into();
    (board, s, curves)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_board_is_valid_as_it_is() {
        let (b, s, _) = demo_board(false);
        let (v, why) = board::validate(b.clone(), &[]);
        assert!(why.is_empty(), "{why:#?}");
        assert_eq!(v.pages[0].tiles.len(), b.pages[0].tiles.len());
        for id in b.entities() {
            assert!(s.by_id.contains_key(&id), "{id} has a state");
        }
    }

    #[test]
    fn a_custom_widgets_rows_go_by_place() {
        let (b, s, c) = demo_board(false);
        let v = board::view(
            &b.widgets[0],
            &s,
            &c,
            &Pending::default(),
            &BoardWords::new(|k| k.to_string()),
            Instant::now(),
        );
        let rows = custom_rows(&v);
        assert_eq!(rows[0].text, "Floor lamp");
        assert_eq!(rows[4].right, "12.5°C");
        assert_eq!(rows[5].text, "TV volume");
        assert_eq!(rows[5].right, "35%");
    }
}
