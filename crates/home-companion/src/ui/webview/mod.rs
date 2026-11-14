// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! "Casa": Home Assistant's own frontend inside the body.
//!
//! The web helper (`home-companion-web`, its own process) renders the page off screen and sends each
//! frame as a dma-buf over a Unix socket (web/PROTOCOL.md). Here, on the UI thread:
//!
//! - the frames become GL textures of Slint's own context (gl.rs) and are drawn by
//!   crates/spike-ui/ui/casa.slint over the companion screen; `presented` and `release` go
//!   back as the test viewer (crates/webview-viewer) does them, `presented` once the frame
//!   that used the texture has been rendered;
//! - touches on the page go to the helper (view pixels = the screen's logical pixels), except
//!   those on the home button and on the cards drawn here, and the swipe up from the bottom
//!   edge (gesture.rs), which goes home;
//! - `show` / `hide`, the CPU governor while visible, the helper's cgroup freezer after a
//!   while hidden (system.rs), the option menus, the warning before heavy views, the spinner
//!   and error card from `state`;
//! - the helper is never waited for: the socket is read on its own thread (link.rs) and
//!   written without blocking; a helper that stops reading is dropped after 3 s and the
//!   connection retried.
//!
//! The page is loaded at the helper's start, hidden, so "Casa" opens at once.

pub mod controls;
pub mod gesture;
pub mod gl;
pub mod link;
pub mod system;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Image, ModelRc, Timer, TimerMode, VecModel};
use webview_proto::{self as proto, LoadState, Message, TouchKind as WebTouch};

use super::Screen;
use super::backlight::WakeSource;
use super::screen::Ui;
use super::touch::{TouchEvent, TouchKind};
use crate::core::Phase;

/// `[web]` in the configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebCfg {
    /// The "Home Assistant" button opens "Casa" (false: the button does nothing yet). Without
    /// `[home_assistant] url` it waits for the address the first start finds (the body starts
    /// again then, and the helper with it).
    pub enabled: bool,
    /// The page the helper shows, as a path on `[home_assistant] url`, e.g. "/lovelace/0"
    /// (see [`page_url`]). The body does not load it itself (the helper starts with it,
    /// `--url`, from `home-companion web-url`); `home-companion web-login` logs in with `[home_assistant] url` and
    /// `token_file`.
    pub path: String,
    /// Device scale of the page (1.25: Home Assistant's touch layout on the 1280x800 panel).
    pub scale: f32,
    /// The helper's socket.
    pub socket: String,
    /// CPU governor while "Casa" is visible ("" = leave it alone).
    pub governor: String,
    /// Freeze the helper's cgroup after this long hidden (0 = never).
    pub freeze_after_seconds: f32,
    /// Back home after this long without a touch on "Casa" (0 = never, the default: the page can
    /// stay up as a wall dashboard).
    pub idle_return_seconds: f32,
    /// A swipe up from the bottom edge goes home (like a phone's home gesture), with a thin
    /// home indicator bar at the bottom.
    pub edge_swipe: bool,
    /// The round home button and the home indicator bar show up with a touch and fade out
    /// this long after the last one (and for this long when the view opens); 0 = always shown.
    pub home_button_seconds: f32,
    /// Path prefixes of views that are heavy for the device (WebGL, live video, endless
    /// animations): a warning before they open, e.g. ["/dashboard-3d"].
    pub heavy_views: Vec<String>,
    /// Load the dashboard when the helper starts (true, the default: the first opening is
    /// immediate, ~400 MB stay in use), or only when the view first opens (false: ~11 s then;
    /// the helper waits on a tiny page of Home Assistant's own origin, `/robots.txt`, its login
    /// script is set for that origin). Hidden, WebKit draws nothing and the page never finishes:
    /// the preload "shows" the page to the helper (its frames are not put on the screen) until it
    /// is ready, at most [`WARM_MAX`].
    pub preload: bool,
}

/// Pauses the page's endless animations while they are off the screen (offscreen.js).
const OFFSCREEN_JS: &str = include_str!("offscreen.js");

/// A script as a `javascript:` URL for the helper's `load` (comment lines dropped, the rest
/// percent-encoded): it runs in the page as it is, nothing is navigated.
pub fn javascript_url(src: &str) -> String {
    let mut out = String::from("javascript:");
    for line in src.lines().filter(|l| !l.trim_start().starts_with("//")) {
        for b in line.trim().bytes().chain(std::iter::once(b'\n')) {
            if b.is_ascii_alphanumeric() || b"(){}[];,.:=+!'|<>/*&_-?$".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out.push_str("void%200");
    out
}

/// The preload drawn off the screen at most this long (a page that never gets ready).
pub const WARM_MAX: Duration = Duration::from_secs(120);

/// The tiny page of Home Assistant's origin the helper waits on when `preload` is false.
pub const IDLE_PATH: &str = "/robots.txt";

impl Default for WebCfg {
    fn default() -> Self {
        WebCfg {
            enabled: false,
            path: "/lovelace/0".into(),
            scale: 1.25,
            socket: "/run/home-companion-web/home-companion-web.sock".into(),
            governor: "performance".into(),
            freeze_after_seconds: 360.0,
            idle_return_seconds: 0.0,
            edge_swipe: true,
            home_button_seconds: 4.0,
            heavy_views: Vec::new(),
            preload: true,
        }
    }
}

impl WebCfg {
    pub fn validate(&self) -> Result<(), String> {
        if !(0.5..=4.0).contains(&self.scale) {
            return Err(format!("web.scale {}: between 0.5 and 4", self.scale));
        }
        if self.freeze_after_seconds < 0.0 || self.idle_return_seconds < 0.0 {
            return Err("web: times must not be negative".into());
        }
        if !(0.0..=600.0).contains(&self.home_button_seconds) {
            return Err(format!(
                "web.home_button_seconds {}: between 0 (always shown) and 600",
                self.home_button_seconds
            ));
        }
        if self.enabled && self.socket.is_empty() {
            return Err("web.socket is empty".into());
        }
        if !self.path.starts_with('/') {
            return Err(format!("web.path {:?}: a path, starting with /", self.path));
        }
        for p in &self.heavy_views {
            if !p.starts_with('/') {
                return Err(format!("web.heavy_views {p:?}: a path, starting with /"));
            }
        }
        Ok(())
    }
}

/// "http://host:8123/lovelace/0" → "http://host:8123".
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

/// The page the helper opens: `path` on Home Assistant's origin, e.g.
/// ("http://ha.example:8123/", "/lovelace/0") → "http://ha.example:8123/lovelace/0".
pub fn page_url(ha_url: &str, path: &str) -> Option<String> {
    Some(format!("{}{path}", origin(ha_url)?))
}

/// The `hassTokens` JSON the helper writes into the page's localStorage (`--token-stdin`), for
/// a long-lived token: no refresh token, an expiry far away.
pub fn hass_tokens(url: &str, token: &str) -> Result<String, String> {
    let origin = origin(url).ok_or("home_assistant.url is not set")?;
    let token = token.trim();
    if token.is_empty() {
        return Err("the token file is empty".into());
    }
    Ok(serde_json::json!({
        "hassUrl": origin,
        "clientId": format!("{origin}/"),
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": 1800,
        "refresh_token": "",
        "expires": 4_102_444_800_000u64,
    })
    .to_string())
}

/// The path of Home Assistant's frontend that shows `view` (BrainMsg::ShowView): a dashboard
/// path as it is; a camera as the more-info dialog over the dashboard (`dashboard`, the view's
/// usual page), which the frontend opens from its `more-info-entity-id` query parameter (since
/// 2025: src/common/url/more-info-query-params.ts, restored on every `location-changed`). None:
/// not a path of the same origin, or not a camera's entity id.
pub fn view_path(view: &crate::brain_link::View, dashboard: &str) -> Option<String> {
    use crate::brain_link::View;
    let same_origin =
        |p: &str| p.starts_with('/') && !p.starts_with("//") && !p.chars().any(|c| c.is_control());
    match view {
        View::Path(p) => same_origin(p).then(|| p.clone()),
        View::Camera(e) => {
            let ok = e.strip_prefix("camera.").is_some_and(|o| {
                !o.is_empty()
                    && o.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            });
            if !ok || !same_origin(dashboard) {
                return None;
            }
            let base = dashboard.split('#').next().unwrap_or(dashboard);
            let sep = if base.contains('?') { '&' } else { '?' };
            Some(format!("{base}{sep}more-info-entity-id={e}"))
        }
    }
}

/// A `javascript:` URL that moves the loaded frontend to `path` the way its own links do
/// (src/common/navigate.ts: `history.pushState` and a `location-changed` event): no reload,
/// the dashboard is there at once.
pub fn navigate_url(path: &str) -> String {
    let p = serde_json::to_string(path).unwrap_or_else(|_| "\"/\"".into());
    javascript_url(&format!(
        "history.pushState(null,'',{p});\nwindow.dispatchEvent(new CustomEvent('location-changed',{{detail:{{replace:false}}}}));\n"
    ))
}

static VIEW_REQUESTED: std::sync::Mutex<Option<crate::brain_link::View>> =
    std::sync::Mutex::new(None);

/// Opens "Casa" at a view the brain asked for (see [`view_path`]).
pub fn request_view(view: crate::brain_link::View) {
    if let Ok(mut v) = VIEW_REQUESTED.lock() {
        *v = Some(view);
    }
}

fn take_view_request() -> Option<crate::brain_link::View> {
    VIEW_REQUESTED.lock().ok().and_then(|mut v| v.take())
}

static VISIBLE: AtomicBool = AtomicBool::new(false);
static HOME_REQUESTED: AtomicBool = AtomicBool::new(false);
static LOGIN_REQUESTED: AtomicBool = AtomicBool::new(false);
static OPEN_REQUESTED: AtomicBool = AtomicBool::new(false);
static PRESENT: AtomicBool = AtomicBool::new(false);

/// This body has "Casa" (`[web] enabled`, not a demo).
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// "Casa" is on screen (the home underneath stays still and the backlight bright).
pub fn visible() -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

/// Back home from "Casa" (from any thread: Vol+ and Vol− together, keycombo.rs). Works even
/// when the helper hangs: nothing here waits for it.
pub fn request_home() {
    HOME_REQUESTED.store(true, Ordering::Relaxed);
}

/// Linked by QR code: the helper starts again with the new tokens (it reads them only at start).
pub fn request_login() {
    LOGIN_REQUESTED.store(true, Ordering::Relaxed);
}

/// Opens "Casa" (after a link: the user sees Home Assistant at once), as soon as a helper is
/// connected: it shows its own loading (a hidden page does not get far: Home Assistant's frontend
/// waits to be drawn). 12 s at most without one: then the view's own "offline".
pub fn request_open() {
    OPEN_REQUESTED.store(true, Ordering::Relaxed);
}

/// The words of "Casa" in the user's language.
#[derive(Debug, Clone, Default)]
pub struct Labels {
    pub home: String,
    pub retry: String,
    pub loading: String,
    pub offline: String,
    pub open: String,
    pub cancel: String,
    pub release: String,
    pub heavy: String,
    pub heavy_title: String,
    pub heavy_note: String,
    pub close: String,
    pub offline_text: String,
    pub no_gpu: String,
    /// The helper's own error texts are English: shown with these words instead.
    pub page_error: String,
    pub page_timeout: String,
}

impl Labels {
    /// The locale's words (`{name}` = the character's); English for the ones it lacks (an
    /// older locales directory).
    fn of(t: &locale::Locale, name: &str) -> Labels {
        let w = |k: &str, en: &str| -> String {
            match t.text(k) {
                v if v == k => en.replace("{name}", name),
                v => v.replace("{name}", name),
            }
        };
        Labels {
            home: w("casa_home", "Home"),
            retry: w("casa_retry", "Retry"),
            page_error: w("casa_page_error", "Home Assistant's page did not open."),
            page_timeout: w(
                "casa_page_timeout",
                "Home Assistant's page did not finish loading.",
            ),
            loading: w("casa_loading", "Loading Home Assistant…"),
            offline: w(
                "casa_offline",
                "Home Assistant's view is not available right now.",
            ),
            open: w("casa_open", "Open it"),
            cancel: w("casa_cancel", "Cancel"),
            release: w("casa_release", "Release to go back to {name}"),
            heavy: w(
                "casa_heavy",
                "It takes about 10 seconds to open and uses a lot of memory.",
            ),
            heavy_title: w("casa_heavy_title", "This view is heavy"),
            heavy_note: w("casa_heavy_note", "Heavy view: it takes a while"),
            close: w("close", "Close"),
            offline_text: w(
                "ha_unavailable_text",
                "Home Assistant is not answering. I retry by myself every 30 seconds; timers and alarms keep working.",
            ),
            no_gpu: w(
                "casa_no_gpu",
                "Home Assistant's view needs the GPU renderer.",
            ),
        }
    }
}

/// One row of a native option menu: (label, enabled, group label, selected).
pub type MenuRow = (String, bool, bool, bool);

/// The parts of the generated window that "Casa" uses (casa.slint's `CasaBridge`).
pub trait CasaScreen {
    fn casa_set_visible(&self, v: bool);
    fn casa_set_frame(&self, i: Image, has: bool);
    fn casa_set_status(&self, status: &str, progress: f32, message: &str);
    fn casa_set_labels(&self, l: &Labels, offline: &str);
    fn casa_set_menu(&self, rows: Option<&[MenuRow]>);
    fn casa_set_confirm(&self, text: Option<&str>);
    fn casa_set_pull(&self, pull: f32, armed: bool);
    /// The page's way in and out: pushed `px` away along its swipe, animated or at once.
    fn casa_set_slide(&self, px: f32, animated: bool);
    /// The swipe up from the bottom edge is on (its indicator bar is shown).
    fn casa_set_edge_swipe(&self, on: bool);
    /// The edge of the home button, the indicator bar and the swipe ("bottom", "left"…).
    fn casa_set_edge(&self, edge: &str);
    /// A short line under the toolbar ("" = none).
    fn casa_set_note(&self, note: &str);
    /// The character's face (the swipe chip, the conversation bar).
    fn casa_set_face(&self, face: Image);
    /// The home button and the indicator bar are shown.
    fn casa_set_controls(&self, shown: bool);
    /// The home button: top left (logical pixels), a finger on it, being dragged.
    fn casa_set_home_button(&self, x: f32, y: f32, pressed: bool, dragging: bool);
    fn casa_on_home(&self, f: impl Fn() + 'static);
    fn casa_on_retry(&self, f: impl Fn() + 'static);
    fn casa_on_menu_chosen(&self, f: impl Fn(i32) + 'static);
    fn casa_on_confirm(&self, f: impl Fn(bool) + 'static);
}

macro_rules! impl_casa {
    ($m:ident) => {
        impl CasaScreen for super::$m::CompanionWindow {
            fn casa_set_visible(&self, v: bool) {
                self.global::<super::$m::CasaBridge>().set_visible(v)
            }
            fn casa_set_frame(&self, i: Image, has: bool) {
                let b = self.global::<super::$m::CasaBridge>();
                b.set_frame(i);
                b.set_has_frame(has);
            }
            fn casa_set_status(&self, status: &str, progress: f32, message: &str) {
                let b = self.global::<super::$m::CasaBridge>();
                b.set_status(status.into());
                b.set_progress(progress);
                b.set_message(message.into());
            }
            fn casa_set_labels(&self, l: &Labels, offline: &str) {
                let b = self.global::<super::$m::CasaBridge>();
                b.set_label_home(l.home.as_str().into());
                b.set_label_retry(l.retry.as_str().into());
                b.set_label_loading(l.loading.as_str().into());
                b.set_label_offline(offline.into());
                b.set_label_open(l.open.as_str().into());
                b.set_label_cancel(l.cancel.as_str().into());
                b.set_label_release(l.release.as_str().into());
                b.set_label_heavy_title(l.heavy_title.as_str().into());
                b.set_label_close(l.close.as_str().into());
                b.set_label_offline_text(l.offline_text.as_str().into());
            }
            fn casa_set_menu(&self, rows: Option<&[MenuRow]>) {
                let b = self.global::<super::$m::CasaBridge>();
                match rows {
                    Some(rows) => {
                        let v: Vec<super::$m::CasaMenuRow> = rows
                            .iter()
                            .map(|(l, e, g, s)| super::$m::CasaMenuRow {
                                label: l.as_str().into(),
                                enabled: *e,
                                group: *g,
                                selected: *s,
                            })
                            .collect();
                        b.set_menu_items(ModelRc::new(VecModel::from(v)));
                        b.set_menu_open(true);
                    }
                    None => b.set_menu_open(false),
                }
            }
            fn casa_set_confirm(&self, text: Option<&str>) {
                let b = self.global::<super::$m::CasaBridge>();
                if let Some(t) = text {
                    b.set_confirm_text(t.into());
                }
                b.set_confirm_open(text.is_some());
            }
            fn casa_set_slide(&self, px: f32, animated: bool) {
                let b = self.global::<super::$m::CasaBridge>();
                b.set_slide_animated(animated);
                b.set_slide(px);
            }
            fn casa_set_pull(&self, pull: f32, armed: bool) {
                let b = self.global::<super::$m::CasaBridge>();
                b.set_pull(pull);
                b.set_pull_armed(armed);
            }
            fn casa_set_edge_swipe(&self, on: bool) {
                self.global::<super::$m::CasaBridge>().set_edge_swipe(on)
            }
            fn casa_set_edge(&self, edge: &str) {
                self.global::<super::$m::CasaBridge>().set_edge(edge.into())
            }
            fn casa_set_note(&self, note: &str) {
                self.global::<super::$m::CasaBridge>().set_note(note.into())
            }
            fn casa_set_face(&self, face: Image) {
                self.global::<super::$m::CasaBridge>().set_face(face)
            }
            fn casa_set_controls(&self, shown: bool) {
                self.global::<super::$m::CasaBridge>()
                    .set_controls_shown(shown)
            }
            fn casa_set_home_button(&self, x: f32, y: f32, pressed: bool, dragging: bool) {
                let b = self.global::<super::$m::CasaBridge>();
                // Dragging first: the snap back to an edge animates, the drag does not.
                b.set_home_dragging(dragging);
                b.set_home_pressed(pressed);
                b.set_home_x(x);
                b.set_home_y(y);
            }
            fn casa_on_home(&self, f: impl Fn() + 'static) {
                self.global::<super::$m::CasaBridge>().on_home(f)
            }
            fn casa_on_retry(&self, f: impl Fn() + 'static) {
                self.global::<super::$m::CasaBridge>().on_retry(f)
            }
            fn casa_on_menu_chosen(&self, f: impl Fn(i32) + 'static) {
                self.global::<super::$m::CasaBridge>().on_menu_chosen(f)
            }
            fn casa_on_confirm(&self, f: impl Fn(bool) + 'static) {
                self.global::<super::$m::CasaBridge>()
                    .on_confirm_answered(f)
            }
        }
    };
}

impl_casa!(sw);

/// What the UI needs to run "Casa".
#[derive(Debug, Clone)]
pub struct WebOptions {
    pub cfg: WebCfg,
    pub locales: PathBuf,
    pub language: String,
    /// Where the language picked on screen is saved (settings.toml).
    pub state_dir: PathBuf,
    /// The GPU renderer draws the screen (set by the renderer; the page needs it).
    pub gpu: bool,
    /// The dashboard's full address (loaded at the first opening when `cfg.preload` is false).
    pub page: String,
}

#[derive(Debug, Clone, Copy)]
struct Shown {
    buffer: u32,
    seq: u64,
    received: Instant,
    /// Its `presented` was already sent (skipped while hidden, or a redraw of a shown frame).
    answered: bool,
    /// The helper dropped its buffer after it was set on screen.
    dropped: bool,
}

/// Who gets the current touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    None,
    Slint,
    Page,
    /// The home button (a tap goes home, a drag moves it).
    Button,
}

struct Conn {
    sock: Arc<OwnedFd>,
    generation: u64,
    out: VecDeque<Message>,
    stuck_since: Option<Instant>,
    freezer: system::Freezer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Loading,
    Error,
    Offline,
}

struct Inner {
    /// The helper has the dashboard (false: it waits on IDLE_PATH until the view opens).
    page_loaded: bool,
    /// The preload: the helper draws the page, unseen, since then (until it is ready).
    warming: Option<Instant>,
    /// Created at the first frame (Slint's GL context exists by then).
    importer: Option<gl::Importer>,
    gpu: bool,
    labels: Labels,
    language: String,
    visible: bool,
    conn: Option<Conn>,
    status: Status,
    progress: f32,
    message: String,
    textures: HashMap<u32, gl::Imported>,
    /// Textures dropped while Slint may still draw them (buffer id, texture): freed once the
    /// Slint image is another one (or "Casa" is hidden) and a render has happened.
    doomed: Vec<(u32, gl::Imported)>,
    /// The buffer whose texture is the Slint image now.
    image_buffer: Option<u32>,
    pending: Option<Shown>,
    in_flight: Option<Shown>,
    on_screen: Option<u32>,
    last_shown: Option<Shown>,
    tick_queued: bool,
    menu: Option<u32>,
    confirm: Option<u32>,
    swipe: gesture::EdgeSwipe,
    /// The pull down from the top edge that brings the control center (unless the home button
    /// rests on the top edge: then that edge is its swipe's), and whether this touch is its.
    top: gesture::EdgeSwipe,
    top_touch: bool,
    /// When the home button and the indicator bar are shown, and whether they are now.
    controls: controls::Controls,
    controls_shown: bool,
    /// Where the home button rests, and the touch on it.
    place: controls::Place,
    drag: Option<controls::Drag>,
    target: Target,
    governor: system::Governor,
    hidden_since: Instant,
    /// Counts the ways in and out: a late step of an older one does nothing.
    slide_gen: u32,
    last_touch: Instant,
    /// Show → first frame on screen (log).
    show_at: Option<Instant>,
    tap_at: Option<Instant>,
    freeze_note: bool,
    /// "Casa" asked to open ([`request_open`]): opens once a helper is connected (after a link
    /// it is starting again), or after 12 s whatever it is doing.
    open_wanted: Option<Instant>,
    /// When a frame was last given to Slint (the import is capped during a conversation).
    frame_set_at: Instant,
    /// The heavy view being asked about, and the ones opened before (no question again).
    confirm_path: Option<String>,
    heavy_opened: std::collections::HashSet<String>,
    /// The note under the toolbar goes away then.
    note_until: Option<Instant>,
    /// Hidden by a long answer or a ringing timer: shown again once back home.
    reopen: bool,
    /// A view asked by the brain (request_view), applied when the view opens.
    view_wanted: Option<String>,
    /// The page is not the dashboard (a view asked by the brain): back to it when hidden.
    navigated: bool,
}

/// During "Okay Alfred" over the view, frames are imported at most this often (the CPU is for
/// the speech; the dashboard only needs to show the switch flip).
pub const CONVERSATION_FRAME: Duration = Duration::from_millis(100);
/// The conversation bar's area (logical pixels): touches there are Slint's.
pub const BAR: (f32, f32, f32, f32) = (280.0, 0.0, 1000.0, 160.0);

pub struct Casa<S: Screen> {
    ui: Rc<Ui<S>>,
    cfg: WebCfg,
    /// The dashboard's address (WebOptions::page).
    page: String,
    locales: PathBuf,
    state_dir: PathBuf,
    inner: RefCell<Inner>,
    queue: link::Queue,
    _link: Option<link::Link>,
    timer: Timer,
    this: RefCell<Weak<Self>>,
    stats: Cell<(u64, u64)>,
}

thread_local! {
    static DRAIN: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

impl<S: Screen> Casa<S> {
    /// Sets up "Casa" on the UI thread (after the window exists) and starts the connection.
    pub fn new(ui: Rc<Ui<S>>, opts: WebOptions) -> Rc<Self> {
        PRESENT.store(true, Ordering::Relaxed);
        if !opts.gpu {
            log::warn!("web view: not the GPU renderer: \"Casa\" shows a card instead of the page");
        }
        let text = locale::load_or_english(&opts.locales, &opts.language).ok();
        let name = ui.character_name();
        let labels = text
            .as_ref()
            .map(|t| Labels::of(t, &name))
            .unwrap_or_default();
        let now = Instant::now();
        let queue: link::Queue = Default::default();
        let drain_queued = Arc::new(AtomicBool::new(false));
        let link = if opts.gpu {
            let q = queue.clone();
            let flag = drain_queued.clone();
            let hello = proto::Hello {
                version: proto::VERSION,
                width: 1280,
                height: 800,
                scale_milli: (opts.cfg.scale * 1000.0).round() as u32,
            };
            match link::spawn(opts.cfg.socket.clone().into(), hello, q, move || {
                if !flag.swap(true, Ordering::AcqRel) {
                    let flag = flag.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        flag.store(false, Ordering::Release);
                        DRAIN.with(|d| {
                            if let Some(f) = &*d.borrow() {
                                f();
                            }
                        });
                    });
                }
            }) {
                Ok(l) => Some(l),
                Err(e) => {
                    log::error!("web view: link thread: {e}");
                    None
                }
            }
        } else {
            None
        };
        let casa = Rc::new(Casa {
            ui,
            inner: RefCell::new(Inner {
                importer: None,
                gpu: opts.gpu,
                labels,
                language: opts.language.clone(),
                visible: false,
                conn: None,
                page_loaded: false,
                warming: None,
                status: Status::Offline,
                progress: 0.0,
                message: String::new(),
                textures: HashMap::new(),
                doomed: Vec::new(),
                image_buffer: None,
                pending: None,
                in_flight: None,
                on_screen: None,
                last_shown: None,
                tick_queued: false,
                menu: None,
                confirm: None,
                swipe: if opts.cfg.edge_swipe {
                    gesture::EdgeSwipe::default()
                } else {
                    gesture::EdgeSwipe::new(800.0, -1.0, 120.0, 16.0)
                },
                top: {
                    let mut g = gesture::EdgeSwipe::new(800.0, 20.0, 64.0, 16.0);
                    g.set_side(controls::Edge::Top);
                    g
                },
                top_touch: false,
                controls: controls::Controls::new(opts.cfg.home_button_seconds),
                controls_shown: false,
                place: saved_place(&opts.state_dir),
                drag: None,
                target: Target::None,
                governor: system::Governor::new(&opts.cfg.governor),
                hidden_since: now,
                slide_gen: 0,
                last_touch: now,
                show_at: None,
                tap_at: None,
                freeze_note: false,
                open_wanted: None,
                frame_set_at: now,
                confirm_path: None,
                heavy_opened: Default::default(),
                note_until: None,
                reopen: false,
                view_wanted: None,
                navigated: false,
            }),
            cfg: opts.cfg,
            page: opts.page,
            locales: opts.locales,
            state_dir: opts.state_dir,
            queue,
            _link: link,
            timer: Timer::default(),
            this: RefCell::new(Weak::new()),
            stats: Cell::new((0, 0)),
        });
        *casa.this.borrow_mut() = Rc::downgrade(&casa);
        casa.wire();
        casa
    }

    fn weak(&self) -> Weak<Self> {
        self.this.borrow().clone()
    }

    fn wire(self: &Rc<Self>) {
        let s = &self.ui.screen;
        s.casa_set_edge_swipe(self.cfg.edge_swipe);
        s.casa_set_face(self.ui.face());
        self.show_button(None);
        self.apply_labels();
        self.show_status();
        let w = Rc::downgrade(self);
        s.casa_on_home(move || {
            if let Some(c) = w.upgrade() {
                c.close("home button");
            }
        });
        let w = Rc::downgrade(self);
        s.casa_on_retry(move || {
            if let Some(c) = w.upgrade() {
                c.send(Message::Reload);
            }
        });
        let w = Rc::downgrade(self);
        s.casa_on_menu_chosen(move |i| {
            if let Some(c) = w.upgrade() {
                c.menu_answer(i);
            }
        });
        let w = Rc::downgrade(self);
        s.casa_on_confirm(move |yes| {
            if let Some(c) = w.upgrade() {
                c.confirm_answer(yes);
            }
        });
        let w = Rc::downgrade(self);
        DRAIN.with(|d| {
            *d.borrow_mut() = Some(Box::new(move || {
                if let Some(c) = w.upgrade() {
                    c.drain();
                }
            }))
        });
        let w = Rc::downgrade(self);
        self.timer
            .start(TimerMode::Repeated, Duration::from_millis(100), move || {
                if let Some(c) = w.upgrade() {
                    c.housekeeping();
                }
            });
    }

    fn apply_labels(&self) {
        let i = self.inner.borrow();
        let offline = if i.gpu {
            &i.labels.offline
        } else {
            &i.labels.no_gpu
        };
        self.ui.screen.casa_set_labels(&i.labels, offline);
    }

    /// The language may have been changed on screen since the last opening.
    fn refresh_language(&self) {
        let lang = crate::settings::Settings::in_dir(&self.state_dir).language();
        let Some(lang) = lang else { return };
        if lang == self.inner.borrow().language {
            return;
        }
        if let Ok(t) = locale::load_or_english(&self.locales, &lang) {
            let mut i = self.inner.borrow_mut();
            i.labels = Labels::of(&t, &self.ui.character_name());
            i.language = lang;
        }
        self.apply_labels();
    }

    fn show_status(&self) {
        let i = self.inner.borrow();
        let s = match i.status {
            Status::Ok => "ok",
            Status::Loading => "loading",
            Status::Error => "error",
            Status::Offline => "offline",
        };
        self.ui.screen.casa_set_status(s, i.progress, &i.message);
    }

    // ------------------------------------------------------------------ show / hide

    /// The preload is over (the page is ready, or WARM_MAX ran out): hidden again for the helper,
    /// unless the view is on the screen meanwhile.
    fn end_warm(&self, why: &str) {
        let (since, visible) = {
            let mut i = self.inner.borrow_mut();
            (i.warming.take(), i.visible)
        };
        if let Some(t) = since {
            log::info!(event = "casa_warm", seconds = t.elapsed().as_secs_f64(); "casa: preload over ({why}) after {:.1} s", t.elapsed().as_secs_f64());
            if !visible {
                self.send(Message::Hide);
            }
        }
    }

    /// The first opening with a helper waiting on the idle page: the dashboard now.
    fn load_page(&self) {
        let first = {
            let mut i = self.inner.borrow_mut();
            let first = !i.page_loaded && i.conn.is_some();
            if first {
                i.page_loaded = true;
                i.status = Status::Loading;
                i.progress = 0.0;
                i.message.clear();
            }
            first
        };
        if first {
            log::info!(event = "casa_load"; "casa: loading the dashboard (first opening)");
            self.send(Message::Load {
                url: self.page.clone(),
            });
            self.show_status();
        }
    }

    /// The page at `path` of the frontend (a view asked by the brain): moved there in place
    /// when the dashboard is loaded, else loaded there (the first opening without a preload).
    fn go_to(&self, path: &str) {
        let (loaded, connected) = {
            let i = self.inner.borrow();
            (i.page_loaded, i.conn.is_some())
        };
        if !connected {
            log::warn!(event = "casa_view", path = path; "casa: no helper for the view {path}");
            return;
        }
        log::info!(event = "casa_view", path = path, loaded = loaded; "casa: to {path}");
        if loaded {
            self.send(Message::Load {
                url: navigate_url(path),
            });
        } else {
            let Some(url) = page_url(&self.page, path) else {
                return;
            };
            {
                let mut i = self.inner.borrow_mut();
                i.page_loaded = true;
                i.status = Status::Loading;
                i.progress = 0.0;
                i.message.clear();
            }
            self.send(Message::Load { url });
            self.show_status();
        }
        self.inner.borrow_mut().navigated = true;
    }

    /// The "Home Assistant" button.
    pub fn open(&self) {
        if self.inner.borrow().visible {
            return;
        }
        self.refresh_language();
        let now = Instant::now();
        let hidden_for = {
            let mut i = self.inner.borrow_mut();
            i.visible = true;
            i.show_at = Some(now);
            i.last_touch = now;
            i.swipe.reset();
            i.target = Target::None;
            i.drag = None;
            i.controls.poke(now);
            i.governor.boost();
            if let Some(c) = &mut i.conn {
                c.freezer.thaw();
            }
            now.duration_since(i.hidden_since)
        };
        VISIBLE.store(true, Ordering::Relaxed);
        log::info!(
            event = "casa_show", hidden_s = hidden_for.as_secs_f64();
            "casa: shown (hidden for {:.0} s)", hidden_for.as_secs_f64()
        );
        self.send(Message::Show);
        self.load_page();
        // A page that failed while hidden (its 90 s ran out before anyone looked, the dashboard
        // does not finish while it is not drawn): loaded again now, instead of an error.
        if self.inner.borrow().status == Status::Error {
            log::info!(event = "casa_reload", why = "error while hidden"; "casa: reloading the page that failed while hidden");
            {
                let mut i = self.inner.borrow_mut();
                i.status = Status::Loading;
                i.progress = 0.0;
                i.message.clear();
            }
            self.send(Message::Reload);
            self.show_status();
        }
        // The frame kept from before (stale until the page draws a fresh one), at once.
        let stale = {
            let i = self.inner.borrow();
            i.pending
                .or(i.last_shown)
                .filter(|f| i.textures.contains_key(&f.buffer))
        };
        match stale {
            Some(f) => {
                self.inner.borrow_mut().pending = None;
                self.set_frame(Shown {
                    answered: true,
                    ..f
                });
            }
            None => self.clear_frame(),
        }
        self.ui.screen.casa_set_pull(0.0, false);
        self.show_button(None);
        self.update_controls(now);
        // In the way it leaves, backwards: from out of the screen to its place.
        let gone = self.slide_length();
        self.slide(gone, 0.0, false);
        self.ui.screen.casa_set_visible(true);
        // The home underneath: still character, bright backlight (screen.rs reads `visible`).
        self.ui.step();
    }

    /// Back to the home screen.
    pub fn close(&self, why: &str) {
        if !self.inner.borrow().visible {
            return;
        }
        let (menu, confirm, touching, pull) = {
            let mut i = self.inner.borrow_mut();
            let pull = i.swipe.pull();
            i.visible = false;
            i.hidden_since = Instant::now();
            i.governor.restore();
            i.swipe.reset();
            i.controls.reset();
            i.drag = None;
            let touching = i.target == Target::Page;
            i.target = Target::None;
            (i.menu.take(), i.confirm.take(), touching, pull)
        };
        VISIBLE.store(false, Ordering::Relaxed);
        log::info!(event = "casa_hide", reason = why; "casa: hidden ({why})");
        // A view asked by the brain: the dashboard again for the next opening.
        if std::mem::take(&mut self.inner.borrow_mut().navigated) {
            self.send(Message::Load {
                url: navigate_url(&self.cfg.path),
            });
        }
        if touching {
            self.send_touch(WebTouch::Cancel, 0.0, 0.0);
        }
        if let Some(menu) = menu {
            self.send(Message::MenuResult { menu, index: -1 });
        }
        if let Some(id) = confirm {
            self.send(Message::ConfirmResult {
                id,
                accepted: false,
            });
        }
        // Closed before the preload got ready: the helper keeps drawing it (end_warm hides it).
        if self.inner.borrow().warming.is_none() {
            self.send(Message::Hide);
        }
        let s = &self.ui.screen;
        s.casa_set_menu(None);
        s.casa_set_confirm(None);
        // Out the way the swipe pushes it, from where the finger left it; hidden at the end.
        s.casa_set_pull(0.0, false);
        let gone = self.slide_length();
        self.slide(pull, gone, true);
        self.show_button(None);
        self.update_controls(Instant::now());
        self.ui.wake(WakeSource::Touch);
    }

    /// How far the page goes to be out of the screen: across, for the side edges.
    fn slide_length(&self) -> f32 {
        match self.inner.borrow().place.edge {
            controls::Edge::Left | controls::Edge::Right => controls::VIEW_W,
            controls::Edge::Top | controls::Edge::Bottom => controls::VIEW_H,
        }
    }

    /// The page from `from` to `to` px away (400 ms in casa.slint), then hidden if `hide`. The
    /// start is set at once and the move a frame later, so Slint animates from it.
    fn slide(&self, from: f32, to: f32, hide: bool) {
        let generation = {
            let mut i = self.inner.borrow_mut();
            i.slide_gen = i.slide_gen.wrapping_add(1);
            i.slide_gen
        };
        self.ui.screen.casa_set_slide(from, false);
        let this = self.this.borrow().clone();
        Timer::single_shot(Duration::from_millis(16), move || {
            let Some(c) = this.upgrade() else { return };
            if c.inner.borrow().slide_gen != generation {
                return;
            }
            c.ui.screen.casa_set_slide(to, true);
            if hide {
                let this = Rc::downgrade(&c);
                Timer::single_shot(Duration::from_millis(420), move || {
                    let Some(c) = this.upgrade() else { return };
                    let stale = {
                        let i = c.inner.borrow();
                        i.slide_gen != generation || i.visible
                    };
                    if !stale {
                        c.ui.screen.casa_set_visible(false);
                        c.ui.screen.casa_set_slide(0.0, false);
                    }
                });
            }
        });
    }

    // ------------------------------------------------------------------ the socket

    fn drain(&self) {
        loop {
            let next = self.queue.lock().unwrap().pop_front();
            let Some((generation, ev)) = next else { break };
            match ev {
                link::Event::Connected { sock, hello, pid } => {
                    self.connected(generation, sock, hello, pid)
                }
                link::Event::Message(m, fds) => {
                    if self.current_generation() == Some(generation) {
                        self.on_message(m, fds);
                    }
                }
                link::Event::Disconnected(why) => {
                    if self.current_generation() == Some(generation) {
                        self.disconnect(&why);
                    }
                }
            }
        }
    }

    fn current_generation(&self) -> Option<u64> {
        self.inner.borrow().conn.as_ref().map(|c| c.generation)
    }

    fn connected(
        &self,
        generation: u64,
        sock: Arc<OwnedFd>,
        hello: proto::Hello,
        pid: Option<i32>,
    ) {
        if self.inner.borrow().conn.is_some() {
            self.disconnect("replaced by a new connection");
        }
        let freezer = match pid.map(system::freeze_file) {
            Some(Ok(f)) => system::Freezer::new(f),
            Some(Err(why)) => {
                let mut i = self.inner.borrow_mut();
                if !i.freeze_note {
                    i.freeze_note = true;
                    log::info!("web view: {why}");
                }
                system::Freezer::none()
            }
            None => system::Freezer::none(),
        };
        if hello.width != 1280 || hello.height != 800 {
            log::warn!(
                "web view: the helper renders {}x{}: drawn scaled to the screen",
                hello.width,
                hello.height
            );
        }
        let visible = {
            let mut i = self.inner.borrow_mut();
            i.conn = Some(Conn {
                sock,
                generation,
                out: VecDeque::new(),
                stuck_since: None,
                freezer,
            });
            i.page_loaded = self.cfg.preload;
            i.warming = (self.cfg.preload && !i.visible).then(Instant::now);
            // A new helper has been hidden for no time: the old one's hidden time would freeze it
            // before it loads anything.
            i.hidden_since = Instant::now();
            i.status = Status::Ok;
            i.message.clear();
            i.visible
        };
        self.show_status();
        self.send(Message::Guard {
            paths: self.cfg.heavy_views.clone(),
        });
        let warming = self.inner.borrow().warming.is_some();
        if warming {
            log::info!(event = "casa_warm"; "casa: preloading the dashboard (drawn, not shown)");
        }
        self.send(if visible || warming {
            Message::Show
        } else {
            Message::Hide
        });
        if visible {
            self.inner.borrow_mut().show_at = Some(Instant::now());
            self.load_page();
        }
    }

    /// The connection is gone: every texture goes (after the next render: Slint may still draw
    /// the one on screen), the card says so; link.rs reconnects.
    fn disconnect(&self, why: &str) {
        let Some(conn) = self.inner.borrow_mut().conn.take() else {
            return;
        };
        // SAFETY: shutdown on our own socket: the link thread's reads end (EOF).
        unsafe { libc::shutdown(conn.sock.as_raw_fd(), libc::SHUT_RDWR) };
        drop(conn); // thaws a frozen helper
        log::warn!(event = "casa_disconnected", reason = why; "web view: helper disconnected: {why}");
        {
            let mut i = self.inner.borrow_mut();
            let all: Vec<u32> = i.textures.keys().copied().collect();
            for id in all {
                if let Some(t) = i.textures.remove(&id) {
                    i.doomed.push((id, t));
                }
            }
            i.pending = None;
            i.in_flight = None;
            i.on_screen = None;
            i.last_shown = None;
            i.status = Status::Offline;
            i.menu = None;
            i.confirm = None;
        }
        self.ui.screen.casa_set_menu(None);
        self.ui.screen.casa_set_confirm(None);
        self.clear_frame();
        self.show_status();
    }

    fn send(&self, m: Message) {
        let mut i = self.inner.borrow_mut();
        let Some(c) = i.conn.as_mut() else { return };
        if !c.out.is_empty() {
            queue(&mut c.out, m);
            let n = c.out.len();
            drop(i);
            if n > 512 {
                self.disconnect("the helper does not read (512 messages waiting)");
            }
            return;
        }
        match proto::send(c.sock.as_fd(), &m, &[]) {
            Ok(()) => {}
            Err(proto::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                c.out.push_back(m);
                c.stuck_since.get_or_insert_with(Instant::now);
            }
            Err(e) => {
                drop(i);
                self.disconnect(&format!("send: {e}"));
            }
        }
    }

    /// Sends what waits; a helper that has not read anything for 3 s is hung: dropped.
    fn flush(&self) {
        let mut hung = None;
        {
            let mut i = self.inner.borrow_mut();
            let Some(c) = i.conn.as_mut() else { return };
            while let Some(m) = c.out.front() {
                match proto::send(c.sock.as_fd(), m, &[]) {
                    Ok(()) => {
                        c.out.pop_front();
                        c.stuck_since = None;
                    }
                    Err(proto::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        hung = Some(format!("send: {e}"));
                        break;
                    }
                }
            }
            if c.out.is_empty() {
                c.stuck_since = None;
            } else if c.freezer.frozen() {
                // We froze it: it cannot read, it is not hung.
                c.stuck_since = Some(Instant::now());
            } else if c
                .stuck_since
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(3))
            {
                hung = Some(format!(
                    "the helper has not read for 3 s ({} messages waiting): hung?",
                    c.out.len()
                ));
            }
        }
        if let Some(why) = hung {
            self.disconnect(&why);
        }
    }

    fn on_message(&self, m: Message, fds: Vec<OwnedFd>) {
        match m {
            Message::Frame(f) => self.on_frame(f, fds),
            Message::Drop { buffer } => self.on_drop(buffer),
            Message::State(s) => {
                // The dashboard is there: its endless animations off the screen stop drawing.
                if s.state == LoadState::Ready && self.inner.borrow().page_loaded {
                    self.send(Message::Load {
                        url: javascript_url(OFFSCREEN_JS),
                    });
                    self.end_warm("ready");
                }
                // The idle page (no dashboard yet): its states mean nothing (its readiness check
                // runs out after 90 s).
                if !self.inner.borrow().page_loaded {
                    log::debug!("web view: idle page state {:?}", s.state);
                    return;
                }
                {
                    let mut i = self.inner.borrow_mut();
                    match s.state {
                        LoadState::Loading => {
                            i.status = Status::Loading;
                            i.progress = s.progress_milli as f32 / 1000.0;
                        }
                        LoadState::Ready => {
                            i.status = Status::Ok;
                            i.progress = 1.0;
                        }
                        LoadState::Error => {
                            i.status = Status::Error;
                            // The helper's text is English and technical: logged, not shown.
                            log::warn!(event = "casa_error"; "web view: {}", s.message);
                            i.message = if s.message.contains("did not show its content") {
                                i.labels.page_timeout.clone()
                            } else {
                                i.labels.page_error.clone()
                            };
                        }
                    }
                }
                log::debug!(
                    "web view: state {:?} {} {:?} {:?}",
                    s.state,
                    s.progress_milli,
                    s.title,
                    s.message
                );
                self.show_status();
            }
            Message::Menu(m) => {
                let visible = self.inner.borrow().visible;
                if !visible {
                    self.send(Message::MenuResult {
                        menu: m.menu,
                        index: -1,
                    });
                    return;
                }
                let rows: Vec<MenuRow> = m
                    .items
                    .iter()
                    .enumerate()
                    .map(|(n, it)| {
                        (
                            it.label.clone(),
                            it.flags & proto::MENU_ITEM_ENABLED != 0,
                            it.flags & proto::MENU_ITEM_GROUP != 0,
                            n as i32 == m.selected,
                        )
                    })
                    .collect();
                log::info!("casa: menu {} with {} items", m.menu, rows.len());
                if let Some(old) = self.inner.borrow_mut().menu.replace(m.menu) {
                    log::debug!("casa: menu {old} replaced");
                }
                self.ui.screen.casa_set_menu(Some(&rows));
            }
            Message::Confirm(c) => {
                let (visible, text) = {
                    let i = self.inner.borrow();
                    (i.visible, i.labels.heavy.clone())
                };
                if !visible {
                    self.send(Message::ConfirmResult {
                        id: c.id,
                        accepted: false,
                    });
                    return;
                }
                if self.inner.borrow().heavy_opened.contains(&c.path) {
                    // Opened before: no question, a short note instead.
                    log::info!("casa: heavy view {}: opened before, no question", c.path);
                    self.send(Message::ConfirmResult {
                        id: c.id,
                        accepted: true,
                    });
                    let note = {
                        let mut i = self.inner.borrow_mut();
                        i.note_until = Some(Instant::now() + Duration::from_secs(2));
                        i.labels.heavy_note.clone()
                    };
                    self.ui.screen.casa_set_note(&note);
                    return;
                }
                log::info!("casa: heavy view {}: asking", c.path);
                {
                    let mut i = self.inner.borrow_mut();
                    i.confirm = Some(c.id);
                    i.confirm_path = Some(c.path.clone());
                }
                self.ui.screen.casa_set_confirm(Some(&text));
            }
            Message::Hello(_) => {}
            other => log::debug!("web view: unexpected from the helper: {:?}", other.kind()),
        }
    }

    fn menu_answer(&self, index: i32) {
        let Some(menu) = self.inner.borrow_mut().menu.take() else {
            return;
        };
        log::info!(
            "casa: menu {menu}: {}",
            if index < 0 {
                "cancelled".into()
            } else {
                format!("item {index}")
            }
        );
        self.ui.screen.casa_set_menu(None);
        self.send(Message::MenuResult { menu, index });
    }

    fn confirm_answer(&self, accepted: bool) {
        let Some(id) = self.inner.borrow_mut().confirm.take() else {
            return;
        };
        let path = self.inner.borrow_mut().confirm_path.take();
        if accepted && let Some(p) = path {
            self.inner.borrow_mut().heavy_opened.insert(p);
        }
        log::info!(
            "casa: heavy view {}",
            if accepted { "opened" } else { "not opened" }
        );
        self.ui.screen.casa_set_confirm(None);
        self.send(Message::ConfirmResult { id, accepted });
    }

    // ------------------------------------------------------------------ frames

    fn on_frame(&self, f: proto::Frame, fds: Vec<OwnedFd>) {
        let now = Instant::now();
        if f.has_fds {
            let res = {
                let mut i = self.inner.borrow_mut();
                if i.importer.is_none() {
                    match gl::Importer::new() {
                        Ok(imp) => i.importer = Some(imp),
                        Err(e) => log::warn!("web view: {e}"),
                    }
                }
                match &i.importer {
                    Some(imp) => imp.import(&f, &fds),
                    None => Err("no GL importer".into()),
                }
            };
            match res {
                Ok(t) => {
                    log::info!(
                        "web view: buffer {} imported: {}x{} {} modifier 0x{:x}",
                        f.buffer,
                        f.width,
                        f.height,
                        gl::Importer::fourcc_name(f.fourcc),
                        f.modifier
                    );
                    let old = self.inner.borrow_mut().textures.insert(f.buffer, t);
                    if let Some(old) = old {
                        self.retire(f.buffer, old);
                    }
                }
                Err(e) => {
                    log::warn!("web view: buffer {} import failed: {e}", f.buffer);
                    self.send(Message::Presented(proto::Presented {
                        buffer: f.buffer,
                        skipped: true,
                        seq: f.seq,
                        time_ns: proto::monotonic_ns(),
                    }));
                    self.send(Message::Release { buffer: f.buffer });
                    return;
                }
            }
            drop(fds);
        } else if !self.inner.borrow().textures.contains_key(&f.buffer) {
            log::debug!("web view: frame for unknown buffer {} ignored", f.buffer);
            self.send(Message::Presented(proto::Presented {
                buffer: f.buffer,
                skipped: true,
                seq: f.seq,
                time_ns: proto::monotonic_ns(),
            }));
            return;
        }
        let shown = Shown {
            buffer: f.buffer,
            seq: f.seq,
            received: now,
            answered: false,
            dropped: false,
        };
        let (visible, busy) = {
            let i = self.inner.borrow();
            (i.visible, i.in_flight.is_some() || self.throttled(&i, now))
        };
        if !visible || busy {
            // Not drawn now: hidden (answered at once, kept for `show`), or a frame still
            // waits for its render (the newest one waits, older ones are skipped).
            let shown = if visible {
                shown
            } else {
                self.skip(shown);
                Shown {
                    answered: true,
                    ..shown
                }
            };
            let old = self.inner.borrow_mut().pending.replace(shown);
            if let Some(old) = old {
                self.skip(old);
                if old.buffer != shown.buffer {
                    self.release(old.buffer);
                }
            }
            return;
        }
        self.set_frame(shown);
    }

    /// `presented` (skipped) for a frame that will not be drawn.
    fn skip(&self, f: Shown) {
        if f.answered {
            return;
        }
        self.send(Message::Presented(proto::Presented {
            buffer: f.buffer,
            skipped: true,
            seq: f.seq,
            time_ns: proto::monotonic_ns(),
        }));
    }

    /// `release` once nothing will sample the buffer (not waiting, not being drawn, not on screen).
    fn release(&self, buffer: u32) {
        let send = {
            let i = self.inner.borrow();
            let used = i.pending.is_some_and(|p| p.buffer == buffer)
                || i.in_flight.is_some_and(|p| p.buffer == buffer)
                || i.on_screen == Some(buffer)
                || i.image_buffer == Some(buffer);
            !used && i.textures.contains_key(&buffer)
        };
        if send {
            self.send(Message::Release { buffer });
        }
    }

    /// A texture that Slint may be drawing: freed after the next render.
    fn retire(&self, buffer: u32, t: gl::Imported) {
        {
            let mut i = self.inner.borrow_mut();
            if i.image_buffer != Some(buffer) {
                if let Some(imp) = &i.importer {
                    imp.forget(t);
                }
                return;
            }
            i.doomed.push((buffer, t));
        }
        self.queue_tick();
    }

    /// During a conversation over the view, a frame waits until [`CONVERSATION_FRAME`] after the
    /// last one (the housekeeping draws the one kept).
    fn throttled(&self, i: &Inner, now: Instant) -> bool {
        matches!(self.ui.phase(), Phase::Listening | Phase::Thinking)
            && now.duration_since(i.frame_set_at) < CONVERSATION_FRAME
    }

    fn set_frame(&self, f: Shown) {
        self.inner.borrow_mut().frame_set_at = Instant::now();
        let (img, same) = {
            let i = self.inner.borrow();
            let Some(t) = i.textures.get(&f.buffer) else {
                return;
            };
            (gl::Importer::image(t), i.image_buffer == Some(f.buffer))
        };
        {
            let mut i = self.inner.borrow_mut();
            i.image_buffer = Some(f.buffer);
            i.in_flight = Some(f);
        }
        self.ui.screen.casa_set_frame(img, true);
        if same {
            // The same texture with new content: Slint sees no property change.
            self.ui.screen.window().request_redraw();
        }
        self.queue_tick();
    }

    fn clear_frame(&self) {
        {
            let mut i = self.inner.borrow_mut();
            i.image_buffer = None;
            i.in_flight = None;
        }
        self.ui.screen.casa_set_frame(Image::default(), false);
        self.queue_tick();
    }

    /// Runs once the event loop has rendered what was set (the next loop iteration).
    fn queue_tick(&self) {
        if std::mem::replace(&mut self.inner.borrow_mut().tick_queued, true) {
            return;
        }
        let w = self.weak();
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(c) = w.upgrade() {
                c.tick();
            }
        });
    }

    fn tick(&self) {
        self.inner.borrow_mut().tick_queued = false;
        // Textures no frame shows any more.
        {
            let mut i = self.inner.borrow_mut();
            if !i.doomed.is_empty() {
                let keep = if i.visible { i.image_buffer } else { None };
                let doomed = std::mem::take(&mut i.doomed);
                for (b, t) in doomed {
                    if Some(b) == keep {
                        i.doomed.push((b, t));
                    } else if let Some(imp) = &i.importer {
                        imp.forget(t);
                    }
                }
            }
        }
        let Some(f) = self.inner.borrow_mut().in_flight.take() else {
            return;
        };
        self.flipped(f);
        let next = {
            let mut i = self.inner.borrow_mut();
            if i.visible { i.pending.take() } else { None }
        };
        if let Some(n) = next {
            self.set_frame(n);
        }
    }

    fn flipped(&self, f: Shown) {
        let now = Instant::now();
        {
            let mut i = self.inner.borrow_mut();
            if let Some(t) = i.show_at {
                let ms = now.duration_since(t).as_secs_f64() * 1e3;
                if f.answered {
                    log::info!(event = "casa_first_frame", stale = true, ms = ms; "casa: show → stale frame (seq {}) on screen in {ms:.1} ms", f.seq);
                } else {
                    log::info!(event = "casa_first_frame", stale = false, ms = ms; "casa: show → fresh frame (seq {}) on screen in {ms:.1} ms", f.seq);
                    i.show_at = None;
                }
            }
            if !f.answered
                && let Some(t) = i.tap_at.take()
            {
                log::debug!(
                    "casa: touch → frame on screen in {:.1} ms",
                    now.duration_since(t).as_secs_f64() * 1e3
                );
            }
        }
        if !f.answered {
            let (n, lat) = self.stats.get();
            self.stats.set((
                n + 1,
                lat + now.duration_since(f.received).as_micros() as u64,
            ));
            self.send(Message::Presented(proto::Presented {
                buffer: f.buffer,
                skipped: false,
                seq: f.seq,
                time_ns: proto::monotonic_ns(),
            }));
        }
        let old = {
            let mut i = self.inner.borrow_mut();
            i.last_shown = (!f.dropped).then_some(f);
            if f.dropped {
                i.on_screen.take()
            } else {
                i.on_screen.replace(f.buffer).filter(|&o| o != f.buffer)
            }
        };
        if let Some(old) = old {
            self.release(old);
        }
    }

    fn on_drop(&self, buffer: u32) {
        let skip = {
            let mut i = self.inner.borrow_mut();
            let p = i.pending.take_if(|p| p.buffer == buffer);
            if i.on_screen == Some(buffer) {
                i.on_screen = None;
            }
            i.last_shown.take_if(|f| f.buffer == buffer);
            if let Some(f) = i.in_flight.as_mut().filter(|f| f.buffer == buffer) {
                f.dropped = true;
            }
            p
        };
        if let Some(p) = skip {
            self.skip(p);
        }
        let t = self.inner.borrow_mut().textures.remove(&buffer);
        if let Some(t) = t {
            self.retire(buffer, t);
        }
        log::debug!("web view: buffer {buffer} dropped");
    }

    // ------------------------------------------------------------------ touch

    fn send_touch(&self, kind: WebTouch, x: f32, y: f32) {
        self.send(Message::Touch(proto::Touch {
            kind,
            id: 0,
            x,
            y,
            time_ns: proto::monotonic_ns(),
        }));
    }

    /// A touch of the screen while "Casa" may be visible: true = taken (not for Slint).
    pub fn touch(&self, ev: &TouchEvent) -> bool {
        let (x, y) = (ev.x, ev.y);
        let target = {
            let mut i = self.inner.borrow_mut();
            if !i.visible {
                return false;
            }
            let now = Instant::now();
            if ev.kind == TouchKind::Down {
                // The button takes the touch only if it was shown already (and not under a
                // menu or the warning): a touch on its spot while hidden is the page's.
                let on_home = i.controls.armed(now)
                    && i.menu.is_none()
                    && i.confirm.is_none()
                    && i.place.hit(x, y);
                let talking = matches!(
                    self.ui.phase(),
                    Phase::Listening | Phase::Thinking | Phase::Speaking | Phase::Error
                );
                let on_bar = talking && x >= BAR.0 && x < BAR.2 && y >= BAR.1 && y < BAR.3;
                let overlay = i.menu.is_some()
                    || self.ui.screen.get_control_open()
                    || self.ui.house_overlay(x, y)
                    || i.confirm.is_some()
                    || on_bar
                    || matches!(i.status, Status::Error | Status::Offline)
                    || i.image_buffer.is_none();
                i.target = if on_home {
                    i.drag = Some(controls::Drag::start(i.place, (x, y)));
                    Target::Button
                } else if overlay {
                    Target::Slint
                } else {
                    Target::Page
                };
                i.last_touch = now;
                i.tap_at = Some(now);
            }
            // Any touch brings the controls up, and keeps them up.
            i.controls.poke(now);
            let t = i.target;
            if ev.kind == TouchKind::Up {
                i.target = Target::None;
            }
            t
        };
        self.update_controls(Instant::now());
        match target {
            Target::Slint => false,
            Target::None => true,
            Target::Button => {
                self.button_touch(ev);
                true
            }
            Target::Page => {
                let kind = match ev.kind {
                    TouchKind::Down => gesture::Kind::Down,
                    TouchKind::Move => gesture::Kind::Move,
                    TouchKind::Up => gesture::Kind::Up,
                };
                let (route, pull, armed, top) = {
                    let mut i = self.inner.borrow_mut();
                    if kind == gesture::Kind::Down {
                        i.top_touch = i.swipe.side() != controls::Edge::Top && y < i.top.edge;
                    }
                    if i.top_touch {
                        let r = i.top.event(kind, x, y);
                        (r, 0.0, false, true)
                    } else {
                        let r = i.swipe.event(kind, x, y);
                        (r, i.swipe.pull(), i.swipe.armed(), false)
                    }
                };
                if !top {
                    self.ui.screen.casa_set_pull(pull, armed);
                }
                let wk = match ev.kind {
                    TouchKind::Down => WebTouch::Down,
                    TouchKind::Move => WebTouch::Motion,
                    TouchKind::Up => WebTouch::Up,
                };
                match route {
                    gesture::Route::Page => self.send_touch(wk, x, y),
                    gesture::Route::Hold => {}
                    gesture::Route::PageFrom { x: x0, y: y0 } => {
                        self.send_touch(WebTouch::Down, x0, y0);
                        self.send_touch(wk, x, y);
                    }
                    gesture::Route::Tap { x: x0, y: y0 } => {
                        self.send_touch(WebTouch::Down, x0, y0);
                        self.send_touch(WebTouch::Up, x0, y0);
                    }
                    gesture::Route::Home if top => {
                        log::info!(event = "control_center", from = "casa"; "control center over Home Assistant");
                        self.ui.open_control();
                    }
                    gesture::Route::Home => {
                        self.ui.home_sound();
                        self.close("swipe in from the edge")
                    }
                }
                true
            }
        }
    }

    /// A touch that started on the home button: a tap goes home, a drag moves it.
    fn button_touch(&self, ev: &TouchEvent) {
        let mut i = self.inner.borrow_mut();
        let Some(mut d) = i.drag else { return };
        match ev.kind {
            TouchKind::Down => {
                drop(i);
                self.show_button(None);
            }
            TouchKind::Move => {
                let at = d.motion(ev.x, ev.y);
                i.drag = Some(d);
                drop(i);
                if at.is_some() {
                    self.show_button(at);
                }
            }
            TouchKind::Up => {
                i.drag = None;
                match d.end(ev.x, ev.y) {
                    controls::DragEnd::Tap => {
                        drop(i);
                        self.show_button(None);
                        self.ui.home_sound();
                        self.close("home button");
                    }
                    controls::DragEnd::Moved(p) => {
                        i.place = p;
                        drop(i);
                        self.show_button(None);
                        log::info!(
                            event = "casa_home_button", edge = p.edge.name(), offset = p.offset as f64;
                            "casa: home button moved to the {} edge at {:.0}", p.edge.name(), p.offset
                        );
                        crate::settings::Settings::in_dir(&self.state_dir)
                            .save_casa_home_button(p.edge.name(), p.offset);
                    }
                }
            }
        }
    }

    /// The home button where it rests, or at `dragged` (top left) under the finger.
    fn show_button(&self, dragged: Option<(f32, f32)>) {
        let (place, drag) = {
            let mut i = self.inner.borrow_mut();
            // The indicator bar and the swipe follow the button to its edge.
            if i.drag.is_none() {
                let edge = i.place.edge;
                i.swipe.set_side(edge);
            }
            (i.place, i.drag)
        };
        if drag.is_none() {
            self.ui.screen.casa_set_edge(place.edge.name());
        }
        let pressed = drag.is_some();
        let moving = drag.is_some_and(|d| d.moving());
        let (x, y) = match dragged {
            Some(xy) if moving => xy,
            _ if moving => return,
            _ => place.xy(),
        };
        self.ui.screen.casa_set_home_button(x, y, pressed, moving);
    }

    /// The home button and the indicator bar: shown or not (the property set only on a change).
    fn update_controls(&self, now: Instant) {
        let shown = {
            let mut i = self.inner.borrow_mut();
            let shown = i.visible && (i.drag.is_some() || i.controls.shown(now));
            if shown == i.controls_shown {
                return;
            }
            i.controls_shown = shown;
            shown
        };
        self.ui.screen.casa_set_controls(shown);
    }

    /// A new link: the helper is killed and systemd starts it again with the new tokens (the
    /// view shows "loading" meanwhile and reconnects by itself). With no helper connected, the
    /// next one that starts logs in with them anyway.
    fn login_again(&self) {
        let done = match self.inner.borrow().conn.as_ref() {
            Some(c) => c.freezer.kill(),
            None => {
                log::info!("web view: linked; no helper connected, the next one logs in");
                return;
            }
        };
        match done {
            Ok(()) => {
                log::info!(event = "casa_login"; "web view: linked, the helper starts again");
                self.disconnect("starting again with the new link");
            }
            Err(e) => log::warn!(
                event = "casa_login";
                "web view: linked, but the helper cannot start again ({e}): \
                 restart home-companion-web.service for the new tokens"
            ),
        }
    }

    // ------------------------------------------------------------------ every 100 ms

    fn housekeeping(&self) {
        if HOME_REQUESTED.swap(false, Ordering::Relaxed) {
            self.ui.home_sound();
            self.close("volume keys together");
        }
        if LOGIN_REQUESTED.swap(false, Ordering::Relaxed) {
            self.login_again();
        }
        if OPEN_REQUESTED.swap(false, Ordering::Relaxed) {
            self.inner.borrow_mut().open_wanted = Some(Instant::now());
        }
        if let Some(view) = take_view_request() {
            match view_path(&view, &self.cfg.path) {
                Some(path) => {
                    let mut i = self.inner.borrow_mut();
                    i.view_wanted = Some(path);
                    i.open_wanted = Some(Instant::now());
                }
                None => log::warn!(event = "casa_view"; "casa: not a view to open: {view:?}"),
            }
        }
        let open_now = {
            let i = self.inner.borrow();
            i.open_wanted
                .is_some_and(|t| i.conn.is_some() || t.elapsed() >= Duration::from_secs(12))
        };
        if open_now {
            self.inner.borrow_mut().open_wanted = None;
            // From the setup page straight to Home Assistant; the home is under it on the way back.
            if self.ui.screen.get_page() == "setup" {
                self.ui.screen.set_page("home");
            }
            let view = self.inner.borrow_mut().view_wanted.take();
            if let Some(path) = view {
                self.go_to(&path);
            }
            self.open();
        }
        self.flush();
        if self
            .inner
            .borrow()
            .warming
            .is_some_and(|t| t.elapsed() >= WARM_MAX)
        {
            self.end_warm("not ready in time");
        }
        let now = Instant::now();
        let (visible, last_touch, hidden_since) = {
            let i = self.inner.borrow();
            (i.visible, i.last_touch, i.hidden_since)
        };
        let note_over = self.inner.borrow().note_until.is_some_and(|t| now >= t);
        if note_over {
            self.inner.borrow_mut().note_until = None;
            self.ui.screen.casa_set_note("");
        }
        let phase = self.ui.phase();
        if !visible {
            // Back home after a long answer or a ringing timer: the view comes back.
            let reopen = self.inner.borrow().reopen;
            if reopen && matches!(phase, Phase::Home | Phase::Offline) && !self.ui.noticing() {
                self.inner.borrow_mut().reopen = false;
                log::info!("casa: shown again after {}", "a conversation or a timer");
                self.open();
                return;
            }
            if reopen && !phase.in_conversation() && phase != Phase::Ringing {
                self.inner.borrow_mut().reopen = false;
            }
        }
        if visible {
            self.update_controls(now);
            // A frame kept back by the cap, now due.
            let due = {
                let i = self.inner.borrow();
                i.in_flight.is_none() && i.pending.is_some() && !self.throttled(&i, now)
            };
            if due && let Some(f) = self.inner.borrow_mut().pending.take() {
                self.set_frame(f);
            }
            // "Okay Alfred" over the view: the bar (companion.slint); a long answer or a ringing
            // timer takes Alfred's layout, and the view comes back afterwards.
            let takeover = phase == Phase::Ringing
                || (phase == Phase::Speaking && self.ui.long_answer())
                || (matches!(phase, Phase::Home | Phase::Offline) && self.ui.noticing());
            if takeover {
                self.close("a long answer, a timer or a reminder");
                self.inner.borrow_mut().reopen = true;
            } else if phase.in_conversation() || phase == Phase::Error {
                // Nobody touches the page while talking: not "left alone".
                self.inner.borrow_mut().last_touch = now;
            } else if self.cfg.idle_return_seconds > 0.0
                && now.duration_since(last_touch).as_secs_f32() >= self.cfg.idle_return_seconds
            {
                self.close("nobody touched it");
            }
            return;
        }
        if self.cfg.freeze_after_seconds <= 0.0 {
            return;
        }
        let hidden = now.duration_since(hidden_since).as_secs_f32();
        let mut i = self.inner.borrow_mut();
        // Not while the page loads or the preload draws it (it would load on show), unless it
        // never finishes.
        let settled = (i.status != Status::Loading && i.warming.is_none())
            || hidden >= 3.0 * self.cfg.freeze_after_seconds;
        if let Some(c) = i.conn.as_mut()
            && hidden >= self.cfg.freeze_after_seconds
            && settled
            && c.freezer.usable()
            && !c.freezer.frozen()
            && c.out.is_empty()
            && c.freezer.freeze()
        {
            log::info!(event = "casa_frozen", hidden_s = hidden as f64; "web view: helper frozen after {hidden:.0} s hidden");
        }
    }
}

/// Where the user left the home button (settings.toml), or the default.
fn saved_place(state_dir: &Path) -> controls::Place {
    crate::settings::Settings::in_dir(state_dir)
        .casa_home_button()
        .and_then(|(edge, offset)| {
            Some(controls::Place {
                edge: controls::Edge::from_name(&edge)?,
                offset,
            })
        })
        .map(controls::Place::clamped)
        .unwrap_or_default()
}

/// Queues a message for a helper that is behind: a touch motion replaces the previous one.
fn queue(out: &mut VecDeque<Message>, m: Message) {
    if let Message::Touch(t) = &m
        && t.kind == WebTouch::Motion
        && let Some(Message::Touch(last)) = out.back_mut()
        && last.kind == WebTouch::Motion
        && last.id == t.id
    {
        *last = *t;
        return;
    }
    out.push_back(m);
}

/// `home-companion web-login`: the hassTokens JSON line for `home-companion-web --token-stdin`. Linked by QR code
/// (`<state_dir>/ha-auth.json`): the access token, refreshed when it is about to expire, with
/// the refresh token and the link's client id, so Home Assistant's frontend refreshes it by
/// itself; else the long-lived token of `[home_assistant] token_file`. Printed to standard
/// output only (a pipe to the helper), never logged.
pub fn login_line(cfg: &crate::config::Config) -> Result<String, String> {
    let ha = &cfg.home_assistant;
    match cfg.ha_auth().linked_tokens() {
        Ok(Some(t)) => return linked_hass_tokens(&ha.url, &t),
        Ok(None) => {}
        Err(e) => return Err(format!("Home Assistant link: {e}")),
    }
    let token = std::fs::read_to_string(Path::new(&ha.token_file))
        .map_err(|e| format!("{}: {e}", ha.token_file))?;
    hass_tokens(&ha.url, &token)
}

/// hassTokens of linked tokens (Home Assistant's frontend: `expires` in milliseconds).
pub fn linked_hass_tokens(url: &str, t: &crate::ha::auth::Tokens) -> Result<String, String> {
    let origin = origin(url).ok_or("home_assistant.url is not set")?;
    let now = crate::ha::auth::now_secs();
    Ok(serde_json::json!({
        "hassUrl": origin,
        "clientId": t.client_id,
        "access_token": t.access_token,
        "token_type": "Bearer",
        "expires_in": t.expires_at.saturating_sub(now),
        "refresh_token": t.refresh_token,
        "expires": t.expires_at * 1000,
    })
    .to_string())
}

/// `home-companion web-url`: the page for `home-companion-web --url`, from `[home_assistant] url` and `[web] path`.
/// Fails while `[web] enabled` is false: home-companion-web.service uses it as its `ExecCondition`, so the
/// helper (and its few hundred MB) does not run when "Casa" is off.
pub fn url_line(ha: &crate::config::HomeAssistantCfg, web: &WebCfg) -> Result<String, String> {
    if !web.enabled {
        return Err("web.enabled is false".into());
    }
    let path = if web.preload {
        web.path.as_str()
    } else {
        IDLE_PATH
    };
    page_url(&ha.url, path).ok_or_else(|| "home_assistant.url is not an http(s) URL".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_of_the_brain() {
        use crate::brain_link::View;
        let p = |s: &str| View::Path(s.into());
        let cam = |s: &str| View::Camera(s.into());
        assert_eq!(
            view_path(&p("/lovelace/energy"), "/lovelace/0").as_deref(),
            Some("/lovelace/energy")
        );
        assert_eq!(view_path(&p("lovelace"), "/lovelace/0"), None);
        assert_eq!(view_path(&p("//evil.example/x"), "/lovelace/0"), None);
        assert_eq!(view_path(&p("/a\nb"), "/lovelace/0"), None);
        assert_eq!(
            view_path(&cam("camera.front_door"), "/lovelace/0").as_deref(),
            Some("/lovelace/0?more-info-entity-id=camera.front_door")
        );
        assert_eq!(
            view_path(&cam("camera.door"), "/dashboard-x/home?kiosk#top").as_deref(),
            Some("/dashboard-x/home?kiosk&more-info-entity-id=camera.door")
        );
        assert_eq!(view_path(&cam("light.kitchen"), "/lovelace/0"), None);
        assert_eq!(view_path(&cam("camera.a&b=c"), "/lovelace/0"), None);
        assert_eq!(view_path(&cam("camera."), "/lovelace/0"), None);
    }

    #[test]
    fn navigation_in_place() {
        let u = navigate_url("/lovelace/0?more-info-entity-id=camera.door");
        assert_eq!(
            u,
            "javascript:history.pushState(null,'',%22/lovelace/0?more-info-entity-id=camera.door%22);%0A\
             window.dispatchEvent(new%20CustomEvent('location-changed',{detail:{replace:false}}));%0Avoid%200"
        );
        // A quote in the path stays inside the string.
        assert!(navigate_url("/a\"b").contains("%22/a%5C%22b%22"));
    }

    #[test]
    fn scripts_become_javascript_urls() {
        let u = javascript_url("// a comment\nlet a = 1 % 2;\n  f(\"x\");\n");
        assert_eq!(
            u,
            "javascript:let%20a%20=%201%20%25%202;%0Af(%22x%22);%0Avoid%200"
        );
        assert!(!javascript_url(OFFSCREEN_JS).contains(' '));
    }

    #[test]
    fn origin_and_tokens() {
        assert_eq!(
            origin("http://ha.example:8123/lovelace/0").as_deref(),
            Some("http://ha.example:8123")
        );
        assert_eq!(
            origin("https://ha.example").as_deref(),
            Some("https://ha.example")
        );
        assert_eq!(origin("ftp://x/y"), None);
        assert_eq!(origin("http:///x"), None);
        let j: serde_json::Value =
            serde_json::from_str(&hass_tokens("http://ha.example:8123/x", " abc\n").unwrap())
                .unwrap();
        assert_eq!(j["access_token"], "abc");
        assert_eq!(j["hassUrl"], "http://ha.example:8123");
        assert_eq!(j["clientId"], "http://ha.example:8123/");
        assert!(hass_tokens("http://ha.example", "  ").is_err());
        assert!(hass_tokens("", "abc").is_err());
        let t = crate::ha::auth::Tokens {
            hass_url: "http://ha.example:8123".into(),
            client_id: "http://192.0.2.5:8124/".into(),
            access_token: "A".into(),
            refresh_token: "R".into(),
            expires_at: crate::ha::auth::now_secs() + 1000,
        };
        let j: serde_json::Value =
            serde_json::from_str(&linked_hass_tokens("http://ha.example:8123/", &t).unwrap())
                .unwrap();
        assert_eq!(j["clientId"], "http://192.0.2.5:8124/");
        assert_eq!(j["refresh_token"], "R");
        assert_eq!(j["expires"], t.expires_at * 1000);
        assert!(j["expires_in"].as_u64().unwrap() <= 1000);
        assert_eq!(
            page_url("http://ha.example:8123/", "/lovelace/0").as_deref(),
            Some("http://ha.example:8123/lovelace/0")
        );
        assert_eq!(page_url("", "/lovelace/0"), None);
    }

    #[test]
    fn config_checks() {
        let mut c = WebCfg::default();
        assert!(c.validate().is_ok());
        c.scale = 9.0;
        assert!(c.validate().is_err());
        c.scale = 1.0;
        c.heavy_views = vec!["dashboard".into()];
        assert!(c.validate().is_err());
        c.heavy_views = vec!["/dashboard-3d".into()];
        assert!(c.validate().is_ok());
        let ha = crate::config::HomeAssistantCfg {
            url: "http://ha.example:8123".into(),
            ..Default::default()
        };
        c.enabled = false;
        assert!(
            url_line(&ha, &c).is_err(),
            "home-companion-web.service's ExecCondition"
        );
        c.enabled = true;
        assert_eq!(
            url_line(&ha, &c).as_deref(),
            Ok("http://ha.example:8123/lovelace/0")
        );
        // Not preloaded: the helper waits on a tiny page of the same origin.
        c.preload = false;
        assert_eq!(
            url_line(&ha, &c).as_deref(),
            Ok("http://ha.example:8123/robots.txt")
        );
        c.preload = true;
        c.path = "lovelace/0".into();
        assert!(c.validate().is_err());
        c.path = "/lovelace/0".into();
        c.home_button_seconds = 0.0;
        assert!(c.validate().is_ok());
        c.home_button_seconds = -1.0;
        assert!(c.validate().is_err());
        c.home_button_seconds = f32::NAN;
        assert!(c.validate().is_err());
    }

    #[test]
    fn queued_motions_collapse() {
        let t = |kind, y| {
            Message::Touch(proto::Touch {
                kind,
                id: 0,
                x: 1.0,
                y,
                time_ns: 0,
            })
        };
        let mut q = VecDeque::new();
        queue(&mut q, t(WebTouch::Down, 1.0));
        queue(&mut q, t(WebTouch::Motion, 2.0));
        queue(&mut q, t(WebTouch::Motion, 3.0));
        queue(&mut q, t(WebTouch::Up, 3.0));
        assert_eq!(q.len(), 3);
        assert_eq!(q[1], t(WebTouch::Motion, 3.0));
    }
}
