// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The companion's landscape screen (ui/companion.slint) in a desktop window,
//! with any character of characters/, to work on the layout from a laptop. It
//! sets the character (poses, colours, phrases), the clock, the date and a
//! fake sound wave; weather, messages and the answer are samples from the
//! locale file, the timer is the .slint one.
//!
//!   companion-sim [--character DIR] [--lang it|en|…]
//!                 [--state home|listening|thinking|speaking|error]
//!                 [--locales DIR] [--settings FILE] [--settings-open] [--no-notifications]
//!                 [--screenshot FILE.png] [--seconds N]
//!
//! DIR defaults to characters/alfred of this repository, the locales to its
//! locales/. The language is --lang, else the one chosen last time (saved in
//! FILE, default ~/.config/home-companion/settings.toml), else English;
//! it sets the body's words (locales/<code>.toml, English if missing) and the
//! character's phrases (its first language if missing). Keys: 1-5 switch
//! state, N hides or shows the sample notifications (`--no-notifications`); the Settings button or a double tap on the character's mouth opens
//! the settings panel (`--settings-open` at start), where the language is
//! chosen; S saves a screenshot (FILE-keyN.png, default out/companion.png).
//! With --seconds the window closes after N seconds, saving --screenshot first.

use std::cell::Cell;
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use character::{Character, Crop, Rgb, State};
use locale::Locale;
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel};

include!(concat!(env!("OUT_DIR"), "/companion.rs"));

const BARS: usize = 72;

/// The body's words of a language (locales/<code>.toml) on the screen, with
/// the sample data the simulator shows.
fn set_body_text(app: &CompanionWindow, t: &Locale) {
    app.set_label_home_assistant(t.text("home_assistant").into());
    app.set_label_talk(t.text("talk").into());
    app.set_label_stop(t.text("stop").into());
    app.set_label_ok(t.text("ok").into());
    app.set_label_you_said(t.text("you_said").into());
    app.set_label_settings(t.text("settings").into());
    app.set_label_language(t.text("language").to_uppercase().into());
    app.set_temperature(t.sample("temperature").into());
    set_notifications(app, t, !app.get_notifications_hidden());
    app.set_answer(t.sample("answer").into());
}

/// The sample notifications (a timer and a message), or none.
fn set_notifications(app: &CompanionWindow, t: &Locale, shown: bool) {
    let (timers, messages) = if shown {
        let timer = TimerRow { name: "Pasta".into(), left: "07:42".into() };
        let message = MessageRow { title: t.sample("message_title").into(), text: t.sample("message_text").into() };
        (vec![timer], vec![message])
    } else {
        (vec![], vec![])
    };
    app.set_timers(ModelRc::new(VecModel::from(timers)));
    app.set_messages(ModelRc::new(VecModel::from(messages)));
}

/// The simulator's saved settings: `language = "it"` in a small TOML file.
struct Settings {
    path: PathBuf,
}

impl Settings {
    /// `$XDG_CONFIG_HOME/home-companion/settings.toml`, or under `~/.config`.
    fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| ".".into());
        base.join("home-companion").join("settings.toml")
    }

    fn language(&self) -> Option<String> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let table: toml::Table = toml::from_str(&text).map_err(|e| eprintln!("{}: {e}", self.path.display())).ok()?;
        table.get("language")?.as_str().map(String::from)
    }

    fn save_language(&self, lang: &str) {
        let mut table: toml::Table =
            std::fs::read_to_string(&self.path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
        table.insert("language".into(), lang.into());
        let result = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&self.path, toml::to_string(&table).unwrap_or_default()));
        if let Err(e) = result {
            eprintln!("companion-sim: {}: {e}", self.path.display());
        }
    }
}

struct Args {
    character: PathBuf,
    lang: Option<String>,
    locales: PathBuf,
    settings: PathBuf,
    state: String,
    settings_open: bool,
    no_notifications: bool,
    screenshot: Option<PathBuf>,
    seconds: Option<f64>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        character: character::repo_characters().join("alfred"),
        lang: None,
        locales: locale::repo_locales(),
        settings: Settings::default_path(),
        state: "home".into(),
        settings_open: false,
        no_notifications: false,
        screenshot: None,
        seconds: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--character" => a.character = value()?.into(),
            "--lang" => a.lang = Some(value()?),
            "--state" => a.state = value()?,
            "--locales" => a.locales = value()?.into(),
            "--settings" => a.settings = value()?.into(),
            "--settings-open" => a.settings_open = true,
            "--no-notifications" => a.no_notifications = true,
            "--screenshot" => a.screenshot = Some(value()?.into()),
            "--seconds" => a.seconds = Some(value()?.parse().map_err(|e| format!("--seconds: {e}"))?),
            // `--backend winit` is accepted so the spike-ui command line works here too.
            "--backend" => {
                value()?;
            }
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    Ok(a)
}

/// Screen state (ui/companion.slint) → character state.
fn character_state(screen: &str) -> State {
    match screen {
        "listening" => State::Listening,
        "thinking" => State::Thinking,
        "speaking" => State::Speaking,
        "error" => State::Error,
        _ => State::Idle,
    }
}

fn color(c: Rgb) -> Color {
    Color::from_rgb_u8(c.0, c.1, c.2)
}

/// A pose picture cropped to `crop`, as a Slint image (RGBA).
fn load_pose(path: &Path, crop: Crop) -> Result<Image, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut data = vec![0; reader.output_buffer_size().ok_or("png too large")?];
    let info = reader.next_frame(&mut data).map_err(|e| format!("{}: {e}", path.display()))?;
    let channels = info.color_type.samples();
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(crop.width(), crop.height());
    let out = buf.make_mut_slice();
    for y in 0..crop.height() {
        let row = &data[(crop.y0 + y) as usize * info.line_size..];
        for x in 0..crop.width() {
            let p = &row[(crop.x0 + x) as usize * channels..];
            let a = if channels == 4 { p[3] } else { 255 };
            out[(y * crop.width() + x) as usize] = Rgba8Pixel { r: p[0], g: p[1], b: p[2], a };
        }
    }
    Ok(Image::from_rgba8(buf))
}

fn set_character(app: &CompanionWindow, c: &Character, lang: &str) {
    let p = c.phrases(lang);
    app.set_character_name(c.name.clone().into());
    app.set_character_background(color(c.background));
    app.set_color_listening(color(c.color(State::Listening)));
    app.set_color_thinking(color(c.color(State::Thinking)));
    app.set_color_speaking(color(c.color(State::Speaking)));
    app.set_color_error(color(c.color(State::Error)));
    app.set_phrase_listening(p.listening.clone().into());
    app.set_phrase_thinking(p.thinking.clone().into());
    app.set_phrase_error(p.error.clone().into());
    app.set_phrase_idle(p.idle.clone().into());
}

/// Everything that depends on the language: the character's phrases, the
/// body's words, the language list of the settings panel (the character's
/// languages, named by their locale file); the date is redrawn on the next
/// tick. Returns the locale in use (English if `lang` has none).
fn set_language(app: &CompanionWindow, c: &Character, locales: &Path, lang: &str) -> Result<Locale, String> {
    let text = locale::load_or_english(locales, lang).map_err(|e| e.join("; "))?;
    set_character(app, c, lang);
    set_body_text(app, &text);
    app.set_label_suspend(text.text("suspend").into());
    app.set_label_suspend_question(text.text("suspend_confirm").replace("{name}", &c.name).into());
    app.set_label_cancel(text.text("cancel").into());
    let rows: Vec<LanguageRow> = c
        .languages()
        .map(|code| {
            let name = locale::load(locales, code).map_or_else(|_| code.to_uppercase(), |l| l.name);
            LanguageRow { code: code.into(), name: name.into(), selected: code == lang }
        })
        .collect();
    app.set_languages(ModelRc::new(VecModel::from(rows)));
    app.set_clock("".into());
    Ok(text)
}

/// The mouth's box on screen, scaled and placed like the idle pose
/// (character-cx, character-top, character-height in ui/companion.slint).
fn set_mouth(app: &CompanionWindow, c: &Character) {
    let (_, crop) = c.pose(State::Idle);
    let scale = app.get_character_height() / crop.height() as f32;
    let left = app.get_character_cx() - crop.width() as f32 * scale / 2.0;
    let top = app.get_character_top();
    app.set_mouth_x(left + (c.mouth.x0 - crop.x0) as f32 * scale);
    app.set_mouth_y(top + (c.mouth.y0 - crop.y0) as f32 * scale);
    app.set_mouth_width(c.mouth.width() as f32 * scale);
    app.set_mouth_height(c.mouth.height() as f32 * scale);
}

/// Local time as (day of week 0 = Sunday, day, month 0-11, hour, minute).
fn local_now() -> (usize, i32, usize, i32, i32) {
    // SAFETY: time(NULL) and localtime_r only write into the struct we pass.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_wday as usize, tm.tm_mday, tm.tm_mon as usize, tm.tm_hour, tm.tm_min)
    }
}

/// A fake voice: bars symmetric around the centre, louder in the middle.
fn wave(state: &str, t: f64) -> Vec<f32> {
    let (level, speed) = match state {
        "listening" => (0.45, 7.0),
        "speaking" => (0.8, 9.0),
        "thinking" => (0.12, 2.0),
        _ => (0.0, 0.0),
    };
    let half: Vec<f32> = (0..BARS / 2)
        .map(|i| {
            let k = i as f64 / (BARS / 2) as f64; // 0 at the centre
            let env = (1.0 - k).powf(1.5);
            let wobble = 0.5 + 0.5 * (TAU * (t * speed / 4.0) + i as f64 * 0.9).sin() * (t * speed / 3.0 + i as f64).cos();
            (level * env * wobble) as f32
        })
        .collect();
    half.iter().rev().chain(half.iter()).copied().collect()
}

fn save_png(app: &CompanionWindow, path: &Path) -> Result<(), String> {
    let buf = app.window().take_snapshot().map_err(|e| format!("take_snapshot: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), buf.width(), buf.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| format!("png: {e}"))?;
    w.write_image_data(buf.as_bytes()).map_err(|e| format!("png: {e}"))?;
    eprintln!("companion-sim: screenshot {}", path.display());
    Ok(())
}

fn main() -> Result<(), String> {
    let args = parse_args()?;
    let character = character::load(&args.character)
        .map_err(|e| format!("{}:\n  {}", args.character.display(), e.join("\n  ")))?;
    let character = Rc::new(character);
    // --lang, else the saved choice, else English.
    let settings = Settings { path: args.settings.clone() };
    let lang = args.lang.clone().or_else(|| settings.language()).unwrap_or_else(|| locale::DEFAULT.into());

    // The glyphs are pre-rendered at the sizes used in the .slint file; at a
    // HiDPI scale factor (Retina: 2) Slint would pick the nearest smaller size.
    // Scale 1 also shows the device's real pixels.
    if std::env::var_os("SLINT_SCALE_FACTOR").is_none() {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("SLINT_SCALE_FACTOR", "1") };
    }
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
        .map_err(|e| format!("winit backend (software renderer): {e}"))?;
    let app = CompanionWindow::new().map_err(|e| format!("window: {e}"))?;
    app.set_notifications_hidden(args.no_notifications);
    let text = Rc::new(std::cell::RefCell::new(set_language(&app, &character, &args.locales, &lang)?));
    set_mouth(&app, &character);
    let (weak, c, t, locales) = (app.as_weak(), character.clone(), text.clone(), args.locales.clone());
    app.on_language_chosen(move |code| {
        let Some(app) = weak.upgrade() else { return };
        match set_language(&app, &c, &locales, &code) {
            Ok(l) => {
                *t.borrow_mut() = l;
                settings.save_language(&code);
                eprintln!("companion-sim: language {code} (saved in {})", settings.path.display());
            }
            Err(e) => eprintln!("companion-sim: {e}"),
        }
    });

    // Poses are decoded once, on first use.
    let poses: Rc<std::cell::RefCell<HashMap<State, Image>>> = Rc::default();
    let show_pose = {
        let (weak, poses, character) = (app.as_weak(), poses.clone(), character.clone());
        move |screen: &str| {
            let Some(app) = weak.upgrade() else { return };
            let state = character_state(screen);
            let mut cache = poses.borrow_mut();
            if !cache.contains_key(&state) {
                let (path, crop) = character.pose(state);
                let image = load_pose(path, crop).unwrap_or_else(|e| {
                    eprintln!("companion-sim: {e}");
                    Image::default()
                });
                cache.insert(state, image);
            }
            app.set_pose(cache[&state].clone());
        }
    };
    show_pose(&args.state);
    app.on_state_changed(move |s| show_pose(&s));
    app.set_state(args.state.clone().into());
    app.set_settings_open(args.settings_open);

    let base = args.screenshot.clone().unwrap_or_else(|| "out/companion.png".into());
    let shots = Rc::new(Cell::new(0u32));
    let weak = app.as_weak();
    app.on_screenshot_requested(move || {
        let Some(app) = weak.upgrade() else { return };
        shots.set(shots.get() + 1);
        let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("companion");
        if let Err(e) = save_png(&app, &base.with_file_name(format!("{stem}-key{}.png", shots.get()))) {
            eprintln!("companion-sim: {e}");
        }
    });
    let weak = app.as_weak();
    app.on_talk_clicked(move || weak.upgrade().into_iter().for_each(|a| a.set_state("listening".into())));
    let weak = app.as_weak();
    app.on_stop_clicked(move || weak.upgrade().into_iter().for_each(|a| a.set_state("home".into())));
    app.on_home_assistant_clicked(|| eprintln!("companion-sim: Home Assistant"));
    app.on_message_ok(|i| eprintln!("companion-sim: OK on message {i}"));
    let (weak, t) = (app.as_weak(), text.clone());
    app.on_notifications_toggled(move || {
        let Some(app) = weak.upgrade() else { return };
        app.set_notifications_hidden(!app.get_notifications_hidden());
        set_notifications(&app, &t.borrow(), !app.get_notifications_hidden());
    });

    let start = Instant::now();
    let done = Cell::new(false);
    let weak = app.as_weak();
    let wave_model = Rc::new(VecModel::from(wave("home", 0.0)));
    app.set_wave(ModelRc::from(wave_model.clone()));
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(1000 / 15), move || {
        let Some(app) = weak.upgrade() else { return };
        let t = start.elapsed().as_secs_f64();
        let (wday, day, month, hour, minute) = local_now();
        let clock = format!("{hour:02}:{minute:02}");
        if app.get_clock() != clock.as_str() {
            app.set_clock(clock.into());
            app.set_date(text.borrow().date(wday, day as u32, month).into());
        }
        let state = app.get_state();
        if state != "home" {
            for (i, v) in wave(&state, t).into_iter().enumerate() {
                wave_model.set_row_data(i, v);
            }
        }
        if !done.get() && args.seconds.is_some_and(|n| t >= n) {
            done.set(true);
            if let Some(path) = &args.screenshot {
                if let Err(e) = save_png(&app, path) {
                    eprintln!("companion-sim: {e}");
                }
            }
            let _ = slint::quit_event_loop();
        }
    });
    app.run().map_err(|e| format!("event loop: {e}"))
}
