// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The UI thread's side of the body: character and language on the screen, the commands of
//! the core applied to it, the clock. `load_pose`, `set_character`, `set_language`,
//! `set_mouth`, `set_body_text`, `local_now` and `save_png` are copied from
//! crates/spike-ui/src/bin/companion-sim.rs (a binary: copying is the only way to share them),
//! made generic over [`Screen`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use character::{Animation, Character, Crop, Rgb, State};
use locale::Locale;
use slint::{Color, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};

use super::anim::{self, Clip, Mode, Playhead, Sequencer, Show, Timing};
use super::backlight::{Awake, Backlight, Fade, Levels, WakeSource};
use super::cards::{self, Alive, CardWords};
use super::house::{self, Dnd, HouseCards};
use super::light::{Auto, Curve};
use super::lists::{self, Choice, ShopList};
use super::next;
use super::widgets::{self, Band, Kind as WidgetKind, Lists, Page as WidgetPage};
use super::{
    AlarmLineView, MemoriesView, MemoryLine, MusicView, Screen, SettingsView, TimerLineView,
    UpdateView,
};
use crate::alarms::Alarm;
use crate::brain_link::{self, BrainLink, BrainMsg, Upcoming};
use crate::core::{CoreEvent, MirrorInfo, Phase, UiCommand, UiEvent};
use crate::ha::hub::{HubCmd, HubMsg};
use crate::ha::weather::Weather;
use crate::history::History;
use crate::settings::{
    CONVERSATION_MAX_DEFAULT, CONVERSATION_MAX_MAX, CONVERSATION_MAX_MIN, Settings, WakeSensitivity,
};
use crate::settings_model::{self as model, Change};

mod board_ui;
mod panel_ui;
mod remote_ui;
pub mod samples;
use crate::timers::{TimerView, now_unix};
use serde_json::json;

fn color(c: Rgb) -> Color {
    Color::from_rgb_u8(c.0, c.1, c.2)
}

/// How far (per channel) a pose's background may be from the character's and still count as it.
pub const PLATE_TOLERANCE: u8 = 6;

/// A pose picture cropped to `crop`, as a Slint image (RGBA). Pixels within
/// [`PLATE_TOLERANCE`] of `bg` become exactly `bg`: the pictures' backgrounds are a shade off
/// the window's, which shows as a faint rectangle around the stage on the panel in the dark.
pub fn load_pose(path: &Path, crop: Crop, bg: Rgb) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut data = vec![0; reader.output_buffer_size().ok_or("png too large")?];
    let info = reader
        .next_frame(&mut data)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let channels = info.color_type.samples();
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(crop.width(), crop.height());
    let out = buf.make_mut_slice();
    for y in 0..crop.height() {
        let row = &data[(crop.y0 + y) as usize * info.line_size..];
        for x in 0..crop.width() {
            let p = &row[(crop.x0 + x) as usize * channels..];
            let a = if channels == 4 { p[3] } else { 255 };
            let mut px = [p[0], p[1], p[2], a];
            character::flatten(&mut px, bg, PLATE_TOLERANCE);
            out[(y * crop.width() + x) as usize] = Rgba8Pixel {
                r: px[0],
                g: px[1],
                b: px[2],
                a,
            };
        }
    }
    Ok(Image::from_rgba8(buf))
}

/// A pose of the character on the home: one of its own (by state), or one downloaded from
/// Settings → Poses (by name, src/poses.rs) that a card of Home Assistant asks for.
#[derive(Clone, Debug, PartialEq)]
enum Look {
    Own(State),
    Downloaded(String),
}

impl Look {
    fn name(&self) -> &str {
        match self {
            Look::Own(s) => s.name(),
            Look::Downloaded(n) => n,
        }
    }
}

/// The state of that name (`working`, `muted`), as in character.toml.
fn state_named(name: &str) -> Option<State> {
    State::ALL.into_iter().find(|s| s.name() == name)
}

/// What a card's `pose: <name>` shows: the downloaded pose of that name first, else the
/// character's own pose of the state of that name (`has_pose`); None: neither, the card shows
/// alone.
fn card_look_of(name: &str, downloaded: bool, has_pose: impl Fn(State) -> bool) -> Option<Look> {
    if downloaded {
        return Some(Look::Downloaded(name.to_string()));
    }
    state_named(name).filter(|s| has_pose(*s)).map(Look::Own)
}

/// The home's pose: a card's pose in place of the moment's (`moment`), except on the pages with
/// their own animation ([`paged`]), where only the moment's link and working poses show
/// (`Ui::home_state`).
fn home_look_of(card: Option<Look>, paged: bool, moment: State) -> Look {
    match card {
        Some(l) if !paged => l,
        _ => Look::Own(moment),
    }
}

/// A downloaded pose's animation table (`<name>.toml`): `file` (in `dir`), `size`, `frames`, `fps`,
/// `pause` (optional) and `rect`, as an animation of character.toml.
fn pose_animation(text: &str, dir: &Path) -> Result<Animation, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        file: String,
        size: [u32; 2],
        frames: [u32; 2],
        fps: f32,
        #[serde(default)]
        pause: f32,
        rect: [f32; 4],
    }
    let r: Raw = toml::from_str(text).map_err(|e| e.to_string())?;
    if r.file.contains('/') || r.file.contains("..") {
        return Err(format!("file = {:?}: a file next to the pose", r.file));
    }
    if r.frames[1] <= r.frames[0] || !(r.fps > 0.0 && r.fps <= 60.0) {
        return Err("frames = [first, end] with end > first, fps 1 to 60".into());
    }
    Ok(Animation {
        name: anim::CARD_POSE.into(),
        file: dir.join(r.file),
        size: (r.size[0], r.size[1]),
        first: r.frames[0],
        end: r.frames[1],
        fps: r.fps,
        pause: r.pause,
        rect: r.rect,
    })
}

/// A whole picture of the character that is not a pose (the do not disturb prop), with the
/// background snapped as the poses'; none (an empty image) when the character has no such file.
fn load_prop(path: &Path, bg: Rgb) -> Image {
    if !path.exists() {
        return Image::default();
    }
    let size = std::fs::File::open(path)
        .map_err(|e| e.to_string())
        .and_then(|f| {
            png::Decoder::new(std::io::BufReader::new(f))
                .read_info()
                .map(|r| (r.info().width, r.info().height))
                .map_err(|e| e.to_string())
        });
    match size.and_then(|(w, h)| {
        load_pose(
            path,
            Crop {
                x0: 0,
                y0: 0,
                x1: w,
                y1: h,
            },
            bg,
        )
    }) {
        Ok(i) => i,
        Err(e) => {
            log::warn!("{}: {e}", path.display());
            Image::default()
        }
    }
}

/// The link's QR code as a picture of the page's 264 px square: dark modules on ivory, each
/// module a whole number of pixels (centred; the quiet zone takes the rest), so no module is
/// drawn wider than another.
fn qr_image(text: &str) -> Option<Image> {
    const SIZE: usize = 264;
    let (side, modules) = crate::ha::link::qr_modules(text)?;
    let k = (SIZE / side).max(1);
    let size = SIZE.max(side * k);
    let off = (size - side * k) / 2;
    let (dark, light) = ([0x03, 0x05, 0x0d], [0xef, 0xe9, 0xdf]);
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(size as u32, size as u32);
    for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i % size, i / size);
        let on = x >= off
            && y >= off
            && (x - off) / k < side
            && (y - off) / k < side
            && modules[(y - off) / k * side + (x - off) / k];
        let c = if on { dark } else { light };
        *px = Rgba8Pixel {
            r: c[0],
            g: c[1],
            b: c[2],
            a: 255,
        };
    }
    Some(Image::from_rgba8(buf))
}

/// A word of the locale with `{name}` filled in.
fn word(t: &Locale, key: &str, name: &str) -> String {
    t.text(key).replace("{name}", name)
}

/// The brain's `memory.list` as the "Ricordi" page shows it: what Alfred knows (in the order the
/// assistant reads it; past `shown` dimmed), what is to confirm, and how many known facts the
/// assistant finds only when asked.
fn memory_lines(
    a: &serde_json::Value,
    date: impl Fn(i64) -> String,
) -> (Vec<MemoryLine>, Vec<MemoryLine>, usize) {
    let shown = a["shown"].as_u64().map_or(usize::MAX, |n| n as usize);
    let (mut facts, mut confirm) = (Vec::new(), Vec::new());
    for f in a["facts"].as_array().into_iter().flatten() {
        let line = MemoryLine {
            id: f["id"].as_i64().unwrap_or(0),
            text: f["text"].as_str().unwrap_or("").to_string(),
            date: date(f["updated"].as_i64().unwrap_or(0)),
            dim: false,
        };
        if f["status"] == "candidate" {
            confirm.push(line);
        } else {
            facts.push(MemoryLine {
                dim: facts.len() >= shown,
                ..line
            });
        }
    }
    let over = facts.len().saturating_sub(shown);
    (facts, confirm, over)
}

/// The note under the known facts: how many the assistant finds only when asked ("" = none).
fn more_line(l: &Locale, over: usize) -> String {
    match over {
        0 => String::new(),
        1 => l.text("memories_more_one").to_string(),
        n => l.text("memories_more").replace("{n}", &n.to_string()),
    }
}

/// "Right" (`yes`) or "Wrong"/delete on a fact of the "Ricordi" page, for the brain.
fn memory_answer(id: i64, yes: bool) -> serde_json::Value {
    if yes {
        json!({"cmd": "memory.confirm", "id": id})
    } else {
        json!({"cmd": "memory.delete", "id": id})
    }
}

/// The use of the AI in words: the Home Assistant page's row (today only) and the use page
/// (today large, this month, one quiet line, the last days' bars, where the numbers come from).
/// An estimate says so ("~", "estimated") and tells where precise numbers come from.
fn usage_view(v: &crate::usage::View, text: impl Fn(&str) -> String) -> super::UsageView {
    use crate::usage::{Source, amount};
    let sep = match text("decimal_separator") {
        s if s == "decimal_separator" => ".".to_string(),
        s => s,
    };
    let precise = v.is_precise();
    let money = |x: f64| amount(x, precise, &sep);
    let key = |estimate: &str, exact: &str| text(if precise { exact } else { estimate });
    let detail = match v.source {
        Source::Estimate if v.today.questions > 0 => text("usage_detail")
            .replace("{q}", &v.today.questions.to_string())
            .replace(
                "{stt}",
                &format!("{:.1}", v.today.stt_seconds / 60.0).replace('.', &sep),
            )
            .replace(
                "{tts}",
                &format!("{:.1}", v.today.tts_seconds / 60.0).replace('.', &sep),
            ),
        Source::Estimate => String::new(),
        Source::Precise { requests, tokens } => [
            requests.map(|n| text("usage_requests").replace("{n}", &n.to_string())),
            tokens.map(|n| text("usage_tokens").replace("{n}", &n.to_string())),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · "),
    };
    let max = v.days.iter().map(|d| d.1).fold(0.0f64, f64::max);
    let n = v.days.len();
    let bars = v
        .days
        .iter()
        .enumerate()
        .map(|(i, (day, cost))| {
            // Today and every week before it.
            let label = if (n - 1 - i) % 7 == 0 {
                day.to_string()
            } else {
                String::new()
            };
            let h = if max > 0.0 { (cost / max) as f32 } else { 0.0 };
            (label, h, money(*cost))
        })
        .collect();
    super::UsageView {
        label: text("usage_label"),
        line: key("usage_line_estimate", "usage_line_precise")
            .replace("{today}", &money(v.today_cost)),
        today_label: key("usage_today_estimate", "usage_today"),
        today: format!("{} $", money(v.today_cost)),
        month: text("usage_month_line").replace("{month}", &money(v.month_cost)),
        detail,
        chart_label: text("usage_days"),
        max: if max > 0.0 {
            format!("{} $", money(max))
        } else {
            String::new()
        },
        bars,
        // Where to get the integration on a line of its own (a link is not broken across lines).
        note: if precise {
            text("usage_source")
        } else {
            format!("{}\n{}", text("usage_hint"), crate::ha::usage::WHERE)
        },
        precise,
    }
}

/// The choice rows of the Home Assistant page: (kind, label, the one in use) for each kind with
/// something to choose between (two options or more); "—" when none is in use (the voice: the
/// engine's own default).
fn ha_picks(
    c: &crate::ha::setup::Choices,
    text: impl Fn(&str) -> String,
) -> Vec<(String, String, String)> {
    crate::ha::setup::CHOICES
        .iter()
        .filter(|k| c.of(k).len() >= 2)
        .map(|k| {
            let now = c.current_name(k);
            (
                k.to_string(),
                text(&format!("ha_choice_{k}")),
                match (now.is_empty(), *k) {
                    (true, "voice") => text("ha_choice_voice_default"),
                    (true, _) => "—".into(),
                    _ => now,
                },
            )
        })
        .collect()
}

/// The body's words of a language (locales/<code>.toml).
pub fn set_body_text<S: Screen>(app: &S, t: &Locale, name: &str) {
    app.set_words(&|k| word(t, k, name));
    app.set_phrase_starting(t.text("starting"));
    // Monday first, one letter (L M M G V S D).
    let letters: Vec<String> = (1..=7)
        .map(|i| {
            t.days[i % 7]
                .chars()
                .next()
                .map_or(String::new(), |c| c.to_uppercase().collect())
        })
        .collect();
    app.set_day_letters(&letters);
}

pub fn set_character<S: Screen>(app: &S, c: &Character, lang: &str) {
    let p = c.phrases(lang);
    app.set_character_name(&c.name);
    app.set_character_background(color(c.background));
    app.set_color_listening(color(c.color(State::Listening)));
    app.set_color_thinking(color(c.color(State::Thinking)));
    app.set_color_speaking(color(c.color(State::Speaking)));
    app.set_color_error(color(c.color(State::Error)));
    app.set_phrase_listening(&p.listening);
    app.set_phrase_thinking(&p.thinking);
    app.set_phrase_error(&p.error);
    app.set_phrase_idle(&p.idle);
}

/// Everything that depends on the language; returns the locale in use (English if `lang`
/// has none).
pub fn set_language<S: Screen>(
    app: &S,
    c: &Character,
    locales: &Path,
    lang: &str,
) -> Result<Locale, String> {
    let text = locale::load_or_english(locales, lang).map_err(|e| e.join("; "))?;
    set_character(app, c, lang);
    set_body_text(app, &text, &c.name);
    let rows: Vec<(String, String, bool)> = c
        .languages()
        .map(|code| {
            let name = locale::load(locales, code).map_or_else(|_| code.to_uppercase(), |l| l.name);
            (code.to_string(), name, code == lang)
        })
        .collect();
    app.set_languages(&rows);
    Ok(text)
}

/// The mouth's box on screen, scaled and placed like the idle pose.
pub fn set_mouth<S: Screen>(app: &S, c: &Character) {
    let (_, crop) = c.pose(State::Idle);
    let scale = app.get_character_height() / crop.height() as f32;
    let left = app.get_character_cx() - crop.width() as f32 * scale / 2.0;
    let top = app.get_character_top();
    app.set_mouth(
        left + (c.mouth.x0 - crop.x0) as f32 * scale,
        top + (c.mouth.y0 - crop.y0) as f32 * scale,
        c.mouth.width() as f32 * scale,
        c.mouth.height() as f32 * scale,
    );
}

/// Local time as (day of week 0 = Sunday, day, month 0-11, hour, minute).
pub fn local_now() -> (usize, i32, usize, i32, i32) {
    // SAFETY: time(NULL) and localtime_r only write into the struct we pass.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (
            tm.tm_wday as usize,
            tm.tm_mday,
            tm.tm_mon as usize,
            tm.tm_hour,
            tm.tm_min,
        )
    }
}

pub fn save_png<S: Screen>(app: &S, path: &Path) -> Result<(), String> {
    let buf = app
        .window()
        .take_snapshot()
        .map_err(|e| format!("take_snapshot: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), buf.width(), buf.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| format!("png: {e}"))?;
    w.write_image_data(buf.as_bytes())
        .map_err(|e| format!("png: {e}"))?;
    log::info!(event = "screenshot", path = path.to_string_lossy().as_ref(); "screenshot {}", path.display());
    Ok(())
}

/// Screen state and pose of a phase.
/// How long a reflex's nod stays on the home.
const NOD_FOR: Duration = Duration::from_secs(3);
/// How long the satisfied pose stays after a job went well.
const SATISFIED_FOR: Duration = Duration::from_secs(10);
/// The bow after the Home Assistant setup (the `done` clip): the satisfied pose comes after it.
const BOW: Duration = Duration::from_secs(3);

/// What decides the home's pose besides idle.
#[derive(Debug, Clone, Copy, Default)]
struct HomeMoment {
    mic_off: bool,
    linking: bool,
    working: bool,
    satisfied: bool,
    nod: bool,
}

/// The home's pose for the moment, the most lasting first: the microphone off, the link pages,
/// a job under way, one gone well, a reflex's nod; else idle.
fn home_state_of(m: HomeMoment) -> State {
    if m.mic_off {
        State::Muted
    } else if m.linking {
        State::Link
    } else if m.working {
        State::Working
    } else if m.satisfied {
        State::Satisfied
    } else if m.nod {
        State::Nod
    } else {
        State::Idle
    }
}

fn view(p: Phase) -> (&'static str, State) {
    match p {
        Phase::Home => ("home", State::Idle),
        Phase::Offline => ("home", State::Offline),
        Phase::Ringing => ("ringing", State::TimerFinished),
        Phase::Listening => ("listening", State::Listening),
        Phase::Thinking => ("thinking", State::Thinking),
        Phase::Speaking => ("speaking", State::Speaking),
        Phase::Error => ("error", State::Error),
    }
}

/// Screen state (companion.slint) → pose, for the debug keys 1-5.
fn character_state(screen: &str) -> State {
    match screen {
        "listening" => State::Listening,
        "thinking" => State::Thinking,
        "speaking" => State::Speaking,
        "error" => State::Error,
        "ringing" => State::TimerFinished,
        _ => State::Idle,
    }
}

/// Fixed data for `--state` (golden screenshots and docs): nothing from the core is shown.
#[derive(Debug, Clone, Default)]
pub struct Demo {
    pub phase: Phase,
    /// The volume HUD shown at this percentage (`--state volume`).
    pub volume: Option<u8>,
    /// A desk page ("altro", "chat", "timers", "settings", "music", "setup", "screen").
    pub page: &'static str,
    /// A dialog over it ("suspend", "new-timer", "new-alarm").
    pub dialog: &'static str,
    pub cards: bool,
    /// `--state cards-N`: the home with exactly N cards (message, timer, alarm, music).
    pub cards_n: Option<usize>,
    /// The compact home (Settings → Screen).
    pub compact: bool,
    /// `--state music-video`: the music page with a 16:9 picture (a video's thumbnail).
    pub music_wide: bool,
    /// `--state music-big|music-ambient|music-inbar`: a layout of the music page to try.
    pub music_layout: i32,
    pub night: bool,
    pub starting: bool,
    /// The Home Assistant page: 1 = a run in progress, 2 = "All set".
    pub ha: u8,
    /// `--state cards-setup`: the "Complete" card of the setup in Home Assistant on top.
    pub setup_card: bool,
    /// `--state reminder-due`: a reminder due now, full screen.
    pub notice: bool,
    /// `--state control` / `control-music`: the control center pulled down.
    pub control: bool,
    /// `--state mirror-*` / `home-mirroring`: a screen mirrored, its frame's size.
    pub mirror: Option<(u32, u32)>,
    /// `--state mirror-opening-NN`: the page's opening frozen at NN % (animation frames).
    pub mirror_at: Option<f32>,
    /// `--state first-ha-*`: the first start's Home Assistant page ("searching", "found", "none").
    pub find: &'static str,
    /// `--state ha-home` / `weather` / `ha-lists` / `reminders-events`: v0.2's Home Assistant
    /// screens on sample data.
    pub v02: &'static str,
    /// v0.2 `--state house-done*` / `house-ask*`: the brain's "done" card (1) or question (2).
    pub house: u8,
    /// `--state dnd`: do not disturb on.
    pub dnd: bool,
    /// `--state house-done-card`: the small "done" card, after the Undo pop-up.
    pub house_card_only: bool,
    /// `--state usage-precise`: the use page with the numbers of the `openai_usage` integration.
    pub usage_precise: bool,
    /// `--state house-done-group*`: four lights off by one sentence, one card.
    pub house_group: bool,
    /// The question's window after a spoken yes: "Doing it…", Cancel only.
    pub house_pending: bool,
    /// `--state house-done-asked`: the done going on in the question's window.
    pub house_from_ask: bool,
    /// `--state mic-*`: the microphone off by the switch (line and tile); `mic_hud` = the
    /// switch's card, Some(on).
    pub mic_switch_off: bool,
    pub mic_hud: Option<bool>,
    /// `--state thinking-answer`: Home Assistant's written answer is in, its voice not yet.
    pub answer_early: bool,
    /// `--state ha-pick` / `ha-pick-voice`: what the choice page lists ("ai", "voice").
    pub pick: &'static str,
    /// `--state widget*`: the home's widget on sample data, on this page.
    pub widget: Option<WidgetKind>,
    /// `--state widget-medium` / `widget-line`: one card (a timer) or two over it.
    pub widget_cards: usize,
    /// `--state widget-weather-tomorrow*`: the same day at 19:10, after sunset (the weather
    /// widget shows tomorrow).
    pub evening: bool,
    /// `--state settings-anim-*`: one frame of an animation (its sheet decoded at once).
    pub frame: Option<(&'static str, u32)>,
    /// `--state settings-panel` (1), `panel-ask` (2), `panel-blocked` (3): the browser panel's
    /// page, its access dialog, the home's card after too many requests; `settings-panel-remote`
    /// (4), `remote-sign` (5): the remote screen's switch on, its sign.
    pub panel: u8,
    /// `--state board` (1), `board-confirm` (2), `board-empty` (3), `widget-custom*` (4),
    /// `board-talk` (5), `board-swipe` (6), `board-opening` (7): the board on sample data
    /// (board_ui::demo_board).
    pub board: u8,
}

/// The screens `--state` knows, for the golden screenshots (tools/shoot-golden.sh).
pub const DEMO_STATES: [&str; 118] = [
    "home",
    "cards",
    "cards-setup",
    "listening",
    "thinking",
    "thinking-answer",
    "speaking",
    "error",
    "offline",
    "ringing",
    "reminder-due",
    "volume",
    "altro",
    "chat",
    "timers",
    "settings",
    "language",
    "screen",
    "music",
    "setup",
    "ha",
    "usage",
    "usage-precise",
    "ha-working",
    "ha-done",
    "ha-pick",
    "memories",
    "shopping",
    "reminders",
    "night",
    "starting",
    "suspend",
    "new-timer",
    "new-alarm",
    "control",
    "control-music",
    "mirror-portrait",
    "mirror-landscape",
    "mirror-ipad",
    "mirror-listening",
    "home-mirroring",
    "first-language",
    "first-ha-searching",
    "first-ha-found",
    "first-ha-none",
    "house-done",
    "house-done-card",
    "house-done-group",
    "house-done-group-card",
    "house-done-speaking",
    "house-ask",
    "house-ask-pending",
    "house-done-asked",
    "dnd",
    "control-dnd",
    "mic-switch-off",
    "mic-switch-on",
    "mic-off",
    "control-mic-switch",
    "mic-off-dnd",
    "cards-quiet",
    "music-video",
    "music-big",
    "music-ambient",
    "music-inbar",
    "music-inbar-video",
    "cards-0",
    "cards-1",
    "cards-2",
    "cards-3",
    "cards-4",
    "what",
    "update",
    "update-confirm",
    "widget",
    "widget-weather",
    "widget-weather-compact",
    "widget-weather-tomorrow",
    "widget-weather-tomorrow-compact",
    "widget-weather-line",
    "widget-weather-medium",
    "widget-shopping",
    "widget-medium",
    "widget-line",
    "widget-compact",
    "widget-quotes",
    "widget-quotes-line",
    "widget-cities",
    "trends",
    "city",
    "widget-notes",
    "notes",
    "widget-sensors",
    "widget-check",
    "widget-playing",
    "widget-home",
    "trends-sensors",
    "settings-widgets",
    "settings-anim-in",
    "settings-anim-hold",
    "settings-anim-out",
    "ha-anim-hold",
    "ha-anim-type",
    "muted-anim-in",
    "settings-panel",
    "panel-ask",
    "panel-blocked",
    "settings-panel-remote",
    "remote-sign",
    "poses",
    "board",
    "board-confirm",
    "board-empty",
    "board-talk",
    "board-swipe",
    "board-opening",
    "widget-custom",
    "widget-custom-line",
];

impl Demo {
    /// `--state S`: a phase, a page, a dialog or a system state on fixed sample data.
    pub fn parse(s: &str) -> Option<Demo> {
        let home = Demo {
            phase: Phase::Home,
            page: "home",
            ..Default::default()
        };
        let page = |page: &'static str| Demo {
            page,
            ..home.clone()
        };
        let d = match s {
            "volume" => Demo {
                volume: Some(40),
                ..home.clone()
            },
            "cards" => Demo {
                cards: true,
                ..home.clone()
            },
            "reminder-due" => Demo {
                notice: true,
                ..home.clone()
            },
            "control" => Demo {
                control: true,
                ..home.clone()
            },
            "control-music" => Demo {
                control: true,
                cards: true,
                ..home.clone()
            },
            "mirror-portrait" => Demo {
                mirror: Some((368, 800)),
                ..page("mirror")
            },
            "mirror-landscape" => Demo {
                mirror: Some((1280, 592)),
                ..page("mirror")
            },
            "mirror-ipad" => Demo {
                mirror: Some((1066, 800)),
                ..page("mirror")
            },
            "mirror-listening" => Demo {
                phase: Phase::Listening,
                mirror: Some((368, 800)),
                ..page("mirror")
            },
            // Not a golden one: the conversation bar over a landscape sender.
            "mirror-landscape-speaking" => Demo {
                phase: Phase::Speaking,
                mirror: Some((1280, 592)),
                ..page("mirror")
            },
            "home-mirroring" => Demo {
                mirror: Some((368, 800)),
                ..home.clone()
            },
            // Frames of the opening (not golden screenshots): mirror-opening-40 = 40 %, a
            // portrait phone; mirror-opening-landscape-40 the same with a landscape one.
            s if s.starts_with("mirror-opening-") => {
                let rest = &s["mirror-opening-".len()..];
                let (size, pct) = match rest.strip_prefix("landscape-") {
                    Some(p) => ((1280, 592), p),
                    None => ((368, 800), rest),
                };
                let pct: f32 = pct.parse().ok()?;
                Demo {
                    mirror: Some(size),
                    mirror_at: Some((pct / 100.0).clamp(0.0, 1.0)),
                    ..page("mirror")
                }
            }
            "ha-home" => Demo {
                v02: "home",
                ..home.clone()
            },
            "ha-next" => Demo {
                v02: "next",
                ..home.clone()
            },
            "weather" => Demo {
                v02: "weather",
                ..page("weather")
            },
            "ha-lists" => Demo {
                v02: "lists",
                ..page("ha-lists")
            },
            "reminders-events" => Demo {
                v02: "events",
                ..page("reminders")
            },
            "house-done" => Demo {
                house: 1,
                cards: true,
                ..home.clone()
            },
            "house-done-card" => Demo {
                house: 1,
                cards: true,
                house_card_only: true,
                ..home.clone()
            },
            "house-done-group" => Demo {
                house: 1,
                cards: true,
                house_group: true,
                ..home.clone()
            },
            "house-done-group-card" => Demo {
                house: 1,
                cards: true,
                house_group: true,
                house_card_only: true,
                ..home.clone()
            },
            "house-done-speaking" => Demo {
                house: 1,
                phase: Phase::Speaking,
                ..home.clone()
            },
            "house-ask-pending" => Demo {
                house: 2,
                house_pending: true,
                ..home.clone()
            },
            "house-ask" => Demo {
                house: 2,
                ..home.clone()
            },
            "house-done-asked" => Demo {
                house: 1,
                house_from_ask: true,
                ..home.clone()
            },
            "house-ask-mirror" => Demo {
                house: 2,
                mirror: Some((1280, 592)),
                ..page("mirror")
            },
            "house-done-mirror" => Demo {
                house: 1,
                mirror: Some((1280, 592)),
                ..page("mirror")
            },
            "dnd" => Demo {
                dnd: true,
                ..home.clone()
            },
            "mic-switch-off" => Demo {
                mic_switch_off: true,
                mic_hud: Some(false),
                ..home.clone()
            },
            "mic-switch-on" => Demo {
                mic_hud: Some(true),
                ..home.clone()
            },
            "mic-off" => Demo {
                mic_switch_off: true,
                ..home.clone()
            },
            "mic-off-dnd" => Demo {
                mic_switch_off: true,
                dnd: true,
                ..home.clone()
            },
            "cards-0" | "cards-1" | "cards-2" | "cards-3" | "cards-4" => Demo {
                cards: true,
                compact: true,
                cards_n: s[6..].parse().ok(),
                ..home.clone()
            },
            "music-video" => Demo {
                music_wide: true,
                ..page("music")
            },
            "music-big" | "music-ambient" | "music-inbar" | "music-inbar-video" => Demo {
                music_layout: match s {
                    "music-big" => 1,
                    "music-ambient" => 2,
                    _ => 3,
                },
                music_wide: s.ends_with("-video"),
                ..page("music")
            },
            "cards-quiet" => Demo {
                cards: true,
                compact: true,
                mic_switch_off: true,
                dnd: true,
                ..home.clone()
            },
            "control-mic-switch" => Demo {
                control: true,
                mic_switch_off: true,
                ..home.clone()
            },
            "thinking-answer" => Demo {
                phase: Phase::Thinking,
                answer_early: true,
                ..home.clone()
            },
            "dnd-night" => Demo {
                dnd: true,
                night: true,
                ..home.clone()
            },
            "control-dnd" => Demo {
                control: true,
                dnd: true,
                ..home.clone()
            },
            "cards-setup" => Demo {
                cards: true,
                setup_card: true,
                ..home.clone()
            },
            "night" => Demo {
                night: true,
                ..home.clone()
            },
            "widget"
            | "widget-weather"
            | "widget-weather-compact"
            | "widget-weather-tomorrow"
            | "widget-weather-tomorrow-compact"
            | "widget-weather-line"
            | "widget-weather-medium"
            | "widget-shopping"
            | "widget-medium"
            | "widget-line"
            | "widget-compact"
            | "widget-quotes"
            | "widget-quotes-line"
            | "widget-cities"
            | "widget-notes"
            | "widget-sensors"
            | "widget-check"
            | "widget-playing"
            | "widget-home" => Demo {
                widget: Some(match s {
                    s if s.starts_with("widget-weather") => WidgetKind::Weather,
                    "widget-shopping" => WidgetKind::Shopping,
                    "widget-quotes" | "widget-quotes-line" => WidgetKind::Quotes,
                    "widget-cities" => WidgetKind::Cities,
                    "widget-notes" => WidgetKind::Notes,
                    "widget-sensors" => WidgetKind::Sensors,
                    "widget-check" => WidgetKind::Check,
                    "widget-playing" => WidgetKind::Playing,
                    "widget-home" => WidgetKind::Home,
                    _ => WidgetKind::Next,
                }),
                widget_cards: match s {
                    "widget-medium" | "widget-weather-medium" => 1,
                    "widget-line" | "widget-quotes-line" | "widget-weather-line" => 2,
                    _ => 0,
                },
                compact: s == "widget-compact" || s.ends_with("-compact"),
                evening: s.starts_with("widget-weather-tomorrow"),
                ..home.clone()
            },
            "starting" => Demo {
                starting: true,
                phase: Phase::Offline,
                ..home.clone()
            },
            "altro" | "chat" | "timers" | "settings" | "language" | "screen" | "music"
            | "setup" | "ha" | "memories" | "reminders" | "shopping" | "what" => {
                page(DEMO_STATES.into_iter().find(|x| *x == s)?)
            }
            "update" => page("update"),
            "poses" => page("poses"),
            "settings-widgets" => page("widgets"),
            // The settings animation, a frame of each part: the toolbox out, the spanner held,
            // both on their way back (on the home).
            "settings-anim-in" => Demo {
                frame: Some(("settings_in", 30)),
                ..page("settings")
            },
            "settings-anim-hold" => Demo {
                frame: Some(("settings_hold", 32)),
                ..page("settings")
            },
            "settings-anim-out" => Demo {
                frame: Some(("settings_out", 24)),
                ..home.clone()
            },
            // The laptop on the Home Assistant page (held, typing) and the earmuffs going on.
            "ha-anim-hold" => Demo {
                frame: Some(("ha_hold", 20)),
                ..page("ha")
            },
            "ha-anim-type" => Demo {
                frame: Some(("ha_type", 20)),
                ..page("ha")
            },
            "muted-anim-in" => Demo {
                frame: Some(("muted_in", 50)),
                ..home.clone()
            },
            "board" | "board-confirm" | "board-empty" | "board-swipe" => Demo {
                board: match s {
                    "board" => 1,
                    "board-confirm" => 2,
                    "board-swipe" => 6,
                    _ => 3,
                },
                ..page("board")
            },
            // Not golden ones: a conversation over the board (the face's ring, the bar); a
            // finger half way across to the next page; one pulling it in from the home's edge.
            "board-talk" => Demo {
                phase: Phase::Listening,
                board: 5,
                ..page("board")
            },
            "board-opening" => Demo {
                board: 7,
                ..home.clone()
            },
            "widget-custom" | "widget-custom-line" => Demo {
                widget: Some(WidgetKind::Custom),
                widget_cards: if s == "widget-custom-line" { 2 } else { 0 },
                board: 4,
                ..home.clone()
            },
            "trends" => Demo {
                widget: Some(WidgetKind::Quotes),
                ..page("trends")
            },
            "city" => Demo {
                widget: Some(WidgetKind::Cities),
                ..page("city")
            },
            "notes" => Demo {
                widget: Some(WidgetKind::Notes),
                ..page("notes")
            },
            "trends-sensors" => Demo {
                widget: Some(WidgetKind::Sensors),
                ..page("trends")
            },
            "settings-panel" => Demo {
                panel: 1,
                ..page("panel")
            },
            "panel-ask" => Demo {
                panel: 2,
                ..page("settings")
            },
            "panel-blocked" => Demo {
                panel: 3,
                ..home.clone()
            },
            "settings-panel-remote" => Demo {
                panel: 4,
                ..page("panel")
            },
            "remote-sign" => Demo {
                panel: 5,
                ..home.clone()
            },
            "update-confirm" => Demo {
                dialog: "update",
                ..page("update")
            },
            "first-language" => page("welcome"),
            "first-ha-searching" => Demo {
                find: "searching",
                ..page("find-ha")
            },
            "first-ha-found" => Demo {
                find: "found",
                ..page("find-ha")
            },
            "first-ha-none" => Demo {
                find: "none",
                ..page("find-ha")
            },
            "ha-working" => Demo {
                ha: 1,
                ..page("ha")
            },
            "ha-done" => Demo {
                ha: 2,
                ..page("ha")
            },
            "ha-pick" => Demo {
                pick: "ai",
                ..page("ha-pick")
            },
            // Not a golden one: the voices of the voice engine in use.
            "ha-pick-voice" => Demo {
                pick: "voice",
                ..page("ha-pick")
            },
            "usage" => page("usage"),
            "usage-precise" => Demo {
                usage_precise: true,
                ..page("usage")
            },
            "suspend" => Demo {
                dialog: "suspend",
                ..page("settings")
            },
            "new-timer" => Demo {
                dialog: "new-timer",
                ..page("timers")
            },
            "new-alarm" => Demo {
                dialog: "new-alarm",
                ..page("timers")
            },
            _ => Demo {
                phase: Phase::parse(s)?,
                ..home
            },
        };
        Some(d)
    }
}

/// How long the volume HUD stays after the last key.
pub const VOLUME_BAR: Duration = Duration::from_secs(2);
/// How long the card of the microphone's switch stays.
pub const MIC_CARD: Duration = Duration::from_millis(2500);
/// The controls over a mirrored screen that fills the panel stay this long after a touch
/// (as the home button over Home Assistant's view).
pub const MIRROR_CHROME: Duration = Duration::from_secs(4);
/// Mirroring on and no frame for this long: the decoder is stuck, the session is closed.
pub const MIRROR_NO_FRAME: Duration = Duration::from_secs(8);
/// A desk page left alone goes back home after this.
pub const PAGE_IDLE: Duration = Duration::from_secs(60);
/// The camera thread's answer: the photo, small enough for the screen, or why not.
type PhotoRx = std::sync::mpsc::Receiver<Result<SharedPixelBuffer<Rgba8Pixel>, String>>;
/// The gallery's thumbnails, from their thread.
type ThumbsRx = std::sync::mpsc::Receiver<Vec<SharedPixelBuffer<Rgba8Pixel>>>;
/// A shopping item tapped stays ticked this long, and a second tap takes it back.
pub const SHOP_UNDO: Duration = Duration::from_secs(5);
/// The camera page goes home after this long without a touch or a photo.
pub const CAMERA_IDLE: Duration = Duration::from_secs(120);
/// The countdown before a photo (seconds).
pub const COUNTDOWN: u64 = 3;

/// How long the photo just taken stays in the camera's frame before the mirror comes back.
pub const SHOT_SHOWN: Duration = Duration::from_secs(4);
/// The "Annulla" chip after a timer or an alarm is removed.
pub const UNDO: Duration = Duration::from_secs(5);
/// The last long answer stays as a card this long.
pub const ANSWER_CARD: Duration = Duration::from_secs(5 * 60);
/// The next alarm shows at night if it rings within this many minutes.
pub const NIGHT_ALARM_MINUTES: u32 = 12 * 60;
/// The character at night without a pose of its own: its idle pose this bright on the
/// background.
pub const NIGHT_DIM: f32 = 0.18;

/// Backlight, awake period and animations (config `[screen]`).
#[derive(Debug, Clone)]
pub struct ScreenOptions {
    /// `screen.backlight`: "" = none (the level is only logged).
    pub backlight: String,
    pub levels: Levels,
    /// Night mode: the levels never go above these (the bedside clock, and while awake).
    pub night_levels: Levels,
    /// Automatic brightness from the light sensor (light.rs): its curve; None = fixed levels.
    pub auto: Option<Curve>,
    pub awake_seconds: f32,
    pub animations: bool,
    pub loading_seconds: f32,
}

impl Default for ScreenOptions {
    fn default() -> Self {
        ScreenOptions {
            backlight: String::new(),
            levels: Levels {
                idle: 600,
                active: 4095,
            },
            night_levels: Levels {
                idle: 40,
                active: 400,
            },
            auto: None,
            awake_seconds: 16.0,
            animations: true,
            loading_seconds: 30.0,
        }
    }
}

pub struct UiOptions {
    pub locales: PathBuf,
    pub language: String,
    /// Simulator: the sample weather is shown while Home Assistant gives none.
    pub sample_weather: bool,
    pub demo: Option<Demo>,
    pub screen: ScreenOptions,
    /// Where the conversations and the settings are kept (None: nowhere, as in `--state`).
    pub state_dir: Option<PathBuf>,
    /// A Home Assistant token is configured (else the setup page, at the first start).
    pub ha_linked: bool,
    /// Home Assistant's host, for the setup page.
    pub ha_host: Option<String>,
    /// Show the setup page at start when not linked (the device, not the simulator); the
    /// first-start pages too.
    pub setup_at_start: bool,
    /// What the first start still has to ask (the language, Home Assistant's address).
    pub first_start: crate::settings::FirstStart,
    /// The QR-code link of the setup page (None: no Home Assistant address, or `--state`). Its
    /// words are filled in when the page opens.
    pub link: Option<crate::ha::link::LinkSetup>,
    /// Updates (Settings → Info): where from; None: no checking (`--state`, the simulator, tests).
    pub update: Option<crate::update::UpdateCfg>,
    /// The browser panel's socket and port (None: no socket: `--state`, tests).
    pub panel: Option<crate::panel::PanelCfg>,
}

/// A sheet decoded only while it is needed: being decoded, and whether it is wanted (its clips
/// are dropped when they come after it was no longer needed).
#[derive(Default)]
struct Lazy {
    incoming: Option<Receiver<Clip>>,
    on: bool,
}

/// The animation side of the UI.
struct Motion {
    seq: Sequencer,
    clips: HashMap<&'static str, Arc<Clip>>,
    incoming: Option<Receiver<Clip>>,
    /// The sheets of [`LAZY_CLIPS`], in the same order.
    lazy: [Lazy; 5],
    /// The sheet of the pose of Home Assistant's cards, being decoded.
    pose_incoming: Option<Receiver<Clip>>,
    playhead: Option<Playhead>,
    /// The picture a clip fades in over (`Show::Fade`): what was on screen when it started.
    fade_from: Option<Vec<u8>>,
    shown: Show,
    awake: Awake,
    backlight: Backlight,
    /// Start-up: until when the loading animation may play (None = over).
    loading_until: Option<Instant>,
    /// Frames presented and their CPU time, for the stats line.
    presented: u64,
    present_time: Duration,
}

/// The downloaded poses' pictures read so far, by name (None: not downloaded).
type PosePictures = HashMap<String, Option<Image>>;

/// Settings → Poses as the downloads' thread told it last.
#[derive(Debug, Default)]
struct PoseView {
    /// The catalogue's poses of the character (None: not read yet), or why it could not be read.
    catalogue: Option<Result<Vec<crate::poses::Pose>, String>>,
    /// The pose being downloaded.
    busy: Option<String>,
    /// The last failure: (pose, why).
    failed: Option<(String, String)>,
}

/// Something removed by touch, kept for [`UNDO`] before it is really gone.
#[derive(Debug, Clone)]
enum Undo {
    Timer(String),
    Alarm(u32),
    /// A note of the board (the brain's id).
    Note(i64),
}

/// Freed sheets are many small blocks: glibc keeps them for later unless asked (`malloc_trim`).
fn give_back_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only releases free heap memory; no pointer is involved.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// The night's animations: decoded from a little before the night to a little after it, freed
/// by day (their sheet is the largest).
const NIGHT_CLIPS: [&str; 3] = ["fall_asleep", "sleeping", "wake_from_sleep"];
/// The settings pages' animations: decoded while a settings page is open (and until
/// `settings_out` has played), freed after.
const SETTINGS_CLIPS: [&str; 3] = ["settings_in", "settings_hold", "settings_out"];
/// The Home Assistant page's animations (the laptop): the same, with that page.
const HA_CLIPS: [&str; 6] = [
    "ha_in",
    "ha_hold",
    "ha_type_in",
    "ha_type",
    "ha_type_out",
    "ha_out",
];
/// The earmuffs going on and off: the same, while the microphone is off (the `muted` loop is
/// decoded at start like the other loops).
const EARMUFF_CLIPS: [&str; 2] = ["muted_in", "muted_out"];
/// The plug of the link pages: the same, with those pages.
const LINK_CLIPS: [&str; 3] = ["link_in", "link_hold", "link_out"];
/// A job gone well: the same, while it lasts (it comes after a bow: the time to decode it).
const SATISFIED_CLIPS: [&str; 3] = ["satisfied_in", "satisfied_hold", "satisfied_out"];
/// The sheets decoded only while they are needed ([`Sequencer::uses`]). The nod's (4 MB) is
/// kept: it comes at once after a reflex, often.
const LAZY_CLIPS: [&[&str]; 5] = [
    &SETTINGS_CLIPS,
    &HA_CLIPS,
    &EARMUFF_CLIPS,
    &LINK_CLIPS,
    &SATISFIED_CLIPS,
];
/// The settings pages: Settings and the pages under it.
const SETTINGS_PAGES: [&str; 7] = [
    "settings", "language", "screen", "widgets", "panel", "update", "poses",
];
/// One of [`LAZY_CLIPS`].
fn lazy_clip(name: &str) -> bool {
    LAZY_CLIPS.iter().any(|g| g.contains(&name))
}

/// A page where the character has its own animation over idle (the settings pages, More, the
/// Home Assistant page): no card's pose there, and of the moment's only the link and a job
/// (`Ui::home_state`).
fn paged(page: &str) -> bool {
    SETTINGS_PAGES.contains(&page) || matches!(page, "altro" | "ha")
}

/// How long before and after the night hours its animations are kept.
const NIGHT_MARGIN_S: i64 = 10 * 60;

/// A reminder full screen: its id, since when, the time it was for ("15:00").
struct Notice {
    id: i64,
    since: Instant,
    when: String,
    /// The last time it sounded.
    sounded: Instant,
}

/// The screen stays bright this long for it, and again after each reminding sound (then it
/// dims like the home, the words stay until OK or "in 10 min").
pub const NOTICE_BRIGHT: Duration = Duration::from_secs(60);
/// Until OK (or "stop"), the reminder sounds again this often for its first minutes, then
/// every NOTICE_AGAIN_LATER (nobody home: not every 20 s for hours). By day only; the brain
/// also says it once after two minutes through Home Assistant.
pub const NOTICE_AGAIN: Duration = Duration::from_secs(15);
pub const NOTICE_AGAIN_FOR: Duration = Duration::from_secs(5 * 60);
pub const NOTICE_AGAIN_LATER: Duration = Duration::from_secs(3 * 60);
/// "In 10 min".
pub const NOTICE_SNOOZE_MIN: i64 = 10;

/// What the desk shows besides the core's phase.
#[derive(Default)]
struct Desk {
    /// (name, kind) of what rings.
    ringing: Vec<(String, String)>,
    ringing_since: Option<Instant>,
    timers: Vec<TimerView>,
    alarms: Vec<Alarm>,
    undo: Option<Undo>,
    /// The sentence heard in the conversation going on, and the answer.
    heard: String,
    answer: String,
    last_answer: Option<(String, Instant)>,
    /// When a page was last touched (a page left alone goes home).
    touched: Option<Instant>,
    night: (bool, u32, u32),
    /// Night mode is on now (the schedule).
    night_now: bool,
    /// The night layout is on screen (the dimmed pose with it).
    night_shown: bool,
    music: Option<MusicView>,
    /// Reminder cards from home-companion-brain (id, title, text), oldest first; the first is on the home.
    reminders: Vec<(i64, String, String)>,
    /// The due reminder shown full screen (one of `reminders`), until OK or "in 10 min"
    /// (sounding again every NOTICE_AGAIN).
    notice: Option<Notice>,
    /// The "Ricordi" page as last received.
    memories: MemoriesView,
    /// The shopping list's open items (uid, text); None = not configured or not received yet.
    shopping: Option<Vec<ShopList>>,
    /// Calendar events from the brain (`BrainMsg::Upcoming`), soonest first.
    events: Vec<Upcoming>,
    /// Cards sent by Home Assistant (`home_companion_card`).
    ha_cards: cards::HaCards,
    /// home-companion-brain's reminders still to come (due, seconds since the epoch; text), soonest first.
    upcoming: Vec<(i64, String)>,
    /// The core's tiles of the control center (UiCommand::Control).
    control_airplay: Option<Option<String>>,
    mic_on: bool,
    /// The device's switch is on mute.
    mic_switch_off: bool,
    /// The control center's Notte: night now (true) or day now (false) against the schedule,
    /// until the schedule itself comes to the same.
    night_override: Option<bool>,
    /// The control center's brightness by hand, in percent (None: automatic).
    brightness_manual: Option<u8>,
    music_paused: bool,
    /// A phone or a tablet mirrors its screen (UiCommand::Mirror), and the page it opened over.
    mirror: Option<MirrorInfo>,
    mirror_back: String,
    /// The first setup is done: the home's card that opens Home Assistant (closed with its X).
    setup_ready: bool,
}

/// The UI controller (UI thread only).
pub struct Ui<S: Screen> {
    pub screen: S,
    /// AirPlay screen mirroring: the video socket's client while a sender mirrors.
    mirror: RefCell<Option<super::mirror_video::Mirror>>,
    /// The sender's picture's aspect ratio: UxPlay reports it before mirroring starts.
    mirror_aspect: Cell<Option<f32>>,
    character: Rc<Character>,
    opts: UiOptions,
    locale: RefCell<Locale>,
    poses: RefCell<HashMap<State, Image>>,
    /// The character at night (its sleeping pose, or idle dimmed).
    night_pose: RefCell<Option<Image>>,
    /// The memory of the start-up animation given back once (give_back_memory).
    trimmed: Cell<bool>,
    /// The core knows a reminder is full screen (UiEvent::Notice, for the stop word).
    notice_told: Cell<bool>,
    view: Cell<Phase>,
    core: Sender<CoreEvent>,
    motion: RefCell<Motion>,
    /// Runs while the character moves (one tick per frame at the animations' fps).
    frame_timer: Timer,
    /// Hides the volume HUD.
    volume_timer: Timer,
    wifi_timer: Timer,
    mic_timer: Timer,
    /// Hides the controls over a mirrored screen that fills the panel.
    mirror_timer: Timer,
    /// Ends the undo chip.
    undo_timer: Timer,
    /// The backlight's fade, and its 25 Hz steps while it runs.
    fade: Cell<Option<Fade>>,
    fade_timer: Timer,
    /// Automatic brightness (None: the configured levels).
    auto: RefCell<Option<Auto>>,
    lux_seq: Cell<u32>,
    /// The percentage on the HUD.
    volume: Cell<u8>,
    /// The weather from Home Assistant (None = no real data: the box is hidden).
    weather: RefCell<Option<Weather>>,
    desk: RefCell<Desk>,
    history: RefCell<History>,
    settings: Option<Settings>,
    /// Linked to Home Assistant (a token of either kind); starts as `opts.ha_linked`.
    ha_linked: Cell<bool>,
    /// The camera (camera.rs): there is one, a photo is being taken (its result comes here),
    /// and the timer that hides the preview.
    camera: bool,
    photo_rx: RefCell<Option<PhotoRx>>,
    photo_timer: Timer,
    /// The camera page: the live picture, its timer (frames, the countdown, the cover), the
    /// last frame shown, when the countdown started and its last number, and when the page was
    /// last used (it goes home after [`CAMERA_IDLE`]).
    cam_live: RefCell<Option<super::camera::Live>>,
    cam_timer: Timer,
    cam_seq: Cell<u64>,
    cam_count: Cell<Option<(Instant, u64)>>,
    /// A countdown asked (by voice) that starts once the conversation is over and the mirror shows.
    cam_count_wanted: Cell<bool>,
    cam_used: Cell<Instant>,
    /// The gallery: the photos listed (newest first), their thumbnails coming from a thread, the
    /// one shown whole.
    /// Updates (update.rs): the checking thread (None: no checking) and the page's timer.
    updater: Option<crate::update::Checker>,
    update_timer: Timer,
    /// Watches how an install ends, to say it in a dialog until OK; the end shown (its status
    /// line), so it is said once.
    update_watch: Timer,
    /// Shopping items tapped and waiting [`SHOP_UNDO`] before they are bought, and their timer.
    shop_ticks: RefCell<HashMap<String, Instant>>,
    shop_timer: Timer,
    update_result_shown: RefCell<String>,
    gallery: RefCell<Vec<PathBuf>>,
    thumbs_rx: RefCell<Option<ThumbsRx>>,
    viewing: RefCell<Option<PathBuf>>,
    /// The back of a page was just pressed: the next page change makes the home or tap sound.
    back_pending: Cell<Option<Instant>>,
    /// The link in progress while the setup page is shown.
    link: RefCell<Option<crate::ha::link::Link>>,
    /// Just linked: when "linked" gives way to "opening Home Assistant…".
    link_done: Cell<Option<Instant>>,
    /// The Wi-Fi network's name, read when the setup page opens.
    wifi_name: RefCell<Option<String>>,
    /// home-companion-brain's control socket (None in demos).
    brain: Option<BrainLink>,
    /// Ticks of the shopping lists and the choice of lists, to Home Assistant's thread
    /// (crate::ha::hub).
    hub_tx: RefCell<Option<Sender<HubCmd>>>,
    /// Home Assistant → Lists and calendars.
    choice: RefCell<Choice>,
    /// The kinds of the cards on the home, in order (what an OK on card `i` means).
    card_kinds: RefCell<Vec<&'static str>>,
    /// The setup of Home Assistant (crate::ha::setup) and what its page shows.
    ha_setup: RefCell<Option<crate::ha::setup::Setup>>,
    ha_page: RefCell<HaPage>,
    /// The AI's use: the core's estimate, the integration's precise numbers (when it counts
    /// them) and when those were last asked for (crate::ha::usage).
    usage_estimate: RefCell<Option<crate::usage::View>>,
    usage_precise: RefCell<Option<crate::usage::Precise>>,
    usage_asked: Cell<Option<Instant>>,
    /// The last "keep bright" while the QR code page listens.
    ha_keep: Cell<Option<Instant>>,
    /// Settings → Screen → "Home compatta" (the clock higher, four cards).
    home_compact: Cell<bool>,
    /// The home's widget (widgets.rs): its turn, the pages that are on (none: no widget, the
    /// home as before), the interval of the turn (None: by hand only).
    widget: RefCell<Band>,
    widget_on: RefCell<Vec<WidgetPage>>,
    /// Every page in the turn's order (the ones off keep their place).
    widget_order: RefCell<Vec<WidgetPage>>,
    /// The names given to pages from the browser (none: "Markets", "Markets 2").
    widget_names: RefCell<Vec<(WidgetPage, String)>>,
    /// The page whose own page a tap on the widget opened (the markets' or the cities' of
    /// that page).
    widget_opened: Cell<Option<WidgetPage>>,
    widget_every: Cell<Option<Duration>>,
    /// The data of the pages chosen from the browser and of Home Assistant's (feeds/, ha/).
    widget_data: RefCell<widgets::Data>,
    /// Home Assistant's side of the widget: what it is asked (settings), the entities it may
    /// show (the catalogue, for the browser panel's searches), the data it sent.
    widget_ha: RefCell<crate::ha::widgets::Want>,
    ha_catalog: RefCell<Vec<crate::ha::widgets::Entity>>,
    ha_widgets: RefCell<crate::ha::widgets::WidgetsData>,
    /// What the hub was last told of the night and the screen (its curves wait), and when the
    /// browser panel last had the catalogue asked again.
    ha_quiet: Cell<Option<bool>>,
    pub(super) catalog_asked: Cell<Option<Instant>>,
    /// The local hour the night's schedule reads instead of the clock (the tests, so that their
    /// outcome never depends on when they run; None: the clock).
    hour_for_tests: Cell<Option<u32>>,
    /// The sensors' last 24 hours, by entity.
    ha_curves: RefCell<HashMap<String, crate::ha::widgets::Curve>>,
    /// The board's notes (the brain's `notes` event), oldest first.
    board: RefCell<Vec<brain_link::Note>>,
    /// The markets chosen from the browser (settings.toml), in their order, and the coins'
    /// currency.
    widget_quotes: RefCell<Lists<crate::feeds::QuoteChoice>>,
    widget_currency: RefCell<String>,
    /// The other cities chosen from the browser, and their weather as last fetched.
    widget_cities: RefCell<Lists<crate::feeds::CityChoice>>,
    /// Home Assistant's sensors chosen from the browser, page by page (`widget_ha.sensors` is
    /// all of them, for the hub).
    widget_sensors: RefCell<Lists<String>>,
    /// The home's weather is in °F (its last known unit): the other cities follow.
    fahrenheit: Cell<bool>,
    city_weather: RefCell<Vec<crate::feeds::openmeteo::CityWeather>>,
    /// The feeds' thread (feeds/) and the revision of its data last shown.
    feeds: RefCell<crate::feeds::Feeds>,
    feeds_rev: Cell<u64>,
    /// The first start's welcome page is still to be answered.
    language_pending: Cell<bool>,
    /// The first start's search for Home Assistant on the network, while it runs.
    find: RefCell<Option<Receiver<crate::ha::discover::Outcome>>>,
    /// Home Assistant picked: the restart after "saving" has been on screen a moment.
    restart_timer: Timer,
    /// v0.2: the "done" and Yes/No cards of the brain's house commands.
    house: RefCell<HouseCards>,
    /// The Yes/No question open, as last told to the core (UiEvent::Question).
    asking: Cell<Option<i64>>,
    /// Its Yes button blinks (a spoken yes in its confirming second), as last drawn.
    /// The countdown's length last put on screen (it is set again when it moved enough).
    ask_left: Cell<f32>,
    /// Do not disturb (UiCommand::DoNotDisturb).
    dnd: Cell<Dnd>,
    /// The browser panel (panel_ui.rs): who is let in, and what the screen shows of it. Apart:
    /// the sessions are borrowed while a request is answered, the rest is drawn meanwhile.
    panel_sessions: RefCell<crate::panel::sessions::Sessions>,
    panel: RefCell<panel_ui::PanelUi>,
    /// The remote screen (remote_ui.rs): the switch, the sign, the capture.
    remote: RefCell<remote_ui::RemoteUi>,
    /// Settings applied but not written yet, and the timer that writes them (settings_model).
    settings_pending: RefCell<Vec<Change>>,
    settings_timer: Timer,
    /// The home's pose (a card's, or the moment's besides idle: `link`, `working`, `satisfied`,
    /// `nod`, `muted`): the one drawn last, a reflex's nod waiting for the home, and until when
    /// the nod and the satisfied pose last (the satisfied one from when).
    home_pose: RefCell<Look>,
    /// The microphone is off, as the core said last (UiCommand::Control): not before it says so.
    mic_off: Cell<bool>,
    nod_pending: Cell<bool>,
    nod_until: Cell<Option<Instant>>,
    satisfied: Cell<Option<(Instant, Instant)>>,
    /// The pose a card of Home Assistant asks for (`card_look_of`; None: none, or not one the
    /// character has), and the downloaded poses' pictures read so far.
    card_pose: RefCell<(Option<Look>, PosePictures)>,
    /// An announcement plays (UiCommand::Announcing): a pose stays on while it is said.
    announcing: Cell<bool>,
    /// Settings → Poses: the downloads' thread (src/poses.rs), and what it said last.
    pose_downloads: Option<(
        Sender<crate::poses::Cmd>,
        RefCell<Receiver<crate::poses::Msg>>,
    )>,
    pose_view: RefCell<PoseView>,
    /// The board (ui/board.rs): its page, its custom widgets, its calls and question.
    tile_board: RefCell<board_ui::BoardUi>,
    /// Puts a tile back when Home Assistant did not answer (runs only while one waits).
    board_timer: Timer,
    /// A level dragged faster than it may be sent: the last value, sent when it may.
    level_timer: Timer,
    this: RefCell<Weak<Self>>,
}

/// Settings changed within this are written together.
const SETTINGS_WRITE_AFTER: Duration = Duration::from_secs(1);

impl<S: Screen> Ui<S> {
    /// The settings changed since the last write, written now.
    pub fn flush_settings(&self) {
        let pending = std::mem::take(&mut *self.settings_pending.borrow_mut());
        if let Some(s) = &self.settings {
            for c in &pending {
                model::save(s, c);
            }
        }
        self.settings_timer.stop();
    }

    /// A value still waiting to be written, for the keys the screen does not hold.
    fn pending_value(&self, key: &str) -> Option<serde_json::Value> {
        self.settings_pending
            .borrow()
            .iter()
            .rev()
            .find_map(|c| match (key, c) {
                ("update_prerelease", Change::UpdatePrerelease(on)) => Some(json!(on)),
                ("lists", Change::Lists(l)) => Some(json!(l)),
                ("calendars", Change::Calendars(l)) => Some(json!(l)),
                _ => None,
            })
    }
}

impl<S: Screen> model::Host for Ui<S> {
    fn settings(&self) -> Option<&Settings> {
        self.settings.as_ref()
    }

    /// What the screen holds now: every change is applied at once and written a second later.
    fn live(&self, key: &str) -> Option<serde_json::Value> {
        let (on, start, end) = self.desk.borrow().night;
        let hub = self.hub_tx.borrow().is_some();
        Some(match key {
            "volume" => json!(self.volume.get()),
            "language" => json!(self.locale.borrow().code),
            "widgets" => json!(
                self.widget_on
                    .borrow()
                    .iter()
                    .map(|k| k.key())
                    .collect::<Vec<_>>()
            ),
            "widgets_every" => json!(self.widget_every_secs().to_string()),
            "widgets_currency" => json!(self.widget_currency.borrow().clone()),
            "check_openings" => json!(self.widget_ha.borrow().openings),
            "check_batteries" => json!(self.widget_ha.borrow().batteries),
            "widget_order" => json!(
                self.widget_order
                    .borrow()
                    .iter()
                    .map(|k| k.key())
                    .collect::<Vec<_>>()
            ),
            "night_on" => json!(on),
            "night_start" => json!(start),
            "night_end" => json!(end),
            "home_compact" => json!(self.home_compact.get()),
            "wake_sensitivity" => {
                json!(WakeSensitivity::from_step(self.screen.get_wake_step()).key())
            }
            "keep_conversations" => json!(self.history.borrow().keeps()),
            "conversation_mode" => json!(self.screen.get_conversation_mode()),
            "conversation_max" => json!(self.screen.get_conversation_max()),
            "lists" if hub => json!(self.choice.borrow().lists),
            "calendars" if hub => json!(self.choice.borrow().calendars),
            "panel" => json!(self.panel.borrow().on),
            // Not held by the screen: the file (or a change still to write, below).
            _ => return self.pending_value(key),
        })
    }

    fn save(&self, change: &Change) {
        if self.settings.is_none() {
            return;
        }
        {
            let mut p = self.settings_pending.borrow_mut();
            // The latest of each kind (of each page for a page's list): Night and Widgets
            // carry their whole section; a page added or removed is never dropped.
            p.retain(|c| !c.replaced_by(change));
            p.push(change.clone());
        }
        if let Some(me) = self.this.borrow().upgrade() {
            let w = Rc::downgrade(&me);
            self.settings_timer
                .start(TimerMode::SingleShot, SETTINGS_WRITE_AFTER, move || {
                    if let Some(ui) = w.upgrade() {
                        ui.flush_settings();
                    }
                });
        } else {
            self.flush_settings();
        }
    }

    fn dynamic_options(&self, key: &str) -> Vec<(String, String)> {
        match key {
            "language" => self
                .character
                .languages()
                .map(|code| {
                    let name = locale::load(&self.opts.locales, code)
                        .map_or_else(|_| code.to_uppercase(), |l| l.name);
                    (code.to_string(), name)
                })
                .collect(),
            "lists" => self.choice.borrow().known.todo.clone(),
            "calendars" => self.choice.borrow().known.calendar.clone(),
            _ => Vec::new(),
        }
    }

    fn apply(&self, change: Change) {
        self.apply_setting(change);
        // A change from the panel shows on the open page; the screen's own as well.
        self.refresh_settings();
    }
}

/// Settings → Home Assistant, as the setup thread last told it.
#[derive(Default)]
struct HaPage {
    rows: Vec<crate::ha::setup::Row>,
    status: String,
    busy: bool,
    /// The page opened right after the QR link: "All set" opens Home Assistant's view.
    after_link: bool,
    /// "All set" is on screen, waiting for its OK (then, after the link, the view opens).
    done: bool,
    /// The last run asked by the user (to say "All set" only after one).
    asked: bool,
    /// Something needs the user (checked by itself): a line on Settings.
    attention: bool,
    /// What may be chosen (the AI, speech-to-text, voice) and the choice open on screen.
    choices: crate::ha::setup::Choices,
    picking: String,
    /// What the setup is doing right now, for the pop-up while it works ("Passo a ChatGPT…",
    /// then each row as it is done).
    step: String,
    /// `--state ha`: set up already (no state file).
    demo_configured: bool,
}

/// The nightly tidy-up's line on the "Ricordi" page.
fn tidy_line(l: &Locale, tidy: &serde_json::Value) -> String {
    match tidy.get("at").and_then(serde_json::Value::as_i64) {
        Some(at) if tidy["skipped"].as_str().unwrap_or("").is_empty() => {
            let n = |k: &str| tidy[k].as_u64().unwrap_or(0).to_string();
            l.text("tidy_line")
                .replace("{t}", &local_hm(at).0)
                .replace("{merged}", &n("merged"))
                .replace("{updated}", &n("updated"))
                .replace("{retired}", &n("invalidated"))
        }
        _ => l.text("tidy_none").to_string(),
    }
}

/// "5 nov" (the locale's month, three letters).
/// The calendar events of today and tomorrow as rows of the reminders page: ("today 15:00 ·
/// Family", what); all-day ones say "all day".
/// The sample calendar of the demos: a dentist this afternoon, a holiday and a dinner tomorrow.
fn demo_events(l: &Locale) -> Vec<Upcoming> {
    let midnight = DEMO_NOW as i64 - DEMO_NOW as i64 % 86400;
    let ev = |start, end, all_day, title: &str| Upcoming {
        start,
        end,
        all_day,
        title: title.into(),
        calendar: "calendar.demo0".into(),
    };
    vec![
        ev(
            DEMO_NOW as i64 + 2100,
            DEMO_NOW as i64 + 3900,
            false,
            l.sample("event_dentist"),
        ),
        ev(
            midnight + 86400,
            midnight + 2 * 86400,
            true,
            l.sample("event_holiday"),
        ),
    ]
}

fn event_rows(events: &[Upcoming], l: &Locale, now: i64) -> Vec<(String, String)> {
    let day_of = |t: i64| local_hm(t).1;
    next::today_tomorrow(events, now, &day_of)
        .into_iter()
        .map(|(offset, e)| {
            let day = l.text(if offset == 0 {
                "calendar_today"
            } else {
                "calendar_tomorrow"
            });
            let at = if e.all_day {
                l.text("calendar_all_day").to_string()
            } else {
                local_hm(e.start).0
            };
            (format!("{day} {at}"), e.title.clone())
        })
        .collect()
}

fn short_date(l: &Locale, ts: i64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r writes into `tm` only.
    if ts <= 0 || unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return String::new();
    }
    let month: String = l
        .months
        .get(tm.tm_mon as usize)
        .map(|m| m.chars().take(3).collect())
        .unwrap_or_default();
    // "13 nov", "13. Nov." (the locale's `short_date`).
    l.text("short_date")
        .replace("{d}", &tm.tm_mday.to_string())
        .replace("{m}", &month)
}

/// "14:02" of a time in seconds since the epoch, and the day it is on (days since the epoch, local).
/// The markets of the demos: public names, made-up prices, a day's curve each.
fn demo_quotes() -> Vec<widgets::Trend> {
    let at = DEMO_NOW as i64 - 300;
    let q = |name: &str, unit: &str, decimals: u8, prev: f64, value: f64, wave: f64| {
        let points: Vec<f64> = (0..32)
            .map(|i| {
                let t = i as f64 / 31.0;
                prev + (value - prev) * t + (t * wave).sin() * (value - prev).abs() * 0.6
            })
            .collect();
        let low = points.iter().copied().fold(f64::INFINITY, f64::min);
        let high = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        widgets::Trend {
            key: format!("yahoo:{name}"),
            name: name.into(),
            value,
            decimals,
            unit: unit.into(),
            prev: Some(prev),
            pct: true,
            points: widgets::downsample(&points, widgets::CURVE_POINTS),
            low,
            high,
            at,
            fresh: 1800,
            max_age: 48 * 3600,
        }
    };
    vec![
        q("Apple Inc.", "USD", 2, 186.95, 189.20, 7.0),
        q("S&P 500", "", 2, 5640.10, 5612.42, 5.0),
        q("Bitcoin", "USD", 0, 63120.0, 64250.0, 9.0),
        q("EUR/USD", "", 4, 1.0835, 1.0812, 4.0),
        q("Gold", "USD", 2, 2391.5, 2402.8, 6.0),
    ]
}

/// A city's weather as Home Assistant's (`ha::weather::Weather`), so the weather page's code
/// draws it; and its date today there ("2026-11-03").
fn city_as_weather(w: &crate::feeds::openmeteo::CityWeather, now: i64) -> (Weather, String) {
    use crate::ha::weather::{ForecastDay, ForecastHour};
    let date = |t: i64| {
        let days = (t + w.utc_offset as i64).div_euclid(86400);
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}")
    };
    let mut days = w.days.iter().map(|d| ForecastDay {
        // Noon of that day, so a time zone's midnight never falls on the day before.
        date: date(d.at + 12 * 3600 - w.utc_offset as i64),
        condition: d.cond.clone(),
        high: Some(d.high as f64),
        low: Some(d.low as f64),
        ..Default::default()
    });
    let today = date(now);
    let first = days.next();
    let (today_fc, mut next): (Option<ForecastDay>, Vec<ForecastDay>) = match first {
        Some(f) if f.date == today => (Some(f), Vec::new()),
        Some(f) => (None, vec![f]),
        None => (None, Vec::new()),
    };
    next.extend(days);
    (
        Weather {
            condition: w.cond.clone(),
            temperature: w.temp as f64,
            unit: w.unit.clone(),
            today: today_fc,
            next_days: next,
            hours: w
                .hours
                .iter()
                .map(|h| ForecastHour {
                    at: h.at,
                    condition: h.cond.clone(),
                    temperature: Some(h.temp as f64),
                })
                .collect(),
            ..Default::default()
        },
        today,
    )
}

/// The civil date of a day number since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Home Assistant's side of the demos: invented sensors with their day, an open window and a
/// low battery, a song, two people.
fn demo_ha(
    l: &Locale,
) -> (
    crate::ha::widgets::WidgetsData,
    HashMap<String, crate::ha::widgets::Curve>,
) {
    use crate::ha::widgets::{Curve, SensorNow, WidgetsData};
    let names: Vec<String> = l
        .sample("ha_sensors")
        .split('|')
        .map(String::from)
        .collect();
    let name = |i: usize| names.get(i).cloned().unwrap_or_default();
    let sensor = |i: usize, id: &str, value: f64, decimals: u8, unit: &str| SensorNow {
        id: id.into(),
        name: name(i),
        value,
        decimals,
        unit: unit.into(),
    };
    let curve = |from: f64, to: f64, wave: f64| {
        let pts: Vec<f64> = (0..48)
            .map(|i| {
                let t = i as f64 / 47.0;
                from + (to - from) * t + (t * wave).sin() * (to - from).abs().max(0.5) * 0.8
            })
            .collect();
        Curve {
            points: widgets::downsample(&pts, widgets::CURVE_POINTS),
            first: from,
            low: pts.iter().copied().fold(f64::INFINITY, f64::min),
            high: pts.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        }
    };
    let data = WidgetsData {
        sensors: vec![
            sensor(0, "sensor.outdoor", 12.5, 1, "°C"),
            sensor(1, "sensor.living", 21.3, 1, "°C"),
            sensor(2, "sensor.humidity", 48.0, 0, "%"),
            sensor(3, "sensor.power", 412.0, 0, "W"),
        ],
        check: vec![
            widgets::CheckRow {
                name: name(4),
                battery: None,
            },
            widgets::CheckRow {
                name: name(5),
                battery: Some(14),
            },
        ],
        playing: Some(widgets::Playing {
            title: "Clair de lune".into(),
            artist: "Claude Debussy".into(),
            player: name(6),
        }),
        people: vec![
            widgets::Person {
                name: "Ada".into(),
                place: widgets::Place::Home,
            },
            widgets::Person {
                name: "Bob".into(),
                place: widgets::Place::Zone(name(7)),
            },
        ],
    };
    let curves = [
        ("sensor.outdoor", curve(8.0, 12.5, 4.0)),
        ("sensor.living", curve(20.1, 21.3, 6.0)),
        ("sensor.humidity", curve(55.0, 48.0, 3.0)),
        ("sensor.power", curve(380.0, 412.0, 9.0)),
    ]
    .into_iter()
    .map(|(k, c)| (k.to_string(), c))
    .collect();
    (data, curves)
}

/// The other cities of the demos: public places, made-up weather.
fn demo_cities() -> (
    Vec<crate::feeds::CityChoice>,
    Vec<crate::feeds::openmeteo::CityWeather>,
) {
    use crate::feeds::openmeteo::{CityWeather, Day, Hour};
    let at = DEMO_NOW as i64 - 600;
    let midnight = DEMO_NOW as i64 - DEMO_NOW as i64 % 86400;
    let city = |id: u64, name: &str, region: &str, country: &str, lat: f64, lon: f64| {
        crate::feeds::CityChoice {
            id,
            name: name.into(),
            region: region.into(),
            country: country.into(),
            lat,
            lon,
        }
    };
    let weather = |id: u64, temp: f32, cond: &str, offset: i32, conds: [&str; 5]| CityWeather {
        id,
        temp,
        cond: cond.into(),
        hours: (0..8)
            .map(|h| Hour {
                at: midnight + (14 + h) * 3600,
                temp: temp + ((h as f32) * 0.7).sin() * 2.0 - h as f32 * 0.3,
                cond: conds[h as usize % 5].into(),
            })
            .collect(),
        days: (0..5)
            .map(|d| Day {
                at: midnight + d * 86400,
                high: temp + 3.0 - d as f32,
                low: temp - 5.0 + (d as f32 * 0.5),
                cond: conds[d as usize].into(),
            })
            .collect(),
        utc_offset: offset,
        unit: "°C".into(),
        at,
    };
    (
        vec![
            city(2950159, "Berlin", "Berlin", "DE", 52.52, 13.41),
            city(1850147, "Tokyo", "Tokyo", "JP", 35.69, 139.69),
            city(4250542, "Springfield", "Illinois", "US", 39.8, -89.64),
        ],
        vec![
            weather(
                2950159,
                9.0,
                "rainy",
                3600,
                ["rainy", "cloudy", "rainy", "partlycloudy", "cloudy"],
            ),
            weather(
                1850147,
                12.0,
                "clear-night",
                9 * 3600,
                ["clear-night", "sunny", "sunny", "partlycloudy", "rainy"],
            ),
            weather(
                4250542,
                17.0,
                "partlycloudy",
                -6 * 3600,
                ["partlycloudy", "sunny", "cloudy", "pouring", "sunny"],
            ),
        ],
    )
}

/// The words of the widget's pages.
fn widget_words(l: &Locale) -> widgets::Words<'_> {
    let ch = |k: &str, d: char| l.text(k).chars().next().unwrap_or(d);
    widgets::Words {
        next: l.text("widget_next"),
        weather: l.text("widget_weather"),
        shopping: l.text("widget_shopping"),
        quotes: l.text("widget_quotes"),
        cities: l.text("widget_cities"),
        notes: l.text("widget_notes"),
        sensors: l.text("widget_sensors"),
        check: l.text("widget_check"),
        playing: l.text("widget_playing"),
        home: l.text("widget_home"),
        today: l.text("calendar_today"),
        tomorrow: l.text("calendar_tomorrow"),
        to_buy: l.text("tile_shopping"),
        data_from: l.text("widget_data_from"),
        window: l.text("widget_window"),
        open: l.text("widget_open"),
        check_line: l.text("widget_check_line"),
        at_home: l.text("widget_at_home"),
        away: l.text("widget_away"),
        decimal: ch("number_decimal", '.'),
        group: ch("number_group", ','),
    }
}

pub(crate) fn local_hm(t: i64) -> (String, i64) {
    // SAFETY: localtime_r only writes into the struct we pass.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = t as libc::time_t;
        libc::localtime_r(&tt, &mut tm);
        tm
    };
    let day = (t + tm.tm_gmtoff) / 86400;
    (format!("{:02}:{:02}", tm.tm_hour, tm.tm_min), day)
}

/// The local offset from UTC at `t` (seconds), for the hours of the clock.
fn local_offset(t: i64) -> i64 {
    // SAFETY: localtime_r only writes into the struct we pass.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let tt = t as libc::time_t;
        libc::localtime_r(&tt, &mut tm);
        tm.tm_gmtoff
    }
}

/// A house event of the brain, gathered while its messages are read (BrainMsg).
enum HouseEvent {
    Done(i64, String, bool),
    /// A question: id, text, the devices' names, expires_at.
    Ask(i64, String, Vec<String>, i64),
    /// A spoken yes received: id, until_ms.
    Pending(i64, i64),
    /// The question is over: id, answered (yes/no) or not, via, the devices that failed.
    AskOver(i64, Option<bool>, String, Vec<String>),
    /// A new subscription: the question on screen is forgotten (the brain replays it).
    Resubscribed,
    View(brain_link::View),
    Photo,
}

/// Whether `hour` is in the night hours `start..end` (they may cross midnight).
pub fn is_night(hour: u32, on: bool, start: u32, end: u32) -> bool {
    on && if start <= end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

/// A pose drawn on `bg` at `alpha` (night): the same picture, darker, still opaque.
fn dim(image: &Image, bg: Rgb, alpha: f32) -> Image {
    let Some(buf) = image.to_rgba8() else {
        return image.clone();
    };
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(buf.width(), buf.height());
    let mix = |c: u8, b: u8| (b as f32 + (c as f32 - b as f32) * alpha).round() as u8;
    for (o, p) in out.make_mut_slice().iter_mut().zip(buf.as_slice()) {
        *o = Rgba8Pixel {
            r: mix(p.r, bg.0),
            g: mix(p.g, bg.1),
            b: mix(p.b, bg.2),
            a: p.a,
        };
    }
    Image::from_rgba8(out)
}

impl<S: Screen> Ui<S> {
    /// Sets the character, the language, the callbacks and the first view (offline until
    /// Home Assistant connects).
    pub fn new(
        screen: S,
        character: Rc<Character>,
        opts: UiOptions,
        core: Sender<CoreEvent>,
    ) -> Result<Rc<Self>, String> {
        let locale = set_language(&screen, &character, &opts.locales, &opts.language)?;
        set_mouth(&screen, &character);
        screen.set_notifications_hidden(false);
        let now = Instant::now();
        let demo = opts.demo.is_some();
        let animated = opts.screen.animations && !demo;
        // The loading animation first, then what the home needs, then the states.
        let mut wanted: Vec<Animation> = Vec::new();
        if animated {
            // The night's sheet (~19 MB decoded) only around the night: Ui::night_sheets; the
            // settings', the laptop's and the earmuffs' only while needed: Ui::lazy_sheets.
            for name in character::ANIMATIONS
                .iter()
                .filter(|n| !NIGHT_CLIPS.contains(n) && !lazy_clip(n))
            {
                if let Some(a) = character.animation(name) {
                    wanted.push(a.clone());
                }
            }
        }
        let loading = animated && character.animation("loading").is_some();
        let motion = Motion {
            seq: Sequencer::new(now),
            clips: HashMap::new(),
            incoming: (!wanted.is_empty())
                .then(|| anim::load_in_background(wanted, character.background)),
            lazy: Default::default(),
            pose_incoming: None,
            playhead: None,
            fade_from: None,
            shown: Show::Pose,
            awake: Awake::new(opts.screen.awake_seconds),
            backlight: Backlight::open(if demo { "" } else { &opts.screen.backlight }),
            loading_until: loading
                .then(|| now + Duration::from_secs_f32(opts.screen.loading_seconds)),
            presented: 0,
            present_time: Duration::ZERO,
        };
        let settings = opts.state_dir.as_deref().map(Settings::in_dir);
        let history = match (&opts.state_dir, &settings) {
            (Some(dir), Some(s)) => History::load(
                &dir.join("conversations.json"),
                s.keep_conversations(),
                now_unix() as i64,
            ),
            _ => History::in_memory(),
        };
        let night = settings.as_ref().map_or((true, 23, 7), Settings::night);
        let compact = match &opts.demo {
            Some(d) => d.compact,
            None => settings.as_ref().is_some_and(Settings::home_compact),
        };
        // The widget's pages and interval (Settings → Screen → Widgets); the demos: all, 20 s.
        let (widget_on, widget_every) = match (&opts.demo, &settings) {
            (None, Some(s)) => s.widgets(),
            _ => (
                WidgetKind::ALL
                    .into_iter()
                    .filter(|k| k.default_on())
                    .map(WidgetPage::from)
                    .collect(),
                widgets::DEFAULT_EVERY.as_secs() as u32,
            ),
        };
        let widget_order = match (&opts.demo, &settings) {
            (None, Some(s)) => s.widget_order(),
            _ => WidgetPage::all(),
        };
        let (widget_names, widget_sensors) = match (&opts.demo, &settings) {
            (None, Some(s)) => (s.widget_names(), s.sensors()),
            _ => (Vec::new(), Lists::default()),
        };
        let widget_ha = settings
            .as_ref()
            .filter(|_| !demo)
            .map(Settings::widget_ha)
            .unwrap_or_default();
        let (widget_quotes, widget_currency, widget_cities) = match (&opts.demo, &settings) {
            (None, Some(s)) => (s.quotes(), s.widget_currency(), s.cities()),
            _ => (Lists::default(), "usd".to_string(), Lists::default()),
        };
        // The demos never ask the internet (no cache, nothing chosen).
        let feeds = crate::feeds::Feeds::new(if demo { None } else { opts.state_dir.clone() });
        let opts_auto = opts.screen.auto.filter(|_| !demo).map(Auto::new);
        let opts_linked = opts.ha_linked;
        let opts_state_dir = opts.state_dir.clone();
        let opts_locales = opts.locales.clone();
        let opts_panel = opts.panel.clone();
        let panel_on = settings.as_ref().is_some_and(Settings::panel_on);
        let remote_on = settings.as_ref().is_some_and(Settings::panel_screen);
        let opts_first = opts.first_start;
        let has_camera = !demo && super::camera::present();
        // The switch on the Updates page wins over the configuration's `prerelease`.
        let opts_update = opts.update.clone().map(|mut c| {
            if let Some(p) = settings.as_ref().and_then(|s| s.update_prerelease()) {
                c.prerelease = p;
            }
            c
        });
        // Settings → Poses: from the releases of the updates' repository, into the state.
        let pose_downloads = opts_update
            .as_ref()
            .filter(|c| !demo && crate::update::valid_repo(&c.repo))
            .zip(opts.state_dir.as_ref())
            .map(|(c, state)| {
                let (tx, rx) = crate::poses::spawn(
                    c.repo.clone(),
                    character.id.clone(),
                    crate::poses::dir(state, &character.id),
                );
                (tx, RefCell::new(rx))
            });
        let ui = Rc::new(Ui {
            screen,
            mirror: RefCell::new(None),
            mirror_aspect: Cell::new(None),
            character,
            opts,
            locale: RefCell::new(locale),
            poses: RefCell::default(),
            night_pose: RefCell::new(None),
            trimmed: Cell::new(false),
            notice_told: Cell::new(false),
            view: Cell::new(Phase::Offline),
            core,
            motion: RefCell::new(motion),
            frame_timer: Timer::default(),
            volume_timer: Timer::default(),
            wifi_timer: Timer::default(),
            mic_timer: Timer::default(),
            mirror_timer: Timer::default(),
            undo_timer: Timer::default(),
            fade: Cell::new(None),
            fade_timer: Timer::default(),
            auto: RefCell::new(opts_auto),
            lux_seq: Cell::new(0),
            volume: Cell::new(0),
            weather: RefCell::new(None),
            desk: RefCell::new(Desk {
                night,
                ..Default::default()
            }),
            history: RefCell::new(history),
            settings,
            ha_linked: Cell::new(opts_linked),
            camera: has_camera,
            photo_rx: RefCell::new(None),
            photo_timer: Timer::default(),
            cam_live: RefCell::new(None),
            cam_timer: Timer::default(),
            cam_seq: Cell::new(0),
            cam_count: Cell::new(None),
            cam_count_wanted: Cell::new(false),
            cam_used: Cell::new(Instant::now()),
            updater: opts_update
                .as_ref()
                .map(|c| crate::update::Checker::spawn(c, env!("CARGO_PKG_VERSION"))),
            update_timer: Timer::default(),
            update_watch: Timer::default(),
            shop_ticks: RefCell::new(HashMap::new()),
            shop_timer: Timer::default(),
            update_result_shown: RefCell::new(String::new()),
            gallery: RefCell::new(Vec::new()),
            thumbs_rx: RefCell::new(None),
            viewing: RefCell::new(None),
            back_pending: Cell::new(None),
            link: RefCell::new(None),
            link_done: Cell::new(None),
            wifi_name: RefCell::new(None),
            brain: (!demo).then(BrainLink::start),
            hub_tx: RefCell::new(None),
            choice: RefCell::new(Choice::default()),
            card_kinds: RefCell::new(Vec::new()),
            ha_setup: RefCell::new(None),
            ha_page: RefCell::new(HaPage::default()),
            usage_estimate: RefCell::new(None),
            usage_precise: RefCell::new(None),
            usage_asked: Cell::new(None),
            ha_keep: Cell::new(None),
            home_compact: Cell::new(compact),
            widget: RefCell::new(Band::new(Instant::now())),
            widget_on: RefCell::new(widget_on),
            widget_order: RefCell::new(widget_order),
            widget_names: RefCell::new(widget_names),
            widget_opened: Cell::new(None),
            widget_sensors: RefCell::new(widget_sensors),
            widget_data: RefCell::new(widgets::Data::default()),
            board: RefCell::new(Vec::new()),
            widget_ha: RefCell::new(widget_ha),
            ha_catalog: RefCell::new(Vec::new()),
            ha_widgets: RefCell::new(Default::default()),
            ha_curves: RefCell::new(HashMap::new()),
            ha_quiet: Cell::new(None),
            catalog_asked: Cell::new(None),
            // The lib's tests live at noon from the start, whatever the clock says (the night's
            // schedule changes what shows and what may be asked between 23:00 and 07:00).
            hour_for_tests: Cell::new(if cfg!(test) { Some(12) } else { None }),
            widget_quotes: RefCell::new(widget_quotes),
            widget_currency: RefCell::new(widget_currency),
            widget_cities: RefCell::new(widget_cities),
            fahrenheit: Cell::new(false),
            city_weather: RefCell::new(Vec::new()),
            feeds: RefCell::new(feeds),
            feeds_rev: Cell::new(0),
            widget_every: Cell::new(
                (widget_every > 0).then(|| Duration::from_secs(widget_every as u64)),
            ),
            language_pending: Cell::new(opts_first.language),
            find: RefCell::new(None),
            restart_timer: Timer::default(),
            house: RefCell::new(HouseCards::default()),
            asking: Cell::new(None),
            ask_left: Cell::new(0.0),
            dnd: Cell::new(Dnd::default()),
            panel_sessions: panel_ui::load_sessions(opts_state_dir.as_deref()),
            panel: RefCell::new(panel_ui::PanelUi::new(opts_panel, panel_on)),
            remote: RefCell::new(remote_ui::RemoteUi {
                on: remote_on,
                ..Default::default()
            }),
            settings_pending: RefCell::new(Vec::new()),
            settings_timer: Timer::default(),
            home_pose: RefCell::new(Look::Own(State::Idle)),
            mic_off: Cell::new(false),
            nod_pending: Cell::new(false),
            nod_until: Cell::new(None),
            satisfied: Cell::new(None),
            card_pose: RefCell::new((None, HashMap::new())),
            announcing: Cell::new(false),
            pose_downloads,
            pose_view: RefCell::new(PoseView::default()),
            tile_board: RefCell::new(board_ui::BoardUi {
                icons: super::board::icons::Icons::new(super::board::icons::file_beside(
                    &opts_locales,
                )),
                ..Default::default()
            }),
            board_timer: Timer::default(),
            level_timer: Timer::default(),
            this: RefCell::new(Weak::new()),
        });
        *ui.this.borrow_mut() = Rc::downgrade(&ui);
        ui.configure_feeds();
        // How an install ends: a dialog with OK over any page, once (Andrea, 2026-11-16).
        if ui.updater.is_some() && ui.opts.state_dir.is_some() {
            let w = Rc::downgrade(&ui);
            ui.update_watch
                .start(TimerMode::Repeated, Duration::from_secs(3), move || {
                    if let Some(ui) = w.upgrade() {
                        ui.update_result_check();
                    }
                });
            let w = Rc::downgrade(&ui);
            ui.screen.on_update_result_seen(move || {
                if let Some(ui) = w.upgrade() {
                    ui.update_result_seen();
                }
            });
        }
        // Back after an update's restart: its page again, to see the end (Andrea, 2026-11-15).
        if let Some(dir) = ui.opts.state_dir.clone().filter(|_| ui.updater.is_some()) {
            let w = Rc::downgrade(&ui);
            Timer::single_shot(Duration::from_secs(2), move || {
                let going = crate::update::status(&dir).is_some_and(|s| {
                    matches!(s.phase.as_str(), "installing" | "waiting")
                        && now_unix() as i64 - s.at < 600
                });
                if going && let Some(ui) = w.upgrade() {
                    log::info!(event = "update"; "update: back after the restart, its page");
                    ui.screen.set_page("update");
                    ui.on_page("update");
                }
            });
        }
        ui.set_welcome();
        ui.wire();
        ui.wire_panel();
        ui.wire_remote();
        ui.wire_board();
        ui.load_board();
        ui.refresh_remote();
        ui.start_wifi();
        ui.screen.set_error_pose(ui.pose(State::Error));
        ui.screen.set_dnd_prop(load_prop(
            &ui.character.dir.join("poses/dnd.png"),
            ui.character.background,
        ));
        ui.screen.set_music_prop(load_prop(
            &ui.character.dir.join("poses/music.png"),
            ui.character.background,
        ));
        match ui.opts.demo.clone() {
            Some(d) => ui.show_demo(d),
            None => {
                ui.screen.set_heard("");
                ui.screen.set_answer("");
                ui.screen.set_wave(&[0.0; 72]);
                ui.screen.set_music(None);
                ui.render_weather();
                ui.refresh_desk();
                ui.show(Phase::Offline);
                ui.tick_clock();
                if let Some(page) = ui.first_page() {
                    if matches!(page, "welcome" | "find-ha") {
                        // Nothing to wait for: the start-up animation would hide the page.
                        ui.end_loading();
                    }
                    log::info!(event = "first_start", page = page; "first start: page {page}");
                    ui.screen.set_page(page);
                    ui.on_page(page);
                }
            }
        }
        Ok(ui)
    }

    /// The tap sound for a control the UI drives itself (the home button over Home Assistant's
    /// view, whose touches never reach a Slint control).
    pub fn tap_sound(&self) {
        self.send(UiEvent::Tap);
    }

    /// Back home from Home Assistant's view, by the user (button, swipe, Vol+ Vol−).
    pub fn home_sound(&self) {
        self.send(UiEvent::Home);
    }

    /// A screenshot of the screen as it is (SIGUSR2): the core names the file.
    pub fn request_screenshot(&self) {
        self.send(UiEvent::Screenshot);
    }

    fn send(&self, e: UiEvent) {
        // Do not disturb: the cards' sounds stay quiet (the screen's side of it).
        if matches!(e, UiEvent::Reminder) && self.dnd_on() {
            log::debug!("do not disturb: no card sound");
            return;
        }
        let _ = self.core.send(CoreEvent::Ui(e));
    }

    fn dnd_on(&self) -> bool {
        self.dnd.get().active(now_unix() as u64)
    }

    /// The home's cards: three, four on the compact home.
    fn max_cards(&self) -> usize {
        if self.home_compact.get() {
            cards::MAX_CARDS_COMPACT
        } else {
            cards::MAX_CARDS
        }
    }

    fn wire(self: &Rc<Self>) {
        self.screen.set_home_compact(self.home_compact.get());
        let sensitivity = self
            .settings
            .as_ref()
            .map(Settings::wake_sensitivity)
            .unwrap_or_default();
        self.screen.set_wake_step(sensitivity.step());
        self.screen.set_conversation_mode(
            self.settings
                .as_ref()
                .is_some_and(Settings::conversation_mode),
        );
        self.screen.set_conversation_max(
            self.settings
                .as_ref()
                .map_or(CONVERSATION_MAX_DEFAULT, Settings::conversation_max),
        );
        let ui = Rc::downgrade(self);
        self.screen.on_state_changed(move |app, s| {
            let Some(ui) = ui.upgrade() else { return };
            let (screen, pose) = view(ui.view.get());
            // The core's view, or a debug key (1-5 in the simulator).
            let state = if s == screen {
                pose
            } else {
                character_state(&s)
            };
            ui.set_pose(app, state);
        });
        let ui = Rc::downgrade(self);
        self.screen.on_page_changed(move |page| {
            if let Some(ui) = ui.upgrade() {
                ui.desk.borrow_mut().touched = Some(Instant::now());
                ui.on_page(&page);
            }
        });
        let tx = self.core.clone();
        self.screen.on_talk_clicked(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::Talk));
        });
        let ui = Rc::downgrade(self);
        self.screen.on_character_touched(move || {
            if let Some(ui) = ui.upgrade() {
                ui.wake(WakeSource::Touch);
            }
        });
        let tx = self.core.clone();
        self.screen.on_stop_clicked(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::Stop));
        });
        let tx = self.core.clone();
        self.screen.on_false_wake_clicked(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::FalseWake));
        });
        let tx = self.core.clone();
        let ui = Rc::downgrade(self);
        self.screen.on_message_ok(move |i| {
            // A card from Home Assistant: its own OK.
            if let Some(ui) = ui.upgrade()
                && ui.ha_card_ok(i)
            {
                return;
            }
            // The message card on the home is the first reminder while there is one.
            if let Some(ui) = ui.upgrade()
                && ui.ack_reminder()
            {
                return;
            }
            let _ = tx.send(CoreEvent::Ui(UiEvent::MessageOk(i)));
        });
        let ui = Rc::downgrade(self);
        self.screen.on_memory_delete(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.brain_do(memory_answer(id, false));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_memory_confirm(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.brain_do(memory_answer(id, true));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_memories_forget(move || {
            if let Some(ui) = ui.upgrade() {
                ui.brain_do(json!({"cmd": "memory.forget_all", "confirm": true}));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_choice_toggled(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.choice_toggled(&id);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_shop_done(move |uid| {
            if let Some(ui) = ui.upgrade() {
                ui.shop_done(uid);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_job_cancel(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.desk.borrow_mut().reminders.retain(|r| r.0 != id);
                ui.brain_do(json!({"cmd": "jobs.cancel", "id": id}));
                ui.refresh_home();
            }
        });
        let (ui, tx) = (Rc::downgrade(self), self.core.clone());
        self.screen.on_home_assistant_clicked(move || {
            if ui
                .upgrade()
                .is_some_and(|ui| ui.link_instead_of_home_assistant())
            {
                return;
            }
            let _ = tx.send(CoreEvent::Ui(UiEvent::HomeAssistant));
        });
        let (ui, tx) = (Rc::downgrade(self), self.core.clone());
        self.screen.on_volume_key(move |dir| {
            if let Some(ui) = ui.upgrade() {
                ui.wake(WakeSource::Key(if dir > 0 {
                    super::sensors::KEY_VOLUMEUP
                } else {
                    super::sensors::KEY_VOLUMEDOWN
                }));
            }
            let r = crate::volume::VolumeRequest::Step { dir, repeat: false };
            let _ = tx.send(CoreEvent::Volume(r));
        });
        let (ui, tx) = (Rc::downgrade(self), self.core.clone());
        self.screen.on_control_tile(move |key| {
            let Some(ui) = ui.upgrade() else { return };
            ui.wake(WakeSource::Touch);
            match key.as_str() {
                "airplay" | "mic" => {
                    let _ = tx.send(CoreEvent::Ui(UiEvent::ControlTile(key)));
                }
                "night" => ui.toggle_night(),
                "dnd" => ui.toggle_dnd(),
                "camera" => ui.open_camera(false),
                "ha" => {
                    ui.screen.set_control_open(false);
                    ui.screen
                        .set_page(if ui.ha_linked.get() { "ha" } else { "setup" });
                    ui.on_page(&ui.screen.get_page());
                }
                _ => {}
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_photo_delete(move || {
            if let Some(ui) = ui.upgrade() {
                ui.photo_delete();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_pose_toggled(move |name, on| {
            if let Some(ui) = ui.upgrade() {
                ui.pose_toggled(&name, on);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_poses_check(move || {
            if let Some(ui) = ui.upgrade() {
                ui.poses_refresh();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_update_check(move || {
            if let Some(ui) = ui.upgrade() {
                if let Some(u) = &ui.updater {
                    u.check_now();
                }
                ui.refresh_update();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_update_prerelease(move |on| {
            if let Some(ui) = ui.upgrade() {
                model::commit(&*ui, Change::UpdatePrerelease(on));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_update_install(move || {
            if let Some(ui) = ui.upgrade() {
                ui.update_install();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_camera_shoot(move || {
            if let Some(ui) = ui.upgrade() {
                ui.camera_countdown();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_photo_open(move |i| {
            if let Some(ui) = ui.upgrade() {
                ui.photo_open(i);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_done_undo(move || {
            if let Some(ui) = ui.upgrade() {
                ui.undo_done();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_ask_cancel(move || {
            if let Some(ui) = ui.upgrade() {
                let now_ms = (ui.now() * 1000.0) as i64;
                let c = ui.house.borrow_mut().cancel(now_ms);
                match c {
                    house::Cancel::No(id) => {
                        log::info!(event = "house_answer", id = id, yes = false; "house: cancel = no to {id}");
                        ui.send(UiEvent::QuestionAnswer { id, yes: false });
                    }
                    house::Cancel::Later => {
                        log::info!(event = "house_cancel"; "house: cancel, undone when done");
                    }
                    house::Cancel::Undo => ui.undo_done(),
                    house::Cancel::Nothing => {}
                }
                ui.render_house();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_done_ok(move || {
            if let Some(ui) = ui.upgrade() {
                if ui.house.borrow_mut().close_popup() {
                    ui.render_house();
                }
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_ask_answered(move |yes| {
            if let Some(ui) = ui.upgrade() {
                // A tile's question (no brain's open): the body's own answer.
                if ui.house.borrow().ask.is_none() {
                    ui.board_answer(yes, "screen");
                    return;
                }
                // The core sends it to the brain; the card stays until the brain closes it.
                let id = ui.house.borrow_mut().answer(ui.now(), yes);
                if let Some(id) = id {
                    log::info!(event = "house_answer", id = id, yes = yes; "house: {} to {id}", if yes { "yes" } else { "no" });
                    ui.send(UiEvent::QuestionAnswer { id, yes });
                    ui.render_house();
                }
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_control_opened(move || {
            if let Some(ui) = ui.upgrade() {
                ui.open_control();
            }
        });
        let tx = self.core.clone();
        self.screen.on_control_volume(move |v| {
            let r = crate::volume::VolumeRequest::Set(v.clamp(0, 100) as u8);
            let _ = tx.send(CoreEvent::Volume(r));
        });
        let ui = Rc::downgrade(self);
        self.screen.on_control_brightness(move |v| {
            if let Some(ui) = ui.upgrade() {
                ui.desk.borrow_mut().brightness_manual = Some(v.clamp(5, 100) as u8);
                ui.refresh_control();
                ui.step();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_control_brightness_auto(move || {
            if let Some(ui) = ui.upgrade() {
                let now = ui.brightness_percent();
                {
                    let mut d = ui.desk.borrow_mut();
                    d.brightness_manual = if d.brightness_manual.is_some() {
                        None
                    } else {
                        Some(now)
                    };
                }
                ui.refresh_control();
                ui.step();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_mirror_poked(move || {
            if let Some(ui) = ui.upgrade() {
                ui.poke_mirror();
            }
        });
        let tx = self.core.clone();
        self.screen.on_music_toggle(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::MusicToggle));
        });
        let tx = self.core.clone();
        self.screen.on_music_next(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::MusicNext));
        });
        let ui = Rc::downgrade(self);
        self.screen.on_ready_dismissed(move || {
            if let Some(ui) = ui.upgrade() {
                ui.desk.borrow_mut().setup_ready = false;
                ui.refresh_home();
            }
        });
        let tx = self.core.clone();
        self.screen.on_mirror_stop(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::MirrorStop));
        });
        let tx = self.core.clone();
        self.screen.on_music_previous(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::MusicPrevious));
        });
        let tx = self.core.clone();
        self.screen.on_suspend_confirmed(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::Suspend));
        });
        let tx = self.core.clone();
        self.screen.on_tap(move || {
            let _ = tx.send(CoreEvent::Ui(UiEvent::Tap));
        });
        let ui = Rc::downgrade(self);
        self.screen.on_back_pressed(move || {
            if let Some(ui) = ui.upgrade() {
                ui.back_pending.set(Some(Instant::now()));
            }
        });
        let tx = self.core.clone();
        self.screen.on_screenshot_requested(move |_| {
            let _ = tx.send(CoreEvent::Ui(UiEvent::Screenshot));
        });
        let ui = Rc::downgrade(self);
        self.screen.on_language_chosen(move |_app, code| {
            if let Some(ui) = ui.upgrade() {
                model::commit(&*ui, Change::Language(code.to_string()));
            }
        });
        // Timers and alarms by touch.
        let ui = Rc::downgrade(self);
        self.screen.on_timer_start(move |minutes| {
            if let Some(ui) = ui.upgrade() {
                ui.send(UiEvent::TimerStart(minutes as u64 * 60, None));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_timer_pause(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.send(UiEvent::TimerPause(id));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_timer_cancel(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.remove_later(Undo::Timer(id));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_alarm_delete(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.remove_later(Undo::Alarm(id));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_undo_clicked(move || {
            if let Some(ui) = ui.upgrade() {
                ui.undo_timer.stop();
                ui.desk.borrow_mut().undo = None;
                ui.refresh_desk();
                ui.refresh_notes();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_note_delete(move |id| {
            if let Some(ui) = ui.upgrade() {
                log::info!(event = "notes", id = id; "note {id} removed on the screen");
                ui.remove_later(Undo::Note(id));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_alarm_save(move |h, m, days| {
            if let Some(ui) = ui.upgrade() {
                ui.send(UiEvent::AlarmSave(h, m, days));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_alarm_toggled(move |id, on| {
            if let Some(ui) = ui.upgrade() {
                ui.send(UiEvent::AlarmToggle(id, on));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_snooze(move || {
            if let Some(ui) = ui.upgrade() {
                ui.send(UiEvent::Snooze);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_notice_ok(move || {
            if let Some(ui) = ui.upgrade() {
                ui.notice_done(false);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_notice_snooze(move || {
            if let Some(ui) = ui.upgrade() {
                ui.notice_done(true);
            }
        });
        // Conversations.
        let ui = Rc::downgrade(self);
        self.screen.on_chat_new(move || {
            if let Some(ui) = ui.upgrade() {
                ui.history.borrow_mut().start_over();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_chat_clear(move || {
            if let Some(ui) = ui.upgrade() {
                ui.history.borrow_mut().clear();
                ui.desk.borrow_mut().last_answer = None;
                log::info!(event = "conversations_cleared"; "conversations cleared");
                ui.refresh_desk();
            }
        });
        // Settings → Home Assistant: the setup thread does the work, the page shows its rows.
        let ui = Rc::downgrade(self);
        self.screen.on_ha_check(move || {
            if let Some(ui) = ui.upgrade() {
                ui.ha_send(crate::ha::setup::SetupCmd::Repair, true);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_ha_repair(move |id| {
            if let Some(ui) = ui.upgrade() {
                ui.ha_send(crate::ha::setup::SetupCmd::RepairRow(id), true);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_ha_pick(move |kind| {
            if let Some(ui) = ui.upgrade() {
                ui.ha_page.borrow_mut().picking = kind;
                ui.render_ha();
                ui.screen.set_page("ha-pick");
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_ha_picked(move |id| {
            if let Some(ui) = ui.upgrade() {
                let kind = std::mem::take(&mut ui.ha_page.borrow_mut().picking);
                let same = ui.ha_page.borrow().choices.current_of(&kind) == id;
                if !kind.is_empty() && !same {
                    log::info!(event = "ha_setup"; "{kind} chosen on the screen: {id}");
                    {
                        let mut p = ui.ha_page.borrow_mut();
                        let name = p
                            .choices
                            .of(&kind)
                            .iter()
                            .find(|o| o.id == id)
                            .map(|o| o.name.clone())
                            .unwrap_or_default();
                        p.step = word(&ui.locale.borrow(), "ha_step_switch", &ui.character.name)
                            .replace("{what}", &name);
                    }
                    ui.ha_send(crate::ha::setup::SetupCmd::Choose(kind, id), true);
                }
            }
        });
        let ui = Rc::downgrade(self);
        let ui2 = Rc::downgrade(self);
        self.screen.on_ha_dismiss(move || {
            if let Some(ui) = ui2.upgrade() {
                ui.ha_page.borrow_mut().status.clear();
                ui.render_ha();
            }
        });
        self.screen.on_ha_ok(move || {
            if let Some(ui) = ui.upgrade() {
                let after_link = {
                    let mut p = ui.ha_page.borrow_mut();
                    p.done = false;
                    p.status.clear();
                    std::mem::take(&mut p.after_link)
                };
                ui.render_ha();
                // Right after the QR link: back to the home, where a card says it is done and
                // opens Home Assistant's view.
                if after_link {
                    ui.desk.borrow_mut().setup_ready = true;
                    ui.screen.set_page("home");
                    ui.refresh_home();
                }
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_ha_unlink(move || {
            if let Some(ui) = ui.upgrade() {
                log::info!(event = "ha_setup"; "unlink asked on the screen");
                ui.ha_send(crate::ha::setup::SetupCmd::Unlink, true);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_save_chat_toggled(move |keep| {
            if let Some(ui) = ui.upgrade() {
                model::commit(&*ui, Change::KeepConversations(keep));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_wake_step_changed(move |step| {
            if let Some(ui) = ui.upgrade() {
                model::commit(
                    &*ui,
                    Change::WakeSensitivity(WakeSensitivity::from_step(step)),
                );
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_conversation_mode_changed(move |on| {
            if let Some(ui) = ui.upgrade() {
                model::commit(&*ui, Change::ConversationMode(on));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_conversation_max_changed(move |times| {
            if let Some(ui) = ui.upgrade() {
                // The stepper stops at its ends.
                let times = times.clamp(CONVERSATION_MAX_MIN as i32, CONVERSATION_MAX_MAX as i32);
                model::commit(&*ui, Change::ConversationMax(times as u32));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_widget_tapped(move || {
            if let Some(ui) = ui.upgrade()
                && let Some(page) = ui.widget.borrow_mut().tap(Instant::now())
            {
                // Before the screen opens its page (the markets', the cities'): that page's.
                ui.widget_opened.set(Some(page));
                let key = page.key();
                log::info!(event = "widget", page = key.as_str(), why = "tap"; "widget: {key} opened");
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_widget_toggled(move |key, on| {
            if let Some(ui) = ui.upgrade() {
                ui.widget_toggled(&key, on);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_widget_every_stepped(move |d| {
            if let Some(ui) = ui.upgrade() {
                ui.widget_every_stepped(d);
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_city_chosen(move |_| {
            if let Some(ui) = ui.upgrade() {
                ui.refresh_city();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_widget_swiped(move |dir| {
            if let Some(ui) = ui.upgrade() {
                let shown = ui.widget.borrow_mut().swipe(dir, Instant::now());
                if let Some(s) = shown {
                    ui.show_widget(&s);
                }
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_home_compact_changed(move |on| {
            if let Some(ui) = ui.upgrade() {
                model::commit(&*ui, Change::HomeCompact(on));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_night_changed(move |on, start, end| {
            if let Some(ui) = ui.upgrade() {
                model::commit(&*ui, Change::Night(on, start, end));
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_setup_later(move || {
            if let Some(ui) = ui.upgrade()
                && let Some(s) = &ui.settings
            {
                s.save_setup_later();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_find_search(move || {
            if let Some(ui) = ui.upgrade() {
                ui.start_find();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_find_pick(move |url| {
            if let Some(ui) = ui.upgrade() {
                ui.pick_home_assistant(&url);
            }
        });
    }

    fn set_pose(&self, app: &S, state: State) {
        if state == State::Idle && self.desk.borrow().night_shown {
            let night = self.night_pose();
            app.set_pose(night);
            return;
        }
        // The home's pose (a card's, or the moment's), and a card's while an announcement is
        // said over it (the probe's temperature over Alfred at the barbecue).
        let look = if state == State::Idle {
            Some(self.home_look())
        } else if state == State::Speaking && self.announcing.get() {
            self.card_look()
        } else {
            None
        };
        match look {
            // A pose with its own clips in and out (the earmuffs, the plug, satisfied) is drawn
            // by them (they start from idle, their loop holds it): under them, and before
            // them, the idle pose.
            Some(Look::Own(s)) if self.moves(s).is_some() => app.set_pose(self.pose(State::Idle)),
            Some(l) => app.set_pose(self.look_image(&l)),
            None => app.set_pose(self.pose(state)),
        }
    }

    /// The mode of a pose that comes and goes with its own clips: muted (the earmuffs, then the
    /// `muted` loop), link (the plug) and satisfied, when animations are on and the character
    /// has all three clips of it. None: the still pose (with its loop, if any) as before.
    fn moves(&self, s: State) -> Option<Mode> {
        let (mode, clips) = match s {
            State::Muted => (Mode::Muted, ["muted_in", "muted", "muted_out"]),
            State::Link => (Mode::Link, LINK_CLIPS),
            State::Satisfied => (Mode::Satisfied, SATISFIED_CLIPS),
            _ => return None,
        };
        (self.animated() && clips.iter().all(|n| self.character.animation(n).is_some()))
            .then_some(mode)
    }

    /// The character moves (not `--state`, animations on).
    fn animated(&self) -> bool {
        self.opts.screen.animations && self.opts.demo.is_none()
    }

    /// The nod is a clip played once on the home (else the still pose for [`NOD_FOR`]).
    fn nod_clip(&self) -> bool {
        self.animated() && self.character.animation("nod").is_some()
    }

    /// The pose a card of Home Assistant asks for, if the character has it.
    fn card_look(&self) -> Option<Look> {
        self.card_pose.borrow().0.clone()
    }

    /// The home's pose now: a card's (not on the pages with their own animation), else the
    /// moment's.
    fn home_look(&self) -> Look {
        let page = self.screen.get_page();
        home_look_of(self.card_look(), paged(&page), self.home_state())
    }

    /// The picture of a pose: the character's own, or a downloaded one (drawn as idle is).
    fn look_image(&self, l: &Look) -> Image {
        match l {
            Look::Own(s) => self.pose(*s),
            Look::Downloaded(n) => {
                let picture = self.card_pose.borrow().1.get(n).cloned().flatten();
                picture.unwrap_or_else(|| self.pose(State::Idle))
            }
        }
    }

    /// How a pose moves on the home: a downloaded one with its own animation if it has one
    /// (`anim::CARD_POSE`), the character's own as `home_mode`.
    fn look_mode(&self, l: &Look) -> Mode {
        match l {
            Look::Own(s) => self.home_mode(*s),
            Look::Downloaded(_) => Mode::Loop(anim::CARD_POSE),
        }
    }

    /// The home's pose now: idle, or what the moment asks if the character has its pose (else
    /// idle, as without it).
    fn home_state(&self) -> State {
        let now = Instant::now();
        let page = self.screen.get_page();
        let s = home_state_of(HomeMoment {
            mic_off: self.mic_off.get(),
            linking: matches!(page.as_str(), "setup" | "find-ha"),
            working: self.update_going() || self.ha_page.borrow().busy,
            satisfied: self
                .satisfied
                .get()
                .is_some_and(|(from, until)| from <= now && now < until),
            nod: self.nod_until.get().is_some_and(|t| now < t),
        });
        // The pages with their own animation over idle: only a job or the link shows over them;
        // on the Home Assistant page its job is the laptop's typing (`motion_mode`).
        let over = match s {
            State::Link => true,
            State::Working => page.as_str() != "ha",
            _ => false,
        };
        if self.character.has_pose(s) && (!paged(&page) || over) {
            s
        } else {
            State::Idle
        }
    }

    /// How the home moves with its pose: `wake` and `idle` over idle, a pose with its own loop
    /// as in a conversation (the muted pose, the deck chair of `offline`), the others still (an
    /// idle clip would draw idle over them). The night's and the settings' clips are not
    /// decoded by day: their poses stay still here.
    fn home_mode(&self, s: State) -> Mode {
        if s == State::Idle {
            return Mode::Home;
        }
        if let Some(mode) = self.moves(s) {
            return mode;
        }
        self.character
            .state_animation(s)
            .and_then(|a| anim::static_name(&a.name))
            .filter(|n| !NIGHT_CLIPS.contains(n) && !lazy_clip(n))
            .map_or(Mode::Still, Mode::Loop)
    }

    /// The home's pose again when the moment or a card changed it (once a second, and when
    /// something that decides it changes).
    fn refresh_home_pose(&self) {
        if self.home_look() != *self.home_pose.borrow() {
            self.show_home_pose();
        }
    }

    /// The home's pose drawn again, with its motion (when it changed, or its picture did).
    fn show_home_pose(&self) {
        let l = self.home_look();
        *self.home_pose.borrow_mut() = l.clone();
        if self.view.get() != Phase::Home
            || self.motion.borrow().loading_until.is_some()
            || self.desk.borrow().night_shown
            || self.opts.demo.is_some()
        {
            return;
        }
        log::info!(event = "home_pose", pose = l.name(); "home pose: {}", l.name());
        self.set_pose(&self.screen, State::Idle);
        let mode = self.motion_mode(Phase::Home, State::Idle, false);
        self.motion.borrow_mut().seq.set_mode(mode, Instant::now());
        self.step();
    }

    /// A reflex was done: the nod, now on the home or when the home comes back.
    fn nod(&self) {
        if self.nod_clip() {
            // The sequencer plays it on the home (now, after the clip under way, or when the
            // home comes back).
            self.motion.borrow_mut().seq.once("nod", Instant::now());
            self.step();
            return;
        }
        if self.view.get() == Phase::Home {
            self.nod_until.set(Some(Instant::now() + NOD_FOR));
            self.refresh_home_pose();
        } else {
            self.nod_pending.set(true);
        }
    }

    fn pose(&self, state: State) -> Image {
        let mut cache = self.poses.borrow_mut();
        cache
            .entry(state)
            .or_insert_with(|| {
                let (path, crop) = self.character.pose(state);
                load_pose(path, crop, self.character.background).unwrap_or_else(|e| {
                    log::warn!("pose {}: {e}", state.name());
                    Image::default()
                })
            })
            .clone()
    }

    /// The character asleep: its `sleeping` pose, or idle dimmed (made once).
    fn night_pose(&self) -> Image {
        if let Some(i) = self.night_pose.borrow().as_ref() {
            return i.clone();
        }
        let image = if self.character.has_pose(State::Sleeping) {
            self.pose(State::Sleeping)
        } else {
            dim(
                &self.pose(State::Idle),
                self.character.background,
                NIGHT_DIM,
            )
        };
        *self.night_pose.borrow_mut() = Some(image.clone());
        image
    }

    /// Shows a phase: screen state, pose, idle phrase, animation, backlight.
    pub fn show(&self, p: Phase) {
        let prev = self.view.replace(p);
        let now = Instant::now();
        {
            let mut m = self.motion.borrow_mut();
            if m.loading_until.is_some() && p != Phase::Offline {
                // Home Assistant is there: the start-up is over.
                m.loading_until = None;
                Self::drop_loading_clip(&mut m);
                log::info!("start-up animation over: {}", p.name());
                if p == Phase::Home {
                    m.awake.wake(now);
                }
            }
            if p == Phase::Home && prev != Phase::Home && prev != Phase::Offline {
                // The end of a conversation, an announcement or a ringing timer.
                m.awake.wake(now);
            }
            if !matches!(p, Phase::Home | Phase::Offline) {
                // A conversation or a ringing timer needs the screen.
                m.awake.end_sleep();
            }
        }
        if !p.in_conversation() {
            self.end_exchange();
        }
        if p == Phase::Home && self.nod_pending.take() {
            self.nod_until.set(Some(now + NOD_FOR));
        }
        {
            let mut d = self.desk.borrow_mut();
            if p == Phase::Ringing {
                d.ringing_since.get_or_insert(now);
            } else {
                d.ringing_since = None;
            }
        }
        let loading = self.motion.borrow().loading_until.is_some();
        self.screen.set_starting(loading);
        self.screen.set_offline(p == Phase::Offline && !loading);
        let (screen, pose) = view(p);
        let pose = if loading { State::Idle } else { pose };
        let lang = self.locale.borrow().code.clone();
        let phrases = self.character.phrases(&lang);
        self.screen.set_phrase_idle(if p == Phase::Offline {
            &phrases.offline
        } else {
            &phrases.idle
        });
        self.screen.set_state(screen);
        self.update_night(now);
        self.set_pose(&self.screen, pose);
        self.refresh_ringing();
        self.refresh_home();
        let mode = self.motion_mode(p, pose, loading);
        self.motion.borrow_mut().seq.set_mode(mode, now);
        self.step();
    }

    /// An animation decoded here and now (the demos' fixed frames).
    fn demo_clip(&self, name: &'static str) {
        let Some(a) = self.character.animation(name).cloned() else {
            return;
        };
        let t = Instant::now();
        match character::Sheet::load(&a.file, a.size) {
            Ok(mut sheet) => {
                sheet.flatten(self.character.background, PLATE_TOLERANCE);
                log::info!(
                    "animation sheet {}: {} frames, {:.1} MB of changes, decoded in {} ms",
                    a.file.display(),
                    sheet.frames.len(),
                    sheet.bytes() as f64 / 1048576.0,
                    t.elapsed().as_millis()
                );
                let mut m = self.motion.borrow_mut();
                m.seq.add(name, Timing::of(&a));
                m.clips
                    .insert(name, Arc::new(Clip::new(a, Arc::new(sheet))));
            }
            Err(e) => log::warn!("animation {name}: {e}"),
        }
    }

    /// What the character plays in phase `p` (its pose `pose`) with the page on screen.
    fn motion_mode(&self, p: Phase, pose: State, loading: bool) -> Mode {
        let night = self.desk.borrow().night_shown;
        if let Some(d) = &self.opts.demo {
            return d
                .frame
                .map_or(Mode::Still, |(clip, index)| Mode::Frame(clip, index));
        }
        if loading {
            return Mode::Loading;
        }
        match p {
            Phase::Home if night => Mode::Night,
            Phase::Home => {
                // The home's pose (a card's, or the moment's: idle on the settings and More
                // pages unless a job or the link asks), else the page's own animation.
                let l = self.home_look();
                *self.home_pose.borrow_mut() = l.clone();
                let page = self.screen.get_page();
                match l {
                    Look::Own(State::Idle) if SETTINGS_PAGES.contains(&page.as_str()) => {
                        Mode::Settings
                    }
                    Look::Own(State::Idle) if page.as_str() == "altro" => Mode::More,
                    Look::Own(State::Idle) if page.as_str() == "ha" => {
                        if self.ha_page.borrow().busy {
                            Mode::HaBusy
                        } else {
                            Mode::Ha
                        }
                    }
                    l => self.look_mode(&l),
                }
            }
            // A card's pose stays while an announcement is said over it.
            Phase::Speaking if self.announcing.get() && self.card_look().is_some() => {
                self.card_look().map_or(Mode::Still, |l| self.look_mode(&l))
            }
            // Its own loop if the character has one (Alfred in the deck chair).
            Phase::Offline => self
                .character
                .animation("offline")
                .and_then(|a| anim::static_name(&a.name))
                .map_or(Mode::Still, Mode::Loop),
            _ => self
                .character
                .state_animation(pose)
                .and_then(|a| anim::static_name(&a.name))
                .map_or(Mode::Still, Mode::Loop),
        }
    }

    /// A page opened: the settings pages have their own animation (and sheet).
    fn page_motion(&self) {
        let p = self.view.get();
        let loading = self.motion.borrow().loading_until.is_some();
        let mode = self.motion_mode(p, view(p).1, loading);
        self.motion.borrow_mut().seq.set_mode(mode, Instant::now());
        self.step();
    }

    /// Decodes the sheets of [`LAZY_CLIPS`] when the sequencer needs them (a settings page,
    /// the Home Assistant page, the microphone off), frees them once their last clip has played.
    fn lazy_sheets(&self) {
        if self.opts.demo.is_some() || !self.opts.screen.animations {
            return;
        }
        let mut freed = false;
        let mut m = self.motion.borrow_mut();
        for (i, clips) in LAZY_CLIPS.into_iter().enumerate() {
            let want = m.seq.uses(clips);
            if want && !m.lazy[i].on {
                let anims: Vec<Animation> = clips
                    .iter()
                    .filter_map(|n| self.character.animation(n).cloned())
                    .collect();
                if !anims.is_empty() {
                    log::info!(event = "lazy_sheets", clip = clips[0], on = true; "animations {}…: decoding", clips[0]);
                    m.lazy[i].on = true;
                    m.lazy[i].incoming =
                        Some(anim::load_in_background(anims, self.character.background));
                }
            } else if !want && m.lazy[i].on {
                m.lazy[i] = Lazy::default();
                for n in clips {
                    m.clips.remove(n);
                    m.seq.remove(n);
                }
                freed = true;
                log::info!(event = "lazy_sheets", clip = clips[0], on = false; "animations {}…: freed", clips[0]);
            }
        }
        drop(m);
        if freed {
            give_back_memory();
        }
    }

    /// Seconds since the epoch; fixed in demo mode (the golden screenshots).
    fn now(&self) -> f64 {
        if let Some(d) = &self.opts.demo {
            if d.evening {
                DEMO_NOW + DEMO_EVENING
            } else {
                DEMO_NOW
            }
        } else {
            now_unix()
        }
    }

    /// The phase on screen.
    pub fn phase(&self) -> Phase {
        self.view.get()
    }

    pub fn character_name(&self) -> String {
        self.character.name.clone()
    }

    /// The answer being spoken does not fit the conversation bar over Home Assistant's view.
    pub fn long_answer(&self) -> bool {
        self.desk.borrow().answer.chars().count() > BAR_ANSWER
    }

    /// The character's face: a square around the mouth of the idle pose (the swipe chip, the
    /// conversation bar).
    pub fn face(&self) -> Image {
        let c = &self.character;
        let (path, _) = c.pose(State::Idle);
        let m = c.mouth;
        let side = (m.width() * 2).max(64);
        let (w, h) = c.pose_size;
        let cx = (m.x0 + m.x1) / 2;
        let cy = m.y0;
        let x0 = cx.saturating_sub(side / 2).min(w.saturating_sub(side));
        let y0 = cy.saturating_sub(side / 2).min(h.saturating_sub(side));
        let crop = Crop {
            x0,
            y0,
            x1: (x0 + side).min(w),
            y1: (y0 + side).min(h),
        };
        load_pose(path, crop, c.background).unwrap_or_default()
    }

    /// The character's portrait for a round badge (the board's header): head and shoulders of
    /// the idle pose, from a little above the head down to the chest, so the circle cuts the
    /// shoulders as in a photo and not the neck.
    pub fn portrait(&self) -> Image {
        let c = &self.character;
        let (path, _) = c.pose(State::Idle);
        let m = c.mouth;
        let side = (m.width() * 3).max(96);
        let (w, h) = c.pose_size;
        let cx = (m.x0 + m.x1) / 2;
        // The mouth sits a little above the middle of the circle.
        let y_top = m.y0.saturating_sub(side * 2 / 5);
        let x0 = cx.saturating_sub(side / 2).min(w.saturating_sub(side));
        let y0 = y_top.min(h.saturating_sub(side));
        let crop = Crop {
            x0,
            y0,
            x1: (x0 + side).min(w),
            y1: (y0 + side).min(h),
        };
        load_pose(path, crop, c.background).unwrap_or_default()
    }

    /// Something woke the screen up (touch, key, movement, SIGUSR1).
    pub fn wake(&self, why: WakeSource) {
        if self.opts.demo.is_some() {
            return;
        }
        self.panel.borrow_mut().keys_asleep = false;
        let now = Instant::now();
        if why == WakeSource::Touch {
            self.desk.borrow_mut().touched = Some(now);
            if self.screen.get_page() == "board" {
                self.board_home_show();
            }
        }
        {
            let mut m = self.motion.borrow_mut();
            if !m.awake.accepts(why, now) {
                return;
            }
            if m.awake.is_asleep() {
                log::info!(event = "screen_wake", source = format!("{why:?}"); "screen: awake again ({why:?})");
            } else if !m.awake.is_awake(now) {
                log::debug!("wake: {why:?}");
            }
            m.awake.wake(now);
        }
        if self.update_night(now) {
            self.show(self.view.get());
            return;
        }
        self.step();
    }

    /// Vol+ and Vol− together outside "Casa": the screen to sleep now (backlight 0), until
    /// the next wake. home-companion keeps listening.
    pub fn sleep(&self) {
        if self.opts.demo.is_some() || super::webview::visible() {
            return;
        }
        log::info!(event = "screen_sleep"; "screen: to sleep (Vol+ and Vol- together)");
        // Put to sleep on purpose: no access dialog lights it up (the browser waits).
        self.panel.borrow_mut().keys_asleep = true;
        self.motion.borrow_mut().awake.sleep(Instant::now());
        self.step();
    }

    /// New animations, the start-up timeout; then [`Ui::step`]. Called every 20 ms.
    pub fn poll(&self) {
        self.poll_panel();
        self.poll_link();
        self.poll_find();
        self.poll_brain();
        self.poll_ha();
        self.poll_house();
        self.poll_poses();
        let mut added = false;
        {
            let mut m = self.motion.borrow_mut();
            for i in 0..m.lazy.len() {
                let Some(rx) = &m.lazy[i].incoming else {
                    continue;
                };
                let mut got = Vec::new();
                loop {
                    match rx.try_recv() {
                        Ok(c) => got.push(c),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            m.lazy[i].incoming = None;
                            break;
                        }
                    }
                }
                for c in got {
                    if let Some(name) = anim::static_name(&c.anim.name)
                        && m.lazy[i].on
                    {
                        m.seq.add(name, Timing::of(&c.anim));
                        m.clips.insert(name, Arc::new(c));
                        added = true;
                    }
                }
            }
            if let Some(rx) = &m.pose_incoming {
                match rx.try_recv() {
                    Ok(c) => {
                        m.seq.add(anim::CARD_POSE, Timing::of(&c.anim));
                        m.clips.insert(anim::CARD_POSE, Arc::new(c));
                        m.pose_incoming = None;
                        added = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => m.pose_incoming = None,
                }
            }
            if let Some(rx) = &m.incoming {
                let mut got = Vec::new();
                loop {
                    match rx.try_recv() {
                        Ok(c) => got.push(c),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            m.incoming = None;
                            break;
                        }
                    }
                }
                for c in got {
                    // The start-up already over (Home Assistant was quick): not needed.
                    if c.anim.name == "loading" && m.loading_until.is_none() {
                        continue;
                    }
                    if let Some(name) = anim::static_name(&c.anim.name) {
                        m.seq.add(name, Timing::of(&c.anim));
                        m.clips.insert(name, Arc::new(c));
                        added = true;
                    }
                }
            }
        }
        let over = self
            .motion
            .borrow()
            .loading_until
            .is_some_and(|t| Instant::now() >= t);
        if over {
            log::info!("start-up animation over: Home Assistant not connected yet");
            {
                let mut m = self.motion.borrow_mut();
                m.loading_until = None;
                Self::drop_loading_clip(&mut m);
            }
            self.wake(WakeSource::Start);
            self.show(self.view.get());
        } else if added {
            self.step();
        }
    }

    /// The frame and the backlight for now; keeps the frame timer running only while the
    /// character moves.
    pub fn step(&self) {
        let now = Instant::now();
        let night = self.desk.borrow().night_now;
        let (show, moving) = {
            let mut m = self.motion.borrow_mut();
            // "Casa" or a mirrored screen covers the home: the character stays still, the
            // backlight bright.
            let casa = super::webview::visible() || self.mirror_shown();
            let awake = m.awake.is_awake(now) && !casa;
            let show = m.seq.show(now, awake);
            let dim_home = !casa
                && m.loading_until.is_none()
                && matches!(self.view.get(), Phase::Home | Phase::Offline);
            // The light sensor's levels (or the configured ones); night mode caps them.
            let mut levels = self
                .auto
                .borrow()
                .as_ref()
                .and_then(Auto::levels)
                .unwrap_or(self.opts.screen.levels);
            if let Some(p) = self.desk.borrow().brightness_manual {
                levels = manual_levels(self.opts.screen.levels, p);
            }
            // Night, or do not disturb: the night's ceiling (the view excepted).
            if (night || self.dnd_on()) && !casa {
                levels = levels.min(self.opts.screen.night_levels);
            }
            let level = if m.awake.is_asleep() && !casa {
                0
            } else {
                levels.level(dim_home, awake)
            };
            (show, (m.seq.moving(awake), level))
        };
        let (moving, level) = moving;
        if self.opts.demo.is_none() {
            super::backlight::set_screen_off(level == 0);
            self.backlight_to(level);
        }
        self.present(show);
        if moving && !self.frame_timer.running() {
            let ui = self.this.borrow().clone();
            let fps = self
                .motion
                .borrow()
                .clips
                .values()
                .map(|c| c.anim.fps)
                .fold(1.0f32, f32::max);
            self.frame_timer.start(
                TimerMode::Repeated,
                Duration::from_secs_f32(1.0 / fps),
                move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.step();
                    }
                },
            );
        } else if !moving && self.frame_timer.running() {
            self.frame_timer.stop();
        }
        // A sheet decoded when it is needed, freed once its last clip has played.
        self.lazy_sheets();
    }

    /// The backlight towards `level`: a fade (backlight.rs, [`FADE`]), at once to and from 0.
    fn backlight_to(&self, level: u32) {
        let now = Instant::now();
        let current = self.motion.borrow().backlight.current();
        let target = self.fade.get().map_or(current, |f| Some(f.to));
        if target == Some(level) {
            return;
        }
        match current {
            Some(c) if c != 0 && level != 0 && self.backlight_known() => {
                let from = self.fade.get().map_or(c, |f| f.at(now).0);
                self.fade.set(Some(Fade {
                    from,
                    to: level,
                    start: now,
                }));
                if !self.fade_timer.running() {
                    let ui = self.this.borrow().clone();
                    self.fade_timer.start(
                        TimerMode::Repeated,
                        Duration::from_millis(40),
                        move || {
                            if let Some(ui) = ui.upgrade() {
                                ui.fade_step();
                            }
                        },
                    );
                }
            }
            _ => {
                self.fade.set(None);
                self.fade_timer.stop();
                self.motion.borrow_mut().backlight.set(level);
            }
        }
    }

    /// A real backlight (a fade on a missing one would only fill the log).
    fn backlight_known(&self) -> bool {
        !self.opts.screen.backlight.is_empty()
    }

    fn fade_step(&self) {
        let Some(f) = self.fade.get() else {
            self.fade_timer.stop();
            return;
        };
        let (level, done) = f.at(Instant::now());
        let mut m = self.motion.borrow_mut();
        m.backlight.set_quiet(level);
        drop(m);
        if done {
            // The end of the fade is the one logged.
            log::info!(event = "backlight", level = level as u64, from = f.from as u64; "backlight {level} (faded from {})", f.from);
        }
        if done {
            self.fade.set(None);
            self.fade_timer.stop();
        }
    }

    /// A new reading of the light sensor, if any (every 2 s): the levels follow it.
    fn feed_light(&self) {
        let Some((seq, lux)) = super::light::lux() else {
            return;
        };
        if seq == self.lux_seq.replace(seq) {
            return;
        }
        let changed = self.auto.borrow_mut().as_mut().and_then(|a| a.feed(lux));
        if let Some(l) = changed {
            log::info!(
                event = "auto_brightness", lux = lux as f64, idle = l.idle, active = l.active;
                "auto brightness: {lux:.1} lux → idle {}, active {}", l.idle, l.active
            );
            self.step();
        }
    }

    /// Puts `show` on the screen if it changed.
    fn present(&self, show: Show) {
        let t = Instant::now();
        let mut guard = self.motion.borrow_mut();
        let m = &mut *guard;
        if show == m.shown {
            return;
        }
        let (clip, index, step) = match show {
            Show::Frame { clip, index } => (clip, index, None),
            Show::Fade { clip, index, step } => (clip, index, Some(step)),
            Show::Pose => {
                m.shown = Show::Pose;
                m.fade_from = None;
                self.screen.set_frame(Image::default());
                return;
            }
        };
        let Some(c) = m.clips.get(clip).cloned() else {
            return;
        };
        let was = match m.shown {
            Show::Frame { clip, .. } | Show::Fade { clip, .. } => Some(clip),
            Show::Pose => None,
        };
        let new_clip = was != Some(clip) || m.playhead.is_none();
        if new_clip {
            // A fade starts from the picture on screen: a frame of the same sheet, same box.
            let old = m.playhead.take();
            m.fade_from = match (step, old) {
                (Some(_), Some(o))
                    if o.clip.anim.file == c.anim.file
                        && o.clip.anim.size == c.anim.size
                        && o.clip.anim.rect == c.anim.rect =>
                {
                    Some(o.pixels)
                }
                _ => None,
            };
            m.playhead = Some(Playhead::new(c.clone()));
            let (x, y, w, h) = self.frame_rect(&c.anim);
            self.screen.set_frame_rect(x, y, w, h);
        }
        if step.is_none() {
            m.fade_from = None;
        }
        m.shown = show;
        let p = m.playhead.as_mut().expect("set above");
        let changed = p.seek(index) || new_clip || step.is_some();
        if !changed {
            return;
        }
        if let (Some(step), Some(from)) = (step, m.fade_from.as_ref()) {
            let w = step.min(anim::FADE_FRAMES) * 256 / (anim::FADE_FRAMES + 1);
            let blended: Vec<u8> = from
                .iter()
                .zip(&p.pixels)
                .map(|(&a, &b)| ((a as u32 * (256 - w) + b as u32 * w) >> 8) as u8)
                .collect();
            let buf =
                SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&blended, c.width(), c.height());
            self.screen.set_frame(Image::from_rgba8(buf));
            m.presented += 1;
            m.present_time += t.elapsed();
            return;
        }
        // A new image per frame: the software renderer redraws only the frame's box; femtovg
        // uploads it as a new texture (measured on the device: the UI thread at 13.8 % of a core
        // while animating, against 12.6 % with a GL texture updated in place, which needs a
        // rendering notifier: see renderer.rs).
        let buf =
            SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&p.pixels, c.width(), c.height());
        self.screen.set_frame(Image::from_rgba8(buf));
        m.presented += 1;
        m.present_time += t.elapsed();
    }

    /// Frames presented and their CPU time since the last call (for the stats line).
    pub fn take_frame_stats(&self) -> (u64, Duration) {
        let mut m = self.motion.borrow_mut();
        let r = (m.presented, m.present_time);
        m.presented = 0;
        m.present_time = Duration::ZERO;
        r
    }

    /// At exit: full brightness, so whatever uses the screen next is visible.
    pub fn on_exit(&self) {
        self.flush_settings();
        if self.opts.demo.is_none() {
            let level = self.opts.screen.levels.active;
            self.motion.borrow_mut().backlight.set(level);
        }
    }

    /// The backlight level written last (for the stats line and the tests).
    pub fn backlight_level(&self) -> Option<u32> {
        self.motion.borrow().backlight.current()
    }

    /// The animation's rect (pose pixels) on screen, scaled and placed like its pose.
    fn frame_rect(&self, a: &Animation) -> (f32, f32, f32, f32) {
        let state = match a.name.as_str() {
            // The night clips are aligned on idle too (they start or end there).
            "loading" | "wake" | "idle" | "fall_asleep" | "sleeping" | "wake_from_sleep"
            | "settings_in" | "settings_hold" | "settings_out" => State::Idle,
            // The earmuffs and the laptop start from idle (same crop as the muted pose).
            "muted_in" | "muted_out" | "ha_in" | "ha_hold" | "ha_type_in" | "ha_type"
            | "ha_type_out" | "ha_out" | "link_in" | "link_hold" | "link_out" | "satisfied_in"
            | "satisfied_hold" | "satisfied_out" | "nod" => State::Idle,
            // A downloaded pose is drawn as the idle pose is (render_card_pose).
            anim::CARD_POSE => State::Idle,
            "muted" => State::Muted,
            "offline" => State::Offline,
            // A card's pose on the home with its state's clip (`thinking`): that state's crop.
            name if self.view.get() == Phase::Home => state_named(name).unwrap_or(State::Idle),
            _ => view(self.view.get()).1,
        };
        let (_, crop) = self.character.pose(state);
        let scale = self.screen.get_character_height() / crop.height() as f32;
        let left = self.screen.get_character_cx() - crop.width() as f32 * scale / 2.0;
        let top = self.screen.get_character_top();
        let [x0, y0, x1, y1] = a.rect;
        (
            left + (x0 - crop.x0 as f32) * scale,
            top + (y0 - crop.y0 as f32) * scale,
            (x1 - x0) * scale,
            (y1 - y0) * scale,
        )
    }

    /// v0.2's Home Assistant screens on sample data (`Demo::v02`): the home with a card from
    /// Home Assistant, a calendar event and two shopping lists; the weather page; Lists and
    /// calendars.
    fn show_demo_v02(&self, d: &Demo, l: &Locale) {
        let names = |k: &str| -> Vec<String> { l.sample(k).split('|').map(String::from).collect() };
        match d.v02 {
            "home" | "next" => {
                let mut desk = self.desk.borrow_mut();
                if d.v02 == "next" {
                    // The next event and the lists need room: no timer, no card.
                    desk.timers.clear();
                } else {
                    desk.ha_cards.apply(
                        crate::ha::events::CardEvent::Show(crate::ha::events::HaCard {
                            id: Some("washing".into()),
                            title: Some(l.sample("ha_card_title_sample").into()),
                            text: l.sample("ha_card_text").into(),
                            icon: Some("mdi:washing-machine".into()),
                            seconds: None,
                            // Shown only if a pose "laundry" was downloaded (none in the screenshots).
                            pose: Some("laundry".into()),
                            quiet: false,
                        }),
                        Instant::now(),
                    );
                }
                desk.events = demo_events(l);
                let items = |v: &[&str]| -> Vec<(String, String)> {
                    v.iter().map(|t| (t.to_string(), t.to_string())).collect()
                };
                let n = names("list_names");
                desk.shopping = Some(vec![
                    ShopList {
                        entity: "todo.groceries".into(),
                        name: n[0].clone(),
                        items: items(&["Bread", "Milk"]),
                    },
                    ShopList {
                        entity: "todo.hardware".into(),
                        name: n[1].clone(),
                        items: items(&["Screws"]),
                    },
                ]);
            }
            "weather" => {
                let w = Weather::sample_week("2026-11-03");
                let t = super::weather::text(&w, l);
                self.screen.set_weather(Some((
                    &w.condition,
                    t.condition.as_str(),
                    t.rounded.as_str(),
                )));
                let rows = super::weather::forecast_rows(&w, l, "2026-11-03", 7);
                self.screen.set_weather_page(t.icon, &t.high, &t.low, &rows);
            }
            "lists" => {
                let (lists, cals) = (names("list_names"), names("calendar_names"));
                let named = |p: &str, v: &[String]| -> Vec<(String, String)> {
                    v.iter()
                        .enumerate()
                        .map(|(i, n)| (format!("{p}.demo{i}"), n.clone()))
                        .collect()
                };
                let mut c = self.choice.borrow_mut();
                c.known = crate::ha::hub::Entities {
                    todo: named("todo", &lists),
                    calendar: named("calendar", &cals),
                };
                c.calendars = vec!["calendar.demo0".into()];
                drop(c);
                self.render_choices();
            }
            _ => {}
        }
    }

    /// `--state widget*`: today's dentist and dinner, a reminder, tomorrow's holiday and car
    /// service; the shopping list; the week's weather; no card, a timer, or a timer and a card
    /// from Home Assistant.
    fn show_demo_widget(&self, d: &Demo, l: &Locale) {
        if d.widget.is_none() {
            return;
        }
        let midnight = DEMO_NOW as i64 - DEMO_NOW as i64 % 86400;
        let mut desk = self.desk.borrow_mut();
        let mut events = demo_events(l);
        let ev = |start: i64, title: &str| Upcoming {
            start,
            end: start + 3600,
            all_day: false,
            title: title.into(),
            calendar: "calendar.demo0".into(),
        };
        events.push(ev(midnight + 19 * 3600 + 1800, l.sample("event_dinner")));
        events.push(ev(midnight + 86400 + 9 * 3600, l.sample("event_car")));
        desk.events = events;
        desk.upcoming = vec![(midnight + 18 * 3600, l.sample("reminder_text").to_string())];
        let items: Vec<(String, String)> = l
            .sample("shop_items")
            .split('|')
            .enumerate()
            .map(|(i, t)| (i.to_string(), t.to_string()))
            .collect();
        desk.shopping = Some(vec![ShopList {
            entity: "todo.groceries".into(),
            name: l
                .sample("list_names")
                .split('|')
                .next()
                .unwrap_or("")
                .into(),
            items,
        }]);
        if d.widget == Some(WidgetKind::Quotes) {
            self.widget_data.borrow_mut().quotes = demo_quotes();
        }
        if d.widget == Some(WidgetKind::Notes) {
            let midnight = DEMO_NOW as i64 - DEMO_NOW as i64 % 86400;
            *self.board.borrow_mut() = l
                .sample("notes_board")
                .split('|')
                .enumerate()
                .map(|(i, t)| brain_link::Note {
                    id: i as i64 + 1,
                    text: t.into(),
                    created: midnight - (3 - i as i64) * 86400 + 9 * 3600,
                })
                .collect();
        }
        if matches!(
            d.widget,
            Some(WidgetKind::Sensors | WidgetKind::Check | WidgetKind::Playing | WidgetKind::Home)
        ) {
            let (data, curves) = demo_ha(l);
            *self.ha_widgets.borrow_mut() = data;
            *self.ha_curves.borrow_mut() = curves;
            self.widget_on.borrow_mut().push(WidgetKind::Home.into());
        }
        if d.widget == Some(WidgetKind::Cities) {
            let (choices, weather) = demo_cities();
            *self.widget_cities.borrow_mut() = Lists::one(choices);
            *self.city_weather.borrow_mut() = weather;
        }
        desk.timers.truncate(d.widget_cards.min(1));
        // The next alarm would be a card too: not in these.
        desk.alarms.clear();
        if d.widget_cards > 1 {
            desk.ha_cards.apply(
                crate::ha::events::CardEvent::Show(crate::ha::events::HaCard {
                    id: Some("washing".into()),
                    title: Some(l.sample("ha_card_title_sample").into()),
                    text: l.sample("ha_card_text").into(),
                    icon: Some("mdi:washing-machine".into()),
                    seconds: None,
                    pose: None,
                    quiet: false,
                }),
                Instant::now(),
            );
        }
        drop(desk);
        if d.widget == Some(WidgetKind::Notes) {
            self.refresh_notes();
        }
        if !self.ha_widgets.borrow().sensors.is_empty() {
            self.apply_ha_widgets();
        }
        // The afternoon (or the evening) hour by hour, from the hour going on (14:00), and the
        // day's sun.
        let now = self.now() as i64;
        let hour = now - now % 3600;
        *self.weather.borrow_mut() = Some(
            Weather::sample_week(DEMO_DATE)
                .with_sample_hours(hour)
                .with_sample_sun(DEMO_NOW as i64 / 86400, now),
        );
    }

    fn show_demo(&self, d: Demo) {
        let l = self.locale.borrow().clone();
        let w = |k: &str| l.text(k).to_string();
        self.screen.set_clock(if d.night {
            "23:48"
        } else if d.evening {
            "19:10"
        } else {
            "14:25"
        });
        self.screen.set_date(&l.date(2, 3, 10));
        // Home Assistant unreachable: no weather either.
        self.screen.set_weather(
            (d.phase != Phase::Offline)
                .then(|| ("partlycloudy", l.text("weather_partlycloudy"), "14°")),
        );
        self.screen.set_heard(l.sample("heard"));
        // While thinking the answer is not in yet, unless the demo is about the written answer
        // coming before its voice.
        if d.phase != Phase::Thinking || d.answer_early {
            self.screen.set_answer(l.sample("answer"));
        } else {
            self.screen.set_answer("");
        }
        let demo_wave: Vec<f32> = (0..72)
            .map(|i| {
                let k = (i as f32 - 35.5).abs() / 36.0;
                let a = match d.phase {
                    Phase::Listening => 0.55,
                    Phase::Speaking => 0.8,
                    Phase::Thinking => 0.15,
                    _ => 0.0,
                };
                a * (1.0 - k).powf(1.5) * (0.6 + 0.4 * ((i as f32) * 0.9).sin().abs())
            })
            .collect();
        self.screen.set_wave(&demo_wave);
        let timer = |id: &str, name: &str, left: &str, fraction: f32, paused: bool| TimerView {
            id: id.into(),
            name: name.into(),
            left: left.into(),
            fraction,
            paused,
            due_unix: DEMO_NOW + 462.0,
        };
        let eggs = l.sample("timer_name").to_string();
        {
            let mut desk = self.desk.borrow_mut();
            desk.timers = vec![timer("a", &eggs, "07:42", 0.77, false)];
            if matches!(d.page, "timers" | "altro") {
                desk.timers.push(timer("b", "Pasta", "11:05", 0.55, true));
            }
            desk.alarms = vec![
                Alarm {
                    id: 1,
                    hour: 7,
                    minute: 0,
                    days: crate::alarms::WEEKDAYS,
                    on: true,
                    rung_minute: 0,
                },
                Alarm {
                    id: 2,
                    hour: 9,
                    minute: 30,
                    days: crate::alarms::WEEKEND,
                    on: false,
                    rung_minute: 0,
                },
            ];
            desk.ringing = vec![(eggs.clone(), "timer".into())];
            desk.ringing_since = Some(Instant::now() - Duration::from_secs(12));
            if d.cards {
                desk.music = Some(MusicView {
                    title: "Clair de lune".into(),
                    artist: "Claude Debussy".into(),
                    album: "Suite bergamasque".into(),
                    source: "AirPlay".into(),
                });
            }
            if d.page == "music" {
                // A still of the spectrum: a song's bass, a voice, the cymbals.
                const BANDS: [f32; 16] = [
                    0.62, 0.70, 0.86, 0.95, 0.74, 0.58, 0.66, 0.80, 0.52, 0.47, 0.55, 0.40, 0.33,
                    0.26, 0.20, 0.12,
                ];
                const PEAKS: [f32; 16] = [
                    0.72, 0.80, 0.93, 1.0, 0.84, 0.66, 0.74, 0.93, 0.60, 0.57, 0.62, 0.50, 0.40,
                    0.33, 0.27, 0.20,
                ];
                self.screen.set_music_spectrum(&BANDS, &PEAKS);
                let design =
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spike-ui/ui/design");
                if d.music_wide {
                    self.screen
                        .set_music_cover(load_cover(&design.join("cover-sample-wide.png")), false);
                }
                let layout = if d.music_layout > 0 {
                    d.music_layout
                } else {
                    2
                };
                self.screen.set_music_layout(layout);
                desk.music = Some(MusicView {
                    title: "Clair de lune".into(),
                    artist: "Claude Debussy".into(),
                    album: "Suite bergamasque".into(),
                    source: "AirPlay · iPhone".into(),
                });
            }
        }
        {
            // Yesterday evening, then today.
            let t = DEMO_NOW as i64 - SESSION_DEMO;
            let mut h = self.history.borrow_mut();
            h.push(t, BRAIN, l.sample("chat_q1"), l.sample("chat_a1"));
            h.push(
                t + SESSION_DEMO - 600,
                BRAIN,
                l.sample("heard"),
                l.sample("answer"),
            );
            h.push(
                t + SESSION_DEMO - 540,
                BRAIN,
                l.sample("chat_q2"),
                l.sample("chat_a2"),
            );
        }
        self.screen.set_music(self.desk.borrow().music.as_ref());
        self.show_demo_v02(&d, &l);
        self.show_demo_board(d.board);
        self.show_demo_widget(&d, &l);
        if d.panel > 0 {
            self.demo_panel(d.panel == 2);
            if d.panel == 3 {
                self.panel.borrow_mut().blocked = true;
                self.refresh_home();
            }
            if d.panel >= 4 {
                self.demo_remote(d.panel == 5);
            }
        }
        if let Some((w, h)) = d.mirror {
            let info = MirrorInfo {
                source: l.sample("mirror_source").to_string(),
                width: w,
                height: h,
            };
            self.screen.set_mirror(true, &info.source, w, h);
            self.screen
                .set_mirror_frame(super::mirror::placeholder(w, h));
            self.screen.set_face(self.face());
            self.screen.set_mirror_freeze(d.mirror_at.unwrap_or(-1.0));
            // The bar over a landscape sender as it is in its first seconds; the tablet clean.
            self.screen.set_mirror_chrome(w == 1280);
            self.desk.borrow_mut().mirror = Some(info);
        }
        if d.control {
            let st = crate::ui::control::ControlState {
                wifi: Some("Home".into()),
                bluetooth: None,
                airplay: Some(if d.cards {
                    "iPhone".into()
                } else {
                    String::new()
                }),
                mic_on: !d.mic_switch_off,
                mic_switch_off: d.mic_switch_off,
                night_now: false,
                night_from: "23:00".into(),
                dnd: d.dnd.then(|| "07:00".into()),
                ha: Some(true),
                camera: Some(crate::ui::control::Camera::Ready),
            };
            let tiles = crate::ui::control::tiles(&st, &w);
            self.screen
                .set_control(true, &tiles, 62, true, &w("cc_auto"));
        }
        self.screen.set_setup(
            &w("wifi_named").replace("{ssid}", "Home"),
            &w("setup_ha").replace("{host}", "ha.example.org"),
        );
        // A real code of the link, to a sample address (screenshots, docs).
        let sample = crate::ha::link::authorize_url(
            "http://ha.example.org:8123",
            "http://192.0.2.20:8124/",
            "0123456789abcdef0123456789abcdef",
        );
        self.screen.set_setup_link(qr_image(&sample), "", false);
        self.refresh_desk();
        // The widget on the demo's page (as if swiped there).
        if let Some(kind) = d.widget {
            for _ in 0..WidgetKind::ALL.len() {
                if self.widget.borrow().kind() == Some(kind) {
                    break;
                }
                let shown = self.widget.borrow_mut().swipe(1, Instant::now());
                if let Some(s) = shown {
                    self.show_widget(&s);
                }
            }
        }
        if d.cards {
            let msgs = vec![(
                l.sample("message_title").to_string(),
                l.sample("message_text").to_string(),
            )];
            let timers = self.desk.borrow().timers.clone();
            let m = self.desk.borrow().music.clone().unwrap_or_default();
            let of = |i: usize, n: usize| {
                w("of")
                    .replace("{i}", &i.to_string())
                    .replace("{n}", &n.to_string())
            };
            let more = |n: usize| w("more_timers").replace("{n}", &n.to_string());
            let cw = CardWords {
                of: &of,
                more_timers: &more,
                paused: l.text("paused"),
            };
            let name = &self.character.name;
            // house-done-card(s): the "done" card first in the band, after its pop-up.
            let done = (d.house == 1 && d.house_card_only).then(|| {
                let first = if d.house_group {
                    // The first of the sample lights, in the screen's language.
                    let lights = l.sample("house_lights");
                    let first_light = lights.split(", ").next().unwrap_or(lights);
                    l.text("house_and_more")
                        .replace("{name}", first_light)
                        .replace("{n}", "3")
                } else {
                    l.sample("house_light").to_string()
                };
                (first, l.text("house_state_off").to_string(), true)
            });
            let alive = Alive {
                done,
                setup: d.setup_card.then(|| {
                    (
                        word(&l, "setup_card_complete_title", name),
                        word(&l, "setup_card_complete_text", name),
                    )
                }),
                messages: &msgs,
                timers: &timers,
                music: Some((
                    m.title.clone(),
                    format!("{} · {}", m.artist, m.source),
                    true,
                )),
                // The band full (four cards): the next alarm too.
                alarm: ((d.dnd && d.mic_switch_off) || d.cards_n.is_some()).then(|| {
                    (
                        l.text("next_alarm").replace("{when}", "07:00"),
                        l.text("weekdays").to_string(),
                    )
                }),
                ..Default::default()
            };
            let mut shown = cards::pick_n(&alive, &cw, self.max_cards());
            if let Some(n) = d.cards_n {
                shown.truncate(n);
            }
            self.screen.set_cards(&shown);
        }
        if d.notice {
            {
                let mut desk = self.desk.borrow_mut();
                desk.reminders = vec![(
                    1,
                    l.text("reminder_title").to_string(),
                    l.sample("reminder_text").to_string(),
                )];
                desk.notice = Some(Notice {
                    id: 1,
                    since: Instant::now(),
                    when: "14:25".into(),
                    sounded: Instant::now(),
                });
            }
            self.refresh_notice();
        }
        if d.night {
            let mut desk = self.desk.borrow_mut();
            desk.night_now = true;
            desk.night_shown = true;
        }
        if d.night {
            self.screen.set_night(true, "07:00", true);
        }
        self.screen.set_still(true);
        if let Some((clip, _)) = d.frame {
            self.demo_clip(clip);
        }
        if d.starting {
            // The start-up screen: as `show` draws it while the loading animation plays.
            self.motion.borrow_mut().loading_until =
                Some(Instant::now() + Duration::from_secs(3600));
        }
        self.show(d.phase);
        if d.night {
            self.screen.set_night(true, "07:00", true);
        }
        if matches!(d.page, "memories" | "reminders" | "shopping" | "altro") {
            let n = 2.to_string();
            self.screen.set_memories(&MemoriesView {
                present: true,
                tile: l.text("tile_memories").replace("{n}", &n),
                tile_known: l.text("tile_memories").replace("{n}", &n),
                reminders_tile: l.text("tile_reminders").replace("{n}", "1"),
                status: format!(
                    "{} · {}",
                    l.text("brain_status")
                        .replace("{facts}", &n)
                        .replace("{model}", "gpt-5-mini"),
                    l.text("tidy_line")
                        .replace("{t}", "03:30")
                        .replace("{merged}", "2")
                        .replace("{updated}", "1")
                        .replace("{retired}", "1")
                ),
                sent: word(&l, "memories_sent", &self.character.name),
                more: more_line(&l, 1),
                facts: vec![
                    MemoryLine {
                        id: 2,
                        text: l.sample("memory_fact2").into(),
                        date: "4 nov".into(),
                        dim: false,
                    },
                    MemoryLine {
                        id: 1,
                        text: l.sample("memory_fact1").into(),
                        date: "2 nov".into(),
                        dim: true,
                    },
                ],
                confirm: vec![MemoryLine {
                    id: 3,
                    text: l.sample("memory_fact3").into(),
                    date: "13 nov".into(),
                    dim: false,
                }],
                jobs: vec![(
                    3,
                    format!("{} 18:00", w("today")),
                    l.sample("reminder_text").into(),
                )],
                events: if d.v02 == "events" {
                    event_rows(&demo_events(&l), &l, DEMO_NOW as i64)
                } else {
                    Vec::new()
                },
            });
            let items: Vec<(String, String, bool)> = l
                .sample("shop_items")
                .split('|')
                .enumerate()
                .map(|(i, t)| (i.to_string(), t.to_string(), false))
                .collect();
            self.screen.set_shopping(
                true,
                &items,
                &l.text("tile_shopping")
                    .replace("{n}", &items.len().to_string()),
            );
        }
        if matches!(d.page, "poses" | "settings") {
            self.render_poses_demo();
        }
        if (d.page == "ha" && d.ha <= 2) || d.page == "ha-pick" {
            use crate::ha::setup::{Level, Row};
            let row = |id: &str, level, what: &str, args: &[(&str, &str)], repair| Row {
                id: id.into(),
                level,
                key: format!("ha_{id}_{what}"),
                args: args
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                repair,
            };
            let mut p = self.ha_page.borrow_mut();
            p.demo_configured = true;
            p.rows = vec![
                row("link", Level::Ok, "ok", &[], false),
                row(
                    "wyoming",
                    Level::Ok,
                    "ok",
                    &[("satellite", "assist_satellite.alfred")],
                    false,
                ),
                row("mcp", Level::Ok, "ok", &[("time", "14:02")], false),
                row("ai", Level::Ok, "ok", &[("ai", "OpenAI")], false),
                row(
                    "agent",
                    Level::Ok,
                    "ok",
                    &[("agent", "conversation.alfred"), ("ai", "OpenAI")],
                    false,
                ),
                row(
                    "pipeline",
                    Level::Ok,
                    "ok",
                    &[("stt", "OpenAI STT"), ("tts", "OpenAI TTS")],
                    false,
                ),
                row("satellite", Level::Warn, "changed_preferred", &[], true),
                row("vad", Level::Ok, "ok", &[], false),
                row("ai_task", Level::Skipped, "none", &[], false),
            ];
            // Three AIs, the speech-to-text and the voices Home Assistant has (sample data).
            let picks = |v: &[(&str, &str)]| -> Vec<crate::ha::setup::Pick> {
                v.iter()
                    .map(|(id, name)| crate::ha::setup::Pick {
                        id: id.to_string(),
                        name: name.to_string(),
                    })
                    .collect()
            };
            p.choices = crate::ha::setup::Choices {
                ai: picks(&[
                    ("gemini", "Google Gemini"),
                    ("openai", "OpenAI"),
                    ("claude", "Claude"),
                ]),
                stt: picks(&[
                    ("stt.google_ai_stt", "Google AI STT"),
                    ("stt.home_assistant_cloud", "Home Assistant Cloud"),
                    ("stt.openai_stt", "OpenAI STT"),
                ]),
                tts: picks(&[
                    ("tts.google_ai_tts", "Google AI TTS"),
                    ("tts.home_assistant_cloud", "Home Assistant Cloud"),
                    ("tts.openai_tts", "OpenAI TTS"),
                ]),
                voice: picks(&[
                    ("marin", "Marin"),
                    ("cedar", "Cedar"),
                    ("alloy", "Alloy"),
                    ("ash", "Ash"),
                    ("echo", "Echo"),
                    ("onyx", "Onyx"),
                ]),
                current: crate::ha::setup::Choice {
                    ai: "openai".into(),
                    stt: "stt.openai_stt".into(),
                    tts: "tts.openai_tts".into(),
                    voice: "cedar".into(),
                },
                ..Default::default()
            };
            if d.page == "ha-pick" {
                p.picking = d.pick.into();
            }
            if d.ha == 1 {
                p.busy = true;
                p.asked = true;
            }
            if d.ha == 2 {
                p.done = true;
                p.rows[6] = row("satellite", Level::Ok, "ok", &[], false);
                p.status = word(&l, "ha_ready", &self.character.name);
            }
            drop(p);
            self.render_ha();
            self.screen.set_ha_linked(true);
        }
        if d.page == "find-ha" {
            let rows = vec![
                ("Home".to_string(), "http://ha.example:8123".to_string()),
                (
                    l.sample("find_second").to_string(),
                    "http://192.0.2.20:8123".to_string(),
                ),
            ];
            match d.find {
                "found" => self.screen.set_find("found", &rows, ""),
                "none" => self.screen.set_find(
                    "none",
                    &[],
                    &word(&l, "find_none_text", &self.character.name),
                ),
                _ => self.screen.set_find("searching", &[], ""),
            }
        }
        self.screen.set_page(d.page);
        match d.page {
            "trends" => {
                if d.widget == Some(WidgetKind::Sensors) {
                    self.screen.set_trends_kind("sensors");
                }
                self.refresh_trends()
            }
            "city" => self.refresh_city(),
            "notes" | "reminders" => self.refresh_notes(),
            _ => {}
        }
        if !d.dialog.is_empty() {
            self.screen.set_dialog(d.dialog);
        }
        if let Some(v) = d.volume {
            self.screen.set_volume(v, true);
        }
        // Settings → Info: a newer release out (sample hash).
        if d.page == "update" {
            let sha = "d4ede2782c0b31223b00e62a56b95c3620b20992f419a1ae8001bf075bafc61f";
            let repo = crate::update::DEFAULT_REPO;
            self.screen.set_update(&UpdateView {
                facts: l
                    .text("update_facts")
                    .replace("{v}", env!("CARGO_PKG_VERSION"))
                    .replace("{repo}", repo)
                    .replace("{when}", &l.text("update_today").replace("{t}", "14:25")),
                state: l.text("update_available").replace("{v}", "0.3.1"),
                notes: crate::update::notes_text(
                    "A small release: **the camera's colours** and two fixes.\n\n## New\n\n- The camera tuned for the S5KC505A: natural colours, brighter in a dim room.\n\n## Fixed\n\n- The swipe from the edge of the Home Assistant view works every time.\n- German: the last lines of the Memories page are no longer hidden.",
                ),
                sha: crate::update::sha_groups(sha),
                attention: true,
                install: l.text("update_install").replace("{v}", "0.3.1"),
                busy: false,
                question: l.text("update_question").replace("{v}", "0.3.1"),
                confirm: l.text("update_confirm").into(),
                steps: Vec::new(),
                progress: -1.0,
                prerelease: Some(false),
            });
        }
        // Sample use for the Home Assistant page's row and the use page.
        if matches!(d.page, "ha" | "usage") {
            let mut u = crate::usage::Usage::default();
            let now = DEMO_NOW as i64;
            for k in 0..30i64 {
                let q = [3u32, 8, 12, 5, 0, 9, 14, 6, 11, 7][(k % 10) as usize];
                let day = crate::usage::local_day(now - k * 86_400);
                for _ in 0..q {
                    u.add(&day, 4.0, 6.0, "");
                }
            }
            let est = u.view(now, &crate::usage::Prices::default());
            if d.usage_precise {
                // The integration's numbers: a little under the estimate, to the tenth of a cent.
                let days: Vec<(u32, f64)> = est.days.iter().map(|&(n, c)| (n, c * 0.83)).collect();
                *self.usage_precise.borrow_mut() = Some(crate::usage::Precise {
                    today_cost: days.last().map_or(0.0, |d| d.1),
                    month_cost: est.month_cost * 0.83,
                    requests: Some(est.today.questions as u64 * 2),
                    tokens: Some(est.today.questions as u64 * 3_150),
                    days,
                });
            }
            self.show_usage(&est);
        }
        if d.page == "what" {
            self.refresh_what();
        }
        if d.dnd {
            self.screen.set_dnd(
                true,
                &format!("{} · {}", w("dnd"), w("cc_until").replace("{t}", "07:00")),
            );
        }
        if d.mic_switch_off {
            self.screen.set_mic_off(true, &w("mic_off_line"));
        }
        if let Some(on) = d.mic_hud {
            self.screen
                .set_mic_hud(true, on, &w(if on { "mic_hud_on" } else { "mic_hud_off" }));
        }
        match d.house {
            1 => {
                let word = |k: &str| {
                    let x = l.text(k);
                    (x != k).then(|| x.to_string())
                };
                // One light, or (house-done-group) four switched off by one sentence.
                let names: Vec<String> = if d.house_from_ask {
                    vec![l.sample("house_blind").to_string()]
                } else if d.house_group {
                    l.sample("house_lights")
                        .split(", ")
                        .map(str::to_string)
                        .collect()
                } else {
                    vec![l.sample("house_light").to_string()]
                };
                let mut cards = house::HouseCards::default();
                let t0 = Instant::now();
                for (i, n) in names.iter().enumerate() {
                    let line = if d.house_from_ask {
                        house::done_line(n, Some("closed"), "close_cover", &word)
                    } else {
                        house::done_line(n, Some("off"), "turn_off", &word)
                    };
                    cards.changed(i as i64, line, true, t0);
                }
                let card = cards.done.expect("a card");
                let (line, name, state) =
                    card.texts(l.text("house_and_more"), l.text("house_others"));
                self.screen.set_done(true, &line, true);
                self.screen.set_done_parts(&name, &state);
                self.screen.set_done_popup(!d.house_card_only, 0.62);
                self.screen.set_done_from_ask(d.house_from_ask);
            }
            2 => {
                self.screen
                    .set_ask(true, l.sample("house_question"), "", 0.62);
                self.screen
                    .set_ask_working(d.house_pending, l.sample("house_blind"), false);
            }
            _ => {}
        }
    }

    /// The controls over the mirrored screen: shown now, hidden [`MIRROR_CHROME`] after the
    /// last touch (the demos keep them).
    fn poke_mirror(&self) {
        self.screen.set_mirror_chrome(true);
        if self.opts.demo.is_some() {
            return;
        }
        let ui = self.this.borrow().clone();
        self.mirror_timer
            .start(TimerMode::SingleShot, MIRROR_CHROME, move || {
                if let Some(ui) = ui.upgrade() {
                    ui.screen.set_mirror_chrome(false);
                }
            });
    }

    /// The volume HUD with `percent`, hidden [`VOLUME_BAR`] after the last call.
    pub fn show_volume(&self, percent: u8) {
        self.volume.set(percent);
        self.screen.set_volume(percent, true);
        self.refresh_settings();
        let ui = self.this.borrow().clone();
        self.volume_timer
            .start(TimerMode::SingleShot, VOLUME_BAR, move || {
                if let Some(ui) = ui.upgrade() {
                    ui.screen.set_volume(ui.volume.get(), false);
                }
            });
    }

    /// The Wi-Fi bars now and every 15 s (page headers, control center); a fixed 3 in demo mode.
    pub fn start_wifi(&self) {
        if self.opts.demo.is_some() {
            self.screen.set_wifi_level(3);
            return;
        }
        let ui = self.this.borrow().clone();
        let read = move || {
            if let Some(ui) = ui.upgrade() {
                ui.screen.set_wifi_level(wifi_level("wlan0").unwrap_or(-1));
            }
        };
        read();
        self.wifi_timer
            .start(TimerMode::Repeated, Duration::from_secs(15), read);
    }

    /// The cloud services' use: the core's estimate came in (it changes after each
    /// conversation).
    fn show_usage(&self, v: &crate::usage::View) {
        *self.usage_estimate.borrow_mut() = Some(v.clone());
        self.render_usage();
    }

    /// Its row on the Home Assistant page and its page (the chart): the integration's numbers
    /// when it counts them, else the estimate; nothing before either is known.
    fn render_usage(&self) {
        let estimate = self.usage_estimate.borrow().clone();
        let precise = self.usage_precise.borrow().clone();
        if estimate.is_none() && precise.is_none() {
            return;
        }
        let v = crate::usage::View::choose(&estimate.unwrap_or_default(), precise.as_ref());
        let l = self.locale.borrow();
        let name = &self.character.name;
        self.screen
            .set_usage(&usage_view(&v, |k| word(&l, k, name)));
    }

    /// The precise use asked of Home Assistant once at start, then every minute while its row
    /// or its page is on screen (and at once when they open).
    fn tick_usage(&self, opened: bool) {
        const EVERY: Duration = Duration::from_secs(60);
        let last = self.usage_asked.get();
        if last.is_some_and(|t| t.elapsed() < if opened { EVERY / 4 } else { EVERY }) {
            return;
        }
        if last.is_some() && !matches!(self.screen.get_page().as_str(), "ha" | "usage") {
            return;
        }
        if let Some(s) = self.ha_setup.borrow().as_ref() {
            self.usage_asked.set(Some(Instant::now()));
            let _ = s.tx.send(crate::ha::setup::SetupCmd::Usage);
        }
    }

    /// The card of the microphone's switch for [`MIC_CARD`], the screen woken.
    pub fn show_mic_switch(&self, on: bool) {
        self.wake(WakeSource::MicSwitch);
        let text = self
            .locale
            .borrow()
            .text(if on { "mic_hud_on" } else { "mic_hud_off" })
            .to_string();
        self.screen.set_mic_hud(true, on, &text);
        let ui = self.this.borrow().clone();
        self.mic_timer
            .start(TimerMode::SingleShot, MIC_CARD, move || {
                if let Some(ui) = ui.upgrade() {
                    ui.screen.set_mic_hud(false, on, "");
                }
            });
    }

    /// Applies a command of the core (ignored in demo mode, except screenshots).
    /// Takes the mirrored screen's waiting frames (every UI tick while a sender mirrors): the
    /// newest is shown; a new size (the sender rotated) lays the page out again.
    pub fn poll_mirror(&self) {
        let stuck = self
            .mirror
            .borrow()
            .as_ref()
            .is_some_and(|m| m.stuck(MIRROR_NO_FRAME));
        if stuck {
            log::warn!(event = "mirror", state = "stuck"; "mirroring: no frame in {} s, the session is closed (the sender can start again)", MIRROR_NO_FRAME.as_secs());
            if let Some(mut m) = self.mirror.borrow_mut().take() {
                m.disconnect("no frame");
            }
            let _ = self.core.send(CoreEvent::Ui(UiEvent::MirrorStop));
            return;
        }
        let frame = self.mirror.borrow_mut().as_mut().and_then(|m| m.poll());
        let Some((img, w, h)) = frame else { return };
        let info = self.desk.borrow().mirror.clone();
        if let Some(i) = info.filter(|i| (i.width, i.height) != (w, h)) {
            self.set_mirror(Some(MirrorInfo {
                width: w,
                height: h,
                ..i
            }));
        }
        self.screen.set_mirror_frame(img);
    }

    pub fn apply(&self, cmd: UiCommand) {
        if self.opts.demo.is_some() && !matches!(cmd, UiCommand::Screenshot(_)) {
            return;
        }
        match cmd {
            UiCommand::View(p) => self.show(p),
            UiCommand::Heard(s) => {
                if !s.trim().is_empty() {
                    self.desk.borrow_mut().heard = s.clone();
                }
                self.screen.set_heard(&s);
            }
            UiCommand::Answer(s) => {
                self.screen.set_answer(&s);
                self.desk.borrow_mut().answer = s.clone();
                if !s.trim().is_empty() {
                    let heard = std::mem::take(&mut self.desk.borrow_mut().heard);
                    self.history
                        .borrow_mut()
                        .push(now_unix() as i64, BRAIN, &heard, &s);
                    if s.chars().count() > cards::LONG_ANSWER {
                        self.desk.borrow_mut().last_answer = Some((s, Instant::now()));
                    }
                    self.refresh_chat();
                }
            }
            UiCommand::Timers(rows) => {
                self.desk.borrow_mut().timers = rows;
                self.refresh_home();
                self.refresh_timers();
            }
            UiCommand::Alarms(list) => {
                self.desk.borrow_mut().alarms = list;
                self.refresh_timers();
            }
            UiCommand::Messages(rows) => {
                self.desk.borrow_mut().ringing = rows;
                self.refresh_ringing();
            }
            UiCommand::Wave(bars) => self.screen.set_wave(&bars),
            UiCommand::Volume(p) => self.show_volume(p),
            UiCommand::VolumeNow(p) => {
                self.volume.set(p);
                self.screen.set_volume(p, false);
            }
            UiCommand::ScreenOff => self.screen_off(),
            UiCommand::TakePhoto => {
                if let Some(me) = self.this.borrow().upgrade() {
                    me.open_camera(true);
                }
            }
            UiCommand::DoNotDisturb(until) => self.set_dnd(until),
            UiCommand::Announcing(on) => {
                self.announcing.set(on);
                self.screen.set_announcing(on);
            }
            UiCommand::Conversation {
                state,
                trigger,
                handled_by,
            } => {
                if let Some(tx) = self.hub_tx.borrow().as_ref() {
                    let _ = tx.send(HubCmd::Fire {
                        event_type: crate::ha::events::CONVERSATION_EVENT.into(),
                        data: crate::ha::events::conversation_data(state, trigger, handled_by),
                    });
                }
            }
            UiCommand::Suspending => {
                self.remote_suspended(true);
                self.screen.set_settings_open(false);
                self.motion.borrow_mut().awake.sleep(Instant::now());
                self.step();
            }
            UiCommand::Resumed => {
                self.remote_suspended(false);
                self.wake(WakeSource::Resume);
                self.screen.window().request_redraw();
            }
            UiCommand::NoticeStop => {
                log::info!(event = "reminder_notice"; "reminder: stop word, as its OK");
                self.notice_done(false);
            }
            UiCommand::MirrorAspect(a) => {
                self.mirror_aspect.set(Some(a));
                if let Some(m) = self.mirror.borrow_mut().as_mut() {
                    m.set_aspect(a);
                }
            }
            UiCommand::MirrorVideo(m) => match m {
                // The page opens on its "arriving" line; the first frame gives its size.
                Some((source, path)) => {
                    let mut m = super::mirror_video::Mirror::new(path);
                    if let Some(a) = self.mirror_aspect.get() {
                        m.set_aspect(a);
                    }
                    *self.mirror.borrow_mut() = Some(m);
                    self.set_mirror(Some(MirrorInfo {
                        source,
                        width: 0,
                        height: 0,
                    }));
                }
                None => {
                    if let Some(mut m) = self.mirror.borrow_mut().take() {
                        m.disconnect("mirroring over");
                    }
                    self.set_mirror(None);
                }
            },
            UiCommand::Music(n) => {
                let path = n.as_ref().and_then(|n| n.cover.as_deref());
                let cover = path.and_then(load_cover);
                self.screen.set_music_layout(2);
                // A sender with no track information (a Mac's system sound): a title anyway.
                let untitled = self.locale.borrow().text("music_untitled").to_string();
                {
                    let mut d = self.desk.borrow_mut();
                    d.music_paused = n.as_ref().is_some_and(|n| !n.playing);
                    d.music = n.as_ref().map(|n| MusicView {
                        title: if n.title.is_empty() {
                            untitled.clone()
                        } else {
                            n.title.clone()
                        },
                        artist: n.artist.clone(),
                        album: n.album.clone(),
                        source: if n.source.is_empty() {
                            "AirPlay".into()
                        } else {
                            format!("AirPlay · {}", n.source)
                        },
                    });
                }
                let paused = self.desk.borrow().music_paused;
                self.show_music();
                self.screen.set_music_cover(cover, paused);
                self.refresh_home();
            }
            UiCommand::MusicSpectrum(b, p) => self.screen.set_music_spectrum(&b, &p),
            UiCommand::Mirror(m) => self.set_mirror(m),
            UiCommand::Control {
                airplay,
                mic_on,
                mic_switch_off,
            } => {
                {
                    let mut d = self.desk.borrow_mut();
                    d.control_airplay = airplay;
                    d.mic_on = mic_on;
                    d.mic_switch_off = mic_switch_off;
                }
                let line = self.locale.borrow().text("mic_off_line").to_string();
                self.screen.set_mic_off(!mic_on, &line);
                self.refresh_control();
                self.mic_off.set(!mic_on);
                self.refresh_home_pose();
            }
            UiCommand::MicSwitch(on) => self.show_mic_switch(on),
            UiCommand::ReflexDone => self.nod(),
            UiCommand::Usage(v) => self.show_usage(&v),
            UiCommand::Screenshot(path) => {
                if let Err(e) = save_png(&self.screen, &path) {
                    log::warn!("screenshot: {e}");
                }
            }
        }
    }

    /// A screen-mirroring session started, changed size or ended (UiCommand::Mirror): the page
    /// opens over what was on screen (Home Assistant's view and the control center close), and
    /// closes back to it at the end; while the user is home a card brings it back.
    fn set_mirror(&self, m: Option<MirrorInfo>) {
        let page = self.screen.get_page();
        let was = self.desk.borrow().mirror.is_some();
        let back = self.desk.borrow().mirror_back.clone();
        let next = super::mirror::page_on_change(was, m.as_ref(), &page, &back);
        match &m {
            Some(i) => {
                log::info!(event = "mirror"; "mirroring {} {}x{}", i.source, i.width, i.height);
                if !was {
                    self.desk.borrow_mut().mirror_back = super::mirror::back_page(&page);
                    self.screen.set_face(self.face());
                    self.screen.set_control_open(false);
                    if super::webview::visible() {
                        super::webview::request_home();
                    }
                    // The user is looking at the phone's picture: bright at once.
                    self.wake(WakeSource::Setup);
                    self.poke_mirror();
                }
                self.screen.set_mirror(true, &i.source, i.width, i.height);
            }
            None => {
                if was {
                    log::info!(event = "mirror"; "mirroring over");
                }
                self.screen.set_mirror(false, "", 0, 0);
                // The last picture goes: the next session starts on its own "waiting" line.
                self.screen.set_mirror_frame(Image::default());
            }
        }
        self.desk.borrow_mut().mirror = m;
        self.show_music();
        if let Some(p) = next {
            self.screen.set_page(&p);
        }
        self.refresh_home();
        self.step();
    }

    /// The music strip: AirPlay music, not the sound of a mirrored screen.
    fn show_music(&self) {
        let d = self.desk.borrow();
        self.screen
            .set_music(d.music.as_ref().filter(|_| d.mirror.is_none()));
    }

    /// The mirrored screen is the page on screen (it keeps the backlight up, like "Casa").
    fn mirror_shown(&self) -> bool {
        self.desk.borrow().mirror.is_some() && self.screen.get_page() == super::mirror::PAGE
    }

    /// The weather from Home Assistant (`None`: no real data, it goes away). Ignored in demo
    /// mode (`--state`: the sample).
    pub fn set_weather(&self, w: Option<Weather>) {
        if self.opts.demo.is_some() {
            return;
        }
        // The other cities in the home's unit: the last one known (a weather gone for a while
        // does not turn them back to °C).
        if let Some(f) = w.as_ref().map(|w| w.unit.contains('F'))
            && f != self.fahrenheit.get()
        {
            self.fahrenheit.set(f);
            self.configure_feeds();
        }
        *self.weather.borrow_mut() = w;
        self.render_weather();
        // The widget's weather page: new hours show when the page comes back.
        self.refresh_widget();
    }

    fn render_weather(&self) {
        if self.opts.demo.is_some() {
            return;
        }
        let w = self.weather.borrow();
        let sample = Weather::sample();
        let shown = w.as_ref().or(self.opts.sample_weather.then_some(&sample));
        let l = self.locale.borrow();
        let text = shown.map(|w| (w.condition.clone(), super::weather::text(w, &l)));
        self.screen.set_weather(
            text.as_ref()
                .map(|(c, t)| (c.as_str(), t.condition.as_str(), t.rounded.as_str())),
        );
        if let (Some(w), Some((_, t))) = (shown, &text) {
            let rows = super::weather::forecast_rows(w, &l, &crate::ha::local_date(), 7);
            self.screen.set_weather_page(t.icon, &t.high, &t.low, &rows);
        }
    }

    /// Clock and date (every second; only a change redraws); the backlight when nothing moves;
    /// the night; a page left alone; the ringing time; the undo chip.
    pub fn tick_clock(&self) {
        if self.opts.demo.is_some() {
            return;
        }
        self.tick_panel();
        self.tick_remote();
        self.refresh_home_pose();
        self.feed_light();
        let now = Instant::now();
        self.board_home_tick(now);
        if self.desk.borrow_mut().ha_cards.expire(now) {
            self.refresh_home();
        }
        if self.update_night(now) {
            self.show(self.view.get());
        } else if !self.frame_timer.running() {
            self.step();
        }
        let (wday, day, month, hour, minute) = local_now();
        let clock = format!("{hour:02}:{minute:02}");
        let new_minute = self.screen.get_clock() != clock;
        if new_minute {
            // Once a minute: the reminders still to come (set by voice through Home Assistant,
            // the brain does not tell), for the home's "next reminder" card.
            if let Some(b) = &self.brain {
                b.ask("jobs.list");
            }
            self.screen.set_clock(&clock);
            self.night_sheets();
            if self.motion.borrow().loading_until.is_none() && !self.trimmed.replace(true) {
                give_back_memory();
            }
            self.screen
                .set_date(&self.locale.borrow().date(wday, day as u32, month));
            self.refresh_night_line();
            self.refresh_tiles();
        }
        // A page left alone goes home (not the setup page, not with a question open, not the
        // music page while there is music, not the board: like Home Assistant's view, it is
        // what the screen is for then).
        let page = self.screen.get_page();
        let touched = self.desk.borrow().touched;
        let music_on = self.desk.borrow().music.is_some();
        if page != "home"
            && !(page == "music" && music_on)
            && page != "setup"
            && page != "welcome"
            && page != "find-ha"
            && page != super::mirror::PAGE
            && page != "board"
            && !(page == "update" && self.update_going())
            && self.screen.get_dialog().is_empty()
            && touched.is_none_or(|t| now.duration_since(t) >= PAGE_IDLE)
        {
            self.screen.set_page("home");
        }
        self.tick_widget(now, new_minute);
        if self.view.get() == Phase::Ringing {
            self.refresh_ringing();
        }
        // The full-screen reminder until OK: it sounds again every NOTICE_AGAIN (not at night,
        // not over a conversation), bright for a while each time.
        let again = {
            let mut d = self.desk.borrow_mut();
            let night = d.night_now;
            match d.notice.as_mut() {
                Some(n)
                    if !night
                        && now.duration_since(n.sounded)
                            >= if now.duration_since(n.since) < NOTICE_AGAIN_FOR {
                                NOTICE_AGAIN
                            } else {
                                NOTICE_AGAIN_LATER
                            } =>
                {
                    n.sounded = now;
                    Some(now.duration_since(n.since))
                }
                _ => None,
            }
        };
        if let Some(age) = again
            && !self.view.get().in_conversation()
        {
            log::info!(event = "reminder_notice"; "reminder: sounding again after {} s", age.as_secs());
            self.wake(WakeSource::Conversation);
            self.send(UiEvent::Reminder);
        }
        let bright = self
            .desk
            .borrow()
            .notice
            .as_ref()
            .is_some_and(|n| now.duration_since(n.sounded) < NOTICE_BRIGHT);
        if bright {
            self.motion.borrow_mut().awake.wake(now);
        }
        let expired = self
            .desk
            .borrow()
            .last_answer
            .as_ref()
            .is_some_and(|(_, t)| now.duration_since(*t) >= ANSWER_CARD);
        if expired {
            self.desk.borrow_mut().last_answer = None;
            self.refresh_home();
        }
    }

    // ---- the desk ---------------------------------------------------------------------------

    /// Night mode now (the schedule) and whether its layout shows (the home, nobody awake);
    /// true when what is drawn must change.
    /// Decodes the night's animations a little before the night, frees them a little after it
    /// (once a minute).
    fn night_sheets(&self) {
        if self.opts.demo.is_some() || !self.opts.screen.animations {
            return;
        }
        let (on, start, end) = self.desk.borrow().night;
        let t = now_unix() as i64;
        let fixed = self.hour_for_tests.get();
        let hour =
            |dt: i64| fixed.unwrap_or_else(|| local_hm(t + dt).0[..2].parse::<u32>().unwrap_or(0));
        let want = [-NIGHT_MARGIN_S, 0, NIGHT_MARGIN_S]
            .iter()
            .any(|dt| is_night(hour(*dt), on, start, end));
        let mut m = self.motion.borrow_mut();
        let have = NIGHT_CLIPS.iter().any(|n| m.clips.contains_key(n));
        if want && !have && m.incoming.is_none() {
            let anims: Vec<Animation> = NIGHT_CLIPS
                .iter()
                .filter_map(|n| self.character.animation(n).cloned())
                .collect();
            if !anims.is_empty() {
                log::info!(event = "night_sheets", on = true; "night animations: decoding");
                m.incoming = Some(anim::load_in_background(anims, self.character.background));
            }
        } else if !want && have {
            for n in NIGHT_CLIPS {
                m.clips.remove(n);
                m.seq.remove(n);
            }
            drop(m);
            give_back_memory();
            log::info!(event = "night_sheets", on = false; "night animations freed");
        }
    }

    /// The start-up animation is over for good: its sheet goes (once its frame is off the
    /// screen the memory goes back too: [`give_back_memory`] at the next minute's tick).
    fn drop_loading_clip(m: &mut Motion) {
        if m.clips.remove("loading").is_some() {
            m.seq.remove("loading");
            log::info!("start-up animation freed");
        }
    }

    /// The camera tile: a photo in a thread (about 2 s), then shown over everything for
    /// [`SHOT_SHOWN`] in the camera's frame and kept in `<state>/photos` (Andrea, 2026-11-15).
    fn take_photo(self: &Rc<Self>) {
        if !self.camera || self.photo_rx.borrow().is_some() {
            return;
        }
        // The cover may have closed since the tiles were drawn: no black photo.
        if super::camera::lens_covered() == Some(true) {
            self.refresh_control();
            return;
        }
        let Some(dir) = self.opts.state_dir.as_ref().map(|d| d.join("photos")) else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("camera".into())
            .spawn(move || {
                let r =
                    super::camera::capture(&dir).and_then(|p| super::camera::preview(&p, 880, 660));
                let _ = tx.send(r);
            });
        if let Err(e) = spawned {
            log::warn!(event = "camera_photo"; "camera: {e}");
            return;
        }
        *self.photo_rx.borrow_mut() = Some(rx);
        log::info!(event = "camera_photo"; "camera: taking a photo");
        self.refresh_control();
        let ui = Rc::downgrade(self);
        self.photo_timer
            .start(TimerMode::Repeated, Duration::from_millis(100), move || {
                if let Some(ui) = ui.upgrade() {
                    ui.photo_poll();
                }
            });
    }

    fn photo_poll(self: &Rc<Self>) {
        let got = match self.photo_rx.borrow().as_ref().map(|r| r.try_recv()) {
            Some(Ok(r)) => r,
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) => return,
            _ => Err("the camera thread ended".into()),
        };
        *self.photo_rx.borrow_mut() = None;
        self.photo_timer.stop();
        self.refresh_control();
        self.wake(WakeSource::Touch);
        let l = self.locale.borrow();
        let (img, caption) = match got {
            Ok(buf) => {
                let hm = local_hm(self.now() as i64).0;
                (
                    Some(Image::from_rgba8(buf)),
                    l.text("photo_saved").replace("{t}", &hm),
                )
            }
            Err(e) => {
                log::warn!(event = "camera_photo"; "camera: {e}");
                (None, l.text("photo_failed").to_string())
            }
        };
        drop(l);
        self.screen.set_control_open(false);
        // Shown where the mirror was, the same frame (Andrea, 2026-11-15), then the mirror again.
        self.screen.set_camera_caption(&caption);
        match img {
            Some(img) => self.screen.set_camera(Some(img), "photo", ""),
            None => self.screen.set_camera(None, "failed", ""),
        }
        let ui = Rc::downgrade(self);
        self.photo_timer
            .start(TimerMode::SingleShot, SHOT_SHOWN, move || {
                if let Some(ui) = ui.upgrade()
                    && ui.screen.get_page() == "camera"
                {
                    ui.camera_start();
                }
            });
    }

    /// Settings → Info: what the check found and what the update service says.
    fn refresh_update(&self) {
        // The demo's sample stays (`--state update`).
        if self.opts.demo.is_some() {
            return;
        }
        let v = self.update_view();
        self.screen.set_update(&v);
    }

    fn update_view(&self) -> UpdateView {
        let l = self.locale.borrow();
        let current = env!("CARGO_PKG_VERSION");
        let Some(u) = &self.updater else {
            return UpdateView {
                facts: l.text("update_facts_off").replace("{v}", current),
                state: l.text("update_off").into(),
                progress: -1.0,
                ..Default::default()
            };
        };
        let st = u.state();
        let now = now_unix() as i64;
        let checked = match st.checked_at {
            Some(t) => {
                let (hm, day) = local_hm(t);
                if day == local_hm(now).1 {
                    l.text("update_today").replace("{t}", &hm)
                } else {
                    format!("{} {hm}", short_date(&l, t))
                }
            }
            None => l.text("update_never").into(),
        };
        let dir = self.opts.state_dir.clone();
        let service = dir.as_deref().and_then(crate::update::status);
        let requested = dir.as_deref().is_some_and(crate::update::requested);
        // What the service said in the last hour (a "done" stays a day).
        let recent = service.filter(|s| {
            let age = now - s.at;
            age < if s.phase == "done" { 86_400 } else { 3_600 }
        });
        let working = |s: &crate::update::Status| {
            matches!(
                s.phase.as_str(),
                "downloading" | "checking" | "installing" | "waiting"
            )
        };
        let busy = requested || recent.as_ref().is_some_and(working);
        // An install under way, or just over (ten minutes): its steps.
        let shown_steps = if requested {
            Some(None)
        } else {
            recent
                .as_ref()
                .filter(|s| now - s.at < 600 || working(s))
                .filter(|s| {
                    working(s) || matches!(s.phase.as_str(), "done" | "failed" | "rolled-back")
                })
                .map(Some)
        };
        let target = recent
            .as_ref()
            .map(|s| s.version.clone())
            .filter(|v| !v.is_empty())
            .or_else(|| st.latest.as_ref().map(|r| r.version.clone()))
            .unwrap_or_default();
        let (steps, progress) = match shown_steps {
            Some(s) => self.update_steps(&l, s, &target),
            None => (Vec::new(), -1.0),
        };
        let (state, attention) = match &recent {
            _ if requested => (l.text("update_progress").replace("{v}", &target), false),
            Some(s) if working(s) => (l.text("update_progress").replace("{v}", &s.version), false),
            // The service's reason is for the log (English, technical); the screen says what happened.
            Some(s) if s.phase == "failed" => (l.text("update_failed").into(), true),
            Some(s) if s.phase == "rolled-back" => (l.text("update_rolled_back").into(), true),
            Some(s) if s.phase == "done" && s.version == current => {
                (l.text("update_done").replace("{v}", current), false)
            }
            _ if st.checking => (l.text("update_checking").into(), false),
            _ if st.failed => (l.text("update_check_failed").into(), true),
            _ => match &st.latest {
                Some(r) if r.sha256.is_empty() => {
                    (l.text("update_no_sha").replace("{v}", &r.version), true)
                }
                Some(r) => (l.text("update_available").replace("{v}", &r.version), true),
                None if st.checked_at.is_some() => (l.text("update_none").into(), false),
                None => (String::new(), false),
            },
        };
        // Offered only with the hash to show (and to check against).
        let install = match &st.latest {
            Some(r) if !busy && !r.sha256.is_empty() => {
                l.text("update_install").replace("{v}", &r.version)
            }
            _ => String::new(),
        };
        let offered = st.latest.as_ref().filter(|_| !busy);
        let question = offered
            .map(|r| l.text("update_question").replace("{v}", &r.version))
            .unwrap_or_default();
        let facts = l
            .text("update_facts")
            .replace("{v}", current)
            .replace("{repo}", &u.repo)
            .replace("{when}", &checked);
        UpdateView {
            facts,
            state,
            notes: offered.map(|r| r.notes.clone()).unwrap_or_default(),
            sha: offered
                .filter(|r| !r.sha256.is_empty())
                .map(|r| crate::update::sha_groups(&r.sha256))
                .unwrap_or_default(),
            attention,
            install,
            busy,
            question,
            confirm: l.text("update_confirm").into(),
            steps,
            progress,
            prerelease: Some(u.prerelease()),
        }
    }

    /// The five steps of an install for the page, and the download's progress (-1: none).
    fn update_steps(
        &self,
        l: &Locale,
        s: Option<&crate::update::Status>,
        version: &str,
    ) -> (Vec<super::UpdateStepView>, f32) {
        use crate::update::StepState;
        let states = crate::update::steps(s);
        let mb = |b: u64| {
            format!("{:.1}", b as f64 / 1_000_000.0).replace('.', l.text("decimal_separator"))
        };
        let mut progress = -1.0;
        let rows = crate::update::STEPS
            .iter()
            .zip(states)
            .map(|(step, state)| {
                let label = match *step {
                    "downloading" => l.text("update_step_download").replace("{v}", version),
                    "checking" => l.text("update_step_check").to_string(),
                    "installing" => l.text("update_step_install").to_string(),
                    "waiting" => l.text("update_step_verify").to_string(),
                    _ => l.text("update_step_done").replace("{v}", version),
                };
                let detail = match (*step, state, s) {
                    ("downloading", StepState::Now, None) => {
                        l.text("update_step_starting").to_string()
                    }
                    ("downloading", StepState::Now, Some(st)) if st.total > 0 => {
                        progress = (st.bytes as f32 / st.total as f32).clamp(0.0, 1.0);
                        l.text("update_mb_of")
                            .replace("{a}", &mb(st.bytes))
                            .replace("{b}", &mb(st.total))
                    }
                    ("downloading", StepState::Now, Some(st)) => {
                        l.text("update_mb").replace("{a}", &mb(st.bytes))
                    }
                    ("installing", StepState::Now, _) => {
                        l.text("update_step_install_detail").to_string()
                    }
                    // A service before 0.3.0 does not say how long is left.
                    ("waiting", StepState::Now, Some(st)) if st.left > 0 => l
                        .text("update_step_verify_left")
                        .replace("{s}", &st.left.to_string()),
                    _ => String::new(),
                };
                super::UpdateStepView {
                    label,
                    detail,
                    state: match state {
                        StepState::Done => "done",
                        StepState::Now => "now",
                        StepState::Todo => "todo",
                        StepState::Failed => "failed",
                    },
                }
            })
            .collect();
        (rows, progress)
    }

    /// An install asked or under way (the Updates page does not go home by itself then).
    fn update_going(&self) -> bool {
        let Some(dir) = self.opts.state_dir.as_deref() else {
            return false;
        };
        crate::update::requested(dir)
            || crate::update::status(dir).is_some_and(|s| {
                matches!(
                    s.phase.as_str(),
                    "downloading" | "checking" | "installing" | "waiting"
                ) && now_unix() as i64 - s.at < 3_600
            })
    }

    /// How the last install ended, if it is recent and not acknowledged yet: a dialog with OK.
    fn update_result_check(&self) {
        let Some(dir) = self.opts.state_dir.as_deref() else {
            return;
        };
        let Some(s) = crate::update::status(dir) else {
            return;
        };
        if !matches!(s.phase.as_str(), "done" | "failed" | "rolled-back")
            || now_unix() as i64 - s.at > 86_400
        {
            return;
        }
        let key = format!("{} {} {}", s.phase, s.version, s.at);
        let seen =
            std::fs::read_to_string(crate::update::dir(dir).join("seen")).unwrap_or_default();
        if seen.trim() == key || *self.update_result_shown.borrow() == key {
            return;
        }
        if !self.screen.get_dialog().is_empty() {
            return;
        }
        let l = self.locale.borrow();
        let current = env!("CARGO_PKG_VERSION");
        if s.phase == "done" {
            let now = Instant::now();
            self.satisfied.set(Some((now, now + SATISFIED_FOR)));
        }
        let (title, text, ok) = match s.phase.as_str() {
            "done" => (
                l.text("update_result_done_title").to_string(),
                l.text("update_result_done_text").replace("{v}", current),
                true,
            ),
            "rolled-back" => (
                l.text("update_result_failed_title").to_string(),
                l.text("update_result_rolled_text")
                    .replace("{new}", &s.version)
                    .replace("{v}", current),
                false,
            ),
            _ => (
                l.text("update_result_failed_title").to_string(),
                l.text("update_result_failed_text").replace("{v}", current),
                false,
            ),
        };
        drop(l);
        log::info!(event = "update", phase = s.phase.as_str(); "update: the end said on the screen ({})", s.phase);
        *self.update_result_shown.borrow_mut() = key;
        self.wake(WakeSource::Touch);
        self.screen.show_update_result(&title, &text, ok);
        // Done: the character's bow and the success sound, as for a house command.
        if ok {
            self.motion.borrow_mut().seq.done(Instant::now());
            self.send(UiEvent::Success);
        }
    }

    /// OK on how the install ended: not said again, and home.
    fn update_result_seen(&self) {
        let key = self.update_result_shown.borrow().clone();
        if let Some(dir) = self.opts.state_dir.as_deref() {
            let _ = std::fs::write(crate::update::dir(dir).join("seen"), format!("{key}\n"));
        }
        if let Some(me) = self.this.borrow().upgrade() {
            me.screen.set_page("home");
            me.on_page("home");
        }
    }

    /// Install: the request to the root service (update.rs); the page follows its status.
    fn update_install(&self) {
        let (Some(u), Some(dir)) = (&self.updater, self.opts.state_dir.as_deref()) else {
            return;
        };
        let Some(r) = u.state().latest else {
            return;
        };
        match crate::update::request(dir, &r.tag, &r.sha256) {
            Ok(()) => {
                log::info!(event = "update", version = r.version.as_str(); "update: {} asked", r.version)
            }
            Err(e) => log::warn!(event = "update"; "update: asking for {}: {e}", r.version),
        }
        self.refresh_update();
    }

    /// The camera page (the control center's tile, "fammi una foto" with `countdown`).
    pub fn open_camera(self: &Rc<Self>, countdown: bool) {
        if !self.camera {
            return;
        }
        self.wake(WakeSource::Touch);
        self.screen.set_control_open(false);
        if self.screen.get_page() != "camera" {
            self.screen.set_page("camera");
            self.on_page("camera");
        }
        // Not yet: Alfred may still be answering, and the mirror is not on. camera_tick starts it
        // when both are true (Andrea, 2026-11-15: always see yourself first, always able to stop).
        self.cam_count_wanted.set(countdown);
    }

    /// The live picture on (or the cover's notice), with the page's timer.
    fn camera_start(self: &Rc<Self>) {
        self.cam_used.set(Instant::now());
        if self.cam_live.borrow().is_none() {
            if super::camera::lens_covered() == Some(true) {
                self.screen.set_camera(None, "covered", "");
            } else {
                match super::camera::Live::start() {
                    Ok(l) => {
                        *self.cam_live.borrow_mut() = Some(l);
                        self.screen.set_camera(None, "starting", "");
                    }
                    Err(e) => {
                        log::warn!(event = "camera_live"; "camera: {e}");
                        self.screen.set_camera(None, "starting", "");
                    }
                }
            }
        }
        let ui = Rc::downgrade(self);
        self.cam_timer
            .start(TimerMode::Repeated, Duration::from_millis(66), move || {
                if let Some(ui) = ui.upgrade() {
                    ui.camera_tick();
                }
            });
    }

    fn camera_stop(&self) {
        self.cam_timer.stop();
        self.cam_count.set(None);
        self.cam_count_wanted.set(false);
        if self.cam_live.borrow_mut().take().is_some() {
            self.cam_seq.set(0);
        }
    }

    /// Shoot (the button, or the voice): 3, 2, 1, then the photo. Pressed again during the
    /// countdown (the button says Cancel then): no photo.
    fn camera_countdown(self: &Rc<Self>) {
        if self.cam_count.get().is_some() {
            log::info!(event = "camera_photo"; "camera: countdown cancelled");
            self.cam_count.set(None);
            self.cam_used.set(Instant::now());
            return;
        }
        if self.photo_rx.borrow().is_some() {
            return;
        }
        self.cam_used.set(Instant::now());
        self.cam_count.set(Some((Instant::now(), COUNTDOWN + 1)));
    }

    /// 15 times a second while the page is there: the newest frame, the countdown, the cover.
    fn camera_tick(self: &Rc<Self>) {
        if self.screen.get_page() != "camera" {
            self.camera_stop();
            return;
        }
        let now = Instant::now();
        // A countdown asked by voice: once the conversation is over and the mirror has been on a
        // second.
        if self.cam_count_wanted.get()
            && matches!(self.view.get(), Phase::Home | Phase::Offline)
            && self.cam_seq.get() >= 12
        {
            self.cam_count_wanted.set(false);
            self.camera_countdown();
        }
        // The countdown: a number and a tick each second, then the photo.
        let mut count = String::new();
        if let Some((t0, last)) = self.cam_count.get() {
            let gone = now.duration_since(t0).as_secs();
            if gone >= COUNTDOWN {
                self.cam_count.set(None);
                self.camera_shoot();
                return;
            }
            let n = COUNTDOWN - gone;
            if n != last {
                self.cam_count.set(Some((t0, n)));
                self.send(UiEvent::CountdownTick);
            }
            count = n.to_string();
        }
        // The cover, once a second: closed, the live picture stops; opened, it starts.
        if self.cam_seq.get() % 15 == 0 || self.cam_live.borrow().is_none() {
            let covered = super::camera::lens_covered() == Some(true);
            if covered && self.cam_live.borrow().is_some() {
                self.cam_live.borrow_mut().take();
                self.cam_count.set(None);
            }
            if covered {
                self.screen.set_camera(None, "covered", "");
                self.cam_seq.set(self.cam_seq.get() + 1);
                return;
            }
            if self.cam_live.borrow().is_none() {
                self.cam_seq.set(0);
                if let Ok(l) = super::camera::Live::start() {
                    *self.cam_live.borrow_mut() = Some(l);
                }
                self.screen.set_camera(None, "starting", &count);
                return;
            }
        }
        let frame = self
            .cam_live
            .borrow()
            .as_ref()
            .and_then(|l| l.frame(self.cam_seq.get()));
        match frame {
            Some((n, buf)) => {
                self.cam_seq.set(n);
                self.screen
                    .set_camera(Some(Image::from_rgba8(buf)), "live", &count);
            }
            None if self.cam_seq.get() == 0 => self.screen.set_camera(None, "starting", &count),
            None => self.screen.set_camera(None, "live", &count),
        }
        // Left alone: home (the camera off).
        if self.cam_count.get().is_none() && now.duration_since(self.cam_used.get()) >= CAMERA_IDLE
        {
            log::info!(event = "camera_live"; "camera: page left alone, home");
            self.camera_stop();
            self.screen.set_page("home");
            self.on_page("home");
        }
    }

    /// The photo itself: the live picture stops (the camera is one), a flash, the full-size
    /// photo in its thread, then it is shown (photo_poll) and the live picture comes back.
    fn camera_shoot(self: &Rc<Self>) {
        self.cam_live.borrow_mut().take();
        self.cam_seq.set(0);
        self.cam_timer.stop();
        self.screen.set_camera(None, "taking", "");
        self.take_photo();
    }

    /// The gallery: the photos listed now, their thumbnails decoded in a thread.
    fn refresh_photos(self: &Rc<Self>) {
        let Some(dir) = self.opts.state_dir.as_ref().map(|d| d.join("photos")) else {
            return;
        };
        let list = super::camera::photos(&dir);
        *self.gallery.borrow_mut() = list.clone();
        self.screen.set_photo_thumbs(Vec::new());
        if list.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || {
                let thumbs = list
                    .iter()
                    .map(|p| {
                        super::camera::preview(p, 256, 192)
                            .unwrap_or_else(|_| SharedPixelBuffer::new(1, 1))
                    })
                    .collect();
                let _ = tx.send(thumbs);
            });
        if spawned.is_err() {
            return;
        }
        *self.thumbs_rx.borrow_mut() = Some(rx);
        let ui = Rc::downgrade(self);
        Timer::single_shot(Duration::from_millis(100), move || {
            if let Some(ui) = ui.upgrade() {
                ui.thumbs_poll();
            }
        });
    }

    fn thumbs_poll(self: &Rc<Self>) {
        let got = match self.thumbs_rx.borrow().as_ref().map(|r| r.try_recv()) {
            Some(Ok(t)) => Some(t),
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) => None,
            _ => {
                self.thumbs_rx.borrow_mut().take();
                return;
            }
        };
        match got {
            Some(t) => {
                self.thumbs_rx.borrow_mut().take();
                self.screen
                    .set_photo_thumbs(t.into_iter().map(Image::from_rgba8).collect());
            }
            None => {
                let ui = Rc::downgrade(self);
                Timer::single_shot(Duration::from_millis(100), move || {
                    if let Some(ui) = ui.upgrade() {
                        ui.thumbs_poll();
                    }
                });
            }
        }
    }

    /// One photo of the gallery on its page, with Delete.
    fn photo_open(self: &Rc<Self>, i: usize) {
        let Some(p) = self.gallery.borrow().get(i).cloned() else {
            return;
        };
        let Ok(buf) = super::camera::preview(&p, 880, 660) else {
            return;
        };
        let caption = p
            .file_stem()
            .map(|s| photo_caption(&s.to_string_lossy()))
            .unwrap_or_default();
        *self.viewing.borrow_mut() = Some(p);
        self.screen.set_photo(Image::from_rgba8(buf), &caption);
        self.screen.set_page("photo");
    }

    fn photo_delete(self: &Rc<Self>) {
        if let Some(p) = self.viewing.borrow_mut().take() {
            match std::fs::remove_file(&p) {
                Ok(()) => log::info!(event = "camera_photo"; "camera: {} deleted", p.display()),
                Err(e) => log::warn!(event = "camera_photo"; "camera: {}: {e}", p.display()),
            }
        }
        self.screen.set_page("photos");
        self.on_page("photos");
    }

    /// The control center comes down (the pull from the top, here or over "Casa").
    pub fn open_control(&self) {
        *self.wifi_name.borrow_mut() = wifi_ssid("wlan0");
        self.wake(WakeSource::Touch);
        self.screen.set_control_open(true);
        self.refresh_control();
    }

    /// The control center's tiles from what is true now.
    fn refresh_control(&self) {
        if self.opts.demo.is_some() {
            return;
        }
        let st = {
            let d = self.desk.borrow();
            super::control::ControlState {
                wifi: self.wifi_name.borrow().clone(),
                bluetooth: None,
                airplay: d.control_airplay.clone().map(|s| s.unwrap_or_default()),
                mic_on: d.mic_on,
                mic_switch_off: d.mic_switch_off,
                night_now: d.night_now,
                night_from: format!("{:02}:00", d.night.1),
                dnd: self.dnd_until_hm(),
                ha: self.ha_linked.get().then_some(true),
                camera: self.camera.then(|| {
                    use super::control::Camera;
                    if self.photo_rx.borrow().is_some() {
                        Camera::Busy
                    } else if super::camera::lens_covered() == Some(true) {
                        Camera::Covered
                    } else {
                        Camera::Ready
                    }
                }),
            }
        };
        let (auto, pct) = {
            let d = self.desk.borrow();
            (d.brightness_manual.is_none(), d.brightness_manual)
        };
        let pct = pct.unwrap_or_else(|| self.brightness_percent());
        let l = self.locale.borrow();
        let w = |k: &str| l.text(k).to_string();
        let tiles = super::control::tiles(&st, &w);
        self.screen.set_control(
            self.screen.get_control_open(),
            &tiles,
            pct,
            auto,
            &w("cc_auto"),
        );
    }

    /// The backlight now, as a percentage of the configured full level.
    fn brightness_percent(&self) -> u8 {
        let full = self.opts.screen.levels.active.max(1);
        let now = self.motion.borrow().backlight.current().unwrap_or(full);
        ((now as u64 * 100 / full as u64).clamp(5, 100)) as u8
    }

    /// The control center's Notte: the other one now, until the schedule agrees.
    fn toggle_night(&self) {
        {
            let mut d = self.desk.borrow_mut();
            let now = d.night_now;
            d.night_override = Some(!now);
            log::info!(event = "night", on = !now, by = "control"; "night mode {} (control center)", if now { "off" } else { "on" });
        }
        self.update_night(Instant::now());
        self.refresh_control();
        self.step();
    }

    fn update_night(&self, now: Instant) -> bool {
        let (on, start, end) = self.desk.borrow().night;
        let hour = self
            .hour_for_tests
            .get()
            .unwrap_or_else(|| local_now().3 as u32);
        let scheduled = self.opts.demo.is_none() && is_night(hour, on, start, end);
        let night = {
            let mut d = self.desk.borrow_mut();
            // The control center's choice lasts until the schedule says the same.
            if d.night_override == Some(scheduled) {
                d.night_override = None;
            }
            d.night_override.unwrap_or(scheduled)
        };
        let awake = {
            let m = self.motion.borrow();
            m.awake.is_awake(now) || m.loading_until.is_some()
        };
        let shown = night
            && !awake
            && matches!(self.view.get(), Phase::Home | Phase::Offline)
            && self.screen.get_page() == "home";
        let mut d = self.desk.borrow_mut();
        if self.opts.demo.is_some() {
            return false;
        }
        let changed = d.night_shown != shown;
        if d.night_now != night {
            log::info!(event = "night", on = night; "night mode {}", if night { "on" } else { "off" });
        }
        d.night_now = night;
        d.night_shown = shown;
        drop(d);
        if changed {
            self.refresh_night_line();
        }
        changed
    }

    fn refresh_night_line(&self) {
        let d = self.desk.borrow();
        if !d.night_shown {
            self.screen.set_night(false, "", false);
            return;
        }
        let now = crate::alarms::Now::at(self.now() as i64);
        let alarms = crate::alarms::Alarms::from_list(d.alarms.clone());
        if let Some((a, _)) = alarms.next_within(now, NIGHT_ALARM_MINUTES) {
            self.screen.set_night(true, &a.time(), true);
        } else if let Some(t) = d.timers.iter().find(|t| !t.paused) {
            self.screen
                .set_night(true, &format!("{} {}", t.name, t.left), false);
        } else {
            self.screen.set_night(true, "", false);
        }
    }

    /// A page was opened: its data, fresh.
    /// The setup page opened: listen for Home Assistant's answer and show the QR code.
    /// The Home Assistant button while Home Assistant is not linked (no token of either kind):
    /// the link page with its QR code instead of a view that cannot log in. True = handled.
    pub fn link_instead_of_home_assistant(&self) -> bool {
        if self.opts.demo.is_some() {
            return false;
        }
        if !self.ha_linked.get() {
            log::info!("Home Assistant button while not linked: the link page");
            self.screen.set_page("setup");
            return true;
        }
        // No view on this device (an image without WPE WebKit, `[web] enabled = false`): the
        // body's own Home Assistant page instead of a button that does nothing.
        if !super::webview::present() {
            log::info!("Home Assistant button without the view: the Home Assistant page");
            self.screen.set_page("ha");
            return true;
        }
        false
    }

    fn start_link(&self) {
        if self.link.borrow().is_some() || self.opts.demo.is_some() {
            return;
        }
        *self.wifi_name.borrow_mut() = wifi_ssid("wlan0");
        self.refresh_desk();
        let name = self.character.name.clone();
        let (setup, no_url) = {
            let l = self.locale.borrow();
            let setup = self.opts.link.clone().map(|mut s| {
                s.name = name.clone();
                s.done_text = word(&l, "link_page_done", &name);
                s.failed_text = word(&l, "link_page_failed", &name);
                s
            });
            (setup, word(&l, "setup_no_url", &name))
        };
        let linked = self.ha_linked.get();
        let Some(setup) = setup else {
            self.screen.set_setup_link(None, &no_url, false);
            return;
        };
        match crate::ha::link::Link::start(setup) {
            Ok(link) => {
                let qr = qr_image(&link.authorize_url);
                let status = if linked {
                    word(&self.locale.borrow(), "setup_already", &name)
                } else {
                    String::new()
                };
                self.screen.set_setup_link(qr, &status, false);
                *self.link.borrow_mut() = Some(link);
            }
            Err(e) => {
                log::warn!(event = "ha_link"; "Home Assistant link: {e}");
                let l = self.locale.borrow();
                self.screen
                    .set_setup_link(None, &word(&l, "setup_cannot", &name), false);
            }
        }
    }

    /// The page the first start opens with, if any (the device only): the language never
    /// chosen, then Home Assistant never found (until "Più tardi"), then the QR link (until
    /// "Più tardi" or linked).
    fn first_page(&self) -> Option<&'static str> {
        if !self.opts.setup_at_start {
            return None;
        }
        if self.language_pending.get() {
            return Some("welcome");
        }
        let later = self.settings.as_ref().is_some_and(Settings::setup_later);
        if later || self.ha_linked.get() {
            return None;
        }
        Some(if self.opts.first_start.home_assistant {
            "find-ha"
        } else {
            "setup"
        })
    }

    /// The start-up animation stops at once (a first-start page needs the desk now).
    fn end_loading(&self) {
        {
            let mut m = self.motion.borrow_mut();
            if m.loading_until.take().is_none() {
                return;
            }
            Self::drop_loading_clip(&mut m);
        }
        self.wake(WakeSource::Start);
        self.show(self.view.get());
    }

    /// The welcome page's languages: every locale file, each named in itself, and "Welcome"
    /// in each of them.
    fn set_welcome(&self) {
        let rows: Vec<(String, String, String)> = locale::available(&self.opts.locales)
            .into_iter()
            .filter_map(|code| {
                let l = locale::load(&self.opts.locales, &code)
                    .map_err(|e| log::warn!("locale {code}: {}", e.join("; ")))
                    .ok()?;
                Some((code, l.name.clone(), l.text("first_welcome").to_string()))
            })
            .collect();
        let title = welcome_title(rows.iter().map(|r| r.2.as_str()));
        let rows: Vec<(String, String)> = rows.into_iter().map(|(c, n, _)| (c, n)).collect();
        self.screen.set_welcome(&title, &rows);
    }

    /// Looks for Home Assistant on the network (its own thread; [`Ui::poll_find`] shows it).
    fn start_find(&self) {
        if self.opts.demo.is_some() {
            log::info!("demo: Search again");
            return;
        }
        if self.find.borrow().is_some() {
            return;
        }
        self.screen.set_find("searching", &[], "");
        *self.find.borrow_mut() = Some(crate::ha::discover::spawn());
    }

    fn poll_find(&self) {
        let outcome = match self.find.borrow().as_ref().map(Receiver::try_recv) {
            Some(Ok(o)) => o,
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => return,
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                crate::ha::discover::Outcome::Failed("no answer".into())
            }
        };
        self.find.borrow_mut().take();
        let l = self.locale.borrow();
        let (state, rows, line) = find_view(&outcome, &|k| word(&l, k, &self.character.name));
        drop(l);
        self.screen.set_find(state, &rows, &line);
        self.wake(WakeSource::Setup);
    }

    /// One Home Assistant picked: saved in the settings, a moment of "saving", then the body
    /// starts again with it (main.rs) and the QR link page follows.
    fn pick_home_assistant(&self, url: &str) {
        log::info!(event = "ha_picked", url = url; "first start: Home Assistant {url} picked");
        let Some(s) = &self.settings else {
            log::info!("demo: nothing saved");
            return;
        };
        let l = self.locale.borrow().clone();
        if let Err(e) = s.save_ha_url(url) {
            log::warn!("{}: {e}", s.path.display());
            let line = l.text("find_failed").replace("{error}", &e.to_string());
            self.screen.set_find("none", &[], &line);
            return;
        }
        s.clear_setup_later();
        self.screen.set_find(
            "saving",
            &[],
            &word(&l, "find_saving", &self.character.name),
        );
        self.restart_timer
            .start(TimerMode::SingleShot, Duration::from_millis(1500), || {
                crate::signals::request_restart();
            });
    }

    /// The link's news (from [`Ui::poll`]).
    fn poll_link(&self) {
        // The QR code is being scanned with the phone: no touch for a while, but the user is
        // looking at it. Kept bright (every few seconds, not every poll).
        if self.link.borrow().is_some()
            && self
                .ha_keep
                .get()
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.ha_keep.set(Some(Instant::now()));
            self.wake(WakeSource::Setup);
        }
        // "Linked" stays on screen a moment, then "opening Home Assistant…" until its view is
        // ready and slides in over this page (webview.rs).
        if self
            .link_done
            .get()
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(1500))
        {
            self.link_done.set(None);
            if self.screen.get_page() == "setup"
                && self.ha_setup.borrow().is_some()
                && !self.ha_configured()
            {
                // First link: Home Assistant is set up for the body, with the user's yes.
                self.ha_page.borrow_mut().after_link = true;
                self.screen.set_page("ha");
            } else if self.screen.get_page() == "setup" && super::webview::present() {
                let l = self.locale.borrow();
                self.screen.set_setup_link(
                    None,
                    &word(&l, "setup_opening", &self.character.name),
                    true,
                );
                drop(l);
                super::webview::request_open();
            }
        }
        let ev = match self.link.borrow().as_ref() {
            Some(l) => l.poll(),
            None => return,
        };
        let name = self.character.name.clone();
        match ev {
            Some(crate::ha::link::LinkEvent::Linked) => {
                self.link.borrow_mut().take();
                self.wake(WakeSource::Setup);
                self.ha_linked.set(true);
                let l = self.locale.borrow();
                self.screen
                    .set_setup_link(None, &word(&l, "setup_linked", &name), true);
                drop(l);
                self.refresh_settings();
                super::webview::request_login();
                self.link_done.set(Some(Instant::now()));
                self.send(UiEvent::Success);
            }
            Some(crate::ha::link::LinkEvent::Failed(_)) => {
                let l = self.locale.borrow();
                self.screen
                    .set_setup_link(None, &word(&l, "setup_failed", &name), false);
            }
            None => {}
        }
    }

    fn on_page(self: &Rc<Self>, page: &str) {
        // Before anything is drawn: the remote screen's capture learns what may be sent.
        self.sync_remote();
        // The link pages have their pose ("plug it in").
        self.refresh_home_pose();
        if self
            .back_pending
            .take()
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            self.send(if page == "home" {
                UiEvent::Home
            } else {
                UiEvent::Tap
            });
        }
        self.page_motion();
        if page == "setup" && self.opts.first_start.home_assistant && self.opts.demo.is_none() {
            // No address to link to yet: find Home Assistant first (Settings → Home Assistant,
            // the HA button, the control center all land here).
            self.screen.set_page("find-ha");
            return;
        }
        if page == "find-ha" && self.find.borrow().is_none() && self.opts.demo.is_none() {
            self.start_find();
        }
        if page == "setup" {
            self.start_link();
        } else if self.link.borrow().is_some() {
            // Leaving the page closes the port.
            self.link.borrow_mut().take();
        }
        if page == "board" {
            // Its page as it is now (it waited while it was not seen), and the way home.
            self.render_board(false);
            self.board_home_show();
        } else if page == "home" {
            self.board_seen_again();
        }
        if page == "poses" {
            self.poses_refresh();
        } else if page == "settings" {
            // The row's count of installed poses.
            self.render_poses();
        }
        if page == "update" {
            self.refresh_update();
            let ui = Rc::downgrade(self);
            self.update_timer
                .start(TimerMode::Repeated, Duration::from_secs(1), move || {
                    if let Some(ui) = ui.upgrade() {
                        if ui.screen.get_page() == "update" {
                            ui.refresh_update();
                        } else {
                            ui.update_timer.stop();
                        }
                    }
                });
        }
        // The camera runs only on its page.
        if page == "camera" {
            self.camera_start();
        } else {
            self.camera_stop();
        }
        if page == "photos" {
            self.refresh_photos();
        }
        match page {
            "chat" => self.refresh_chat(),
            "timers" => self.refresh_timers(),
            "settings" | "screen" | "widgets" | "panel" | "language" => self.refresh_settings(),
            "altro" => self.refresh_tiles(),
            "memories" => self.refresh_memories(),
            "what" => self.refresh_what(),
            "ha" => {
                self.ha_send(crate::ha::setup::SetupCmd::Check, false);
                self.tick_usage(true);
            }
            "usage" => self.tick_usage(true),
            "trends" => self.refresh_trends(),
            "city" => self.refresh_city(),
            _ => {}
        }
        // The mirroring card is there only while its page is not.
        if self.desk.borrow().mirror.is_some() {
            self.refresh_home();
        }
        if page == "home" && self.update_night(Instant::now()) {
            self.show(self.view.get());
        }
    }

    /// Everything on the desk (start, language, a setting).
    fn refresh_desk(&self) {
        self.refresh_home();
        self.refresh_timers();
        self.refresh_chat();
        self.refresh_settings();
        self.refresh_tiles();
        self.refresh_ringing();
        self.render_usage();
        let l = self.locale.borrow();
        let wifi = std::fs::read_to_string("/sys/class/net/wlan0/operstate")
            .map(|s| s.trim() == "up")
            .unwrap_or(false);
        if self.opts.demo.is_none() {
            let ha = self
                .opts
                .ha_host
                .as_deref()
                .map_or(String::new(), |h| l.text("setup_ha").replace("{host}", h));
            let wifi = match (wifi, self.wifi_name.borrow().as_deref()) {
                (true, Some(name)) => l.text("wifi_named").replace("{ssid}", name),
                (true, None) => l.text("wifi_on").to_string(),
                (false, _) => l.text("wifi_off").to_string(),
            };
            self.screen.set_setup(&wifi, &ha);
        }
    }

    /// Timers without the one waiting for its undo chip.
    fn visible_timers(&self) -> Vec<TimerView> {
        let d = self.desk.borrow();
        d.timers
            .iter()
            .filter(|t| !matches!(&d.undo, Some(Undo::Timer(id)) if *id == t.id))
            .cloned()
            .collect()
    }

    /// The pose of Home Assistant's cards (`pose` of `home_companion_card`, the last card that
    /// has one): a downloaded pose of that name first, else the character's own of the state of
    /// that name, instead of the moment's pose while such a card shows (by day; not on the
    /// settings and More pages). A name it has neither of is as no pose.
    fn render_card_pose(&self) {
        let want = self.desk.borrow().ha_cards.pose().map(String::from);
        let look = want.and_then(|name| {
            let downloaded = {
                let mut cp = self.card_pose.borrow_mut();
                cp.1.entry(name.clone())
                    .or_insert_with(|| self.load_downloaded(&name))
                    .is_some()
            };
            let look = card_look_of(&name, downloaded, |s| self.character.has_pose(s));
            if look.is_none() {
                log::debug!("card pose {name:?}: no such pose");
            }
            look
        });
        if self.card_pose.borrow().0 == look {
            return;
        }
        if let Some(name) = look.as_ref().map(Look::name) {
            log::info!(event = "card_pose", pose = name; "card pose: {name}");
        }
        self.load_pose_animation(match &look {
            Some(Look::Downloaded(n)) => Some(n.as_str()),
            _ => None,
        });
        self.card_pose.borrow_mut().0 = look;
        if self.view.get() == Phase::Home && self.motion.borrow().loading_until.is_none() {
            if self.opts.demo.is_some() {
                self.set_pose(&self.screen, State::Idle);
            } else {
                self.show_home_pose();
            }
        }
    }

    /// The picture of the downloaded pose `name`, if there is one (drawn with idle's crop: it
    /// stands where the character does).
    fn load_downloaded(&self, name: &str) -> Option<Image> {
        let path = self.downloaded_dir()?.join(format!("{name}.png"));
        if !path.is_file() {
            return None;
        }
        let (_, crop) = self.character.pose(State::Idle);
        load_pose(&path, crop, self.character.background)
            .map_err(|e| log::warn!("card pose {name:?}: {e}"))
            .ok()
    }

    /// The animation of a downloaded pose (`<name>.toml` next to its picture, an animation table
    /// as in character.toml without its name, its `file` in the same directory): the old pose's
    /// sheet goes, the new one is decoded in the background and loops over the pose once it is
    /// there.
    fn load_pose_animation(&self, name: Option<&str>) {
        let mut m = self.motion.borrow_mut();
        m.pose_incoming = None;
        if m.clips.remove(anim::CARD_POSE).is_some() {
            m.seq.remove(anim::CARD_POSE);
            if matches!(m.shown, Show::Frame { clip, .. } if clip == anim::CARD_POSE) {
                m.shown = Show::Pose;
                m.playhead = None;
                self.screen.set_frame(Image::default());
            }
        }
        let Some(name) = name else { return };
        if self.opts.demo.is_some() || !self.opts.screen.animations {
            return;
        }
        let Some(dir) = self.downloaded_dir() else {
            return;
        };
        let path = dir.join(format!("{name}.toml"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return; // a still pose
        };
        match pose_animation(&text, &dir) {
            Ok(a) => {
                log::info!("pose {name:?}: animation {}", a.file.display());
                m.pose_incoming =
                    Some(anim::load_in_background(vec![a], self.character.background));
            }
            Err(e) => log::warn!("{}: {e} (the pose stays still)", path.display()),
        }
    }

    /// Settings → Poses opened, or "Look again": the catalogue is read again.
    fn poses_refresh(&self) {
        let Some((tx, _)) = &self.pose_downloads else {
            self.render_poses();
            return;
        };
        {
            let mut v = self.pose_view.borrow_mut();
            v.catalogue = None;
            v.failed = None;
        }
        let _ = tx.send(crate::poses::Cmd::Refresh);
        self.render_poses();
    }

    /// A pose's switch: download it (on) or take it away (off).
    fn pose_toggled(&self, name: &str, on: bool) {
        let Some((tx, _)) = &self.pose_downloads else {
            return;
        };
        if self.pose_view.borrow().busy.is_some() {
            return;
        }
        {
            let mut v = self.pose_view.borrow_mut();
            v.failed = None;
            // The turning sign at once, before the thread says so.
            if on {
                v.busy = Some(name.to_string());
            }
        }
        let _ = tx.send(if on {
            crate::poses::Cmd::Install(name.to_string())
        } else {
            crate::poses::Cmd::Remove(name.to_string())
        });
        self.render_poses();
    }

    /// What the downloads' thread says.
    fn poll_poses(&self) {
        let Some((_, rx)) = &self.pose_downloads else {
            return;
        };
        let mut changed = Vec::new();
        let mut any = false;
        while let Ok(m) = rx.borrow().try_recv() {
            any = true;
            let mut v = self.pose_view.borrow_mut();
            match m {
                crate::poses::Msg::Catalogue(c) => v.catalogue = Some(c),
                crate::poses::Msg::Busy(b) => v.busy = b,
                crate::poses::Msg::Changed(n) => changed.push(n),
                crate::poses::Msg::Failed(n, why) => v.failed = Some((n, why)),
            }
        }
        for n in changed {
            self.pose_files_changed(&n);
        }
        if any {
            self.render_poses();
        }
    }

    /// A downloaded pose came or went: read again, and shown again if a card asks for it.
    fn pose_files_changed(&self, name: &str) {
        {
            let mut cp = self.card_pose.borrow_mut();
            cp.1.remove(name);
            if cp.0.as_ref().is_some_and(|l| l.name() == name) {
                // Taken again by render_card_pose (the new picture, or the character's own).
                cp.0 = None;
            }
        }
        self.render_card_pose();
    }

    /// Settings → Poses and its row: the catalogue with a switch per pose, what is going on,
    /// how many are installed.
    fn render_poses(&self) {
        if self.opts.demo.is_some() {
            return self.render_poses_demo();
        }
        let l = self.locale.borrow();
        let lang = l.code.clone();
        let dir = self.downloaded_dir();
        let installed = |n: &str| {
            dir.as_deref()
                .is_some_and(|d| crate::poses::installed(d, n))
        };
        let count = dir
            .as_deref()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|it| {
                it.filter_map(Result::ok)
                    .filter(|e| {
                        let n = e.file_name().to_string_lossy().to_string();
                        n.ends_with(".png") && !n.ends_with("-anim.png")
                    })
                    .count()
            })
            .unwrap_or(0);
        self.screen.set_poses_value(&if count > 0 {
            l.text("poses_installed").replace("{n}", &count.to_string())
        } else {
            String::new()
        });
        let v = self.pose_view.borrow();
        let poses = match &v.catalogue {
            Some(Ok(s)) => s.as_slice(),
            _ => &[],
        };
        let title = |n: &str| {
            poses
                .iter()
                .find(|s| s.name == n)
                .map(|s| s.title(&lang))
                .unwrap_or_else(|| n.to_string())
        };
        let mb = |bytes: u64| format!("{:.1} MB", bytes as f64 / 1_048_576.0);
        let mut rows: Vec<super::PoseRowData> = poses
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.title(&lang),
                    mb(s.size()),
                    installed(&s.name),
                    v.busy.as_deref() == Some(s.name.as_str()),
                )
            })
            .collect();
        // Installed poses the catalogue no longer lists (renamed, withdrawn): still there to take
        // away. Only against a catalogue that was read: while looking, every pose is unknown.
        if let (Some(Ok(_)), Some(d)) = (&v.catalogue, dir.as_deref()) {
            let known: Vec<&str> = poses.iter().map(|s| s.name.as_str()).collect();
            for (name, size) in crate::poses::unlisted(d, &known) {
                let value = l.text("poses_unlisted").replace("{size}", &mb(size));
                rows.push((name.clone(), name, value, true, false));
            }
        }
        self.screen.set_pose_rows(&rows);
        self.screen.set_poses_busy(v.busy.is_some());
        let state = if let Some(b) = &v.busy {
            l.text("poses_busy").replace("{title}", &title(b))
        } else if let Some((n, why)) = &v.failed {
            l.text("poses_failed_one")
                .replace("{title}", &title(n))
                .replace("{why}", why)
        } else {
            match &v.catalogue {
                None if self.pose_downloads.is_some() => l.text("poses_loading").to_string(),
                Some(Err(_)) => l.text("poses_failed").to_string(),
                Some(Ok(s)) if s.is_empty() => l.text("poses_none").to_string(),
                None => l.text("poses_none").to_string(),
                Some(Ok(_)) => String::new(),
            }
        };
        self.screen.set_poses_state(&state);
    }

    /// `--state poses`: the page on sample data (two of four installed, one downloading).
    fn render_poses_demo(&self) {
        let l = self.locale.borrow();
        let rows: Vec<super::PoseRowData> = [
            ("laundry", "Laundry", "7.2 MB", true, false),
            ("barbecue", "Barbecue", "4.1 MB", false, true),
            ("waste", "Rubbish out", "5.3 MB", true, false),
            ("vacuum", "Robot vacuum", "4.4 MB", false, false),
        ]
        .iter()
        .map(|(n, t, v, on, busy)| (n.to_string(), t.to_string(), v.to_string(), *on, *busy))
        .collect();
        self.screen.set_pose_rows(&rows);
        self.screen.set_poses_busy(true);
        self.screen
            .set_poses_state(&l.text("poses_busy").replace("{title}", "Barbecue"));
        self.screen
            .set_poses_value(&l.text("poses_installed").replace("{n}", "2"));
    }

    /// Where the downloaded poses are (Settings → Poses): `<state_dir>/poses/<character>/`.
    fn downloaded_dir(&self) -> Option<PathBuf> {
        let state = self.opts.state_dir.as_ref()?;
        Some(crate::poses::dir(state, &self.character.id))
    }

    fn refresh_home(&self) {
        self.render_card_pose();
        if self.opts.demo.as_ref().is_some_and(|d| d.cards) {
            return;
        }
        let l = self.locale.borrow();
        let w = |k: &str| l.text(k).to_string();
        let timers = self.visible_timers();
        let on = self.widget_on.borrow().clone();
        let d = self.desk.borrow();
        let offline =
            self.view.get() == Phase::Offline && self.motion.borrow().loading_until.is_none();
        let of = |i: usize, n: usize| {
            w("of")
                .replace("{i}", &i.to_string())
                .replace("{n}", &n.to_string())
        };
        let more = |n: usize| w("more_timers").replace("{n}", &n.to_string());
        let cw = CardWords {
            of: &of,
            more_timers: &more,
            paused: l.text("paused"),
        };
        let reminders: Vec<(String, String)> = d
            .reminders
            .iter()
            .map(|(_, t, x)| (t.clone(), x.clone()))
            .collect();
        let shopping = d
            .shopping
            .as_ref()
            .filter(|v| lists::count(v) > 0)
            .map(|v| {
                let n = lists::count(v);
                let more = if n > 3 { " …" } else { "" };
                (
                    w("shopping_card").replace("{n}", &n.to_string()),
                    format!("{}{more}", lists::first_texts(v, 3).join(" · ")),
                )
            });
        let now = self.now() as i64; // the demo clock in screenshots
        // The next reminder or calendar event, whichever comes first.
        let next = next::merge(&d.upcoming, &d.events, now);
        let upcoming = next.first().map(|n| {
            let (hm, day) = local_hm(n.at);
            let today = local_hm(now).1;
            let when = match (day - today, n.all_day) {
                (0, true) => w("when_allday_today"),
                (1, true) => w("when_allday_tomorrow"),
                (_, true) => short_date(&l, n.at),
                (0, false) => w("when_today").replace("{t}", &hm),
                (1, false) => w("when_tomorrow").replace("{t}", &hm),
                _ => w("when_day")
                    .replace("{d}", &short_date(&l, n.at))
                    .replace("{t}", &hm),
            };
            let more = if next.len() > 1 {
                format!(" (+{})", next.len() - 1)
            } else {
                String::new()
            };
            let key = if n.event {
                "next_event"
            } else {
                "next_reminder"
            };
            (
                format!("{}{more}", w(key).replace("{when}", &when)),
                n.text.clone(),
            )
        });
        let ha_cards = d.ha_cards.views(l.text("ha_card_title"));
        let alarm = crate::alarms::Alarms::from_list(d.alarms.clone())
            .next_within(crate::alarms::Now::at(now), 24 * 60)
            .map(|(a, minutes)| {
                let at = now + minutes as i64 * 60;
                let when = if local_hm(at).1 == local_hm(now).1 {
                    w("when_today").replace("{t}", &a.time())
                } else {
                    w("when_tomorrow").replace("{t}", &a.time())
                };
                // "Una volta" says nothing on a card: only repeating alarms show their days.
                let days = if a.days == 0 {
                    String::new()
                } else {
                    crate::alarms::days_text(a.days, &l.days, |k| l.text(k).to_string())
                };
                (w("next_alarm").replace("{when}", &when), days)
            });
        // The setup in Home Assistant needs the user (found by the check by itself): "Complete"
        // when an AI appeared since, otherwise something to fix. Not while offline.
        let setup = {
            let p = self.ha_page.borrow();
            (p.attention && self.ha_linked.get() && !offline).then(|| {
                let complete = p.rows.iter().any(|r| r.key == "ha_agent_missing");
                let k = if complete {
                    "setup_card_complete"
                } else {
                    "setup_card_fix"
                };
                let name = &self.character.name;
                (
                    word(&l, &format!("{k}_title"), name),
                    word(&l, &format!("{k}_text"), name),
                )
            })
        };
        // The way back to a mirrored screen, while the user is elsewhere.
        let mirror = d
            .mirror
            .as_ref()
            .filter(|_| self.screen.get_page() != super::mirror::PAGE)
            .map(|i| super::mirror::card(l.text("mirror"), l.text("mirror_back"), i));
        let ready = d.setup_ready.then(|| (w("ready_title"), w("ready_text")));
        // A house command that went through, after its pop-up: the first card.
        let done = {
            let h = self.house.borrow();
            h.done.as_ref().filter(|_| !h.popup()).map(|c| {
                let (line, _, _) = c.texts(l.text("house_and_more"), l.text("house_others"));
                let (name, state) = house::split_line(&line);
                (name.to_string(), state.to_string(), c.undo)
            })
        };
        let alive = Alive {
            done,
            status: offline.then(|| (w("offline_title"), w("offline_text"))),
            setup,
            panel: self.panel_card(),
            ready,
            mirror,
            alarm,
            upcoming,
            shopping,
            messages: &reminders,
            ha: &ha_cards,
            timers: &timers,
            // The sound of a mirrored screen is not music: no card for it.
            music: d.music.as_ref().filter(|_| d.mirror.is_none()).map(|m| {
                (
                    m.title.clone(),
                    if m.artist.is_empty() {
                        m.source.clone()
                    } else {
                        format!("{} · {}", m.artist, m.source)
                    },
                    true,
                )
            }),
            answer: d
                .last_answer
                .as_ref()
                .map(|(a, _)| (w("last_answer"), a.clone())),
            next_in_widget: on.contains(&WidgetKind::Next.into()),
            shopping_in_widget: on.contains(&WidgetKind::Shopping.into()),
        };
        let picked = cards::pick_n(&alive, &cw, self.max_cards());
        *self.card_kinds.borrow_mut() = picked.iter().map(|c| c.kind).collect();
        self.screen.set_cards(&picked);
        drop(d);
        drop(l);
        self.refresh_widget();
    }

    /// The widget's pages from the data as it is now, offered to the turn with the slots the
    /// cards leave free (the page on screen keeps its data until it comes back: widgets::Band).
    fn refresh_widget(&self) {
        let on = self.widget_on.borrow().clone();
        // Full or one line, never between (widgets::size).
        let slots = if on.is_empty() {
            0
        } else {
            widgets::size(
                self.max_cards()
                    .saturating_sub(self.card_kinds.borrow().len()),
            )
        };
        let l = self.locale.borrow();
        let d = self.desk.borrow();
        let now = self.now() as i64;
        let next = next::merge(&d.upcoming, &d.events, now);
        let today = if self.opts.demo.is_some() {
            DEMO_DATE.to_string()
        } else {
            crate::ha::local_date()
        };
        let (columns, hero) = self
            .weather
            .borrow()
            .as_ref()
            .map(|w| {
                (
                    super::weather::widget_columns(w, &l, &today, now, &|t| local_hm(t).0),
                    Some(super::weather::hero(w, &l, &today, now, &local_hm)),
                )
            })
            .unwrap_or_default();
        let words = widget_words(&l);
        if !self.city_weather.borrow().is_empty() {
            self.widget_data.borrow_mut().cities = self.city_rows(now);
        }
        let picks = self.widget_picks(&l, &|id: &str| id.to_string());
        let custom = self.custom_pages();
        let data = self.widget_data.borrow();
        let sources = widgets::Sources {
            next: &next,
            weather: columns,
            weather_hero: hero,
            shopping: d.shopping.as_deref(),
            quotes: &data.quotes,
            cities: &data.cities,
            notes: &data.notes,
            sensors: &data.sensors,
            check: &data.check,
            playing: data.playing.as_ref(),
            people: &data.people,
            custom: &custom,
            picks: &picks,
        };
        let mut pages = widgets::pages(&sources, &on, &words, now, &local_hm);
        // The markets in the order chosen, every one of them: one whose price has not come yet
        // (after a start, the sources answer one at a time) waits with "…" instead of missing.
        for p in pages.iter_mut().filter(|p| p.kind == WidgetKind::Quotes) {
            if let Some(pk) = picks.iter().find(|x| x.page == p.page()) {
                *p = widgets::waiting(p.page(), Some(p.clone()), pk, &words);
            }
        }
        drop(data);
        drop(d);
        drop(l);
        let shown = self.widget.borrow_mut().offer(pages, slots, Instant::now());
        if let Some(s) = shown {
            self.show_widget(&s);
        }
    }

    /// What each page of the kinds that repeat shows, in `l`'s words: its items (keys and
    /// names; `sensor_name` names an entity) and its title.
    pub(super) fn widget_picks(
        &self,
        l: &Locale,
        sensor_name: &dyn Fn(&str) -> String,
    ) -> Vec<widgets::Picks> {
        // The demos draw their sample data as it is, chosen by nobody.
        if self.opts.demo.is_some() {
            return Vec::new();
        }
        widgets::picks_of(
            &self.widget_order.borrow(),
            &widgets::AllLists {
                quotes: &self.widget_quotes.borrow(),
                cities: &self.widget_cities.borrow(),
                sensors: &self.widget_sensors.borrow(),
                names: &self.widget_names.borrow(),
            },
            sensor_name,
            &|k| l.text(k).to_string(),
        )
    }

    /// Settings → Screen → Widgets as they are.
    fn refresh_widget_settings(&self) {
        let l = self.locale.borrow();
        let on = self.widget_on.borrow();
        let summary = if on.is_empty() {
            l.text("widgets_off").to_string()
        } else {
            l.text("widgets_on").replace("{n}", &on.len().to_string())
        };
        let names = self.widget_names.borrow();
        let pages: Vec<widgets::PageSetting> = self
            .widget_order
            .borrow()
            .iter()
            .map(|k| widgets::PageSetting {
                page: *k,
                label: self
                    .custom_name(*k)
                    .unwrap_or_else(|| model::page_label(*k, &names, &|w| l.text(w).to_string())),
                on: on.contains(k),
                hint: self.widget_hint(*k, &l),
            })
            .collect();
        let every = self.widget_every.get().map_or(0, |d| d.as_secs() as u32);
        self.screen.set_widget_settings(&pages, every, &summary);
    }

    /// The line under a page in Settings → Screen → Widgets: what it shows is chosen from the
    /// browser (and how many are chosen).
    fn widget_hint(&self, page: WidgetPage, l: &Locale) -> String {
        let kind = page.kind;
        let count = match kind {
            WidgetKind::Quotes | WidgetKind::Cities | WidgetKind::Sensors => {
                Some(self.widget_chosen(page))
            }
            WidgetKind::Check | WidgetKind::Playing | WidgetKind::Home | WidgetKind::Custom => None,
            WidgetKind::Notes => return l.text("widgets_hint_notes").to_string(),
            _ => return String::new(),
        };
        let browser = l.text("widgets_hint_browser");
        match count {
            Some(n) if n > 0 => format!(
                "{} · {browser}",
                l.text(&format!("widgets_hint_{}", kind.key()))
                    .replace("{n}", &n.to_string())
            ),
            _ => browser.to_string(),
        }
    }

    /// How many items of a page are chosen from the browser (markets, cities, sensors).
    fn widget_chosen(&self, page: WidgetPage) -> usize {
        match page.kind {
            WidgetKind::Quotes => self.widget_quotes.borrow().get(page.n).len(),
            WidgetKind::Cities => self.widget_cities.borrow().get(page.n).len(),
            WidgetKind::Sensors => self.widget_sensors.borrow().get(page.n).len(),
            _ => 0,
        }
    }

    /// A switch of Settings → Screen → Widgets: the page joins or leaves the turn (and Next and
    /// Shopping go back to being cards when theirs is off); saved.
    fn widget_toggled(&self, key: &str, on: bool) {
        let Some(kind) = WidgetPage::from_key(key) else {
            return;
        };
        let list: Vec<WidgetPage> = {
            let now = self.widget_on.borrow();
            self.widget_order
                .borrow()
                .iter()
                .copied()
                .filter(|k| if *k == kind { on } else { now.contains(k) })
                .collect()
        };
        log::info!(event = "widget_settings", page = key, on = on; "widget: {key} {}", if on { "on" } else { "off" });
        model::commit(self, Change::Widgets(list, self.widget_every_secs()));
    }

    /// "Change every": one step along 20 s, 30 s, 1 min, by hand.
    fn widget_every_stepped(&self, d: i32) {
        let now = self.widget_every_secs();
        let choices = widgets::EVERY_CHOICES;
        let i = choices.iter().position(|c| *c == now).unwrap_or(0) as i32;
        let next = choices[(i + d.signum()).clamp(0, choices.len() as i32 - 1) as usize];
        log::info!(event = "widget_settings", every = next; "widget: every {next} s (0 = by hand)");
        let on = self.widget_on.borrow().clone();
        model::commit(self, Change::Widgets(on, next));
    }

    fn widget_every_secs(&self) -> u32 {
        self.widget_every.get().map_or(0, |d| d.as_secs() as u32)
    }

    /// A setting written (settings_model): what it changes, at once, on the screen and in the
    /// core. The screen's own switches and the browser panel both end here.
    fn apply_setting(&self, change: Change) {
        match change {
            Change::Widgets(on, every) => {
                *self.widget_on.borrow_mut() = on;
                self.send_widget_want();
                self.widget_every
                    .set((every > 0).then(|| Duration::from_secs(every as u64)));
                self.refresh_widget_settings();
                self.refresh_home();
            }
            Change::WidgetOrder(order) => {
                // The pages on follow the new order.
                let on: Vec<WidgetPage> = {
                    let now = self.widget_on.borrow();
                    order.iter().copied().filter(|k| now.contains(k)).collect()
                };
                *self.widget_order.borrow_mut() = order;
                *self.widget_on.borrow_mut() = on;
                log::info!(event = "widget_settings"; "widget: the pages' order changed");
                self.refresh_widget_settings();
                self.refresh_home();
            }
            Change::WidgetAdded(p, at, on) => {
                {
                    let mut order = self.widget_order.borrow_mut();
                    if !order.contains(&p) {
                        let shown = self.widget_on.borrow();
                        let at = at.unwrap_or_else(|| {
                            shown
                                .last()
                                .and_then(|l| order.iter().position(|x| x == l))
                                .map_or(0, |i| i + 1)
                        });
                        let at = at.min(order.len());
                        order.insert(at, p);
                    }
                    if on {
                        drop(order);
                        let order = self.widget_order.borrow();
                        let mut shown = self.widget_on.borrow_mut();
                        shown.push(p);
                        let now: Vec<WidgetPage> = order
                            .iter()
                            .copied()
                            .filter(|x| shown.contains(x))
                            .collect();
                        *shown = now;
                    }
                }
                self.clear_page_list(p);
                let key = p.key();
                log::info!(event = "widget_settings", page = key.as_str(); "widget: page {key} added");
                self.refresh_widget_settings();
                self.refresh_home();
            }
            Change::WidgetRemoved(p) => {
                if p.n > 1 {
                    self.widget_order.borrow_mut().retain(|x| *x != p);
                    self.widget_on.borrow_mut().retain(|x| *x != p);
                    self.widget_names.borrow_mut().retain(|(x, _)| *x != p);
                    match p.kind {
                        WidgetKind::Quotes => self.widget_quotes.borrow_mut().remove(p.n),
                        WidgetKind::Cities => self.widget_cities.borrow_mut().remove(p.n),
                        WidgetKind::Sensors => self.widget_sensors.borrow_mut().remove(p.n),
                        // A custom widget's tiles are the board's (board.json).
                        _ => {}
                    }
                    let key = p.key();
                    log::info!(event = "widget_settings", page = key.as_str(); "widget: page {key} removed");
                    self.lists_changed();
                }
            }
            Change::WidgetName(p, name) => {
                let name = crate::settings::clean_page_name(&name);
                let mut names = self.widget_names.borrow_mut();
                names.retain(|(x, _)| *x != p);
                if !name.is_empty() {
                    names.push((p, name));
                }
                drop(names);
                self.refresh_widget_settings();
                self.refresh_home();
            }
            Change::Sensors(n, ids) => {
                log::info!(event = "widget_settings", sensors = ids.len(); "widget: {} sensors chosen", ids.len());
                self.widget_sensors.borrow_mut().set(n, ids);
                self.lists_changed();
            }
            Change::WidgetHa(w) => {
                log::info!(event = "widget_settings"; "widget: Home Assistant's entities chosen");
                let sensors = self.widget_sensors.borrow().union(|s| s.clone());
                *self.widget_ha.borrow_mut() = crate::ha::widgets::Want { sensors, ..w };
                self.send_widget_want();
                self.refresh_widget_settings();
            }
            Change::WidgetCurrency(c) => {
                *self.widget_currency.borrow_mut() = c;
                self.configure_feeds();
            }
            Change::Quotes(n, q) => {
                log::info!(event = "widget_settings", quotes = q.len(); "widget: {} markets chosen", q.len());
                self.widget_quotes.borrow_mut().set(n, q);
                self.lists_changed();
            }
            Change::Cities(n, c) => {
                log::info!(event = "widget_settings", cities = c.len(); "widget: {} cities chosen", c.len());
                self.widget_cities.borrow_mut().set(n, c);
                self.lists_changed();
            }
            Change::Night(on, start, end) => {
                self.desk.borrow_mut().night = (on, start, end);
                self.refresh_desk();
                self.tick_clock();
            }
            Change::HomeCompact(on) => {
                self.home_compact.set(on);
                self.screen.set_home_compact(on);
                log::info!(event = "home_compact", on = on; "home: {}", if on { "compact" } else { "normal" });
                self.refresh_desk();
            }
            Change::Language(code) => self.apply_language(&code),
            Change::Volume(p) => {
                self.volume.set(p);
                let r = crate::volume::VolumeRequest::Set(p);
                let _ = self.core.send(CoreEvent::Volume(r));
            }
            Change::WakeSensitivity(s) => {
                self.screen.set_wake_step(s.step());
                // The core saves it too and has the wake word thread apply it.
                self.send(UiEvent::WakeSensitivity(s));
            }
            Change::KeepConversations(keep) => {
                self.history.borrow_mut().set_keep(keep);
                self.refresh_desk();
            }
            Change::ConversationMode(on) => {
                self.screen.set_conversation_mode(on);
                // The core opens the windows: from the next answer on.
                self.send(UiEvent::ConversationMode(on));
            }
            Change::ConversationMax(times) => {
                self.screen.set_conversation_max(times);
                self.send(UiEvent::ConversationMax(times));
            }
            Change::Lists(lists) => {
                self.choice.borrow_mut().lists = lists.clone();
                if let Some(tx) = self.hub_tx.borrow().as_ref() {
                    let _ = tx.send(HubCmd::SetLists(lists));
                }
                self.render_choices();
            }
            Change::Calendars(calendars) => {
                self.choice.borrow_mut().calendars = calendars;
                self.render_choices();
            }
            Change::Panel(on) => self.panel_switched(on),
            Change::PanelScreen(on) => self.remote_switched(on),
            Change::UpdatePrerelease(on) => {
                if let Some(u) = &self.updater {
                    log::info!(event = "update", prerelease = on; "update: release candidates {}", if on { "offered" } else { "not offered" });
                    u.set_prerelease(on);
                }
                self.refresh_update();
            }
        }
    }

    /// A page's list emptied (a page just added).
    fn clear_page_list(&self, p: WidgetPage) {
        match p.kind {
            WidgetKind::Quotes => self.widget_quotes.borrow_mut().set(p.n, Vec::new()),
            WidgetKind::Cities => self.widget_cities.borrow_mut().set(p.n, Vec::new()),
            WidgetKind::Sensors => self.widget_sensors.borrow_mut().set(p.n, Vec::new()),
            _ => {}
        }
    }

    /// A page's list changed (or a page with its list gone): the feeds fetch every page's
    /// items once, Home Assistant is asked for every page's sensors.
    fn lists_changed(&self) {
        let sensors = self.widget_sensors.borrow().union(|s| s.clone());
        self.widget_ha.borrow_mut().sensors = sensors;
        self.send_widget_want();
        self.configure_feeds();
        self.apply_feeds();
        self.refresh_widget_settings();
        self.refresh_home();
    }

    /// Everything that depends on the language, in `code`'s words; the core told.
    fn apply_language(&self, code: &str) {
        let app = &self.screen;
        match set_language(app, &self.character, &self.opts.locales, code) {
            Ok(l) => {
                *self.locale.borrow_mut() = l;
                self.render_weather();
                app.set_clock("");
                self.tick_clock();
                self.refresh_desk();
                // The brain's texts ("6 memories", the reminders' tile) are written when its
                // answers come: asked again so they come in the new language.
                self.refresh_memories();
                self.show(self.view.get());
                self.send(UiEvent::Language(code.to_string()));
                if app.get_page() == "welcome" {
                    self.language_pending.set(false);
                    let next = self.first_page().unwrap_or("home");
                    app.set_page(next);
                    if let Some(me) = self.this.borrow().upgrade() {
                        me.on_page(next);
                    }
                }
            }
            Err(e) => log::warn!("language {code}: {e}"),
        }
    }

    /// The home is not on screen, or not to be looked at: the widget's turn stands still.
    fn widget_paused(&self) -> bool {
        let d = self.desk.borrow();
        self.screen.get_page() != "home"
            || super::webview::visible()
            || d.mirror.is_some() && self.screen.get_page() == super::mirror::PAGE
            || !matches!(self.view.get(), Phase::Home | Phase::Offline)
            || d.night_shown
            || d.notice.is_some()
            || super::backlight::screen_off()
            || self.screen.get_control_open()
            || !self.screen.get_dialog().is_empty()
    }

    /// What the feeds fetch: the choices as they are now.
    fn configure_feeds(&self) {
        let config = crate::feeds::FeedsConfig {
            quotes: self.widget_quotes.borrow().union(|q| q.key()),
            currency: self.widget_currency.borrow().clone(),
            cities: self.widget_cities.borrow().union(|c| c.id),
            fahrenheit: self.fahrenheit.get(),
        };
        self.feeds.borrow_mut().configure(config);
    }

    /// The feeds' second: whether they may ask the internet now (their page on, not the night,
    /// the screen on), and their new data into the widget (shown when its page comes back).
    fn tick_feeds(&self) {
        let (night, off) = (self.desk.borrow().night_now, super::backlight::screen_off());
        // Home Assistant's curves wait too.
        if self.ha_quiet.get() != Some(night || off)
            && let Some(tx) = self.hub_tx.borrow().as_ref()
        {
            self.ha_quiet.set(Some(night || off));
            let _ = tx.send(HubCmd::Quiet(night || off));
        }
        let on = |k: WidgetKind| self.widget_on.borrow().iter().any(|p| p.kind == k);
        let active = crate::feeds::Active {
            quotes: crate::feeds::plan::active(
                on(WidgetKind::Quotes),
                !self.widget_quotes.borrow().is_empty(),
                night,
                !off,
            ),
            cities: crate::feeds::plan::active(
                on(WidgetKind::Cities),
                !self.widget_cities.borrow().is_empty(),
                night,
                !off,
            ),
        };
        let rev = {
            let mut f = self.feeds.borrow_mut();
            f.set_active(active);
            f.rev()
        };
        if rev != self.feeds_rev.get() {
            self.feeds_rev.set(rev);
            self.apply_feeds();
        }
    }

    /// The feeds' data in the screen's terms, for the chosen items in their order.
    fn apply_feeds(&self) {
        let snap = self.feeds.borrow().snapshot();
        let quotes: Vec<widgets::Trend> = self
            .widget_quotes
            .borrow()
            .union(|q| q.key())
            .iter()
            .filter_map(|c| {
                let q = snap.quote(&c.key())?;
                use crate::feeds::{Source, plan};
                let interval = match (c.source, q.market_open) {
                    (Source::Coingecko, _) => plan::COINGECKO,
                    (_, true) => plan::YAHOO_OPEN,
                    (_, false) => plan::YAHOO_CLOSED,
                };
                Some(widgets::Trend {
                    key: c.key(),
                    name: c.name.clone(),
                    value: q.price,
                    decimals: q.decimals,
                    unit: q.unit.clone(),
                    prev: q.prev,
                    pct: true,
                    points: q.points.clone(),
                    low: q.low,
                    high: q.high,
                    at: q.at,
                    fresh: 2 * interval as i64,
                    max_age: 48 * 3600,
                })
            })
            .collect();
        self.widget_data.borrow_mut().quotes = quotes;
        *self.city_weather.borrow_mut() = self
            .widget_cities
            .borrow()
            .union(|c| c.id)
            .iter()
            .filter_map(|c| snap.city(c.id).cloned())
            .collect();
        self.refresh_widget();
    }

    /// The board: the widget's page and the notes' page, without the one waiting for Undo.
    fn refresh_notes(&self) {
        let hidden = match self.desk.borrow().undo {
            Some(Undo::Note(id)) => Some(id),
            _ => None,
        };
        let board: Vec<brain_link::Note> = self
            .board
            .borrow()
            .iter()
            .filter(|n| Some(n.id) != hidden)
            .cloned()
            .collect();
        self.widget_data.borrow_mut().notes = board.iter().map(|n| n.text.clone()).collect();
        let l = self.locale.borrow();
        let rows: Vec<(i64, String, String)> = board
            .iter()
            .map(|n| (n.id, n.text.clone(), short_date(&l, n.created)))
            .collect();
        self.screen.set_notes(
            &rows,
            [
                l.text("widget_notes"),
                l.text("notes_hint"),
                l.text("notes_empty"),
                l.text("reminders"),
            ],
        );
        drop(l);
        self.refresh_widget();
    }

    /// The markets' or the sensors' page, as the data is now.
    fn refresh_trends(&self) {
        let l = self.locale.borrow();
        let w = widget_words(&l);
        let kind = self.screen.get_trends_kind();
        let quotes = kind != "sensors";
        let page = self.opened(if quotes {
            WidgetKind::Quotes
        } else {
            WidgetKind::Sensors
        });
        let title = model::page_label(page, &self.widget_names.borrow(), &|k| {
            l.text(k).to_string()
        });
        let data = self.widget_data.borrow();
        let all = if quotes { &data.quotes } else { &data.sensors };
        // That page's items, in its order.
        let keys: Vec<String> = if quotes {
            self.widget_quotes
                .borrow()
                .get(page.n)
                .iter()
                .map(|q| q.key())
                .collect()
        } else {
            self.widget_sensors.borrow().get(page.n).to_vec()
        };
        let items: Vec<widgets::Trend> = keys
            .iter()
            .filter_map(|k| all.iter().find(|t| t.key == *k).cloned())
            .collect();
        let items = if keys.is_empty() && self.opts.demo.is_some() {
            all.clone()
        } else {
            items
        };
        let symbols: Vec<(String, String)> = self
            .widget_quotes
            .borrow()
            .union(|q| q.key())
            .iter()
            .map(|c| (c.key(), c.id.to_ascii_uppercase()))
            .collect();
        let details = |t: &widgets::Trend| -> String {
            if quotes {
                symbols
                    .iter()
                    .find(|(k, _)| *k == t.key)
                    .map(|(_, id)| id.clone())
                    .unwrap_or_default()
            } else {
                t.unit.clone()
            }
        };
        let cards = widgets::trend_cards(
            &items,
            &details,
            &w,
            l.text("trends_high"),
            l.text("trends_low"),
            self.now() as i64,
            &local_hm,
        );
        let footer = if quotes {
            l.text("trends_credit_markets")
        } else {
            ""
        };
        self.screen.set_trends(&title, &cards, footer);
    }

    /// The page of `kind` a tap on the widget opened (else its first).
    fn opened(&self, kind: WidgetKind) -> WidgetPage {
        self.widget_opened
            .get()
            .filter(|p| p.kind == kind && self.widget_order.borrow().contains(p))
            .unwrap_or_else(|| kind.into())
    }

    /// The other cities' page: the one chosen on its chips (the first at first), its weather as
    /// the home's weather page shows it, in its own time.
    fn refresh_city(&self) {
        let l = self.locale.borrow();
        let page = self.opened(WidgetKind::Cities);
        let lists = self.widget_cities.borrow();
        let choices = lists.get(page.n);
        let weather = self.city_weather.borrow();
        let shown: Vec<(
            &crate::feeds::CityChoice,
            &crate::feeds::openmeteo::CityWeather,
        )> = choices
            .iter()
            .filter_map(|c| Some((c, weather.iter().find(|w| w.id == c.id)?)))
            .collect();
        let names: Vec<String> = shown.iter().map(|(c, _)| c.name.clone()).collect();
        let index =
            (self.screen.get_city_index().max(0) as usize).min(names.len().saturating_sub(1));
        let Some((c, cw)) = shown.get(index) else {
            return;
        };
        let now = self.now() as i64;
        let (w, today) = city_as_weather(cw, now);
        let text = super::weather::text(&w, &l);
        let there = (now + cw.utc_offset as i64).rem_euclid(86400);
        let condition = format!(
            "{} · {}",
            text.condition,
            l.text("city_local_time").replace(
                "{t}",
                &format!("{:02}:{:02}", there / 3600, there % 3600 / 60)
            )
        );
        let rows = super::weather::forecast_rows(&w, &l, &today, 5);
        self.screen.set_city(&super::weather::CityView {
            title: &c.name,
            names: &names,
            index,
            text: &text,
            condition: &condition,
            rows: &rows,
            footer: l.text("city_credit"),
        });
    }

    /// The other cities as the widget's rows: the time there now, the name, the sky, the
    /// temperature.
    fn city_rows(&self, now: i64) -> Vec<widgets::CityRow> {
        samples::city_rows_of(
            &self.widget_cities.borrow().union(|c| c.id),
            &self.city_weather.borrow(),
            now,
        )
    }

    /// The widget's second: its turn, and at a new minute the page on screen with fresh data
    /// (what is over leaves Next).
    fn tick_widget(&self, now: Instant, minute: bool) {
        self.tick_feeds();
        self.board_seen_again();
        if self.widget_on.borrow().is_empty() {
            return;
        }
        if minute {
            self.refresh_widget();
        }
        let paused = self.widget_paused();
        let shown = self
            .widget
            .borrow_mut()
            .tick(now, paused, self.widget_every.get(), minute);
        if let Some(s) = shown {
            self.show_widget(&s);
        }
    }

    fn refresh_ringing(&self) {
        let d = self.desk.borrow();
        let l = self.locale.borrow();
        let (name, kind) = d
            .ringing
            .first()
            .cloned()
            .unwrap_or_else(|| ("Timer".into(), "timer".into()));
        let since = d.ringing_since.map_or(0, |t| t.elapsed().as_secs());
        let since = l
            .text("ready_since")
            .replace("{t}", &format!("{}:{:02}", since / 60, since % 60));
        self.screen.set_ringing(&name, &since, kind == "alarm");
    }

    fn refresh_timers(&self) {
        let l = self.locale.borrow();
        let timers = self.visible_timers();
        let rows: Vec<TimerLineView> = timers
            .iter()
            .map(|t| TimerLineView {
                id: t.id.clone(),
                name: t.name.clone(),
                detail: if t.paused {
                    l.text("paused").to_string()
                } else {
                    l.text("ends_at")
                        .replace("{t}", &local_hm(t.due_unix as i64).0)
                },
                left: t.left.clone(),
                remaining: t.fraction,
                paused: t.paused,
            })
            .collect();
        self.screen.set_timer_rows(&rows);
        let d = self.desk.borrow();
        let alarms: Vec<AlarmLineView> = d
            .alarms
            .iter()
            .filter(|a| !matches!(d.undo, Some(Undo::Alarm(id)) if id == a.id))
            .map(|a| AlarmLineView {
                id: a.id,
                time: a.time(),
                days: crate::alarms::days_text(a.days, &l.days, |k| l.text(k).to_string()),
                on: a.on,
            })
            .collect();
        self.screen.set_alarm_rows(&alarms);
        let undo = match &d.undo {
            Some(Undo::Timer(_)) => l.text("timer_cancelled"),
            Some(Undo::Alarm(_)) => l.text("alarm_deleted"),
            Some(Undo::Note(_)) => l.text("note_removed"),
            None => "",
        };
        self.screen.set_undo(undo);
        drop(d);
        drop(l);
        self.refresh_tiles();
    }

    fn refresh_chat(&self) {
        let l = self.locale.borrow();
        let today = local_hm(self.now() as i64).1;
        let lines = self.history.borrow().lines(|t| {
            let (hm, day) = local_hm(t);
            match today - day {
                0 => format!("{} · {hm}", l.text("today")),
                1 => format!("{} · {hm}", l.text("yesterday")),
                _ => {
                    // SAFETY: localtime_r only writes into the struct we pass.
                    let tm = unsafe {
                        let mut tm: libc::tm = std::mem::zeroed();
                        let tt = t as libc::time_t;
                        libc::localtime_r(&tt, &mut tm);
                        tm
                    };
                    format!(
                        "{} {} · {hm}",
                        tm.tm_mday,
                        l.months[tm.tm_mon.clamp(0, 11) as usize]
                    )
                }
            }
        });
        self.screen.set_chat(&lines);
    }

    fn refresh_tiles(&self) {
        let l = self.locale.borrow();
        let chat = self
            .history
            .borrow()
            .exchanges()
            .last()
            .map_or(String::new(), |e| {
                let (hm, day) = local_hm(e.at);
                if day == local_hm(self.now() as i64).1 {
                    l.text("tile_chat_today").replace("{t}", &hm)
                } else {
                    String::new()
                }
            });
        let d = self.desk.borrow();
        let mut parts = Vec::new();
        if !d.timers.is_empty() {
            let n = d.timers.len();
            parts.push(
                l.text(if n == 1 { "tile_timer" } else { "tile_timers" })
                    .replace("{n}", &n.to_string()),
            );
        }
        let alarms = crate::alarms::Alarms::from_list(d.alarms.clone());
        if let Some((a, _)) = alarms.next_within(crate::alarms::Now::at(self.now() as i64), 24 * 60)
        {
            parts.push(l.text("tile_alarm").replace("{t}", &a.time()));
        }
        // What is inside, not the language alone.
        let settings = l.text("tile_settings").to_string();
        self.screen.set_tiles(&chat, &parts.join(" · "), &settings);
    }

    fn refresh_settings(&self) {
        self.refresh_widget_settings();
        self.refresh_panel();
        let l = self.locale.borrow();
        let (on, start, end) = self.desk.borrow().night;
        let ha = if !self.ha_linked.get() {
            l.text("ha_not_linked")
        } else if self.view.get() == Phase::Offline {
            l.text("ha_not_answering")
        } else {
            l.text("ha_connected")
        };
        let screen = if on {
            l.text("night_on")
                .replace("{from}", &format!("{start:02}"))
                .replace("{to}", &format!("{end:02}"))
        } else {
            l.text("night_off").to_string()
        };
        self.screen
            .set_ha_linked(self.ha_linked.get() && self.ha_setup.borrow().is_some());
        let ha = if self.ha_page.borrow().attention && self.ha_linked.get() {
            l.text("ha_attention")
        } else {
            ha
        };
        self.screen.set_settings_values(&SettingsView {
            language_name: l.name.clone(),
            brains: BRAIN.into(),
            ha_status: ha.into(),
            version: match self.updater.as_ref().and_then(|u| u.state().latest) {
                Some(r) => l
                    .text("update_available_short")
                    .replace("{v}", env!("CARGO_PKG_VERSION"))
                    .replace("{new}", &r.version),
                None => format!("home-companion {}", env!("CARGO_PKG_VERSION")),
            },
            screen_status: screen,
            save_chat: self.history.borrow().keeps(),
            night_on: on,
            night_start: start,
            night_end: end,
        });
    }

    // ---- Settings → Home Assistant ----------------------------------------------------------

    pub fn set_ha_setup(&self, s: crate::ha::setup::Setup) {
        *self.ha_setup.borrow_mut() = Some(s);
        self.refresh_settings();
    }

    fn ha_configured(&self) -> bool {
        self.opts.state_dir.as_ref().is_some_and(|d| {
            crate::ha::setup::State::load(&d.join(crate::ha::setup::STATE_FILE)).configured
        })
    }

    /// A run of the setup thread; `asked` = by the user ("Set up", "Check and repair", "Repair").
    fn ha_send(&self, cmd: crate::ha::setup::SetupCmd, asked: bool) {
        // One run at a time: taps while one runs are ignored (the button is off too).
        if asked && self.ha_page.borrow().busy {
            return;
        }
        if let Some(s) = self.ha_setup.borrow().as_ref() {
            if asked {
                // Busy at once, before the thread says so: a second tap finds it busy.
                self.ha_page.borrow_mut().busy = true;
                let mut p = self.ha_page.borrow_mut();
                p.asked = true;
                p.done = false;
                p.status.clear();
            }
            let _ = s.tx.send(cmd);
        }
        self.render_ha();
    }

    fn render_ha(&self) {
        use crate::ha::setup::Level;
        let l = self.locale.borrow();
        let name = &self.character.name;
        let p = self.ha_page.borrow();
        let rows = if p.rows.is_empty() {
            crate::ha::setup::ROWS
                .iter()
                .map(|id| crate::ha::setup::Row {
                    id: id.to_string(),
                    level: Level::Unknown,
                    key: format!("ha_{id}_unknown"),
                    args: Vec::new(),
                    repair: false,
                })
                .collect()
        } else {
            p.rows.clone()
        };
        let lines = rows
            .iter()
            .filter(|r| r.id != "link" || r.level != Level::Ok)
            .map(|r| super::HaLine {
                id: r.id.clone(),
                label: word(&l, &format!("ha_row_{}", r.id), name),
                detail: r.detail(|k| word(&l, k, name)),
                level: match r.level {
                    Level::Ok => 0,
                    Level::Warn => 1,
                    Level::Bad => 2,
                    Level::Skipped => 3,
                    Level::Unknown => 4,
                },
                repair: r.repair,
            })
            .collect();
        // The line is only a failure now (the pop-up says what it is doing).
        let status = if p.busy {
            String::new()
        } else {
            p.status.clone()
        };
        let picks = ha_picks(&p.choices, |k| word(&l, k, name));
        if !p.picking.is_empty() {
            let rows: Vec<(String, String, bool)> = p
                .choices
                .of(&p.picking)
                .iter()
                .map(|o| {
                    let on = o.id == p.choices.current_of(&p.picking);
                    (o.id.clone(), o.name.clone(), on)
                })
                .collect();
            self.screen.set_ha_pick(
                &word(&l, &format!("ha_choice_{}", p.picking), name),
                &word(&l, &format!("ha_choice_{}_hint", p.picking), name),
                &rows,
            );
        }
        self.screen.set_ha_page(&super::HaPageView {
            rows: lines,
            status,
            busy: p.busy,
            configured: p.demo_configured || self.ha_configured(),
            done: p.done && !p.busy,
            picks,
            step: p.step.clone(),
            // The pop-up only for what the user asked (a check when the page opens is silent).
            popup: p.busy && p.asked,
        });
    }

    fn poll_ha(&self) {
        use crate::ha::setup::{Level, SetupEvent};
        let mut got = Vec::new();
        if let Some(s) = self.ha_setup.borrow().as_ref() {
            got.extend(s.rx.try_iter());
        }
        self.tick_usage(false);
        // The precise use is quiet: no wake, no page to redraw.
        got.retain(|ev| {
            let SetupEvent::Usage(p) = ev else {
                return true;
            };
            *self.usage_precise.borrow_mut() = p.clone();
            self.render_usage();
            false
        });
        if got.is_empty() {
            return;
        }
        // A step of the setup moved the page on: bright again.
        self.wake(WakeSource::Setup);
        let mut settings = false;
        for ev in got {
            let mut p = self.ha_page.borrow_mut();
            match ev {
                SetupEvent::Busy(b) => {
                    let was = p.busy;
                    p.busy = b;
                    if !b {
                        p.step.clear();
                    }
                    if was && !b && p.asked {
                        p.asked = false;
                        let fine = !p.rows.iter().any(|r| r.level == Level::Bad);
                        if p.status.is_empty() && fine {
                            // No line over the rows (it moved them): the character bows and
                            // says it.
                            p.done = true;
                            drop(p);
                            // On the page the hands leave the keys (the laptop's clips); left
                            // already: the bow, then the satisfied pose on the home.
                            if self.screen.get_page() != "ha" {
                                let now = Instant::now();
                                self.motion.borrow_mut().seq.done(now);
                                // After the bow (anim's `done`, 3 s).
                                self.satisfied
                                    .set(Some((now + BOW, now + BOW + SATISFIED_FOR)));
                            }
                            let said =
                                word(&self.locale.borrow(), "ha_ready_say", &self.character.name);
                            self.send(UiEvent::Say(said));
                        }
                    }
                }
                SetupEvent::Rows(rows) => {
                    // While it works: the row that just changed says what it did.
                    if p.busy {
                        if let Some(r) = rows.iter().find(|r| !p.rows.contains(r)) {
                            let l = self.locale.borrow();
                            let name = &self.character.name;
                            p.step = format!(
                                "{} · {}",
                                word(&l, &format!("ha_row_{}", r.id), name),
                                r.detail(|k| word(&l, k, name))
                            );
                        }
                    }
                    p.rows = rows;
                }
                SetupEvent::Choices(c) => p.choices = c,
                SetupEvent::Failed(why) => {
                    let (key, err) = why.split_once('\u{1f}').unwrap_or((why.as_str(), ""));
                    p.status = word(&self.locale.borrow(), key, &self.character.name)
                        .replace("{error}", err);
                }
                SetupEvent::Attention(a) => {
                    p.attention = a;
                    settings = true;
                }
                // Taken above.
                SetupEvent::Usage(_) => {}
                SetupEvent::Unlinked => {
                    *p = HaPage::default();
                    drop(p);
                    *self.usage_precise.borrow_mut() = None;
                    self.render_usage();
                    log::info!(event = "ha_setup"; "unlinked: what the body made is removed, the link too");
                    self.ha_linked.set(false);
                    settings = true;
                    // Back to Altro: the Home Assistant tile says "not linked" and opens the QR code.
                    if self.screen.get_page() == "ha" {
                        self.screen.set_page("altro");
                    }
                }
            }
        }
        self.render_ha();
        // A job started or ended on the page: the laptop's keys.
        if self.screen.get_page() == "ha" {
            self.page_motion();
        }
        if settings {
            self.refresh_settings();
            self.refresh_tiles();
            // The home's "to fix" card follows the attention at once.
            self.refresh_home();
        }
    }

    // ---- the shopping list --------------------------------------------------------------------

    /// Home Assistant's thread (cards, shopping lists): where the screen's ticks and choices
    /// go, and the lists chosen at first (empty = all).
    pub fn set_hub(&self, tx: Sender<HubCmd>, lists: Vec<String>) {
        *self.hub_tx.borrow_mut() = Some(tx);
        self.send_widget_want();
        self.choice.borrow_mut().lists = lists;
        self.choice.borrow_mut().calendars = self
            .settings
            .as_ref()
            .map(|s| s.calendars())
            .unwrap_or_default();
    }

    /// What the widget asks of Home Assistant now: only what the pages that are on show.
    fn widget_want(&self) -> crate::ha::widgets::Want {
        let on = self.widget_on.borrow();
        let w = self.widget_ha.borrow();
        let has = |k: WidgetKind| on.iter().any(|p| p.kind == k);
        let mut want = crate::ha::widgets::Want {
            sensors: if has(WidgetKind::Sensors) {
                w.sensors.clone()
            } else {
                Vec::new()
            },
            player: if has(WidgetKind::Playing) {
                w.player.clone()
            } else {
                String::new()
            },
            people: if has(WidgetKind::Home) {
                w.people.clone()
            } else {
                None
            },
            openings: has(WidgetKind::Check) && w.openings,
            batteries: has(WidgetKind::Check) && w.batteries,
            ..Default::default()
        };
        // The board's tiles: their entities, the wide sensors' days, the Advanced services.
        self.board_want(&mut want);
        want
    }

    fn send_widget_want(&self) {
        if let Some(tx) = self.hub_tx.borrow().as_ref() {
            let _ = tx.send(HubCmd::SetWidgets(self.widget_want()));
        }
    }

    /// Home Assistant's data in the widget's terms: each sensor with its day when it came (the
    /// change against 24 hours ago, never coloured).
    fn apply_ha_widgets(&self) {
        let d = self.ha_widgets.borrow().clone();
        let now = now_unix() as i64;
        let curves = self.ha_curves.borrow();
        let mut data = self.widget_data.borrow_mut();
        data.sensors = d
            .sensors
            .iter()
            .map(|s| {
                let c = curves.get(&s.id);
                widgets::Trend {
                    key: s.id.clone(),
                    name: s.name.clone(),
                    value: s.value,
                    decimals: s.decimals,
                    unit: s.unit.clone(),
                    prev: c.map(|c| c.first),
                    pct: false,
                    points: c.map(|c| c.points.clone()).unwrap_or_default(),
                    low: c.map_or(s.value, |c| c.low.min(s.value)),
                    high: c.map_or(s.value, |c| c.high.max(s.value)),
                    at: now,
                    fresh: i64::MAX / 4,
                    max_age: i64::MAX / 4,
                }
            })
            .collect();
        data.check = d.check.clone();
        // Alfred's own AirPlay has its card already.
        data.playing = d
            .playing
            .clone()
            .filter(|_| self.desk.borrow().music.is_none());
        data.people = d.people.clone();
        drop(data);
        drop(curves);
        self.refresh_widget();
    }

    /// What Home Assistant's thread says.
    pub fn apply_hub(&self, m: HubMsg) {
        match m {
            HubMsg::Catalog(c) => *self.ha_catalog.borrow_mut() = c,
            HubMsg::Curve(entity, c) => {
                match c {
                    Some(c) => self.ha_curves.borrow_mut().insert(entity, c),
                    None => self.ha_curves.borrow_mut().remove(&entity),
                };
                self.apply_ha_widgets();
            }
            HubMsg::Widgets(d) => {
                *self.ha_widgets.borrow_mut() = d;
                self.apply_ha_widgets();
            }
            HubMsg::Card(ev) => {
                // A card that is `quiet` (an update with fresh values) neither sounds nor wakes.
                let chimes = ev.chimes();
                let changed = self.desk.borrow_mut().ha_cards.apply(ev, Instant::now());
                if changed {
                    // Like a reminder: a soft wake and its sound, by day only.
                    if chimes && !self.desk.borrow().night_now {
                        self.wake(WakeSource::Conversation);
                        self.send(UiEvent::Reminder);
                    }
                    self.refresh_home();
                }
            }
            HubMsg::Link { up, last_seen } => super::webview::set_ha_link(up, last_seen),
            HubMsg::Screen(true) => self.wake(WakeSource::HomeAssistant),
            HubMsg::Screen(false) => self.screen_off_by("Home Assistant"),
            HubMsg::Entities(e) => {
                self.choice.borrow_mut().known = e;
                self.render_choices();
            }
            HubMsg::Lists(active) => {
                {
                    let mut d = self.desk.borrow_mut();
                    d.shopping = (!active.is_empty())
                        .then(|| lists::with_active(d.shopping.as_deref().unwrap_or(&[]), &active));
                }
                self.render_shopping();
                self.refresh_home();
            }
            HubMsg::Items { list, items } => {
                if let Some(l) = self
                    .desk
                    .borrow_mut()
                    .shopping
                    .as_mut()
                    .and_then(|v| v.iter_mut().find(|l| l.entity == list))
                {
                    l.items = items;
                }
                self.render_shopping();
                self.refresh_home();
            }
            HubMsg::Tiles(s) => self.board_states(s),
            HubMsg::Called { id, ok, error } => self.board_called(id, ok, error),
        }
    }

    /// The OK on card `i` of the home, when it is one of Home Assistant's.
    fn ha_card_ok(&self, i: usize) -> bool {
        let nth = {
            let kinds = self.card_kinds.borrow();
            let Some(k) = kinds.get(i) else { return false };
            if !cards::HA_KINDS.contains(k) {
                return false;
            }
            kinds[..i]
                .iter()
                .filter(|k| cards::HA_KINDS.contains(k))
                .count()
        };
        let taken = self.desk.borrow_mut().ha_cards.take_nth(nth);
        let gone = taken.is_some();
        if gone {
            self.refresh_home();
        }
        // Told to Home Assistant (an automation may tick a task off, say): only a real OK, and
        // only for a card that has an id; a channel send, nothing waits for it.
        if let Some(id) = taken.and_then(|card| card.id)
            && let Some(tx) = self.hub_tx.borrow().as_ref()
        {
            let _ = tx.send(HubCmd::Fire {
                event_type: crate::ha::events::CARD_OK_EVENT.into(),
                data: crate::ha::events::card_ok_data(&id),
            });
        }
        gone
    }

    /// A tap on a row of Lists and calendars: saved, and the lists change at once.
    fn choice_toggled(&self, id: &str) {
        let mut c = self.choice.borrow().clone();
        if !c.toggle(id) {
            return;
        }
        let change = if id.starts_with("todo.") {
            Change::Lists(c.lists)
        } else {
            Change::Calendars(c.calendars)
        };
        model::commit(self, change);
    }

    fn render_choices(&self) {
        let (todo, cal) = self.choice.borrow().rows();
        self.screen.set_choices(&todo, &cal);
    }

    /// A tap on an item: ticked for [`SHOP_UNDO`], then bought (completed in Home Assistant, gone
    /// from the list); a tap meanwhile takes the tick back (Andrea, 2026-11-16).
    fn shop_done(&self, uid: String) {
        if lists::split_uid(&uid).is_none() {
            return;
        }
        {
            let mut t = self.shop_ticks.borrow_mut();
            if t.remove(&uid).is_none() {
                t.insert(uid, Instant::now());
            }
        }
        self.render_shopping();
        if !self.shop_ticks.borrow().is_empty() && !self.shop_timer.running() {
            let w = self.this.borrow().clone();
            self.shop_timer
                .start(TimerMode::Repeated, Duration::from_millis(500), move || {
                    if let Some(ui) = w.upgrade() {
                        ui.shop_commit(false);
                    }
                });
        }
    }

    /// The ticks whose time is over (all of them with `all`): bought.
    fn shop_commit(&self, all: bool) {
        let due: Vec<String> = {
            let mut t = self.shop_ticks.borrow_mut();
            let due: Vec<String> = t
                .iter()
                .filter(|(_, at)| all || at.elapsed() >= SHOP_UNDO)
                .map(|(u, _)| u.clone())
                .collect();
            for u in &due {
                t.remove(u);
            }
            due
        };
        if self.shop_ticks.borrow().is_empty() {
            self.shop_timer.stop();
        }
        if due.is_empty() {
            return;
        }
        for uid in due {
            let Some((list, item)) = lists::split_uid(&uid) else {
                continue;
            };
            if let Some(v) = self.desk.borrow_mut().shopping.as_mut() {
                lists::remove(v, list, item);
            }
            if let Some(tx) = self.hub_tx.borrow().as_ref() {
                let _ = tx.send(HubCmd::Done {
                    list: list.to_string(),
                    uid: item.to_string(),
                });
            }
        }
        self.render_shopping();
        self.refresh_home();
    }

    fn render_shopping(&self) {
        let d = self.desk.borrow();
        let l = self.locale.borrow();
        match &d.shopping {
            Some(v) => self.screen.set_shopping(
                true,
                &lists::rows(v)
                    .into_iter()
                    .map(|(u, t)| {
                        let k = self.shop_ticks.borrow().contains_key(&u);
                        (u, t, k)
                    })
                    .collect::<Vec<_>>(),
                &l.text("tile_shopping")
                    .replace("{n}", &lists::count(v).to_string()),
            ),
            None => self.screen.set_shopping(false, &[], ""),
        }
    }

    // ---- home-companion-brain: the "Ricordi" page and the reminder cards ------------------------------

    /// A change on the page, then the page again.
    fn brain_do(&self, req: serde_json::Value) {
        if let Some(b) = &self.brain {
            b.send(req);
            b.ask("status");
            b.ask("memory.list");
            b.ask("jobs.list");
        }
    }

    fn refresh_memories(&self) {
        if let Some(b) = &self.brain {
            b.ask("status");
            b.ask("memory.list");
            b.ask("jobs.list");
        }
    }

    /// The OK on the first reminder card: acknowledged (no voice after it). False: no reminder.
    fn ack_reminder(&self) -> bool {
        let first = self.desk.borrow_mut().reminders.first().cloned();
        let Some((id, _, _)) = first else {
            return false;
        };
        {
            let mut d = self.desk.borrow_mut();
            d.reminders.remove(0);
            if d.notice.as_ref().is_some_and(|n| n.id == id) {
                d.notice = None;
            }
        }
        if id > 0
            && let Some(b) = &self.brain
        {
            b.send(json!({"cmd": "job.ack", "id": id}));
        }
        self.refresh_notice();
        self.refresh_home();
        true
    }

    /// The full-screen reminder answered: OK (done) or "in 10 min" (the brain brings it back).
    fn notice_done(&self, snooze: bool) {
        let Some(n) = self.desk.borrow_mut().notice.take() else {
            return;
        };
        self.desk.borrow_mut().reminders.retain(|r| r.0 != n.id);
        if n.id > 0
            && let Some(b) = &self.brain
        {
            if snooze {
                b.send(json!({"cmd": "job.snooze", "id": n.id, "minutes": NOTICE_SNOOZE_MIN}));
            } else {
                b.send(json!({"cmd": "job.ack", "id": n.id}));
            }
        }
        log::info!(event = "reminder_notice", id = n.id, snooze = snooze, shown_s = n.since.elapsed().as_secs(); "reminder: {}", if snooze { "again in 10 minutes" } else { "OK" });
        self.wake(WakeSource::Touch);
        self.refresh_notice();
        self.refresh_home();
    }

    /// A reminder full screen now (Home Assistant's view gives way to it, webview/mod.rs).
    pub fn noticing(&self) -> bool {
        self.desk.borrow().notice.is_some()
    }

    fn refresh_notice(&self) {
        let d = self.desk.borrow();
        let l = self.locale.borrow();
        let shown = d
            .notice
            .as_ref()
            .and_then(|n| d.reminders.iter().find(|r| r.0 == n.id).map(|r| (n, r)));
        let on = shown.is_some();
        if self.notice_told.replace(on) != on {
            self.send(UiEvent::Notice(on));
        }
        match shown {
            Some((n, (_, title, text))) => {
                let when = if n.when.is_empty() {
                    String::new()
                } else {
                    l.text("notice_when").replace("{t}", &n.when)
                };
                self.screen
                    .set_notice(true, title, text, &when, l.text("notice_later"));
            }
            None => self
                .screen
                .set_notice(false, "", "", "", l.text("notice_later")),
        }
    }

    fn poll_brain(&self) {
        let Some(b) = &self.brain else { return };
        let mut changed = false;
        let mut home = false;
        let mut events_seen = false;
        let mut notes_seen = false;
        let mut upcoming_seen: Option<Vec<(i64, String)>> = None;
        let mut timers: Vec<(u64, Option<String>)> = Vec::new();
        let mut cancels: Vec<String> = Vec::new();
        let mut dnd: Option<Option<u64>> = None;
        let mut alarms: Vec<(u8, u8, u8)> = Vec::new();
        let mut house_cards: Vec<HouseEvent> = Vec::new();
        while let Some(m) = b.try_recv() {
            let l = self.locale.borrow();
            let name = self.character.name.clone();
            let mut d = self.desk.borrow_mut();
            let mv = &mut d.memories;
            match m {
                BrainMsg::Present(p) => {
                    if p {
                        house_cards.push(HouseEvent::Resubscribed);
                    }
                    mv.present = p;
                    if !p {
                        mv.status = word(&l, "brain_off", &name);
                        mv.tile = mv.status.clone();
                    }
                    changed = true;
                    if p {
                        b.ask("status");
                    }
                }
                BrainMsg::Reply { answer: None, .. } => {
                    mv.status = word(&l, "brain_off", &name);
                    changed = true;
                }
                BrainMsg::Reply {
                    cmd,
                    answer: Some(a),
                } => {
                    changed = true;
                    match cmd.as_str() {
                        "status" => {
                            mv.present = true;
                            let facts = a["facts"].as_i64().unwrap_or(0);
                            mv.tile_known =
                                l.text("tile_memories").replace("{n}", &facts.to_string());
                            mv.tile = memories_tile(&l, &mv.tile_known, mv.confirm.len());
                            let mut lines = Vec::new();
                            if a["key"] != true {
                                lines.push(word(&l, "brain_no_key", &name));
                            } else {
                                lines.push(
                                    l.text("brain_status")
                                        .replace("{facts}", &facts.to_string())
                                        .replace("{model}", a["model"].as_str().unwrap_or("")),
                                );
                            }
                            lines.push(tidy_line(&l, &a["tidy"]));
                            mv.status = lines.join(" · ");
                        }
                        "memory.list" => {
                            let (facts, confirm, over) = memory_lines(&a, |ts| short_date(&l, ts));
                            mv.sent = if facts.is_empty() {
                                String::new()
                            } else {
                                word(&l, "memories_sent", &name)
                            };
                            mv.more = more_line(&l, over);
                            mv.facts = facts;
                            mv.confirm = confirm;
                            mv.tile = memories_tile(&l, &mv.tile_known, mv.confirm.len());
                        }
                        "jobs.list" => {
                            let mut up: Vec<(i64, String)> = a["jobs"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|j| {
                                    (
                                        j["due_at"].as_i64().unwrap_or(0),
                                        j["text"].as_str().unwrap_or("").to_string(),
                                    )
                                })
                                .collect();
                            up.sort_by_key(|(at, _)| *at);
                            upcoming_seen = Some(up);
                            mv.reminders_tile = match a["jobs"].as_array().map_or(0, Vec::len) {
                                0 => l.text("tile_reminders_none").to_string(),
                                n => l.text("tile_reminders").replace("{n}", &n.to_string()),
                            };
                            mv.jobs = a["jobs"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|j| {
                                    let at = j["due_at"].as_i64().unwrap_or(0);
                                    let (hm, _) = local_hm(at);
                                    (
                                        j["id"].as_i64().unwrap_or(0),
                                        format!("{} {hm}", short_date(&l, at)),
                                        j["text"].as_str().unwrap_or("").to_string(),
                                    )
                                })
                                .collect();
                        }
                        _ => {}
                    }
                }
                BrainMsg::Timer(secs, name) => {
                    log::info!(event = "brain_timer", seconds = secs, name = name.as_deref().unwrap_or(""); "timer from the brain: {secs} s");
                    timers.push((secs, name));
                }
                BrainMsg::TimerCancel(all) => {
                    // The one set last: local timers count up ("local-3" after "local-2").
                    let n = |id: &str| {
                        id.rsplit('-')
                            .next()
                            .and_then(|x| x.parse::<u64>().ok())
                            .unwrap_or(0)
                    };
                    let mut ids: Vec<String> = d.timers.iter().map(|t| t.id.clone()).collect();
                    ids.sort_by_key(|id| n(id));
                    if !all {
                        ids = ids.pop().into_iter().collect();
                    }
                    log::info!(event = "brain_timer_cancel", all = all; "timer cancel from the brain: {ids:?}");
                    cancels.extend(ids);
                }
                BrainMsg::Dnd(on) => {
                    // As the reflex: a length when said, else an hour by day or the morning.
                    let now = now_unix() as i64;
                    let until = on.map(|minutes| match minutes {
                        Some(m) => now as u64 + m.min(24 * 60) * 60,
                        None => crate::core::dnd_default_until(
                            now,
                            crate::core::local_seconds_of_day(now),
                        ),
                    });
                    log::info!(event = "brain_dnd", on = until.is_some(); "do not disturb from the brain");
                    dnd = Some(until);
                }
                BrainMsg::Alarm(h, m, days) => {
                    log::info!(event = "brain_alarm"; "alarm from the brain: {h:02}:{m:02}");
                    alarms.push((h, m, days));
                }
                BrainMsg::Changed(c) => {
                    let word = |k: &str| {
                        let w = l.text(k);
                        (w != k).then(|| w.to_string())
                    };
                    let line = house::done_line(&c.name, c.state.as_deref(), &c.service, &word);
                    log::info!(event = "house_done", id = c.id, entity = c.entity_id.as_str(), undo = c.undo; "house: {line}");
                    house_cards.push(HouseEvent::Done(c.id, line, c.undo));
                }
                BrainMsg::Confirm {
                    id,
                    text,
                    names,
                    expires_at,
                } => {
                    log::info!(event = "house_confirm", id = id; "house: asks {text:?}");
                    house_cards.push(HouseEvent::Ask(id, text, names, expires_at));
                }
                BrainMsg::ConfirmPending { id, until_ms } => {
                    log::info!(event = "house_confirm_pending", id = id; "house: yes to {id} received");
                    house_cards.push(HouseEvent::Pending(id, until_ms));
                }
                BrainMsg::ConfirmDone {
                    id,
                    yes,
                    via,
                    failed,
                } => {
                    log::info!(event = "house_confirm_done", id = id, answered = yes.is_some(), via = via.as_str(); "house: question {id} over");
                    let failed = failed
                        .iter()
                        .map(|n| l.text("house_failed").replace("{name}", n))
                        .collect();
                    house_cards.push(HouseEvent::AskOver(id, yes, via, failed));
                }
                BrainMsg::ShowView(v) => house_cards.push(HouseEvent::View(v)),
                BrainMsg::TakePhoto => house_cards.push(HouseEvent::Photo),
                BrainMsg::Notes(notes) => {
                    *self.board.borrow_mut() = notes;
                    notes_seen = true;
                    changed = true;
                }
                BrainMsg::Upcoming(items) => {
                    d.memories.events = event_rows(&items, &l, now_unix() as i64);
                    d.events = items;
                    changed = true;
                    events_seen = true;
                }
                BrainMsg::Notify(v) => {
                    let id = v["id"].as_i64().unwrap_or(0);
                    let kind = v["kind"].as_str().unwrap_or("reminder");
                    let title = match kind {
                        "briefing" => l.text("briefing_title").to_string(),
                        "missed" => l.text("reminders_missed").to_string(),
                        _ if v["late"] == true => l.text("reminder_late").to_string(),
                        _ => l.text("reminder_title").to_string(),
                    };
                    let text = v["text"].as_str().unwrap_or("").to_string();
                    if id == 0 || !d.reminders.iter().any(|r| r.0 == id) {
                        log::info!(event = "reminder_card", id = id, kind = kind; "reminder card");
                        d.reminders.push((id, title, text));
                        home = true;
                        // A reminder due now (not the morning summary, not a replay of an old
                        // one): full screen, by day only.
                        let due = v["due_at"].as_i64().unwrap_or(0);
                        let fresh = (now_unix() as i64 - due).abs() < 15 * 60;
                        if kind == "reminder"
                            && id > 0
                            && fresh
                            && !d.night_now
                            && d.notice.is_none()
                        {
                            d.notice = Some(Notice {
                                id,
                                since: Instant::now(),
                                when: local_hm(due).0,
                                sounded: Instant::now(),
                            });
                        }
                    }
                }
            }
        }
        if changed {
            let mv = self.desk.borrow().memories.clone();
            self.screen.set_memories(&mv);
        }
        for (secs, name) in timers {
            self.send(UiEvent::TimerStart(secs, name));
        }
        for id in cancels {
            self.send(UiEvent::TimerCancel(id));
        }
        if let Some(until) = dnd {
            self.send(UiEvent::DoNotDisturb(until));
        }
        for (h, m, days) in alarms {
            self.send(UiEvent::AlarmSave(h, m, days));
        }
        if events_seen && !home {
            self.refresh_home();
        }
        if notes_seen {
            self.refresh_notes();
            if !home {
                self.refresh_home();
            }
        }
        for e in house_cards {
            self.house_event(e);
        }
        if let Some(up) = upcoming_seen {
            let differs = self.desk.borrow().upcoming != up;
            if differs {
                self.desk.borrow_mut().upcoming = up;
                if !home {
                    self.refresh_home();
                }
            }
        }
        if home {
            // The card shows with a soft wake (never the bell); at night only the card.
            if !self.desk.borrow().night_now {
                self.wake(WakeSource::Conversation);
                self.send(UiEvent::Reminder);
            }
            self.refresh_notice();
            self.refresh_home();
        }
    }

    /// A house event of the brain on the screen.
    fn house_event(&self, e: HouseEvent) {
        let now = Instant::now();
        match e {
            HouseEvent::Done(id, line, undo) => {
                // One sound and one bow for a group (one command per light).
                if !self.house.borrow_mut().changed(id, line, undo, now) {
                    self.send(UiEvent::HouseDone);
                    self.motion.borrow_mut().seq.done(now);
                }
                // Cancel was touched while it was being carried out: put it back now.
                if self.house.borrow_mut().take_undo_now() {
                    self.undo_done();
                    return;
                }
                // The card is seen: a soft wake (not the screen asleep by "spegni lo schermo"
                // just before, nor the night's, that the touch is for).
                self.wake(WakeSource::Conversation);
            }
            HouseEvent::Ask(id, text, names, expires_at) => {
                if !self
                    .house
                    .borrow_mut()
                    .confirm(id, text, names, expires_at, self.now())
                {
                    log::info!(event = "house_confirm", id = id; "house: question {id} over already");
                    return;
                }
                // The question needs the screen, whatever it was doing.
                self.motion.borrow_mut().awake.end_sleep();
                self.wake(WakeSource::Conversation);
                self.screen.set_control_open(false);
            }
            HouseEvent::Pending(id, until_ms) => {
                if self.house.borrow_mut().pending(id, until_ms) {
                    self.send(UiEvent::QuestionPending(id, until_ms.max(0) as u64));
                }
            }
            HouseEvent::AskOver(id, yes, via, failed) => {
                self.house
                    .borrow_mut()
                    .confirm_done(id, yes, &via, failed, now);
            }
            HouseEvent::Resubscribed => self.house.borrow_mut().clear_question(),
            HouseEvent::View(v) => {
                self.show_view(v);
                return;
            }
            HouseEvent::Photo => {
                if let Some(me) = self.this.borrow().upgrade() {
                    me.open_camera(true);
                }
                return;
            }
        }
        self.render_house();
    }

    /// Home Assistant's view at what the brain asked (a dashboard, a camera).
    fn show_view(&self, v: brain_link::View) {
        if !self.ha_linked.get() {
            log::info!(event = "show_view", shown = false; "brain: {v:?} not shown: not linked to Home Assistant");
            return;
        }
        if !super::webview::present() {
            log::info!(event = "show_view", shown = false; "brain: {v:?} not shown: no Home Assistant view here");
            return;
        }
        log::info!(event = "show_view", shown = true; "brain: open the view {v:?}");
        self.wake(WakeSource::Conversation);
        self.screen.set_control_open(false);
        super::webview::request_view(v);
    }

    /// The cards' time (every poll): the done card goes, the question expires, the countdown
    /// moves; do not disturb ends at its time.
    fn poll_house(&self) {
        if self.opts.demo.is_some() {
            return;
        }
        let now_unix = now_unix();
        if self.house.borrow_mut().tick(Instant::now(), now_unix) {
            self.render_house();
        }
        if self.house.borrow().popup() {
            let left = self.house.borrow().popup_left(Instant::now());
            self.screen.set_done_popup(true, left);
        }
        let mut left = self.house.borrow().ask_left(now_unix);
        if self.house.borrow().ask.is_none() {
            // A tile's question: its 30 s, then a No.
            if self.board_question_tick(Instant::now()) {
                return;
            }
            if let Some((_, _, _, l)) = self.board_question(Instant::now()) {
                left = l;
            }
        }
        if (left - self.ask_left.get()).abs() >= 0.004 {
            self.ask_left.set(left);
            self.screen.set_ask_left(left);
        }
        let mut dnd = self.dnd.get();
        if dnd.expire(now_unix as u64) {
            self.dnd.set(dnd);
            log::info!(event = "dnd", on = false, by = "time"; "do not disturb: over");
            self.render_dnd();
        }
    }

    /// Both cards as they are now.
    fn render_house(&self) {
        // The core hears of every question that comes and goes (it listens for a spoken yes
        // or no as soon as one opens).
        let closed = self.house.borrow_mut().take_closed();
        let h = self.house.borrow().clone();
        if let Some(c) = closed.filter(|c| self.asking.get() == Some(c.id)) {
            self.asking.set(None);
            self.send(UiEvent::QuestionClosed {
                id: c.id,
                yes: c.yes,
                via: c.via,
            });
        }
        let open = h.ask.as_ref().map(|a| a.id);
        if open != self.asking.get() {
            if let Some(old) = self.asking.get() {
                self.send(UiEvent::QuestionClosed {
                    id: old,
                    yes: None,
                    via: "screen".into(),
                });
            }
            self.asking.set(open);
            if let Some(a) = h.ask.as_ref() {
                self.send(UiEvent::Question(a.id, a.text.clone()));
            }
        }
        match &h.done {
            Some(d) => {
                let (line, name, state) = {
                    let l = self.locale.borrow();
                    d.texts(l.text("house_and_more"), l.text("house_others"))
                };
                self.screen.set_done(true, &line, d.undo);
                self.screen.set_done_parts(&name, &state);
            }
            None => self.screen.set_done(false, "", false),
        }
        self.screen
            .set_done_popup(h.popup(), h.popup_left(Instant::now()));
        // On the home the card is one of the context band's (cards.rs).
        self.refresh_home();
        let left = h.ask_left(self.now());
        self.ask_left.set(left);
        // One question on screen: the brain's, else a tile's (board_ui.rs); a tile's open when
        // the brain asks is a No.
        if h.ask.is_some() {
            self.board_question_yields();
        }
        match (&h.ask, self.board_question(Instant::now())) {
            (Some(a), _) => {
                self.screen.set_ask_icon("");
                self.screen.set_ask(true, &a.text, &a.names_line(), left);
                let title = a.working_title(self.locale.borrow().text("house_others"));
                self.screen
                    .set_ask_working(a.working(), &title, a.undo_wanted());
            }
            (None, Some((title, line, icon, left))) => {
                self.screen.set_ask_icon(&icon);
                self.screen.set_ask(true, &title, &line, left);
                self.screen.set_ask_working(false, "", false);
            }
            (None, None) => {
                self.screen.set_ask_icon("");
                self.screen.set_ask(false, "", "", 0.0);
                self.screen.set_ask_working(false, "", false);
            }
        }
        self.screen.set_done_from_ask(h.popup_from_ask());
    }

    /// The done card's Undo (or Cancel in the question's window once it is done): newest first,
    /// as an undo goes: a later snapshot (a group's, taken after its lights went off) must not
    /// be put back over the earlier ones.
    fn undo_done(&self) {
        let ids = self.house.borrow_mut().undo();
        for id in ids.unwrap_or_default().into_iter().rev() {
            log::info!(event = "house_undo", id = id; "house: undo {id}");
            self.brain_do(brain_link::undo(id));
        }
        self.render_house();
    }

    /// A touch at (x, y) is for a card of the house over Home Assistant's view (the Yes/No
    /// question takes every touch; the done strip its own).
    pub fn house_overlay(&self, x: f32, y: f32) -> bool {
        let h = self.house.borrow();
        if h.ask.is_some() {
            return true;
        }
        // The card: 672 x 88 centred at the top, at the bottom during a conversation (its bar
        // is at the top): companion.slint, DoneCard over a full view.
        let y0 = if self.view.get().in_conversation() {
            688.0
        } else {
            24.0
        };
        h.done.is_some() && (304.0..976.0).contains(&x) && (y0..y0 + 88.0).contains(&y)
    }

    /// "Spegni lo schermo": the backlight off until a touch, a key or the wake word (the
    /// panel stays on: only the backlight goes, as Vol+ and Vol− together do). Over Home
    /// Assistant's view the view closes first.
    fn screen_off(&self) {
        self.screen_off_by("voice");
    }

    fn screen_off_by(&self, by: &str) {
        if self.opts.demo.is_some() {
            return;
        }
        if super::webview::visible() {
            super::webview::request_home();
        }
        self.screen.set_control_open(false);
        log::info!(event = "screen_sleep", by; "screen: off (asked by {by})");
        self.motion.borrow_mut().awake.sleep(Instant::now());
        self.step();
    }

    /// Do not disturb on until `until` (unix s) or off.
    fn set_dnd(&self, until: Option<u64>) {
        let mut d = self.dnd.get();
        d.set(until);
        self.dnd.set(d);
        log::info!(event = "dnd", on = until.is_some(); "do not disturb {}", if until.is_some() { "on" } else { "off" });
        self.render_dnd();
    }

    /// The line over the clock, the tile, the backlight's ceiling.
    fn render_dnd(&self) {
        let line = match self.dnd_until_hm() {
            Some(hm) => {
                let l = self.locale.borrow();
                format!(
                    "{} · {}",
                    l.text("dnd"),
                    l.text("cc_until").replace("{t}", &hm)
                )
            }
            None => String::new(),
        };
        self.screen.set_dnd(self.dnd_on(), &line);
        self.refresh_control();
        self.step();
    }

    /// Until when do not disturb lasts ("07:00"), None when off.
    fn dnd_until_hm(&self) -> Option<String> {
        let d = self.dnd.get();
        if !d.active(now_unix() as u64) {
            return None;
        }
        d.until().map(|u| local_hm(u as i64).0)
    }

    /// The control center's tile: on until the morning (the night's end hour), or off. Through
    /// the core, as the reflex: it answers with UiCommand::DoNotDisturb.
    fn toggle_dnd(&self) {
        let until = if self.dnd_on() {
            None
        } else {
            let now = now_unix() as i64;
            let end = self.desk.borrow().night.2;
            Some(house::next_hour(now, local_offset(now), end) as u64)
        };
        self.send(UiEvent::DoNotDisturb(until));
    }

    /// "What to say", in the language on screen.
    fn refresh_what(&self) {
        let l = self.locale.borrow();
        let rows = house::what_rows(&|k: &str| l.text(k).to_string(), &self.character.name);
        self.screen.set_what(&rows);
    }

    /// The end of a conversation: a sentence heard without an answer is kept too.
    fn end_exchange(&self) {
        let heard = std::mem::take(&mut self.desk.borrow_mut().heard);
        if !heard.trim().is_empty() {
            self.history
                .borrow_mut()
                .push(now_unix() as i64, BRAIN, &heard, "");
            self.refresh_chat();
        }
    }

    /// A timer cancelled or an alarm deleted by touch: hidden at once, really gone after
    /// [`UNDO`] unless "Annulla" is pressed.
    fn remove_later(&self, what: Undo) {
        self.commit_undo();
        self.desk.borrow_mut().undo = Some(what);
        self.refresh_home();
        self.refresh_timers();
        self.refresh_notes();
        let ui = self.this.borrow().clone();
        self.undo_timer.start(TimerMode::SingleShot, UNDO, move || {
            if let Some(ui) = ui.upgrade() {
                ui.commit_undo();
            }
        });
    }

    fn commit_undo(&self) {
        let Some(what) = self.desk.borrow_mut().undo.take() else {
            return;
        };
        match what {
            Undo::Timer(id) => self.send(UiEvent::TimerCancel(id)),
            Undo::Alarm(id) => self.send(UiEvent::AlarmDelete(id)),
            Undo::Note(id) => self.brain_do(brain_link::note_delete(id)),
        }
        self.screen.set_undo("");
    }
}

/// An answer longer than this leaves the bar over Home Assistant's view for Alfred's layout.
pub(crate) const BAR_ANSWER: usize = 70;
/// Who answers (one brain for now: Home Assistant's Assist pipeline).
const BRAIN: &str = "Home Assistant";
/// "Now" in demo mode: 2026-11-03 14:25 UTC (tools/shoot-golden.sh runs with TZ=UTC).
const DEMO_NOW: f64 = 1_793_715_900.0;
/// The evening demos' "now": DEMO_NOW + 4 h 45 min = 19:10, after the sample's sunset.
const DEMO_EVENING: f64 = 17_100.0;
/// DEMO_NOW's date.
const DEMO_DATE: &str = "2026-11-03";
/// The demo conversation's second session (seconds after the first).
const SESSION_DEMO: i64 = 24 * 3600;

/// The control center's brightness by hand: `percent` of the configured full level when
/// awake, the home's dim level in the same proportion.
fn manual_levels(full: Levels, percent: u8) -> Levels {
    let active = (full.active as u64 * percent.clamp(1, 100) as u64 / 100).max(20) as u32;
    let idle = (active as u64 * full.idle as u64 / full.active.max(1) as u64).max(10) as u32;
    Levels { idle, active }
}

/// The Wi-Fi signal as bars (`iw dev <if> link`, no privilege needed): 0 not connected, else
/// 1..4 from the signal in dBm; None without `iw`.
fn wifi_level(interface: &str) -> Option<i32> {
    let out = std::process::Command::new("/usr/sbin/iw")
        .args(["dev", interface, "link"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(parse_wifi_level(&String::from_utf8_lossy(&out.stdout)))
}

/// Bars from `iw dev <if> link`: "Not connected." → 0; "signal: -61 dBm" → 3.
pub fn parse_wifi_level(text: &str) -> i32 {
    let dbm = text.lines().find_map(|l| {
        l.trim()
            .strip_prefix("signal:")
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<i32>().ok())
    });
    match dbm {
        None => 0,
        Some(d) if d >= -55 => 4,
        Some(d) if d >= -65 => 3,
        Some(d) if d >= -75 => 2,
        Some(_) => 1,
    }
}

/// The network a Wi-Fi interface is on (`iw dev <if> info`: nl80211 GET_INTERFACE, no privilege
/// needed); None without `iw` or a network.
fn wifi_ssid(interface: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/iw")
        .args(["dev", interface, "info"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    parse_ssid(&String::from_utf8_lossy(&out.stdout))
}

/// The `ssid` line of `iw dev <if> info`.
/// "Welcome · Benvenuto": each language's word once, in the locales' order.
pub fn welcome_title<'a>(words: impl Iterator<Item = &'a str>) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for w in words.filter(|w| !w.is_empty()) {
        if !seen.contains(&w) {
            seen.push(w);
        }
    }
    seen.join(" · ")
}

/// The first start's Home Assistant page for a search's outcome: its state, the rows (name,
/// URL) and the line of explanation (`word` gives the locale's words with `{name}` filled in).
pub fn find_view(
    o: &crate::ha::discover::Outcome,
    word: &dyn Fn(&str) -> String,
) -> (&'static str, Vec<(String, String)>, String) {
    use crate::ha::discover::Outcome;
    match o {
        Outcome::Found(f) if !f.is_empty() => (
            "found",
            f.iter().map(|f| (f.name.clone(), f.url.clone())).collect(),
            String::new(),
        ),
        Outcome::Found(_) => ("none", Vec::new(), word("find_none_text")),
        Outcome::NotInstalled => ("none", Vec::new(), word("find_missing")),
        Outcome::Failed(e) => (
            "none",
            Vec::new(),
            word("find_failed").replace("{error}", e),
        ),
    }
}

fn parse_ssid(info: &str) -> Option<String> {
    info.lines()
        .find_map(|l| l.trim().strip_prefix("ssid "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// "15/11 · 13:56" of a photo's name ("photo-20261115-135638").
fn photo_caption(stem: &str) -> String {
    let d = stem.strip_prefix("photo-").unwrap_or(stem);
    match (d.get(6..8), d.get(4..6), d.get(9..11), d.get(11..13)) {
        (Some(day), Some(mon), Some(h), Some(m)) => format!("{day}/{mon} · {h}:{m}"),
        _ => String::new(),
    }
}

/// AirPlay's cover art, decoded here and not with `Image::load_from_path`: Slint caches images
/// by path, and every cover is written to the same file. At most 320 px (the page shows 304).
fn load_cover(path: &std::path::Path) -> Option<Image> {
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| log::warn!("cover {}: {e}", path.display()))
        .ok()?;
    let rgba = img.thumbnail(320, 320).to_rgba8();
    let (w, h) = rgba.dimensions();
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba.as_raw(), w, h);
    Some(Image::from_rgba8(buf))
}

/// The memories tile in Altro: what waits to be confirmed comes first (the page is where it is
/// done), else how many memories there are.
fn memories_tile(l: &Locale, known: &str, to_confirm: usize) -> String {
    match to_confirm {
        0 => known.to_string(),
        n => l
            .text("tile_memories_confirm")
            .replace("{n}", &n.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn u1_the_memories_page_reads_the_brain() {
        let a = serde_json::json!({"ok": true, "shown": 1, "profile": "", "notes": "", "facts": [
            {"id": 1, "text": "Il gatto si chiama Neve", "updated": 10, "status": "active"},
            {"id": 2, "text": "La caldaia è in cantina", "updated": 20, "status": "active"},
            {"id": 3, "text": "La vicina si chiama Greta", "updated": 30, "status": "candidate"}
        ]});
        let (facts, confirm, over) = super::memory_lines(&a, |t| t.to_string());
        assert_eq!(
            facts.iter().map(|f| (f.id, f.dim)).collect::<Vec<_>>(),
            vec![(1, false), (2, true)]
        );
        assert_eq!(confirm.len(), 1);
        assert_eq!((confirm[0].id, confirm[0].date.as_str()), (3, "30"));
        assert_eq!(over, 1);
        // An older brain (no shown, no status): every fact known, none dimmed.
        let old = serde_json::json!({"facts": [{"id": 1, "text": "x", "updated": 1}]});
        let (facts, confirm, over) = super::memory_lines(&old, |t| t.to_string());
        assert_eq!(
            (facts.len(), confirm.len(), over, facts[0].dim),
            (1, 0, 0, false)
        );
        // Right confirms, Wrong rejects.
        assert_eq!(super::memory_answer(3, true)["cmd"], "memory.confirm");
        assert_eq!(super::memory_answer(3, false)["cmd"], "memory.delete");
        assert_eq!(super::memory_answer(3, false)["id"], 3);
    }

    use super::*;

    #[test]
    fn a_city_s_days_in_its_own_time() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20395), (2025, 11, 3));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        let (_, cities) = demo_cities();
        // Tokyo, nine hours ahead: at 14:25 UTC it is already the next day there.
        let (w, today) = city_as_weather(&cities[1], DEMO_NOW as i64);
        assert_eq!(today, "2026-11-03");
        assert_eq!(w.unit, "°C");
        let late = DEMO_NOW as i64 + 10 * 3600;
        assert_eq!(city_as_weather(&cities[1], late).1, "2026-11-04");
        assert!(w.today.is_some() && w.next_days.len() == 4);
    }

    #[test]
    fn the_home_pose_of_the_moment() {
        let m = HomeMoment::default();
        assert_eq!(home_state_of(m), State::Idle);
        assert_eq!(home_state_of(HomeMoment { nod: true, ..m }), State::Nod);
        assert_eq!(
            home_state_of(HomeMoment {
                satisfied: true,
                nod: true,
                ..m
            }),
            State::Satisfied
        );
        assert_eq!(
            home_state_of(HomeMoment {
                working: true,
                satisfied: true,
                ..m
            }),
            State::Working
        );
        assert_eq!(
            home_state_of(HomeMoment {
                linking: true,
                working: true,
                ..m
            }),
            State::Link
        );
        assert_eq!(
            home_state_of(HomeMoment {
                mic_off: true,
                linking: true,
                ..m
            }),
            State::Muted
        );
    }

    #[test]
    fn a_card_asks_for_a_downloaded_pose_then_one_of_the_character() {
        let alfred = |s: State| s != State::TimerFinished;
        // Downloaded first, even with a state's name.
        assert_eq!(
            card_look_of("laundry", true, alfred),
            Some(Look::Downloaded("laundry".into()))
        );
        assert_eq!(
            card_look_of("working", true, alfred),
            Some(Look::Downloaded("working".into()))
        );
        // Else the character's own pose by its state's name.
        assert_eq!(
            card_look_of("working", false, alfred),
            Some(Look::Own(State::Working))
        );
        assert_eq!(
            card_look_of("offline", false, alfred),
            Some(Look::Own(State::Offline))
        );
        // A state without a pose of its own, or no such name: the card shows alone.
        assert_eq!(card_look_of("timer_finished", false, alfred), None);
        assert_eq!(card_look_of("laundry", false, alfred), None);
    }

    #[test]
    fn a_card_s_pose_in_place_of_the_moment_s() {
        let card = Some(Look::Own(State::Thinking));
        // On the home a card's pose over any moment's pose.
        for moment in [State::Idle, State::Muted, State::Working, State::Nod] {
            assert_eq!(
                home_look_of(card.clone(), false, moment),
                card.clone().unwrap()
            );
        }
        assert_eq!(
            home_look_of(None, false, State::Muted),
            Look::Own(State::Muted)
        );
        // The settings and More pages keep theirs: the moment's (idle, link, working).
        assert_eq!(
            home_look_of(card.clone(), true, State::Idle),
            Look::Own(State::Idle)
        );
        assert_eq!(
            home_look_of(Some(Look::Downloaded("bbq".into())), true, State::Working),
            Look::Own(State::Working)
        );
    }

    #[test]
    fn a_pose_animation_table() {
        let dir = Path::new("/x/poses");
        let a = pose_animation(
            "file = \"laundry-anim.png\"\nsize = [454, 880]\nframes = [0, 80]\nfps = 16\nrect = [271.0, 44.8, 755.1, 983.5]\n",
            dir,
        )
        .unwrap();
        assert_eq!(a.name, anim::CARD_POSE);
        assert_eq!(a.file, dir.join("laundry-anim.png"));
        assert_eq!(
            (a.size, a.first, a.end, a.fps, a.pause),
            ((454, 880), 0, 80, 16.0, 0.0)
        );
        for bad in [
            "file = \"../poses/idle.png\"\nsize = [1, 1]\nframes = [0, 2]\nfps = 16\nrect = [0.0, 0.0, 1.0, 1.0]\n",
            "file = \"a.png\"\nsize = [1, 1]\nframes = [2, 2]\nfps = 16\nrect = [0.0, 0.0, 1.0, 1.0]\n",
            "file = \"a.png\"\nsize = [1, 1]\nframes = [0, 2]\nfps = 0\nrect = [0.0, 0.0, 1.0, 1.0]\n",
            "file = \"a.png\"\nsize = [1, 1]\nframes = [0, 2]\nfps = 16\nrect = [0.0, 0.0, 1.0, 1.0]\nname = \"idle\"\n",
        ] {
            assert!(pose_animation(bad, dir).is_err(), "{bad}");
        }
    }

    #[test]
    fn memories_tile_says_what_waits() {
        let l = locale::load(&locale::repo_locales(), "it").unwrap();
        assert_eq!(memories_tile(&l, "6 ricordi", 0), "6 ricordi");
        assert_eq!(memories_tile(&l, "6 ricordi", 2), "2 da confermare");
    }
    use crate::ha::discover::{Found, Outcome};

    #[test]
    fn the_home_assistant_page_offers_what_has_more_than_one_option() {
        use crate::ha::setup::{Choice, Choices, Pick};
        let pick = |id: &str, name: &str| Pick {
            id: id.into(),
            name: name.into(),
        };
        let c = Choices {
            ai: vec![pick("a", "Gemini"), pick("b", "Claude")],
            stt: vec![pick("stt.one", "Cloud")],
            tts: vec![pick("tts.one", "Cloud"), pick("tts.two", "Piper")],
            voice: vec![pick("cedar", "Cedar"), pick("marin", "Marin")],
            current: Choice {
                ai: "b".into(),
                stt: "stt.one".into(),
                tts: String::new(),
                voice: String::new(),
            },
            ..Default::default()
        };
        let rows = ha_picks(&c, |k| format!("[{k}]"));
        assert_eq!(
            rows,
            [
                ("ai".into(), "[ha_choice_ai]".into(), "Claude".into()),
                ("tts".into(), "[ha_choice_tts]".into(), "—".into()),
                (
                    "voice".into(),
                    "[ha_choice_voice]".into(),
                    "[ha_choice_voice_default]".into()
                ),
            ]
        );
        assert!(ha_picks(&Choices::default(), |k| k.into()).is_empty());
        let dir = locale::repo_locales();
        for lang in ["it", "en", "nl", "de"] {
            let l = locale::load(&dir, lang).unwrap();
            for k in crate::ha::setup::CHOICES {
                for key in [format!("ha_choice_{k}"), format!("ha_choice_{k}_hint")] {
                    assert_ne!(l.text(&key), key, "{lang}");
                }
            }
            assert_ne!(l.text("ha_choice_voice_default"), "ha_choice_voice_default");
        }
    }

    /// The first-start pages' controls on a headless window: each one does what it says.
    #[test]
    fn first_start_controls_do_something() {
        use slint::Model;
        let dir = std::env::temp_dir().join(format!("home-companion-first-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prepared = super::super::renderer::prepare_fb(
            super::super::renderer::Kind::Headless,
            Path::new(""),
            0,
        )
        .unwrap();
        let character =
            Rc::new(character::load(&character::repo_characters().join("alfred")).unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        let opts = UiOptions {
            locales: locale::repo_locales(),
            language: locale::DEFAULT.into(),
            sample_weather: false,
            demo: None,
            screen: ScreenOptions {
                animations: false,
                ..Default::default()
            },
            state_dir: Some(dir.clone()),
            ha_linked: false,
            ha_host: None,
            setup_at_start: true,
            first_start: crate::settings::FirstStart {
                language: true,
                home_assistant: true,
            },
            link: None,
            update: None,
            panel: None,
        };
        let ui = Ui::new(prepared.screen, character, opts, tx).unwrap();
        let w = &ui.screen;
        // The welcome page first, with every locale.
        assert_eq!(w.get_page(), "welcome");
        assert!(w.get_welcome_rows().row_count() >= 2);
        assert!(w.get_welcome_title().contains("Benvenuto"));
        // A language: applied, told to the core (which saves it and switches the reflexes),
        // then Home Assistant's page, searching at once.
        w.invoke_language_chosen("it".into());
        assert_eq!(w.get_language_name(), "Italiano");
        assert!(
            rx.try_iter()
                .any(|e| matches!(e, CoreEvent::Ui(UiEvent::Language(ref c)) if c == "it"))
        );
        assert_eq!(w.get_page(), "find-ha");
        assert_eq!(w.get_find_state(), "searching");
        // A search's outcome shows rows; "Search again" searches again.
        ui.find.borrow_mut().take();
        let rows = vec![("Home".to_string(), "http://192.0.2.10:8123".to_string())];
        w.set_find("found", &rows, "");
        w.invoke_find_search();
        assert_eq!(w.get_find_state(), "searching");
        // A pick: saved for the next start, "saving" on screen.
        w.invoke_find_pick("http://192.0.2.10:8123".into());
        let s = Settings::in_dir(&dir);
        assert_eq!(s.ha_url().as_deref(), Some("http://192.0.2.10:8123"));
        assert_eq!(w.get_find_state(), "saving");
        assert!(ui.restart_timer.running());
        ui.restart_timer.stop();
        // "Later": not again at start; a pick afterwards brings the link page back.
        w.invoke_setup_later();
        assert!(s.setup_later());
        w.invoke_find_pick("http://192.0.2.11:8123".into());
        assert!(!s.setup_later());
        assert_eq!(s.ha_url().as_deref(), Some("http://192.0.2.11:8123"));
        ui.restart_timer.stop();
        // Settings → how readily he answers to his name: normal at first, one step at a time,
        // never past the ends; each step told to the core (which saves it and applies it).
        assert_eq!(w.get_wake_step(), 0);
        let _ = rx.try_iter().count();
        w.invoke_wake_step_changed(1);
        assert_eq!(w.get_wake_step(), 1);
        w.invoke_wake_step_changed(5);
        assert_eq!(w.get_wake_step(), 1, "clamped");
        w.invoke_wake_step_changed(-1);
        assert_eq!(w.get_wake_step(), -1);
        let sent: Vec<WakeSensitivity> = rx
            .try_iter()
            .filter_map(|e| match e {
                CoreEvent::Ui(UiEvent::WakeSensitivity(s)) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            [
                WakeSensitivity::More,
                WakeSensitivity::More,
                WakeSensitivity::Less
            ]
        );
        // Settings → conversation mode: off at first; the switch shows it, tells the core and
        // writes it.
        assert!(!w.get_conversation_mode());
        w.invoke_conversation_mode_changed(true);
        assert!(w.get_conversation_mode());
        w.invoke_conversation_mode_changed(false);
        let sent: Vec<bool> = rx
            .try_iter()
            .filter_map(|e| match e {
                CoreEvent::Ui(UiEvent::ConversationMode(on)) => Some(on),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [true, false]);
        // … and how many times in a row: three at first, one at a time, never past the ends.
        assert_eq!(w.get_conversation_max(), 3);
        w.invoke_conversation_max_changed(4);
        assert_eq!(w.get_conversation_max(), 4);
        w.invoke_conversation_max_changed(0);
        assert_eq!(w.get_conversation_max(), 1, "clamped");
        w.invoke_conversation_max_changed(11);
        assert_eq!(w.get_conversation_max(), 10, "clamped");
        let sent: Vec<u32> = rx
            .try_iter()
            .filter_map(|e| match e {
                CoreEvent::Ui(UiEvent::ConversationMax(n)) => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [4, 1, 10]);
        drop(ui);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The mirror's swipe home from the home button's edge exists only while mirroring: with the
    /// edge on the right it was a 36 px strip at x >= 1244 on every page, which took every touch
    /// there (a swipe from the right edge of any page went home).
    #[test]
    fn the_mirrors_edge_swipe_only_while_mirroring() {
        use super::super::webview::CasaScreen as _;
        use slint::LogicalPosition;
        use slint::platform::{PointerEventButton, WindowEvent};
        let (ui, dir) = headless_ui("mirror-edge");
        let w = &ui.screen;
        let button = PointerEventButton::Left;
        let drag = |from: (f32, f32), to: (f32, f32)| {
            let at = |p: (f32, f32)| LogicalPosition::new(p.0, p.1);
            w.window()
                .dispatch_event(WindowEvent::PointerMoved { position: at(from) });
            w.window().dispatch_event(WindowEvent::PointerPressed {
                position: at(from),
                button,
            });
            for i in 1..=8 {
                let f = i as f32 / 8.0;
                let p = (from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
                w.window()
                    .dispatch_event(WindowEvent::PointerMoved { position: at(p) });
            }
            w.window().dispatch_event(WindowEvent::PointerReleased {
                position: at(to),
                button,
            });
        };
        w.casa_set_edge("right");
        for page in ["more", "settings"] {
            w.set_page(page.into());
            drag((1270.0, 400.0), (1000.0, 400.0));
            assert_eq!(w.get_page(), page, "a swipe from the right edge of {page}");
        }
        // The mirror's own swipe in from the home button's edge still goes home.
        w.set_mirror(true, "Phone", 1280, 800);
        w.set_page("mirror".into());
        drag((1270.0, 400.0), (1000.0, 400.0));
        assert_eq!(w.get_page(), "home", "the mirror's edge swipe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The board follows the finger: pulled in from the home's edge, across its pages and out
    /// to the right from its first page. Let go short of a quarter of the screen (slowly), it
    /// springs back; past it, it goes on. A drag that starts on a tile never taps it.
    #[test]
    fn the_board_follows_the_finger() {
        use super::super::webview::CasaScreen as _;
        use slint::LogicalPosition;
        use slint::Model;
        use slint::platform::{PointerEventButton, WindowEvent};
        let (ui, dir) = headless_ui("board-drag");
        let w = &ui.screen;
        let (hub, cmds) = std::sync::mpsc::channel();
        *ui.hub_tx.borrow_mut() = Some(hub);
        ui.show_demo_board(1);
        w.casa_set_edge("bottom");
        let button = PointerEventButton::Left;
        let tick = |ms: u64| {
            std::thread::sleep(Duration::from_millis(ms));
            slint::platform::update_timers_and_animations();
        };
        let at = |p: (f32, f32)| LogicalPosition::new(p.0, p.1);
        // Down, eight moves, `hold` ms still, then up (a slow drag is no flick).
        let press = |from: (f32, f32), to: (f32, f32)| {
            w.window()
                .dispatch_event(WindowEvent::PointerMoved { position: at(from) });
            w.window().dispatch_event(WindowEvent::PointerPressed {
                position: at(from),
                button,
            });
            for i in 1..=8 {
                let f = i as f32 / 8.0;
                let p = (from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
                w.window()
                    .dispatch_event(WindowEvent::PointerMoved { position: at(p) });
            }
        };
        let lift = |to: (f32, f32)| {
            w.window().dispatch_event(WindowEvent::PointerReleased {
                position: at(to),
                button,
            });
        };
        let slow = |from: (f32, f32), to: (f32, f32)| {
            press(from, to);
            tick(300);
            lift(to);
        };
        let calls = || {
            cmds.try_iter()
                .filter(|c| matches!(c, HubCmd::Call { .. }))
                .count()
        };
        w.set_page("home".into());
        tick(450);
        // From the home's edge: under the finger while it moves.
        press((1265.0, 400.0), (1065.0, 400.0));
        assert!(w.get_board_finger());
        assert_eq!(w.get_board_pull(), 200.0);
        assert_eq!(w.get_page(), "home");
        tick(300);
        lift((1065.0, 400.0));
        assert_eq!(w.get_page(), "home", "200 px, slowly: back");
        assert!(!w.get_board_finger());
        assert_eq!(w.get_board_pull(), 0.0);
        tick(450);
        slow((1265.0, 400.0), (865.0, 400.0));
        assert_eq!(w.get_page(), "board", "400 px: in");
        tick(450);
        let _ = calls();
        // Across the pages, starting on a tile (the floor lamp).
        press((220.0, 340.0), (20.0, 340.0));
        assert_eq!(w.get_board_slide(), -200.0, "the pages follow the finger");
        tick(300);
        lift((20.0, 340.0));
        assert_eq!(w.get_board_slide(), 0.0, "200 px, slowly: back");
        tick(450);
        assert_eq!(
            (w.get_board_index(), w.get_board_title().as_str()),
            (0, "Living room")
        );
        slow((620.0, 340.0), (220.0, 340.0));
        assert_eq!(w.get_board_slide(), -1280.0, "on its way to the next page");
        tick(300);
        assert_eq!(w.get_board_slide(), 0.0);
        assert_eq!(
            (w.get_board_index(), w.get_board_title().as_str()),
            (1, "Night")
        );
        assert_eq!(
            w.get_board_prev_tiles().row_count(),
            15,
            "the first page beside it"
        );
        assert_eq!(w.get_board_next_tiles().row_count(), 0);
        assert_eq!(calls(), 0, "no tile tapped by a drag");
        // Past the last page: it only gives a little.
        press((620.0, 500.0), (220.0, 500.0));
        assert_eq!(w.get_board_slide(), -100.0);
        lift((220.0, 500.0));
        tick(450);
        assert_eq!(w.get_board_index(), 1);
        // Back to the first, then out to the right: a short one stays, a long one goes home.
        slow((220.0, 500.0), (620.0, 500.0));
        tick(450);
        assert_eq!(w.get_board_index(), 0);
        press((220.0, 500.0), (420.0, 500.0));
        assert_eq!(
            w.get_board_back(),
            200.0,
            "the page goes out with the finger"
        );
        assert_eq!(w.get_board_slide(), 0.0);
        tick(300);
        lift((420.0, 500.0));
        assert_eq!(w.get_page(), "board", "200 px, slowly: back in");
        assert_eq!(w.get_board_back(), 0.0);
        tick(450);
        slow((220.0, 500.0), (620.0, 500.0));
        assert_eq!(w.get_page(), "home", "400 px: home");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A page left alone goes home after a minute; the board stays, like Home Assistant's view.
    #[test]
    fn the_board_stays_when_left_alone() {
        let (ui, dir) = headless_ui("board-alone");
        let w = &ui.screen;
        let long_ago = Instant::now().checked_sub(PAGE_IDLE + Duration::from_secs(1));
        for (page, after) in [("more", "home"), ("board", "board")] {
            w.set_page(page.into());
            ui.desk.borrow_mut().touched = long_ago;
            ui.tick_clock();
            assert_eq!(w.get_page(), after, "{page} left alone");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The board's home button: shown by a touch on the board where Home Assistant's view's
    /// rests, gone 4 s after the last touch; dragged, it rests on the nearest edge, saved for
    /// the view too.
    #[test]
    fn the_boards_home_button() {
        use crate::ui::webview::{controls::Place, saved_place};
        let (ui, dir) = headless_ui("board-home");
        let w = &ui.screen;
        w.set_page("board".into());
        ui.wake(WakeSource::Touch);
        assert!(w.get_board_home_shown());
        let (x, y) = Place::default().xy();
        assert_eq!((w.get_board_home_x(), w.get_board_home_y()), (x, y));
        ui.tick_clock();
        assert!(
            w.get_board_home_shown(),
            "still there right after the touch"
        );
        ui.desk.borrow_mut().touched = Instant::now().checked_sub(Duration::from_secs(5));
        ui.tick_clock();
        assert!(!w.get_board_home_shown(), "gone after a while");
        ui.board_home_moved(30.0, 300.0);
        let p = saved_place(&dir);
        assert_eq!((p.edge.name(), p.offset), ("left", 300.0));
        assert_eq!((w.get_board_home_x(), w.get_board_home_y()), p.xy());
        // A touch on another page does not bring it.
        w.set_page("more".into());
        ui.wake(WakeSource::Touch);
        assert!(!w.get_board_home_shown());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A headless Ui on its own state directory, Home Assistant not linked, no first start.
    /// The board on a real (headless) screen: replaced and saved, followed by the hub, a tap
    /// sent at once and waiting for its state, a failure shown, a shutter asking first, a
    /// level sent at most every 300 ms and when the finger lifts, and one question at a time
    /// with the brain's.
    #[test]
    fn the_boards_taps_levels_and_question() {
        use board_ui::Place;
        use slint::Model;
        let (ui, dir) = headless_ui("board");
        let w = &ui.screen;
        let (hub, cmds) = std::sync::mpsc::channel();
        *ui.hub_tx.borrow_mut() = Some(hub);
        let (b, states, curves) = board_ui::demo_board(false);
        ui.ha_curves.borrow_mut().extend(curves);
        assert!(ui.replace_board(b).is_empty());
        assert_eq!(Settings::in_dir(&dir).board().pages.len(), 2, "saved");
        assert!(w.get_board_handle());
        let want = cmds
            .try_iter()
            .filter_map(|c| match c {
                HubCmd::SetWidgets(w) => Some(w),
                _ => None,
            })
            .last()
            .unwrap();
        assert!(want.tiles.contains(&"cover.living_room".to_string()));
        assert_eq!(
            want.tile_curves,
            [
                "sensor.outdoor_temperature",
                "sensor.power",
                "sensor.indoor_humidity"
            ]
        );
        ui.apply_hub(HubMsg::Tiles(states));
        w.set_page("board".into());
        ui.render_board(false);
        assert_eq!(w.get_board_title(), "Living room");
        assert_eq!((w.get_board_index(), w.get_board_count()), (0, 2));
        let tile = |i: usize| w.get_board_tiles().row_data(i).unwrap();
        assert_eq!(w.get_board_tiles().row_count(), 15);
        assert_eq!(
            (tile(1).name.as_str(), tile(1).state.as_str()),
            ("Floor lamp", "on")
        );
        // A light: at once, then it waits for its state.
        ui.board_tap(Place::Page(0), 2);
        let sent = |cmds: &std::sync::mpsc::Receiver<HubCmd>| {
            cmds.try_iter()
                .filter_map(|c| match c {
                    HubCmd::Call {
                        id,
                        domain,
                        service,
                        entity_id,
                        data,
                    } => Some((id, format!("{domain}.{service}"), entity_id, data)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let calls = sent(&cmds);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            (calls[0].1.as_str(), calls[0].2.as_str()),
            ("homeassistant.toggle", "light.ceiling")
        );
        assert_eq!(tile(2).state, "pending");
        // Refused by Home Assistant: the tile says so.
        ui.apply_hub(HubMsg::Called {
            id: calls[0].0,
            ok: false,
            error: Some("no".into()),
        });
        assert_eq!(
            (tile(2).state.as_str(), tile(2).status.as_str()),
            ("failed", "Didn't work")
        );
        // The shutter asks first; No does nothing, Yes sends it.
        ui.board_tap(Place::Page(0), 6);
        assert!(sent(&cmds).is_empty());
        assert!(w.get_ask_shown());
        assert_eq!(w.get_ask_text(), "Close Shutter?");
        assert_eq!(w.get_ask_names(), "Living room");
        assert!(!w.get_ask_icon_path().is_empty());
        ui.board_answer(true, "screen");
        assert!(!w.get_ask_shown());
        let calls = sent(&cmds);
        assert_eq!(
            (calls[0].1.as_str(), calls[0].2.as_str()),
            ("cover.toggle", "cover.living_room")
        );
        // A level: the first value at once, the next within 300 ms waits, the finger's lift goes.
        ui.board_level(Place::Page(0), 0, 40, false);
        ui.board_level(Place::Page(0), 0, 45, false);
        ui.board_level(Place::Page(0), 0, 55, true);
        let calls = sent(&cmds);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0].3, json!({"brightness_pct": 40}));
        assert_eq!(calls[1].3, json!({"brightness_pct": 55}));
        // One question at a time: the brain's wins over a tile's, and a tile's waits for none.
        ui.board_tap(Place::Page(0), 6);
        assert!(w.get_ask_shown());
        ui.house.borrow_mut().confirm(
            9,
            "Open the valve?".into(),
            Vec::new(),
            now_unix() as i64 + 30,
            ui.now(),
        );
        ui.render_house();
        assert_eq!(w.get_ask_text(), "Open the valve?");
        assert_eq!(w.get_ask_icon_path(), "");
        assert!(ui.tile_board.borrow().ask.is_none(), "the tile's is a No");
        ui.board_tap(Place::Page(0), 6);
        assert!(
            ui.tile_board.borrow().ask.is_none(),
            "refused while the brain asks"
        );
        assert_eq!(w.get_ask_text(), "Open the valve?");
        assert!(sent(&cmds).is_empty());
        // Swiping across the pages.
        ui.house.borrow_mut().clear_question();
        ui.render_house();
        w.invoke_board_swiped(1);
        assert_eq!(w.get_board_title(), "Night");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The board reached with a finger on a real (headless) screen, as on the device: Home
    /// Assistant's view hidden, its home button on the right edge, the normal and the compact
    /// home. A swipe in from the home's right edge opens the board; there a tap on a tile, a
    /// drag on a level's bar and a swipe to the right reach their handlers.
    #[test]
    fn the_board_by_touch_from_the_right_edge() {
        use super::super::webview::CasaScreen as _;
        use slint::LogicalPosition;
        use slint::platform::{PointerEventButton, WindowEvent};
        let (ui, dir) = headless_ui("board-touch");
        let w = &ui.screen;
        let (hub, cmds) = std::sync::mpsc::channel();
        *ui.hub_tx.borrow_mut() = Some(hub);
        ui.show_demo_board(1);
        assert!(w.get_board_handle());
        let button = PointerEventButton::Left;
        let drag = |from: (f32, f32), to: (f32, f32)| {
            let at = |p: (f32, f32)| LogicalPosition::new(p.0, p.1);
            w.window()
                .dispatch_event(WindowEvent::PointerMoved { position: at(from) });
            w.window().dispatch_event(WindowEvent::PointerPressed {
                position: at(from),
                button,
            });
            for i in 1..=8 {
                let f = i as f32 / 8.0;
                let p = (from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f);
                w.window()
                    .dispatch_event(WindowEvent::PointerMoved { position: at(p) });
            }
            w.window().dispatch_event(WindowEvent::PointerReleased {
                position: at(to),
                button,
            });
        };
        // The board's page slides in (400 ms): let it come to its place.
        let settle = || {
            std::thread::sleep(Duration::from_millis(450));
            slint::platform::update_timers_and_animations();
        };
        let calls = || {
            cmds.try_iter()
                .filter_map(|c| match c {
                    HubCmd::Call { entity_id, .. } => Some(entity_id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for edge in ["bottom", "right"] {
            w.casa_set_edge(edge);
            for compact in [false, true] {
                w.set_home_compact(compact);
                let case = format!("edge {edge}, compact {compact}");
                w.set_page("home".into());
                settle();
                // A swipe in from the right edge, even one starting on its outer half.
                for x in [1260.0, 1275.0] {
                    w.set_page("home".into());
                    settle();
                    drag((x, 400.0), (1000.0, 400.0));
                    assert_eq!(w.get_page(), "board", "{case}, from {x}");
                }
                settle();
                let _ = calls();
                // A tap on a tile (the floor lamp).
                drag((120.0, 340.0), (120.0, 340.0));
                assert_eq!(calls(), ["light.floor_lamp"], "{case}: tap");
                // A drag along a level's bar (the living room's lights, four cells of the
                // first row).
                drag((300.0, 200.0), (700.0, 200.0));
                assert!(
                    calls().iter().any(|e| e == "light.living_room"),
                    "{case}: level"
                );
                // A swipe to the right on the first page: back home.
                drag((500.0, 700.0), (800.0, 700.0));
                assert_eq!(w.get_page(), "home", "{case}: back");
                // A tap on the edge strip opens nothing.
                drag((1260.0, 500.0), (1260.0, 500.0));
                assert_eq!(w.get_page(), "home", "{case}: tap");
            }
        }
        // The mirror's own swipe in from the home button's edge (here the right one) goes home.
        w.set_mirror(true, "Phone", 1280, 800);
        w.set_page("mirror".into());
        settle();
        drag((1270.0, 400.0), (1000.0, 400.0));
        assert_eq!(w.get_page(), "home", "the mirror's edge swipe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn headless_ui(name: &str) -> (Rc<Ui<super::super::sw::CompanionWindow>>, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("home-companion-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prepared = super::super::renderer::prepare_fb(
            super::super::renderer::Kind::Headless,
            Path::new(""),
            0,
        )
        .unwrap();
        let character =
            Rc::new(character::load(&character::repo_characters().join("alfred")).unwrap());
        let (tx, _rx) = std::sync::mpsc::channel();
        let opts = UiOptions {
            locales: locale::repo_locales(),
            language: "en".into(),
            sample_weather: false,
            demo: None,
            screen: ScreenOptions {
                animations: false,
                ..Default::default()
            },
            state_dir: Some(dir.clone()),
            ha_linked: false,
            ha_host: None,
            setup_at_start: false,
            first_start: crate::settings::FirstStart {
                language: false,
                home_assistant: false,
            },
            link: None,
            update: None,
            panel: None,
        };
        (Ui::new(prepared.screen, character, opts, tx).unwrap(), dir)
    }

    /// The widget on a real (headless) screen: it turns every 20 s on the home and only there,
    /// writes nothing between two turns, and with no page on the home is as before.
    #[test]
    fn widget_turns_on_the_home_only() {
        let (ui, dir) = headless_ui("widget");
        let w = &ui.screen;
        let now = now_unix() as i64;
        {
            let mut d = ui.desk.borrow_mut();
            d.upcoming = vec![(now + 3600, "Dentist".into())];
            d.shopping = Some(vec![ShopList {
                entity: "todo.shopping".into(),
                name: "Shopping".into(),
                items: vec![("1".into(), "Bread".into()), ("2".into(), "Milk".into())],
            }]);
        }
        // No page on: cards as before, no widget.
        ui.widget_on.borrow_mut().clear();
        ui.refresh_home();
        assert!(!w.get_widget_shown());
        // (Home Assistant is not linked: its status card is first.)
        assert_eq!(*ui.card_kinds.borrow(), ["status", "upcoming", "shopping"]);
        // All pages on: those two cards become the widget's pages, in the two slots left.
        *ui.widget_on.borrow_mut() = WidgetPage::all();
        ui.refresh_home();
        assert_eq!(*ui.card_kinds.borrow(), ["status"]);
        assert!(w.get_widget_shown());
        assert_eq!(w.get_widget_kind(), "next");
        // Two slots free: the one-line size (never a shorter full widget, widgets::size).
        assert_eq!((w.get_widget_slots(), w.get_widget_count()), (1, 2));
        let seq = w.get_widget_seq();
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        for s in 1..20 {
            ui.tick_widget(at(s), false);
            assert_eq!(
                (w.get_widget_kind().as_str(), w.get_widget_seq()),
                ("next", seq)
            );
        }
        ui.tick_widget(at(20), false);
        assert_eq!(w.get_widget_kind(), "shopping");
        assert_eq!(w.get_widget_seq(), seq + 1);
        // Another page: the turn stands still.
        w.set_page("altro".into());
        for s in 21..120 {
            ui.tick_widget(at(s), s % 60 == 0);
        }
        assert_eq!(w.get_widget_kind(), "shopping");
        // Back home: a whole interval, then the next page.
        w.set_page("home".into());
        ui.tick_widget(at(120), false);
        ui.tick_widget(at(139), false);
        assert_eq!(w.get_widget_kind(), "shopping");
        ui.tick_widget(at(140), false);
        assert_eq!(w.get_widget_kind(), "next");
        // A timer card: one slot left.
        ui.desk.borrow_mut().timers = vec![TimerView {
            id: "a".into(),
            name: "Eggs".into(),
            left: "07:42".into(),
            fraction: 0.5,
            paused: false,
            due_unix: 0.0,
        }];
        ui.refresh_home();
        assert_eq!(w.get_widget_slots(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The settings model on a real (headless) screen: every item set through it (as the browser
    /// panel does) changes what the screen shows or does, and the file.
    #[test]
    fn settings_model_applies_on_the_screen() {
        let (ui, dir) = headless_ui("model");
        let set = |k: &str, v: serde_json::Value| model::set(&*ui, k, &v).unwrap();
        set("home_compact", json!(true));
        assert!(ui.home_compact.get());
        assert!(ui.screen.get_home_compact());
        set("night_start", json!(21));
        set("night_on", json!(false));
        assert_eq!(ui.desk.borrow().night, (false, 21, 7));
        set("widgets", json!(["weather"]));
        assert_eq!(*ui.widget_on.borrow(), [WidgetKind::Weather]);
        set("widgets_every", json!("0"));
        assert_eq!(ui.widget_every.get(), None);
        set("keep_conversations", json!(false));
        assert!(!ui.history.borrow().keeps());
        set("wake_sensitivity", json!("less"));
        assert_eq!(ui.screen.get_wake_step(), -1);
        set("conversation_mode", json!(true));
        assert!(ui.screen.get_conversation_mode());
        assert_eq!(model::get(&*ui, "conversation_mode"), Some(json!(true)));
        set("conversation_max", json!(5));
        assert_eq!(ui.screen.get_conversation_max(), 5);
        set("volume", json!(55));
        assert_eq!(ui.volume.get(), 55);
        assert_eq!(model::get(&*ui, "volume"), Some(json!(55)));
        set("language", json!("it"));
        assert_eq!(ui.locale.borrow().code, "it");
        assert_eq!(model::get(&*ui, "language"), Some(json!("it")));
        ui.choice.borrow_mut().known.todo =
            vec![("todo.a".into(), "A".into()), ("todo.b".into(), "B".into())];
        set("lists", json!(["todo.b"]));
        assert_eq!(ui.choice.borrow().lists, ["todo.b"]);
        // Written a second later, all at once.
        assert!(!Settings::in_dir(&dir).home_compact());
        ui.flush_settings();
        let saved = Settings::in_dir(&dir);
        assert!(saved.home_compact());
        assert_eq!(saved.night(), (false, 21, 7));
        assert!(saved.conversation_mode());
        assert_eq!(saved.conversation_max(), 5);
        assert_eq!(saved.widgets(), (vec![WidgetKind::Weather.into()], 0));
        assert_eq!(saved.lists(), ["todo.b"]);
        assert_eq!(saved.language().as_deref(), Some("it"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The browser panel on a real (headless) screen, touched where a finger would: the switch,
    /// a request with its dialog (Allow, Refuse), the QR code's ticket, Revoke (twice), and the
    /// cases where nothing may be asked.
    #[test]
    fn panel_access_from_the_screen() {
        use slint::LogicalPosition;
        use slint::Model;
        use slint::platform::{PointerEventButton, WindowEvent};
        let (ui, dir) = headless_ui("panel");
        let w = &ui.screen;
        let tap = |x: f32, y: f32| {
            let position = LogicalPosition::new(x, y);
            let button = PointerEventButton::Left;
            w.window()
                .dispatch_event(WindowEvent::PointerPressed { position, button });
            w.window()
                .dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        let ask = |peer: &str| {
            ui.panel_sessions.borrow_mut().skip_pause();
            ui.panel_call(&json!({"op": "pair.request", "peer": peer,
                "agent": {"browser": "Chrome", "os": "macOS"}}))
        };
        // Off by default: nothing is asked.
        assert_eq!(ask("192.0.2.24")["error"], "off");
        assert_eq!(w.get_dialog(), "");
        // Settings → Access from the browser → the switch.
        w.set_page("panel".into());
        ui.refresh_settings();
        assert!(!w.get_panel_on());
        tap(1172.0, 150.0);
        assert!(w.get_panel_on());
        ui.flush_settings();
        assert!(Settings::in_dir(&dir).panel_on());
        assert!(w.get_panel_address().starts_with("http://"));
        // A request: the dialog with the code; Allow lets the browser in.
        let r = ask("192.0.2.24");
        assert_eq!(w.get_dialog(), "panel-ask");
        assert_eq!(w.get_panel_ask_code(), r["code"].as_str().unwrap());
        assert_eq!(w.get_panel_ask_line(), "Chrome on macOS · 192.0.2.24");
        // A second one while it waits: busy, the dialog stays the first's.
        assert_eq!(ask("192.0.2.25")["error"], "busy");
        // Just appeared: a tap does not answer it yet (700 ms, a timer in the real loop).
        tap(975.0, 572.0);
        assert_eq!(w.get_dialog(), "panel-ask");
        w.set_panel_ask_ready(true);
        tap(975.0, 572.0);
        assert_eq!(w.get_dialog(), "");
        let st = ui.panel_call(&json!({"op": "pair.status", "id": r["id"]}));
        assert_eq!(st["status"], "allowed");
        let token = st["token"].as_str().unwrap().to_string();
        let hello = ui.panel_call(&json!({"op": "hello", "token": token}));
        assert_eq!(hello["session"], true);
        ui.poll_panel();
        assert_eq!(w.get_panel_devices().row_count(), 1);
        // Refuse (the page closed: with it open, requests are let through again).
        w.set_page("settings".into());
        let r = ask("192.0.2.26");
        w.set_panel_ask_ready(true);
        tap(680.0, 572.0);
        assert_eq!(w.get_dialog(), "");
        let st = ui.panel_call(&json!({"op": "pair.status", "id": r["id"]}));
        assert_eq!(st["status"], "refused");
        // Unanswered: the dialog goes when the minute is over.
        let r = ask("192.0.2.27");
        assert_eq!(w.get_dialog(), "panel-ask");
        ui.panel_sessions
            .borrow_mut()
            .expire(crate::panel::sessions::Now::system().plus(Duration::from_secs(61)));
        ui.tick_panel();
        assert_eq!(w.get_dialog(), "");
        let st = ui.panel_call(&json!({"op": "pair.status", "id": r["id"]}));
        assert_eq!(st["status"], "expired");
        // Two failures, then a third: requests stop and the home has a card.
        ui.panel.borrow_mut().blocked = false;
        let r = ask("192.0.2.28");
        assert!(r["ok"].as_bool().unwrap());
        w.set_panel_ask_ready(true);
        tap(680.0, 572.0);
        ui.tick_panel();
        assert!(ui.panel.borrow().blocked);
        assert!(ui.card_kinds.borrow().contains(&"panel"));
        assert_eq!(ask("192.0.2.29")["error"], "blocked");
        // Opening the page lets requests through again.
        w.set_page("panel".into());
        ui.tick_panel();
        assert!(!ui.panel.borrow().blocked);
        // The QR code's ticket lets in once.
        w.set_page("panel".into());
        ui.refresh_panel();
        let ticket = ui.panel.borrow().ticket_shown.clone().unwrap();
        let q = ui.panel_call(&json!({"op": "pair.qr", "ticket": ticket, "peer": "192.0.2.40"}));
        assert_eq!(q["ok"], true);
        let again =
            ui.panel_call(&json!({"op": "pair.qr", "ticket": ticket, "peer": "192.0.2.41"}));
        assert_eq!(again["error"], "bad_ticket");
        ui.poll_panel();
        assert_eq!(w.get_panel_devices().row_count(), 2);
        // A new ticket on the next tick.
        ui.tick_panel();
        assert_ne!(
            ui.panel.borrow().ticket_shown.as_deref(),
            Some(ticket.as_str())
        );
        // Revoke: the first touch arms it, the second revokes.
        let first = w.get_panel_devices().row_data(0).unwrap().id.to_string();
        w.invoke_panel_revoke(first.clone().into());
        assert_eq!(w.get_panel_armed(), first.as_str());
        assert_eq!(w.get_panel_devices().row_count(), 2);
        w.invoke_panel_revoke(first.into());
        assert_eq!(w.get_panel_devices().row_count(), 1);
        // Switched off: everything answers off, the devices stay.
        tap(1172.0, 150.0);
        assert!(!w.get_panel_on());
        assert_eq!(
            ui.panel_call(&json!({"op": "state", "token": token}))["error"],
            "off"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The remote screen: its switch on the panel's page, the sign while a device is in
    /// control, Stop on the screen, touches back into the loop, private pages never sent.
    #[test]
    fn remote_screen_from_the_screen() {
        use slint::LogicalPosition;
        use slint::platform::{PointerEventButton, WindowEvent};
        let (ui, dir) = headless_ui("remote");
        let w = &ui.screen;
        let tap = |x: f32, y: f32| {
            let position = LogicalPosition::new(x, y);
            let button = PointerEventButton::Left;
            w.window()
                .dispatch_event(WindowEvent::PointerPressed { position, button });
            w.window()
                .dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        let (tx, rx) = std::sync::mpsc::channel();
        ui.set_remote_io(crate::ui::capture::Capture::unavailable("renderer"), tx);
        crate::settings_model::commit(&*ui, Change::Panel(true));
        ui.panel_sessions.borrow_mut().skip_pause();
        let r = ui.panel_call(&json!({"op": "pair.request", "peer": "192.0.2.24",
            "agent": {"browser": "Chrome", "os": "macOS"}}));
        ui.panel_answered_for_test(true);
        w.set_dialog("".into());
        let token = ui.panel_call(&json!({"op": "pair.status", "id": r["id"]}))["token"]
            .as_str()
            .unwrap()
            .to_string();
        let call = |op: &str, extra: serde_json::Value| {
            let mut r = json!({"op": op, "token": token, "peer": "192.0.2.24"});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            ui.panel_call(&r)
        };
        // Off by default.
        assert_eq!(call("screen.start", json!({}))["error"], "screen_off");
        // The switch under the panel's.
        w.set_page("panel".into());
        ui.refresh_settings();
        assert!(!w.get_remote_on());
        tap(1172.0, 234.0);
        assert!(w.get_remote_on());
        ui.flush_settings();
        assert!(Settings::in_dir(&dir).panel_screen());
        // The panel's page is never sent (its QR code lets in).
        assert_eq!(call("screen.start", json!({}))["ok"], true);
        assert_eq!(w.get_remote_sign(), "Controlled by Chrome on macOS");
        // Where the sign is, for the real finger's Stop over Home Assistant's view.
        let (sx, sy, sw, sh) = w.remote_sign_rect().unwrap();
        assert!(sw > 200.0 && sh >= 56.0, "{sw}x{sh}");
        assert!(ui.remote_sign_hit(sx + sw - 20.0, sy + sh / 2.0));
        assert!(!ui.remote_sign_hit(sx - 5.0, sy + sh / 2.0));
        assert!(!ui.remote_sign_hit(sx + 10.0, sy + sh + 5.0));
        assert_eq!(call("screen.frame", json!({}))["hidden"], "code");
        assert_eq!(
            call(
                "screen.input",
                json!({"events": [{"k": "d", "x": 0.5, "y": 0.5}]})
            )["error"],
            "screen_hidden"
        );
        assert_eq!(call("screen.home", json!({}))["ok"], true);
        assert_eq!(w.get_page(), "home");
        // Home: frames (none on this renderer), touches into the loop in logical pixels.
        assert_eq!(
            call("screen.frame", json!({}))["error"],
            "screen_unavailable"
        );
        assert_eq!(
            call(
                "screen.input",
                json!({"events": [{"k": "d", "x": 0.5, "y": 0.25}]})
            )["ok"],
            true
        );
        let (epoch, t) = rx.try_recv().unwrap();
        assert_eq!((t.x, t.y), (640.0, 200.0));
        assert!(ui.remote_accepts(epoch));
        // The confirmations of an update and of Home Assistant's unlink: not sent, not touched,
        // and a touch queued before the dialog opened is not dispatched.
        // So are the other confirmations (forget, clear, suspend, a photo, revoking every
        // device): only the dialogs named as allowed are sent and touched.
        for d in [
            "update",
            "unlink",
            "forget",
            "clear",
            "suspend",
            "photo-delete",
            "panel-revoke-all",
            "a-dialog-added-later",
        ] {
            w.set_dialog(d.into());
            assert_eq!(call("screen.frame", json!({}))["hidden"], "confirm", "{d}");
            assert!(!ui.remote_accepts(epoch), "{d}");
        }
        w.set_dialog("new-timer".into());
        assert!(ui.remote_accepts(epoch));
        w.set_dialog("".into());
        assert!(ui.remote_accepts(epoch));
        // A Yes/No question of the house, a board tile's or the brain's: not sent, not touched
        // (a remote finger never answers it).
        let (b, states, _) = board_ui::demo_board(false);
        ui.tile_board.borrow_mut().states = states;
        ui.apply_board(b);
        ui.board_tap(board_ui::Place::Page(0), 6);
        assert!(w.get_ask_shown());
        assert_eq!(call("screen.frame", json!({}))["hidden"], "confirm");
        assert!(!ui.remote_accepts(epoch));
        ui.board_answer(false, "screen");
        assert!(!w.get_ask_shown());
        assert!(ui.remote_accepts(epoch));
        ui.house.borrow_mut().confirm(
            7,
            "Close the shutter?".into(),
            Vec::new(),
            now_unix() as i64 + 30,
            ui.now(),
        );
        ui.render_house();
        assert!(w.get_ask_shown());
        assert_eq!(call("screen.frame", json!({}))["hidden"], "confirm");
        assert!(!ui.remote_accepts(epoch));
        ui.house.borrow_mut().clear_question();
        ui.render_house();
        assert!(ui.remote_accepts(epoch));
        // The memories and the conversations: read only on the device.
        for page in ["memories", "chat"] {
            w.set_page(page.into());
            assert_eq!(
                call("screen.frame", json!({}))["hidden"],
                "private",
                "{page}"
            );
            assert!(!ui.remote_accepts(epoch), "{page}");
        }
        w.set_page("home".into());
        assert!(ui.remote_accepts(epoch));
        // Ended while the sessions were busy answering: no sign, no frame, then the tick ends it.
        {
            let _busy = ui.panel_sessions.borrow_mut();
            ui.remote_end(crate::panel::remote::End::Screen);
        }
        assert_eq!(w.get_remote_sign(), "");
        assert!(
            !ui.remote_accepts(epoch),
            "touches of the old session dropped"
        );
        assert_eq!(ui.remote_hidden_now(), Some("ended"));
        assert!(ui.panel_sessions.borrow().remote.active());
        ui.tick_remote();
        assert!(!ui.panel_sessions.borrow().remote.active());
        assert_eq!(call("screen.start", json!({}))["error"], "screen_stopped");
        ui.panel_sessions.borrow_mut().remote = Default::default();
        assert_eq!(call("screen.start", json!({}))["ok"], true);
        // Stop on the sign: gone at once, and nobody may start for a while.
        w.invoke_remote_stop();
        assert_eq!(w.get_remote_sign(), "");
        assert_eq!(call("screen.start", json!({}))["error"], "screen_stopped");
        // The switch off ends a session too.
        ui.panel_sessions.borrow_mut().remote = Default::default();
        assert_eq!(call("screen.start", json!({}))["ok"], true);
        crate::settings_model::commit(&*ui, Change::PanelScreen(false));
        assert_eq!(w.get_remote_sign(), "");
        assert!(!ui.panel_sessions.borrow().remote.active());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing is asked at night, with do not disturb, during a conversation or over a dialog.
    #[test]
    fn panel_asks_only_when_it_may() {
        let (ui, dir) = headless_ui("panel-quiet");
        crate::settings_model::commit(&*ui, Change::Panel(true));
        let ask = |peer: &str| ui.panel_call(&json!({"op": "pair.request", "peer": peer}));
        ui.desk.borrow_mut().night_now = true;
        assert_eq!(ask("192.0.2.1")["error"], "cannot_ask");
        ui.desk.borrow_mut().night_now = false;
        ui.view.set(Phase::Speaking);
        assert_eq!(ask("192.0.2.1")["error"], "cannot_ask");
        ui.view.set(Phase::Home);
        ui.screen.set_dialog("suspend".into());
        assert_eq!(ask("192.0.2.1")["error"], "cannot_ask");
        ui.screen.set_dialog("".into());
        ui.set_dnd(Some(now_unix() as u64 + 600));
        assert_eq!(ask("192.0.2.1")["error"], "cannot_ask");
        ui.set_dnd(None);
        // Put to sleep with Vol+ and Vol−: nothing lights it up until it wakes.
        ui.panel.borrow_mut().keys_asleep = true;
        assert_eq!(ask("192.0.2.1")["error"], "cannot_ask");
        ui.panel.borrow_mut().keys_asleep = false;
        let r = ask("192.0.2.1");
        assert_eq!(r["ok"], true);
        assert_eq!(ui.screen.get_dialog(), "panel-ask");
        // Switched off while it waits: the dialog goes, the browser hears no, nobody counted.
        crate::settings_model::commit(&*ui, Change::Panel(false));
        assert_eq!(ui.screen.get_dialog(), "");
        crate::settings_model::commit(&*ui, Change::Panel(true));
        let st = ui.panel_call(&json!({"op": "pair.status", "id": r["id"]}));
        assert_eq!(st["status"], "refused");
        ui.panel_sessions.borrow_mut().skip_pause();
        assert_eq!(ask("192.0.2.1")["ok"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tap on the markets' widget opens their page with a card each; on the cities' widget,
    /// the city page, whose chips change the city; on now playing, nothing.
    #[test]
    fn widget_taps_open_their_pages() {
        use slint::LogicalPosition;
        use slint::platform::{PointerEventButton, WindowEvent};
        let (ui, dir) = headless_ui("widget-taps");
        let w = &ui.screen;
        let tap = |x: f32, y: f32| {
            let position = LogicalPosition::new(x, y);
            let button = PointerEventButton::Left;
            w.window()
                .dispatch_event(WindowEvent::PointerPressed { position, button });
            w.window()
                .dispatch_event(WindowEvent::PointerReleased { position, button });
            slint::platform::update_timers_and_animations();
        };
        let show = |kind: WidgetKind| {
            *ui.widget_on.borrow_mut() = vec![kind.into()];
            ui.refresh_home();
            assert_eq!(ui.widget.borrow().kind(), Some(kind));
            w.set_page("home".into());
        };
        ui.widget_data.borrow_mut().quotes = demo_quotes()
            .into_iter()
            .map(|mut t| {
                t.at = now_unix() as i64;
                t
            })
            .collect();
        let choice = |t: &widgets::Trend| crate::feeds::QuoteChoice {
            source: crate::feeds::Source::Yahoo,
            id: t.key["yahoo:".len()..].to_string(),
            name: t.name.clone(),
        };
        let demo: Vec<_> = demo_quotes().iter().map(choice).collect();
        *ui.widget_quotes.borrow_mut() = Lists::one(demo.clone());
        show(WidgetKind::Quotes);
        tap(800.0, 500.0);
        assert_eq!(w.get_page(), "trends");
        assert_eq!(w.get_trends_kind(), "quotes");
        ui.on_page("trends");
        assert_eq!(slint::Model::row_count(&w.get_trend_rows()), 5);
        // A second markets' page with its own two: its own page in the turn, and a tap on it
        // opens its own markets.
        let second = WidgetPage::new(WidgetKind::Quotes, 2);
        ui.widget_order.borrow_mut().push(second);
        ui.widget_quotes.borrow_mut().set(1, demo[..3].to_vec());
        ui.widget_quotes.borrow_mut().set(2, demo[3..].to_vec());
        *ui.widget_on.borrow_mut() = vec![WidgetKind::Quotes.into(), second];
        w.set_page("home".into());
        ui.refresh_home();
        assert_eq!(w.get_widget_count(), 2);
        if ui.widget.borrow().page() != Some(second) {
            let s = ui.widget.borrow_mut().swipe(1, Instant::now()).unwrap();
            ui.screen.set_widget(&s);
        }
        assert_eq!(ui.widget.borrow().page(), Some(second));
        assert_eq!(w.get_widget_title(), "Markets 2");
        tap(800.0, 500.0);
        assert_eq!(w.get_page(), "trends");
        ui.on_page("trends");
        assert_eq!(slint::Model::row_count(&w.get_trend_rows()), 2);
        assert_eq!(w.get_trends_title(), "Markets 2");
        ui.widget_order.borrow_mut().retain(|p| *p != second);
        let (choices, mut weather) = demo_cities();
        for c in &mut weather {
            c.at = now_unix() as i64;
        }
        *ui.widget_cities.borrow_mut() = Lists::one(choices);
        *ui.city_weather.borrow_mut() = weather;
        show(WidgetKind::Cities);
        tap(800.0, 500.0);
        assert_eq!(w.get_page(), "city");
        ui.on_page("city");
        assert_eq!(w.get_city_title(), "Berlin");
        // The second chip: Tokyo.
        tap(796.0, 144.0);
        assert_eq!(w.get_city_title(), "Tokyo");
        // The board: its page from the widget, a note removed waits for Undo.
        *ui.board.borrow_mut() = vec![
            brain_link::Note {
                id: 4,
                text: "Water the plants".into(),
                created: 1,
            },
            brain_link::Note {
                id: 5,
                text: "Call the plumber".into(),
                created: 2,
            },
        ];
        ui.refresh_notes();
        show(WidgetKind::Notes);
        tap(800.0, 500.0);
        assert_eq!(w.get_page(), "notes");
        assert_eq!(slint::Model::row_count(&w.get_note_rows()), 2);
        ui.remove_later(Undo::Note(4));
        assert_eq!(slint::Model::row_count(&w.get_note_rows()), 1);
        assert_eq!(w.get_undo_text(), "Note removed");
        ui.undo_timer.stop();
        ui.desk.borrow_mut().undo = None;
        ui.refresh_notes();
        assert_eq!(slint::Model::row_count(&w.get_note_rows()), 2);
        ui.widget_data.borrow_mut().playing = Some(widgets::Playing {
            title: "Song".into(),
            artist: "Band".into(),
            player: "Speaker".into(),
        });
        show(WidgetKind::Playing);
        assert!(!w.get_widget_tappable());
        tap(800.0, 500.0);
        assert_eq!(w.get_page(), "home");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other cities follow the home's unit when Home Assistant's weather tells it.
    #[test]
    fn cities_follow_the_home_s_unit() {
        let (ui, dir) = headless_ui("cities-unit");
        assert!(!ui.fahrenheit.get());
        let mut w = Weather::sample();
        w.unit = "°F".into();
        ui.set_weather(Some(w));
        assert!(ui.fahrenheit.get());
        ui.set_weather(None);
        assert!(ui.fahrenheit.get(), "a weather gone keeps the unit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Home Assistant's data reaches the widget: each page from its message, a sensor's curve
    /// when it comes; only what the pages that are on show is asked.
    #[test]
    fn home_assistant_data_in_the_widget() {
        let (ui, dir) = headless_ui("widget-ha");
        let (data, curves) = demo_ha(&ui.locale.borrow());
        ui.apply_hub(HubMsg::Widgets(data));
        {
            let d = ui.widget_data.borrow();
            assert_eq!(d.sensors.len(), 4);
            assert!(d.sensors[0].points.is_empty() && d.sensors[0].prev.is_none());
            assert_eq!(d.check.len(), 2);
            assert!(d.playing.is_some());
        }
        let (k, c) = curves
            .into_iter()
            .find(|(k, _)| k == "sensor.outdoor")
            .unwrap();
        let first = c.first;
        ui.apply_hub(HubMsg::Curve(k, Some(c)));
        let d = ui.widget_data.borrow();
        assert_eq!(d.sensors[0].prev, Some(first));
        assert!(!d.sensors[0].points.is_empty());
        drop(d);
        *ui.widget_ha.borrow_mut() = crate::ha::widgets::Want {
            sensors: vec!["sensor.a".into()],
            player: "media_player.b".into(),
            people: Some(vec![]),
            openings: true,
            batteries: true,
            ..Default::default()
        };
        *ui.widget_on.borrow_mut() = vec![WidgetKind::Sensors.into()];
        let w = ui.widget_want();
        assert_eq!(w.sensors, ["sensor.a"]);
        assert!(w.player.is_empty() && w.people.is_none() && !w.openings && !w.batteries);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Settings → Screen → Widgets, touched where a finger would: each switch takes its page
    /// out of the turn (and Next and Shopping back to cards), the stepper changes the interval,
    /// both saved.
    #[test]
    fn widget_settings_switches_do_something() {
        use slint::LogicalPosition;
        use slint::platform::{PointerEventButton, WindowEvent};
        let (ui, dir) = headless_ui("widget-settings");
        let w = &ui.screen;
        let tap = |x: f32, y: f32| {
            let position = LogicalPosition::new(x, y);
            let button = PointerEventButton::Left;
            w.window()
                .dispatch_event(WindowEvent::PointerPressed { position, button });
            w.window()
                .dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        ui.desk.borrow_mut().shopping = Some(vec![ShopList {
            entity: "todo.shopping".into(),
            name: "Shopping".into(),
            items: vec![("1".into(), "Bread".into())],
        }]);
        ui.refresh_home();
        assert!(!ui.card_kinds.borrow().contains(&"shopping"));
        use slint::Model;
        w.set_page("widgets".into());
        ui.refresh_settings();
        let on = |i: usize| w.get_widget_pages().row_data(i).unwrap().on;
        let key = |i: usize| w.get_widget_pages().row_data(i).unwrap().key.to_string();
        assert_eq!(w.get_widget_pages().row_count(), WidgetKind::ALL.len());
        assert_eq!(
            (key(0), key(2), key(9)),
            ("next".into(), "weather".into(), "home".into())
        );
        assert!(on(0) && on(2) && !on(9));
        assert_eq!(w.get_widget_summary(), "10 on");
        // The pages chosen from the browser say so under their switch.
        assert_eq!(
            w.get_widget_pages().row_data(1).unwrap().hint,
            "Chosen from the browser"
        );
        // The first switch: Next off, a card again.
        tap(1172.0, 258.0);
        assert!(!on(0));
        assert!(!ui.widget_on.borrow().contains(&WidgetKind::Next.into()));
        assert_eq!(w.get_widget_summary(), "9 on");
        // The markets (a taller row) and the weather.
        tap(1172.0, 334.0);
        tap(1172.0, 438.0);
        assert!(!on(1) && !on(2));
        assert_eq!(
            ui.widget_on.borrow()[..2],
            [WidgetKind::Cities, WidgetKind::Shopping]
        );
        tap(1172.0, 438.0);
        assert!(on(2));
        assert_eq!(ui.widget_on.borrow()[0], WidgetKind::Weather);
        // "Change every": + to 30 s, 1 min, by hand (and no further), − back.
        assert_eq!(w.get_widget_every(), 20);
        tap(1184.0, 166.0);
        assert_eq!(w.get_widget_every(), 30);
        tap(1184.0, 166.0);
        tap(1184.0, 166.0);
        tap(1184.0, 166.0);
        assert_eq!(w.get_widget_every(), 0);
        assert_eq!(ui.widget_every.get(), None);
        tap(960.0, 166.0);
        assert_eq!(w.get_widget_every(), 60);
        // Saved.
        ui.flush_settings();
        let saved = Settings::in_dir(&dir).widgets();
        assert_eq!(saved.1, 60);
        assert_eq!(saved.0[0], WidgetKind::Weather);
        assert!(
            !saved.0.contains(&WidgetKind::Next.into())
                && !saved.0.contains(&WidgetKind::Home.into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The OK on one of Home Assistant's cards fires `home_companion_card_ok` with the card's id:
    /// by day, with do not disturb on and with the night layout; never for a card without an id,
    /// one that Home Assistant takes away, or a second tap on a card that is gone.
    #[test]
    fn card_ok_tells_home_assistant() {
        use crate::ha::events::{CardEvent, HaCard};
        let dir =
            std::env::temp_dir().join(format!("home-companion-cardok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prepared = super::super::renderer::prepare_fb(
            super::super::renderer::Kind::Headless,
            Path::new(""),
            0,
        )
        .unwrap();
        let character =
            Rc::new(character::load(&character::repo_characters().join("alfred")).unwrap());
        let (tx, events) = std::sync::mpsc::channel();
        let opts = UiOptions {
            locales: locale::repo_locales(),
            language: locale::DEFAULT.into(),
            sample_weather: false,
            demo: None,
            screen: ScreenOptions {
                animations: false,
                ..Default::default()
            },
            state_dir: Some(dir.clone()),
            ha_linked: false,
            ha_host: None,
            setup_at_start: false,
            first_start: crate::settings::FirstStart {
                language: false,
                home_assistant: false,
            },
            link: None,
            update: None,
            panel: None,
        };
        let ui = Ui::new(prepared.screen, character, opts, tx).unwrap();
        let (hub, fired) = std::sync::mpsc::channel();
        ui.set_hub(hub, Vec::new());
        let show = |id: Option<&str>| {
            HubMsg::Card(CardEvent::Show(HaCard {
                id: id.map(String::from),
                title: Some("Task".into()),
                text: "Text".into(),
                icon: None,
                seconds: None,
                pose: None,
                quiet: false,
            }))
        };
        // The OK of the first card of Home Assistant on the home, whatever else is shown.
        let tap = |ui: &Ui<_>| {
            let i = ui
                .card_kinds
                .borrow()
                .iter()
                .position(|k| cards::HA_KINDS.contains(k))
                .expect("a card of Home Assistant on the home");
            ui.ha_card_ok(i)
        };
        let ids = |rx: &std::sync::mpsc::Receiver<HubCmd>| -> Vec<(String, serde_json::Value)> {
            rx.try_iter()
                .filter_map(|c| match c {
                    HubCmd::Fire { event_type, data } => Some((event_type, data)),
                    _ => None,
                })
                .collect()
        };
        let ok = |id: &str| {
            vec![(
                "home_companion_card_ok".to_string(),
                serde_json::json!({"id": id}),
            )]
        };

        // By day: the tap takes the card away and tells Home Assistant its id.
        ui.apply_hub(show(Some("a")));
        assert!(tap(&ui));
        assert_eq!(ids(&fired), ok("a"));
        // Taken away by Home Assistant: nothing fired.
        ui.apply_hub(show(Some("b")));
        ui.apply_hub(HubMsg::Card(CardEvent::Dismiss("b".into())));
        assert!(ids(&fired).is_empty());
        // A card without an id: the tap works, no event.
        ui.apply_hub(show(None));
        assert!(tap(&ui));
        assert!(ids(&fired).is_empty());
        // A card sounds; the same card sent again `quiet` (fresh values) does not, nor a new
        // quiet one, and it still replaces the card by its id.
        let reminders = |rx: &std::sync::mpsc::Receiver<CoreEvent>| {
            rx.try_iter()
                .filter(|e| matches!(e, CoreEvent::Ui(UiEvent::Reminder)))
                .count()
        };
        let quiet = |id: &str, text: &str| {
            HubMsg::Card(CardEvent::Show(HaCard {
                id: Some(id.into()),
                text: text.into(),
                quiet: true,
                ..Default::default()
            }))
        };
        reminders(&events);
        ui.apply_hub(show(Some("kamado")));
        assert_eq!(reminders(&events), 1);
        ui.apply_hub(quiet("kamado", "T1 180 °C"));
        ui.apply_hub(quiet("kamado", "T1 182 °C"));
        ui.apply_hub(quiet("pool", "36 °C"));
        assert_eq!(reminders(&events), 0);
        let texts: Vec<String> = ui
            .desk
            .borrow()
            .ha_cards
            .views("")
            .into_iter()
            .map(|(_, text, _)| text)
            .collect();
        assert_eq!(texts, ["T1 182 °C", "36 °C"]);
        ui.apply_hub(HubMsg::Card(CardEvent::Dismiss("kamado".into())));
        ui.apply_hub(HubMsg::Card(CardEvent::Dismiss("pool".into())));
        ids(&fired);

        // Do not disturb: the tap is what counts.
        ui.set_dnd(Some(now_unix() as u64 + 3600));
        assert!(ui.dnd_on());
        ui.apply_hub(show(Some("c")));
        assert!(tap(&ui));
        assert_eq!(ids(&fired), ok("c"));
        ui.set_dnd(None);
        // The night layout (dimmed): the same. A toggle: at night (the CI ran at 23:05) it is on
        // already and one toggle would turn it off.
        if !ui.desk.borrow().night_now {
            ui.toggle_night();
        }
        assert!(ui.desk.borrow().night_now);
        ui.apply_hub(show(Some("d")));
        assert!(tap(&ui));
        assert_eq!(ids(&fired), ok("d"));
        drop(ui);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn welcome_title_joins_each_word_once() {
        assert_eq!(
            welcome_title(["Welcome", "Benvenuto"].into_iter()),
            "Welcome · Benvenuto"
        );
        assert_eq!(
            welcome_title(["Welcome", "", "Welcome"].into_iter()),
            "Welcome"
        );
    }

    #[test]
    fn find_view_per_outcome() {
        let w = |k: &str| k.to_string() + " {error}";
        let found = Outcome::Found(vec![Found {
            name: "Home".into(),
            url: "http://192.0.2.10:8123".into(),
        }]);
        let (st, rows, line) = find_view(&found, &w);
        assert_eq!((st, line.as_str()), ("found", ""));
        assert_eq!(
            rows,
            vec![("Home".to_string(), "http://192.0.2.10:8123".to_string())]
        );
        assert_eq!(
            find_view(&Outcome::Found(vec![]), &w).2,
            "find_none_text {error}"
        );
        assert_eq!(
            find_view(&Outcome::NotInstalled, &w).2,
            "find_missing {error}"
        );
        let (st, rows, line) = find_view(&Outcome::Failed("x".into()), &w);
        assert_eq!(
            (st, rows.len(), line.as_str()),
            ("none", 0, "find_failed x")
        );
    }

    #[cfg(test)]
    mod wifi_tests {
        #[test]
        fn wifi_bars_from_iw_link() {
            use super::parse_wifi_level as p;
            assert_eq!(p("Not connected."), 0);
            assert_eq!(
                p("Connected to x (on wlan0)\n\tSSID: Casa\n\tsignal: -26 dBm\n"),
                4
            );
            assert_eq!(p("\tsignal: -61 dBm"), 3);
            assert_eq!(p("\tsignal: -70 dBm"), 2);
            assert_eq!(p("\tsignal: -82 dBm"), 1);
        }
    }

    #[test]
    fn the_use_says_whether_it_is_an_estimate() {
        use crate::usage::{Precise, Source, VIEW_DAYS, View};
        let it = locale::load(&locale::repo_locales(), "it").unwrap();
        let text = |k: &str| super::word(&it, k, "Alfred");
        let mut days: Vec<(u32, f64)> = (1..=VIEW_DAYS as u32).map(|d| (d, 0.0)).collect();
        days[10].1 = 0.2;
        days[VIEW_DAYS - 1].1 = 0.05;
        let est = View {
            today_cost: 0.05,
            month_cost: 1.25,
            days: days.clone(),
            source: Source::Estimate,
            ..Default::default()
        };
        let u = super::usage_view(&est, text);
        assert_eq!(u.line, "oggi ~0,05 $ (stima)");
        assert_eq!(u.today_label, "Oggi · stima");
        assert_eq!(u.today, "~0,05 $");
        assert_eq!(u.month, "Questo mese ~1,2 $");
        assert_eq!(u.max, "~0,20 $");
        assert!(
            u.note.contains("github.com/m0s4ik/ha-openai-usage"),
            "{}",
            u.note
        );
        assert!(u.note.contains("Alfred") && !u.precise);
        assert_eq!(u.detail, "", "no questions today");
        // Labels: today and every week before it.
        let labelled: Vec<usize> = (0..VIEW_DAYS)
            .filter(|&i| !u.bars[i].0.is_empty())
            .collect();
        assert_eq!(labelled, [1, 8, 15, 22, 29]);
        assert_eq!(u.bars[10].1, 1.0);
        let p = Precise {
            today_cost: 0.0181,
            month_cost: 1.234,
            requests: Some(7),
            tokens: Some(21_500),
            days,
        };
        let u = super::usage_view(&View::choose(&est, Some(&p)), text);
        assert_eq!(u.line, "oggi 0,018 $");
        assert_eq!(u.today_label, "Oggi");
        assert_eq!(u.month, "Questo mese 1,23 $");
        assert_eq!(u.detail, "7 richieste · 21500 token");
        assert!(u.note.starts_with("Numeri precisi") && u.precise);
        let en = locale::load(&locale::repo_locales(), "en").unwrap();
        let u = super::usage_view(&est, |k: &str| super::word(&en, k, "Alfred"));
        assert_eq!(u.line, "today ~0.05 $ (estimate)");
        assert_eq!(u.today_label, "Today · estimated");
    }
}
