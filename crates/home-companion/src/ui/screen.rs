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
use super::{AlarmLineView, MemoriesView, MusicView, Screen, SettingsView, TimerLineView};
use crate::alarms::Alarm;
use crate::brain_link::{self, BrainLink, BrainMsg, Upcoming};
use crate::core::{CoreEvent, MirrorInfo, Phase, UiCommand, UiEvent};
use crate::ha::hub::{HubCmd, HubMsg};
use crate::ha::weather::Weather;
use crate::history::History;
use crate::settings::Settings;
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
    /// `--state mic-*`: the microphone off by the switch (line and tile); `mic_hud` = the
    /// switch's card, Some(on).
    pub mic_switch_off: bool,
    pub mic_hud: Option<bool>,
    /// `--state thinking-answer`: Home Assistant's written answer is in, its voice not yet.
    pub answer_early: bool,
    /// `--state ha-pick` / `ha-pick-voice`: what the choice page lists ("ai", "voice").
    pub pick: &'static str,
}

/// The screens `--state` knows, for the golden screenshots (tools/shoot-golden.sh).
pub const DEMO_STATES: [&str; 59] = [
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
    "dnd",
    "control-dnd",
    "mic-switch-off",
    "mic-switch-on",
    "mic-off",
    "control-mic-switch",
    "mic-off-dnd",
    "what",
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
            "house-ask" => Demo {
                house: 2,
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
            "starting" => Demo {
                starting: true,
                phase: Phase::Offline,
                ..home.clone()
            },
            "altro" | "chat" | "timers" | "settings" | "language" | "screen" | "music"
            | "setup" | "ha" | "memories" | "reminders" | "shopping" | "what" => {
                page(DEMO_STATES.into_iter().find(|x| *x == s)?)
            }
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
}

/// The animation side of the UI.
struct Motion {
    seq: Sequencer,
    clips: HashMap<&'static str, Arc<Clip>>,
    incoming: Option<Receiver<Clip>>,
    playhead: Option<Playhead>,
    shown: Show,
    awake: Awake,
    backlight: Backlight,
    /// Start-up: until when the loading animation may play (None = over).
    loading_until: Option<Instant>,
    /// Frames presented and their CPU time, for the stats line.
    presented: u64,
    present_time: Duration,
}

/// Something removed by touch, kept for [`UNDO`] before it is really gone.
#[derive(Debug, Clone)]
enum Undo {
    Timer(String),
    Alarm(u32),
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
    /// The first start's welcome page is still to be answered.
    language_pending: Cell<bool>,
    /// The first start's search for Home Assistant on the network, while it runs.
    find: RefCell<Option<Receiver<crate::ha::discover::Outcome>>>,
    /// Home Assistant picked: the restart after "saving" has been on screen a moment.
    restart_timer: Timer,
    /// v0.2: the "done" and Yes/No cards of the brain's house commands.
    house: RefCell<HouseCards>,
    /// The countdown's length last put on screen (it is set again when it moved enough).
    ask_left: Cell<f32>,
    /// Do not disturb (UiCommand::DoNotDisturb).
    dnd: Cell<Dnd>,
    this: RefCell<Weak<Self>>,
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
    format!("{} {month}", tm.tm_mday)
}

/// "14:02" of a time in seconds since the epoch, and the day it is on (days since the epoch, local).
fn local_hm(t: i64) -> (String, i64) {
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
    Ask(i64, String, i64),
    AskOver(i64),
    View(brain_link::View),
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
            // The night's sheet (~19 MB decoded) only around the night: Ui::night_sheets.
            for name in character::ANIMATIONS
                .iter()
                .filter(|n| !NIGHT_CLIPS.contains(n))
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
            playhead: None,
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
        let opts_auto = opts.screen.auto.filter(|_| !demo).map(Auto::new);
        let opts_linked = opts.ha_linked;
        let opts_first = opts.first_start;
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
            language_pending: Cell::new(opts_first.language),
            find: RefCell::new(None),
            restart_timer: Timer::default(),
            house: RefCell::new(HouseCards::default()),
            ask_left: Cell::new(0.0),
            dnd: Cell::new(Dnd::default()),
            this: RefCell::new(Weak::new()),
        });
        *ui.this.borrow_mut() = Rc::downgrade(&ui);
        ui.set_welcome();
        ui.wire();
        ui.start_wifi();
        ui.screen.set_error_pose(ui.pose(State::Error));
        ui.screen.set_dnd_prop(load_prop(
            &ui.character.dir.join("poses/dnd.png"),
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

    fn wire(self: &Rc<Self>) {
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
                ui.brain_do(json!({"cmd": "memory.delete", "id": id}));
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
        self.screen.on_done_undo(move || {
            if let Some(ui) = ui.upgrade() {
                // Newest first, as an undo goes: a later snapshot (a group's, taken after its
                // lights went off) must not be put back over the earlier ones.
                let ids = ui.house.borrow_mut().undo();
                for id in ids.unwrap_or_default().into_iter().rev() {
                    log::info!(event = "house_undo", id = id; "house: undo {id}");
                    ui.brain_do(brain_link::undo(id));
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
                let id = ui.house.borrow_mut().answer();
                if let Some(id) = id {
                    log::info!(event = "house_answer", id = id, yes = yes; "house: {} to {id}", if yes { "yes" } else { "no" });
                    ui.brain_do(brain_link::confirm_answer(id, yes));
                }
                ui.render_house();
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
        self.screen.on_language_chosen(move |app, code| {
            let Some(ui) = ui.upgrade() else { return };
            match set_language(app, &ui.character, &ui.opts.locales, &code) {
                Ok(l) => {
                    *ui.locale.borrow_mut() = l;
                    ui.render_weather();
                    app.set_clock("");
                    ui.tick_clock();
                    ui.refresh_desk();
                    ui.show(ui.view.get());
                    ui.send(UiEvent::Language(code));
                    if app.get_page() == "welcome" {
                        ui.language_pending.set(false);
                        let next = ui.first_page().unwrap_or("home");
                        app.set_page(next);
                        ui.on_page(next);
                    }
                }
                Err(e) => log::warn!("language {code}: {e}"),
            }
        });
        // Timers and alarms by touch.
        let ui = Rc::downgrade(self);
        self.screen.on_timer_start(move |minutes| {
            if let Some(ui) = ui.upgrade() {
                ui.send(UiEvent::TimerStart(minutes as u64 * 60));
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
                ui.history.borrow_mut().set_keep(keep);
                if let Some(s) = &ui.settings {
                    s.save_keep_conversations(keep);
                }
                ui.refresh_desk();
            }
        });
        let ui = Rc::downgrade(self);
        self.screen.on_night_changed(move |on, start, end| {
            if let Some(ui) = ui.upgrade() {
                ui.desk.borrow_mut().night = (on, start, end);
                if let Some(s) = &ui.settings {
                    s.save_night(on, start, end);
                }
                ui.refresh_desk();
                ui.tick_clock();
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
        app.set_pose(self.pose(state));
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
        let night = self.desk.borrow().night_shown;
        let mode = if self.opts.demo.is_some() {
            Mode::Still
        } else if loading {
            Mode::Loading
        } else {
            match p {
                Phase::Home if night => Mode::Night,
                Phase::Home => Mode::Home,
                Phase::Offline => Mode::Still,
                _ => self
                    .character
                    .state_animation(pose)
                    .and_then(|a| anim::static_name(&a.name))
                    .map_or(Mode::Still, Mode::Loop),
            }
        };
        self.motion.borrow_mut().seq.set_mode(mode, now);
        self.step();
    }

    /// Seconds since the epoch; fixed in demo mode (the golden screenshots).
    fn now(&self) -> f64 {
        if self.opts.demo.is_some() {
            DEMO_NOW
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

    /// Something woke the screen up (touch, key, movement, SIGUSR1).
    pub fn wake(&self, why: WakeSource) {
        if self.opts.demo.is_some() {
            return;
        }
        let now = Instant::now();
        if why == WakeSource::Touch {
            self.desk.borrow_mut().touched = Some(now);
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
        self.motion.borrow_mut().awake.sleep(Instant::now());
        self.step();
    }

    /// New animations, the start-up timeout; then [`Ui::step`]. Called every 20 ms.
    pub fn poll(&self) {
        self.poll_link();
        self.poll_find();
        self.poll_brain();
        self.poll_ha();
        self.poll_house();
        let mut added = false;
        {
            let mut m = self.motion.borrow_mut();
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
        let Show::Frame { clip, index } = show else {
            m.shown = Show::Pose;
            self.screen.set_frame(Image::default());
            return;
        };
        let Some(c) = m.clips.get(clip).cloned() else {
            return;
        };
        let new_clip = !matches!(m.shown, Show::Frame { clip: was, .. } if was == clip)
            || m.playhead.is_none();
        if new_clip {
            m.playhead = Some(Playhead::new(c.clone()));
            let (x, y, w, h) = self.frame_rect(&c.anim);
            self.screen.set_frame_rect(x, y, w, h);
        }
        m.shown = show;
        let p = m.playhead.as_mut().expect("set above");
        let changed = p.seek(index) || new_clip;
        if !changed {
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
            "loading" | "wake" | "idle" | "fall_asleep" | "sleeping" | "wake_from_sleep" => {
                State::Idle
            }
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

    fn show_demo(&self, d: Demo) {
        let l = self.locale.borrow().clone();
        let w = |k: &str| l.text(k).to_string();
        self.screen
            .set_clock(if d.night { "23:48" } else { "14:25" });
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
                wifi: Some("Casa".into()),
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
            };
            let tiles = crate::ui::control::tiles(&st, &w);
            self.screen
                .set_control(true, &tiles, 62, true, &w("cc_auto"));
        }
        self.screen.set_setup(
            &w("wifi_named").replace("{ssid}", "Casa"),
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
                    l.text("house_and_more")
                        .replace("{name}", "Studio sinistra")
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
                ..Default::default()
            };
            self.screen.set_cards(&cards::pick(&alive, &cw));
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
                reminders_tile: l.text("tile_reminders").replace("{n}", "1"),
                status: format!(
                    "{} · {}",
                    l.text("brain_status")
                        .replace("{facts}", &n)
                        .replace("{model}", "gemini"),
                    l.text("tidy_line")
                        .replace("{t}", "03:30")
                        .replace("{merged}", "2")
                        .replace("{updated}", "1")
                        .replace("{retired}", "1")
                ),
                profile: l.sample("memory_profile").replace('|', "\n"),
                notes: l.sample("memory_notes").into(),
                facts: vec![
                    (2, l.sample("memory_fact1").into(), "4 nov".into()),
                    (1, l.sample("memory_fact2").into(), "2 nov".into()),
                ],
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
            let items: Vec<(String, String)> = l
                .sample("shop_items")
                .split('|')
                .enumerate()
                .map(|(i, t)| (i.to_string(), t.to_string()))
                .collect();
            self.screen.set_shopping(
                true,
                &items,
                &l.text("tile_shopping")
                    .replace("{n}", &items.len().to_string()),
            );
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
        if !d.dialog.is_empty() {
            self.screen.set_dialog(d.dialog);
        }
        if let Some(v) = d.volume {
            self.screen.set_volume(v, true);
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
                let names: Vec<String> = if d.house_group {
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
                    let line = house::done_line(n, Some("off"), "turn_off", &word);
                    cards.changed(i as i64, line, true, t0);
                }
                let card = cards.done.expect("a card");
                let (line, name, state) =
                    card.texts(l.text("house_and_more"), l.text("house_others"));
                self.screen.set_done(true, &line, true);
                self.screen.set_done_parts(&name, &state);
                self.screen.set_done_popup(!d.house_card_only, 0.62);
            }
            2 => self.screen.set_ask(true, l.sample("house_question"), 0.62),
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
            UiCommand::DoNotDisturb(until) => self.set_dnd(until),
            UiCommand::Suspending => {
                self.screen.set_settings_open(false);
                self.motion.borrow_mut().awake.sleep(Instant::now());
                self.step();
            }
            UiCommand::Resumed => {
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
                let cover = n
                    .as_ref()
                    .and_then(|n| n.cover.as_deref())
                    .and_then(load_cover);
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
            }
            UiCommand::MicSwitch(on) => self.show_mic_switch(on),
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
        *self.weather.borrow_mut() = w;
        self.render_weather();
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
        self.feed_light();
        let now = Instant::now();
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
        if self.screen.get_clock() != clock {
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
        // A page left alone goes home (not the setup page, not with a question open).
        let page = self.screen.get_page();
        let touched = self.desk.borrow().touched;
        if page != "home"
            && page != "setup"
            && page != "welcome"
            && page != "find-ha"
            && page != super::mirror::PAGE
            && self.screen.get_dialog().is_empty()
            && touched.is_none_or(|t| now.duration_since(t) >= PAGE_IDLE)
        {
            self.screen.set_page("home");
        }
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
        let hour = |dt: i64| local_hm(t + dt).0[..2].parse::<u32>().unwrap_or(0);
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
        let hour = local_now().3 as u32;
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
        if self.ha_linked.get() || self.opts.demo.is_some() {
            return false;
        }
        log::info!("Home Assistant button while not linked: the link page");
        self.screen.set_page("setup");
        true
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

    fn on_page(&self, page: &str) {
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
        match page {
            "chat" => self.refresh_chat(),
            "timers" => self.refresh_timers(),
            "settings" | "screen" | "language" => self.refresh_settings(),
            "altro" => self.refresh_tiles(),
            "memories" => self.refresh_memories(),
            "what" => self.refresh_what(),
            "ha" => {
                self.ha_send(crate::ha::setup::SetupCmd::Check, false);
                self.tick_usage(true);
            }
            "usage" => self.tick_usage(true),
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

    fn refresh_home(&self) {
        if self.opts.demo.as_ref().is_some_and(|d| d.cards) {
            return;
        }
        let l = self.locale.borrow();
        let w = |k: &str| l.text(k).to_string();
        let timers = self.visible_timers();
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
        };
        let picked = cards::pick(&alive, &cw);
        *self.card_kinds.borrow_mut() = picked.iter().map(|c| c.kind).collect();
        self.screen.set_cards(&picked);
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
            parts.push(
                l.text("tile_timers")
                    .replace("{n}", &d.timers.len().to_string()),
            );
        }
        let alarms = crate::alarms::Alarms::from_list(d.alarms.clone());
        if let Some((a, _)) = alarms.next_within(crate::alarms::Now::at(self.now() as i64), 24 * 60)
        {
            parts.push(l.text("tile_alarm").replace("{t}", &a.time()));
        }
        let lang = self
            .character
            .languages()
            .find(|c| *c == l.code)
            .map_or(l.name.clone(), |_| l.name.clone());
        let settings = lang.to_string();
        self.screen.set_tiles(&chat, &parts.join(" · "), &settings);
    }

    fn refresh_settings(&self) {
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
            version: format!("home-companion {}", env!("CARGO_PKG_VERSION")),
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
                            self.motion.borrow_mut().seq.done(Instant::now());
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
        self.choice.borrow_mut().lists = lists;
        self.choice.borrow_mut().calendars = self
            .settings
            .as_ref()
            .map(|s| s.calendars())
            .unwrap_or_default();
    }

    /// What Home Assistant's thread says.
    pub fn apply_hub(&self, m: HubMsg) {
        match m {
            HubMsg::Card(ev) => {
                let show = matches!(ev, crate::ha::events::CardEvent::Show(_));
                let changed = self.desk.borrow_mut().ha_cards.apply(ev, Instant::now());
                if changed {
                    // Like a reminder: a soft wake and its sound, by day only.
                    if show && !self.desk.borrow().night_now {
                        self.wake(WakeSource::Conversation);
                        self.send(UiEvent::Reminder);
                    }
                    self.refresh_home();
                }
            }
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
        let gone = self.desk.borrow_mut().ha_cards.dismiss_nth(nth);
        if gone {
            self.refresh_home();
        }
        gone
    }

    /// A tap on a row of Lists and calendars: saved, and the lists change at once.
    fn choice_toggled(&self, id: &str) {
        let mut c = self.choice.borrow_mut();
        if !c.toggle(id) {
            return;
        }
        if let Some(s) = &self.settings {
            if id.starts_with("todo.") {
                s.save_lists(&c.lists);
            } else {
                s.save_calendars(&c.calendars);
            }
        }
        if id.starts_with("todo.")
            && let Some(tx) = self.hub_tx.borrow().as_ref()
        {
            let _ = tx.send(HubCmd::SetLists(c.lists.clone()));
        }
        drop(c);
        self.render_choices();
    }

    fn render_choices(&self) {
        let (todo, cal) = self.choice.borrow().rows();
        self.screen.set_choices(&todo, &cal);
    }

    /// A tick on the page: gone from the screen at once, completed in Home Assistant.
    fn shop_done(&self, uid: String) {
        let Some((list, item)) = lists::split_uid(&uid) else {
            return;
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
        self.render_shopping();
        self.refresh_home();
    }

    fn render_shopping(&self) {
        let d = self.desk.borrow();
        let l = self.locale.borrow();
        match &d.shopping {
            Some(v) => self.screen.set_shopping(
                true,
                &lists::rows(v),
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
        let mut upcoming_seen: Option<Vec<(i64, String)>> = None;
        let mut timers: Vec<u64> = Vec::new();
        let mut cancels: Vec<String> = Vec::new();
        let mut alarms: Vec<(u8, u8, u8)> = Vec::new();
        let mut house_cards: Vec<HouseEvent> = Vec::new();
        while let Some(m) = b.try_recv() {
            let l = self.locale.borrow();
            let name = self.character.name.clone();
            let mut d = self.desk.borrow_mut();
            let mv = &mut d.memories;
            match m {
                BrainMsg::Present(p) => {
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
                            mv.tile = l.text("tile_memories").replace("{n}", &facts.to_string());
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
                            mv.profile = a["profile"].as_str().unwrap_or("").trim().to_string();
                            mv.notes = a["notes"].as_str().unwrap_or("").trim().to_string();
                            mv.facts = a["facts"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|f| {
                                    (
                                        f["id"].as_i64().unwrap_or(0),
                                        f["text"].as_str().unwrap_or("").to_string(),
                                        short_date(&l, f["updated"].as_i64().unwrap_or(0)),
                                    )
                                })
                                .collect();
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
                BrainMsg::Timer(secs) => {
                    log::info!(event = "brain_timer", seconds = secs; "timer from the brain: {secs} s");
                    timers.push(secs);
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
                    expires_at,
                } => {
                    log::info!(event = "house_confirm", id = id; "house: asks {text:?}");
                    house_cards.push(HouseEvent::Ask(id, text, expires_at));
                }
                BrainMsg::ConfirmDone { id, yes } => {
                    log::info!(event = "house_confirm_done", id = id, answered = yes.is_some(); "house: question {id} over");
                    house_cards.push(HouseEvent::AskOver(id));
                }
                BrainMsg::ShowView(v) => house_cards.push(HouseEvent::View(v)),
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
        for secs in timers {
            self.send(UiEvent::TimerStart(secs));
        }
        for id in cancels {
            self.send(UiEvent::TimerCancel(id));
        }
        for (h, m, days) in alarms {
            self.send(UiEvent::AlarmSave(h, m, days));
        }
        if events_seen && !home {
            self.refresh_home();
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
                // The card is seen: a soft wake (not the screen asleep by "spegni lo schermo"
                // just before, nor the night's, that the touch is for).
                self.wake(WakeSource::Conversation);
            }
            HouseEvent::Ask(id, text, expires_at) => {
                if !self
                    .house
                    .borrow_mut()
                    .confirm(id, text, expires_at, self.now())
                {
                    log::info!(event = "house_confirm", id = id; "house: question {id} over already");
                    return;
                }
                // The question needs the screen, whatever it was doing.
                self.motion.borrow_mut().awake.end_sleep();
                self.wake(WakeSource::Conversation);
                self.screen.set_control_open(false);
            }
            HouseEvent::AskOver(id) => {
                self.house.borrow_mut().confirm_done(id);
            }
            HouseEvent::View(v) => {
                self.show_view(v);
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
        let left = self.house.borrow().ask_left(now_unix);
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
        let h = self.house.borrow().clone();
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
        match &h.ask {
            Some(a) => self.screen.set_ask(true, &a.text, left),
            None => self.screen.set_ask(false, "", 0.0),
        }
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
        if self.opts.demo.is_some() {
            return;
        }
        if super::webview::visible() {
            super::webview::request_home();
        }
        self.screen.set_control_open(false);
        log::info!(event = "screen_sleep", by = "voice"; "screen: off (asked by voice)");
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
        }
        self.screen.set_undo("");
    }
}

/// An answer longer than this leaves the bar over Home Assistant's view for Alfred's layout.
const BAR_ANSWER: usize = 70;
/// Who answers (one brain for now: Home Assistant's Assist pipeline).
const BRAIN: &str = "Home Assistant";
/// "Now" in demo mode: 2026-11-03 14:25 UTC (tools/shoot-golden.sh runs with TZ=UTC).
const DEMO_NOW: f64 = 1_793_715_900.0;
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

#[cfg(test)]
mod tests {
    use super::*;
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
        for lang in ["it", "en"] {
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
