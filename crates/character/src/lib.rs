// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Character packages: `characters/<id>/character.toml` plus `poses/<state>.png` and, optionally,
//! `animations/*.png` (sheets of frames, decoded by [`Sheet::load`]).
//! The format and its rules are in `characters/README.md`; [`load`] applies
//! every rule and returns all the problems at once, so a pull request with a
//! new character gets one complete list.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The `format` this crate reads.
pub const FORMAT: u32 = 1;
/// Licences accepted for a character's pictures and phrases.
pub const LICENSES: [&str; 3] = ["CC0-1.0", "CC-BY-4.0", "CC-BY-SA-4.0"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum State {
    Idle,
    Listening,
    Thinking,
    Speaking,
    Error,
    Offline,
    Muted,
    TimerFinished,
    /// Night mode (the body's schedule): asleep. Without its own pose the body dims idle.
    Sleeping,
}

impl State {
    pub const ALL: [State; 9] = [
        State::Idle,
        State::Listening,
        State::Thinking,
        State::Speaking,
        State::Error,
        State::Offline,
        State::Muted,
        State::TimerFinished,
        State::Sleeping,
    ];

    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Listening => "listening",
            State::Thinking => "thinking",
            State::Speaking => "speaking",
            State::Error => "error",
            State::Offline => "offline",
            State::Muted => "muted",
            State::TimerFinished => "timer_finished",
            State::Sleeping => "sleeping",
        }
    }

    /// The pose drawn when this state has none; `None` for the required states.
    pub fn fallback(self) -> Option<State> {
        match self {
            State::Offline | State::Muted | State::Sleeping => Some(State::Idle),
            // The presenting hand: "here it is" (the error pose looked sorry about the eggs).
            State::TimerFinished => Some(State::Speaking),
            _ => None,
        }
    }

    fn from_name(name: &str) -> Option<State> {
        State::ALL.into_iter().find(|s| s.name() == name)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An sRGB colour from `"#rrggbb"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    fn parse(s: &str) -> Option<Rgb> {
        let hex = s
            .strip_prefix('#')
            .filter(|h| h.len() == 6 && h.is_ascii())?;
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
    }
}

/// x0, y0, x1, y1 in the pose pictures (x1, y1 excluded).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Crop {
    pub fn width(&self) -> u32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> u32 {
        self.y1 - self.y0
    }
}

/// The phrases a character says on screen, in one language.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phrases {
    pub idle: String,
    pub listening: String,
    pub thinking: String,
    pub error: String,
    pub offline: String,
}

#[derive(Clone, Debug)]
pub struct Character {
    pub dir: PathBuf,
    pub id: String,
    pub name: String,
    pub license: String,
    pub authors: Vec<String>,
    pub wake_word: String,
    /// Width and height shared by every pose picture.
    pub pose_size: (u32, u32),
    pub background: Rgb,
    /// Where the mouth is in the idle pose: a double tap there opens the language menu.
    pub mouth: Crop,
    colors: BTreeMap<State, Rgb>,
    crop: Crop,
    crop_override: BTreeMap<State, Crop>,
    poses: BTreeMap<State, PathBuf>,
    /// In the order of `languages`: the first one is the default.
    phrases: Vec<(String, Phrases)>,
    animations: BTreeMap<String, Animation>,
}

/// The animations a character may have (characters/README.md, "Animations").
pub const ANIMATIONS: [&str; 10] = [
    "loading",
    "wake",
    "idle",
    "listening",
    "thinking",
    "speaking",
    "error",
    "fall_asleep",
    "sleeping",
    "wake_from_sleep",
];

/// One animation: frames `first..end` of a sheet, shown in `rect` (pose pixels) at `fps`.
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    pub name: String,
    pub file: PathBuf,
    /// Width and height of one frame in the sheet.
    pub size: (u32, u32),
    pub first: u32,
    pub end: u32,
    pub fps: f32,
    /// Seconds still on the last frame before playing again (the idle animation).
    pub pause: f32,
    /// x0, y0, x1, y1 in the pose pictures: where the frames are drawn (scaled to fill it).
    pub rect: [f32; 4],
}

impl Animation {
    pub fn frames(&self) -> u32 {
        self.end - self.first
    }

    /// Seconds to play every frame once.
    pub fn seconds(&self) -> f32 {
        self.frames() as f32 / self.fps
    }
}

impl Character {
    /// Colour of a state (the optional states use their fallback's colour).
    pub fn color(&self, state: State) -> Rgb {
        let mut s = state;
        loop {
            if let Some(c) = self.colors.get(&s) {
                return *c;
            }
            s = s.fallback().expect("required colours are checked by load");
        }
    }

    /// Whether the character has a pose of its own for `state` (no fallback).
    pub fn has_pose(&self, state: State) -> bool {
        self.poses.contains_key(&state)
    }

    /// The picture for a state, after the fallbacks of [`State::fallback`].
    pub fn pose(&self, state: State) -> (&Path, Crop) {
        let mut s = state;
        loop {
            if let Some(p) = self.poses.get(&s) {
                return (p, self.crop_override.get(&s).copied().unwrap_or(self.crop));
            }
            s = s.fallback().expect("required poses are checked by load");
        }
    }

    pub fn languages(&self) -> impl Iterator<Item = &str> {
        self.phrases.iter().map(|(l, _)| l.as_str())
    }

    pub fn animation(&self, name: &str) -> Option<&Animation> {
        self.animations.get(name)
    }

    pub fn animations(&self) -> impl Iterator<Item = &Animation> {
        self.animations.values()
    }

    /// The animation of a state: its own, or its fallback's when the state has no pose of its
    /// own (the ringing timer moves like `error`). `idle` (and the states that fall back to it)
    /// have none here: the body plays `wake` and `idle` by its own rules.
    pub fn state_animation(&self, state: State) -> Option<&Animation> {
        let mut s = state;
        loop {
            if s == State::Idle {
                return None;
            }
            if let Some(a) = self.animations.get(s.name()) {
                return Some(a);
            }
            if self.poses.contains_key(&s) {
                return None;
            }
            s = s.fallback()?;
        }
    }

    /// Phrases in `lang`, or in the first language of the character.
    pub fn phrases(&self, lang: &str) -> &Phrases {
        let found = self.phrases.iter().find(|(l, _)| l == lang);
        &found.unwrap_or(&self.phrases[0]).1
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    format: u32,
    id: String,
    name: String,
    license: String,
    #[serde(default)]
    authors: Vec<String>,
    languages: Vec<String>,
    wake_word: String,
    crop: [u32; 4],
    mouth: [u32; 4],
    #[serde(default)]
    crop_override: BTreeMap<String, [u32; 4]>,
    colors: BTreeMap<String, String>,
    #[serde(default)]
    phrases: BTreeMap<String, Phrases>,
    #[serde(default)]
    animations: BTreeMap<String, RawAnimation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnimation {
    file: String,
    size: [u32; 2],
    frames: [u32; 2],
    fps: f32,
    #[serde(default)]
    pause: f32,
    rect: [f32; 4],
}

/// Characters the screen fonts carry (characters/README.md, "Text").
/// Keep in step with `glyphs` in crates/spike-ui/ui/companion.slint.
pub fn printable(c: char) -> bool {
    matches!(c, ' '..='~' | '\u{a0}'..='\u{ff}' | '…' | '–' | '—' | '•')
}

/// Loads `dir/character.toml` and checks every rule; `Err` lists all the problems.
pub fn load(dir: &Path) -> Result<Character, Vec<String>> {
    let mut errors = Vec::new();
    let file = dir.join("character.toml");
    let text =
        std::fs::read_to_string(&file).map_err(|e| vec![format!("{}: {e}", file.display())])?;
    let raw: Raw = toml::from_str(&text).map_err(|e| vec![format!("{}: {e}", file.display())])?;

    if raw.format != FORMAT {
        errors.push(format!(
            "format {} (this body reads format {FORMAT})",
            raw.format
        ));
    }
    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if raw.id != dir_name {
        errors.push(format!(
            "id \"{}\" is not the directory name \"{dir_name}\"",
            raw.id
        ));
    }
    if raw.id.is_empty()
        || !raw
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push(format!(
            "id \"{}\": only lowercase letters, digits and '-'",
            raw.id
        ));
    }
    if !LICENSES.contains(&raw.license.as_str()) {
        errors.push(format!(
            "license \"{}\" is not one of {LICENSES:?}",
            raw.license
        ));
    }

    let mut check_text = |what: &str, s: &str| {
        if s.trim().is_empty() {
            errors.push(format!("{what} is empty"));
        }
        if let Some(c) = s.chars().find(|c| !printable(*c)) {
            errors.push(format!(
                "{what}: character {c:?} (U+{:04X}) is not in the screen fonts",
                c as u32
            ));
        }
    };
    check_text("name", &raw.name);
    check_text("wake_word", &raw.wake_word);
    for (lang, p) in &raw.phrases {
        for (key, s) in [
            ("idle", &p.idle),
            ("listening", &p.listening),
            ("thinking", &p.thinking),
            ("error", &p.error),
            ("offline", &p.offline),
        ] {
            check_text(&format!("phrases.{lang}.{key}"), s);
        }
    }
    if raw.languages.is_empty() {
        errors.push("languages is empty".into());
    }
    for lang in &raw.languages {
        if !raw.phrases.contains_key(lang) {
            errors.push(format!("language \"{lang}\" has no [phrases.{lang}]"));
        }
    }
    for lang in raw.phrases.keys() {
        if !raw.languages.contains(lang) {
            errors.push(format!("[phrases.{lang}] is not in languages"));
        }
    }

    let background = Rgb::parse(raw.colors.get("background").map_or("", String::as_str))
        .unwrap_or_else(|| {
            errors.push("colors.background: missing or not \"#rrggbb\"".into());
            Rgb(0, 0, 0)
        });
    let mut colors = BTreeMap::new();
    for (key, value) in &raw.colors {
        if key == "background" {
            continue;
        }
        match (State::from_name(key), Rgb::parse(value)) {
            (Some(s), Some(c)) => {
                colors.insert(s, c);
            }
            (None, _) => errors.push(format!("colors.{key}: not a state")),
            (_, None) => errors.push(format!("colors.{key} = \"{value}\": not \"#rrggbb\"")),
        }
    }

    // Poses: required ones present, optional ones only if the file exists, all the same size.
    let mut poses = BTreeMap::new();
    let mut size: Option<(u32, u32, State)> = None;
    for state in State::ALL {
        let path = dir.join("poses").join(format!("{state}.png"));
        if !path.exists() {
            if state.fallback().is_none() {
                errors.push(format!("poses/{state}.png is missing (required)"));
            }
            continue;
        }
        match png_size(&path) {
            Ok(wh) => match size {
                Some((w, h, first)) if (w, h) != wh => errors.push(format!(
                    "poses/{state}.png is {}x{}, poses/{first}.png is {w}x{h}: all poses must be the same size",
                    wh.0, wh.1
                )),
                None => size = Some((wh.0, wh.1, state)),
                _ => {}
            },
            Err(e) => errors.push(format!("poses/{state}.png: {e}")),
        }
        poses.insert(state, path);
    }
    for state in State::ALL.into_iter().filter(|s| s.fallback().is_none()) {
        if !colors.contains_key(&state) {
            errors.push(format!("colors.{state} is missing (required)"));
        }
    }
    let pose_size = size.map_or((0, 0), |(w, h, _)| (w, h));

    let check_crop = |errors: &mut Vec<String>, what: &str, c: [u32; 4]| {
        let crop = Crop {
            x0: c[0],
            y0: c[1],
            x1: c[2],
            y1: c[3],
        };
        if crop.x0 >= crop.x1
            || crop.y0 >= crop.y1
            || crop.x1 > pose_size.0
            || crop.y1 > pose_size.1
        {
            errors.push(format!(
                "{what} = {c:?} is empty or outside the {}x{} poses",
                pose_size.0, pose_size.1
            ));
        }
        crop
    };
    let crop = check_crop(&mut errors, "crop", raw.crop);
    let mouth = check_crop(&mut errors, "mouth", raw.mouth);
    if mouth.x0 < crop.x0 || mouth.x1 > crop.x1 || mouth.y0 < crop.y0 || mouth.y1 > crop.y1 {
        errors.push(format!("mouth = {:?} is outside crop", raw.mouth));
    }
    let mut crop_override = BTreeMap::new();
    for (key, c) in &raw.crop_override {
        let Some(state) = State::from_name(key) else {
            errors.push(format!("crop_override.{key}: not a state"));
            continue;
        };
        let o = check_crop(&mut errors, &format!("crop_override.{key}"), *c);
        if o.height() != crop.height() {
            errors.push(format!(
                "crop_override.{key}: height {} differs from crop's {}",
                o.height(),
                crop.height()
            ));
        }
        crop_override.insert(state, o);
    }

    let mut animations = BTreeMap::new();
    for (name, a) in &raw.animations {
        let what = format!("animations.{name}");
        if !ANIMATIONS.contains(&name.as_str()) {
            errors.push(format!("{what}: not one of {ANIMATIONS:?}"));
            continue;
        }
        if !(a.fps > 0.0 && a.fps <= 60.0) {
            errors.push(format!("{what}.fps = {}: 1 to 60", a.fps));
        }
        if !(a.pause >= 0.0 && a.pause <= 600.0) {
            errors.push(format!("{what}.pause = {}: 0 to 600 seconds", a.pause));
        }
        let [x0, y0, x1, y1] = a.rect;
        let (pw, ph) = (pose_size.0 as f32, pose_size.1 as f32);
        if !(x0 < x1 && y0 < y1 && x0 >= 0.0 && y0 >= 0.0 && x1 <= pw && y1 <= ph) {
            errors.push(format!(
                "{what}.rect = {:?} is empty or outside the {pw}x{ph} poses",
                a.rect
            ));
        }
        if a.frames[0] >= a.frames[1] {
            errors.push(format!(
                "{what}.frames = {:?}: first, end (end excluded), at least one frame",
                a.frames
            ));
        }
        let file = dir.join(&a.file);
        if a.file.contains("..") || !a.file.starts_with("animations/") {
            errors.push(format!(
                "{what}.file = {:?}: must be in animations/",
                a.file
            ));
        } else {
            match sheet_size(&file) {
                Ok((w, h)) => {
                    let [fw, fh] = a.size;
                    if fw != w || fh == 0 || h % fh != 0 {
                        errors.push(format!(
                            "{what}: {} is {w}x{h}, not frames of {fw}x{fh} stacked",
                            a.file
                        ));
                    } else if a.frames[1] > h / fh {
                        errors.push(format!(
                            "{what}.frames = {:?}: {} has {} frames",
                            a.frames,
                            a.file,
                            h / fh
                        ));
                    }
                }
                Err(e) => errors.push(format!("{what}: {}: {e}", a.file)),
            }
        }
        animations.insert(
            name.clone(),
            Animation {
                name: name.clone(),
                file,
                size: (a.size[0], a.size[1]),
                first: a.frames[0],
                end: a.frames[1],
                fps: a.fps,
                pause: a.pause,
                rect: a.rect,
            },
        );
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Character {
        dir: dir.to_path_buf(),
        id: raw.id,
        name: raw.name,
        license: raw.license,
        authors: raw.authors,
        wake_word: raw.wake_word,
        pose_size,
        background,
        mouth,
        colors,
        crop,
        crop_override,
        poses,
        phrases: raw
            .languages
            .iter()
            .map(|l| (l.clone(), raw.phrases[l].clone()))
            .collect(),
        animations,
    })
}

/// Where the figure stands in a pose: top of the head, bottom of the feet and
/// the horizontal centre of the feet (pixels that differ from the background).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Figure {
    pub top: u32,
    pub bottom: u32,
    pub feet_centre: f32,
}

/// Finds the figure in a pose picture: pixels more than `THRESHOLD` away from
/// `background` in any channel; the feet are the lowest 12% of the figure.
pub fn figure(path: &Path, background: Rgb) -> Result<Figure, String> {
    const THRESHOLD: i16 = 24;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut data = vec![0; reader.output_buffer_size().ok_or("png too large")?];
    let info = reader.next_frame(&mut data).map_err(|e| e.to_string())?;
    let (w, h, ch) = (
        info.width as usize,
        info.height as usize,
        info.color_type.samples(),
    );
    let bg = [
        background.0 as i16,
        background.1 as i16,
        background.2 as i16,
    ];
    let is_figure = |x: usize, y: usize| {
        let p = &data[y * info.line_size + x * ch..];
        let alpha = if ch == 4 { p[3] } else { 255 };
        alpha > 128 && (0..3).any(|i| (p[i] as i16 - bg[i]).abs() > THRESHOLD)
    };
    let rows: Vec<bool> = (0..h).map(|y| (0..w).any(|x| is_figure(x, y))).collect();
    let top = rows
        .iter()
        .position(|r| *r)
        .ok_or("no figure found (all background)")?;
    let bottom = rows.iter().rposition(|r| *r).unwrap_or(top);
    let feet_from = bottom - (bottom - top) * 12 / 100;
    let (mut left, mut right) = (w, 0);
    for y in feet_from..=bottom {
        for x in 0..w {
            if is_figure(x, y) {
                left = left.min(x);
                right = right.max(x);
            }
        }
    }
    Ok(Figure {
        top: top as u32,
        bottom: bottom as u32,
        feet_centre: (left + right) as f32 / 2.0,
    })
}

/// The rule "same place and same scale in every pose" (characters/README.md):
/// every pose without its own crop has the idle figure's height within 2%, its
/// feet at the same height and centre within 1.5% of the picture height. A `sleeping` pose
/// reached through a `fall_asleep` animation is exempt: the animation carries the figure there
/// (Alfred, asleep, stands a little lower and smaller).
/// Separate from [`load`] because it decodes every picture (slow on the device).
pub fn check_alignment(c: &Character) -> Vec<String> {
    let mut errors = Vec::new();
    let (idle_path, _) = c.pose(State::Idle);
    let idle = match figure(idle_path, c.background) {
        Ok(f) => f,
        Err(e) => return vec![format!("poses/idle.png: {e}")],
    };
    let tolerance = c.pose_size.1 as f32 * 0.015;
    let idle_height = (idle.bottom - idle.top) as f32;
    for (state, path) in &c.poses {
        if *state == State::Idle
            || c.crop_override.contains_key(state)
            || (*state == State::Sleeping && c.animations.contains_key("fall_asleep"))
        {
            continue;
        }
        let f = match figure(path, c.background) {
            Ok(f) => f,
            Err(e) => {
                errors.push(format!("poses/{state}.png: {e}"));
                continue;
            }
        };
        let height = (f.bottom - f.top) as f32;
        if (height / idle_height - 1.0).abs() > 0.02 {
            errors.push(format!(
                "poses/{state}.png: the figure is {height} px tall, in idle {idle_height} px (scale it by {:.3})",
                idle_height / height
            ));
        }
        if (f.bottom as f32 - idle.bottom as f32).abs() > tolerance {
            errors.push(format!(
                "poses/{state}.png: the feet end at y {}, in idle at y {}",
                f.bottom, idle.bottom
            ));
        }
        if (f.feet_centre - idle.feet_centre).abs() > tolerance {
            errors.push(format!(
                "poses/{state}.png: the feet are centred on x {}, in idle on x {}",
                f.feet_centre, idle.feet_centre
            ));
        }
    }
    errors
}

/// Width and height of a PNG, which must be 8 bits per channel, RGB or RGBA.
fn png_size(path: &Path) -> Result<(u32, u32), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.bit_depth != png::BitDepth::Eight
        || !matches!(info.color_type, png::ColorType::Rgb | png::ColorType::Rgba)
    {
        return Err(format!(
            "{:?} {:?}: must be 8-bit RGB or RGBA",
            info.color_type, info.bit_depth
        ));
    }
    Ok((info.width, info.height))
}

/// Width and height of an animation sheet, which must be an 8-bit RGBA PNG.
fn sheet_size(path: &Path) -> Result<(u32, u32), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.bit_depth != png::BitDepth::Eight
        || info.color_type != png::ColorType::Rgba
        || info.interlaced
    {
        return Err(format!(
            "{:?} {:?}: must be 8-bit RGBA, not interlaced",
            info.color_type, info.bit_depth
        ));
    }
    Ok((info.width, info.height))
}

/// A run of changed pixels: `len` pixels from pixel index `start` (row-major).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub start: u32,
    pub len: u32,
}

/// What changes from one frame to the next: runs of pixels and their RGBA values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    pub runs: Vec<Run>,
    /// RGBA, 4 bytes per pixel of `runs`, in order.
    pub pixels: Vec<u8>,
    /// First and last row touched (inclusive); None when nothing changes.
    pub rows: Option<(u32, u32)>,
}

impl Delta {
    /// Writes the changed pixels into an RGBA frame of `width` pixels per row.
    pub fn apply(&self, frame: &mut [u8]) {
        let mut p = 0;
        for r in &self.runs {
            let (at, n) = (r.start as usize * 4, r.len as usize * 4);
            frame[at..at + n].copy_from_slice(&self.pixels[p..p + n]);
            p += n;
        }
    }

    fn bytes(&self) -> usize {
        self.pixels.len() + self.runs.len() * std::mem::size_of::<Run>()
    }
}

/// RGBA pixels within `tolerance` (per channel) of `bg` become exactly `bg`.
pub fn flatten(rgba: &mut [u8], bg: Rgb, tolerance: u8) {
    for p in rgba.as_chunks_mut::<4>().0 {
        if p[0].abs_diff(bg.0) <= tolerance
            && p[1].abs_diff(bg.1) <= tolerance
            && p[2].abs_diff(bg.2) <= tolerance
        {
            p[0] = bg.0;
            p[1] = bg.1;
            p[2] = bg.2;
        }
    }
}

/// A decoded animation sheet (characters/README.md, "Animations"): frame 0 complete, every
/// other frame as the pixels that change from the previous one. In memory only the changes are
/// kept (Alfred: 1-6 % of the pixels per frame), so every animation stays loaded.
#[derive(Clone, Debug)]
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    /// `frames[0]` covers every pixel.
    pub frames: Vec<Delta>,
}

impl Sheet {
    /// Decodes a sheet of `size` frames row by row (the whole sheet is never in memory). Fails if
    /// frame 0 is not opaque or an alpha is neither 0 nor 255.
    pub fn load(path: &Path, size: (u32, u32)) -> Result<Sheet, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let (w, h) = (reader.info().width, reader.info().height);
        let (fw, fh) = size;
        if reader.info().color_type != png::ColorType::Rgba
            || reader.info().bit_depth != png::BitDepth::Eight
            || reader.info().interlaced
        {
            return Err(format!(
                "{}: must be 8-bit RGBA, not interlaced",
                path.display()
            ));
        }
        if w != fw || fh == 0 || h % fh != 0 {
            return Err(format!(
                "{}: {w}x{h} is not frames of {fw}x{fh} stacked",
                path.display()
            ));
        }
        let mut frames = Vec::with_capacity((h / fh) as usize);
        let mut delta = Delta::default();
        let mut y = 0u32;
        while let Some(row) = reader
            .next_row()
            .map_err(|e| format!("{}: {e}", path.display()))?
        {
            let data = row.data();
            let (frame, fy) = (y / fh, y % fh);
            let mut x = 0u32;
            while x < w {
                let a = data[x as usize * 4 + 3];
                if a != 0 && a != 255 {
                    return Err(format!(
                        "{}: frame {frame}, pixel {x},{fy}: alpha {a} (0 or 255)",
                        path.display()
                    ));
                }
                if a == 0 {
                    if frame == 0 {
                        return Err(format!(
                            "{}: frame 0 must be opaque (pixel {x},{fy})",
                            path.display()
                        ));
                    }
                    x += 1;
                    continue;
                }
                let start = x;
                while x < w && data[x as usize * 4 + 3] == 255 {
                    x += 1;
                }
                delta.runs.push(Run {
                    start: fy * fw + start,
                    len: x - start,
                });
                delta
                    .pixels
                    .extend_from_slice(&data[start as usize * 4..x as usize * 4]);
                delta.rows = Some(delta.rows.map_or((fy, fy), |(a, _)| (a, fy)));
            }
            y += 1;
            if y.is_multiple_of(fh) {
                delta.runs.shrink_to_fit();
                delta.pixels.shrink_to_fit();
                frames.push(std::mem::take(&mut delta));
            }
        }
        Ok(Sheet {
            width: fw,
            height: fh,
            frames,
        })
    }

    /// Bytes held for the frames.
    pub fn bytes(&self) -> usize {
        self.frames.iter().map(Delta::bytes).sum()
    }

    /// Pixels within `tolerance` (per channel) of `bg` become exactly `bg`, in every frame (the
    /// body does the same to the poses, so the pictures' plates match the window).
    pub fn flatten(&mut self, bg: Rgb, tolerance: u8) {
        for d in &mut self.frames {
            flatten(&mut d.pixels, bg, tolerance);
        }
    }

    /// Frame `index` complete (RGBA), applying the changes from frame 0.
    pub fn frame(&self, index: usize) -> Vec<u8> {
        let mut f = vec![0; (self.width * self.height * 4) as usize];
        for d in &self.frames[..=index] {
            d.apply(&mut f);
        }
        f
    }
}

/// Decodes every animation sheet of a character (slow on the device: the body decodes them in
/// a thread; here for the check of a pull request).
pub fn check_animations(c: &Character) -> Vec<String> {
    let mut errors = Vec::new();
    for a in c.animations() {
        if let Err(e) = Sheet::load(&a.file, a.size) {
            errors.push(format!("animations.{}: {e}", a.name));
        }
    }
    errors
}

/// The `characters/` directory of this repository.
pub fn repo_characters() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../characters")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character in the repository follows the format, poses aligned
    /// included (the check for pull requests).
    #[test]
    fn repo_characters_are_valid() {
        let mut found = 0;
        for entry in std::fs::read_dir(repo_characters()).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_dir() {
                continue;
            }
            found += 1;
            let errors = match load(&path) {
                Ok(c) => check_alignment(&c)
                    .into_iter()
                    .chain(check_animations(&c))
                    .collect(),
                Err(errors) => errors,
            };
            if !errors.is_empty() {
                panic!("{}:\n  {}", path.display(), errors.join("\n  "));
            }
        }
        assert!(found > 0, "no characters found");
    }

    #[test]
    fn alfred() {
        let c = load(&repo_characters().join("alfred")).unwrap();
        assert_eq!(c.pose_size, (1024, 1024));
        assert_eq!(c.languages().collect::<Vec<_>>(), ["en", "it"]);
        assert_eq!(c.phrases("it").listening, "Ti ascolto…");
        assert_eq!(c.phrases("xx").listening, "I'm listening…"); // unknown language: the first one
        assert_eq!(
            c.pose(State::Offline).1,
            Crop {
                x0: 128,
                y0: 0,
                x1: 888,
                y1: 1024
            }
        );
        assert!(
            c.pose(State::TimerFinished)
                .0
                .ends_with("poses/speaking.png")
        ); // fallback
        assert!(c.pose(State::Sleeping).0.ends_with("poses/sleeping.png"));
        assert!(c.has_pose(State::Sleeping));
        assert_eq!(c.color(State::Muted), c.color(State::Idle));
        // Animations: the loading sheet holds idle and wake-up; the ringing timer moves like speaking.
        assert_eq!(
            c.animation("idle").unwrap().file,
            c.animation("loading").unwrap().file
        );
        assert_eq!(c.animation("wake").unwrap().frames(), 80);
        // The night: one sheet, fall asleep, sleep (now and then), wake up.
        let night = ["fall_asleep", "sleeping", "wake_from_sleep"].map(|n| c.animation(n).unwrap());
        assert!(night.iter().all(|a| a.file == night[0].file));
        assert_eq!(night[1].pause, 60.0);
        assert_eq!(night.map(|a| a.frames()), [80, 64, 80]);
        assert_eq!(
            c.state_animation(State::TimerFinished).unwrap().name,
            "speaking"
        );
        assert!(c.state_animation(State::Idle).is_none());
        assert!(c.state_animation(State::Offline).is_none());
    }

    /// The sheet format: frame 0 complete, then only the changed pixels.
    #[test]
    fn sheet_decodes_deltas() {
        let dir = std::env::temp_dir().join(format!("character-sheet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.png");
        // 3x2 frames, 3 frames: frame 1 changes pixel (1,0), frame 2 changes nothing.
        let mut data = vec![0u8; 3 * 6 * 4];
        for p in 0..6 {
            data[p * 4..p * 4 + 4].copy_from_slice(&[10, 20, 30, 255]);
        }
        data[(6 + 1) * 4..(6 + 1) * 4 + 4].copy_from_slice(&[200, 100, 50, 255]);
        let mut enc = png::Encoder::new(
            std::io::BufWriter::new(std::fs::File::create(&path).unwrap()),
            3,
            6,
        );
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(&data).unwrap();
        let s = Sheet::load(&path, (3, 2)).unwrap();
        assert_eq!(s.frames.len(), 3);
        assert_eq!(
            s.frames[0].runs,
            vec![Run { start: 0, len: 3 }, Run { start: 3, len: 3 }]
        );
        assert_eq!(s.frames[1].runs, vec![Run { start: 1, len: 1 }]);
        assert_eq!(s.frames[1].rows, Some((0, 0)));
        assert_eq!(s.frames[2].rows, None);
        assert_eq!(&s.frame(2)[4..8], &[200, 100, 50, 255]);
        assert_eq!(&s.frame(0)[4..8], &[10, 20, 30, 255]);
        // A transparent pixel in frame 0 is refused.
        data[0..4].copy_from_slice(&[0, 0, 0, 0]);
        let mut enc = png::Encoder::new(
            std::io::BufWriter::new(std::fs::File::create(&path).unwrap()),
            3,
            6,
        );
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(&data).unwrap();
        assert!(
            Sheet::load(&path, (3, 2))
                .unwrap_err()
                .contains("frame 0 must be opaque")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A pose moved down by 40 px is reported (the check that caught Alfred's listening pose).
    #[test]
    fn misaligned_pose_is_reported() {
        let alfred = repo_characters().join("alfred");
        let dir = std::env::temp_dir()
            .join(format!("character-align-{}", std::process::id()))
            .join("alfred");
        std::fs::create_dir_all(dir.join("poses")).unwrap();
        std::fs::copy(alfred.join("character.toml"), dir.join("character.toml")).unwrap();
        std::os::unix::fs::symlink(alfred.join("animations"), dir.join("animations")).unwrap();
        for e in std::fs::read_dir(alfred.join("poses")).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), dir.join("poses").join(e.file_name())).unwrap();
        }
        // listening.png = idle.png shifted down by 40 rows.
        let mut decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(alfred.join("poses/idle.png")).unwrap(),
        ));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut data).unwrap();
        let line = info.line_size;
        let mut shifted = data[..line * info.height as usize].to_vec();
        shifted.copy_within(0..line * (info.height as usize - 40), line * 40);
        let out = std::fs::File::create(dir.join("poses/listening.png")).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(out), info.width, info.height);
        enc.set_color(info.color_type);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&shifted)
            .unwrap();

        let errors = check_alignment(&load(&dir).unwrap()).join("\n");
        assert!(
            errors.contains("poses/listening.png: the feet end at y"),
            "{errors}"
        );
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn problems_are_listed() {
        let dir = std::env::temp_dir()
            .join(format!("character-test-{}", std::process::id()))
            .join("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("character.toml"),
            r##"format = 2
id = "Bad"
name = "Bad"
license = "proprietary"
languages = ["it"]
wake_word = "Hey “Bad”"
crop = [0, 0, 10, 10]
mouth = [2, 2, 4, 4]
[colors]
background = "#000000"
idle = "red"
[animations.dance]
file = "animations/dance.png"
size = [1, 1]
frames = [0, 1]
fps = 16
rect = [0, 0, 1, 1]
[animations.idle]
file = "../idle.png"
size = [1, 1]
frames = [2, 1]
fps = 0
rect = [0, 0, 1, 1]
"##,
        )
        .unwrap();
        let errors = load(&dir).unwrap_err().join("\n");
        for want in [
            "format 2",
            "directory name",
            "lowercase",
            "license",
            "U+201C",
            "has no [phrases.it]",
            "not \"#rrggbb\"",
            "poses/idle.png is missing",
            "colors.listening is missing",
            "animations.dance: not one of",
            "animations.idle.fps",
            "animations.idle.frames",
            "must be in animations/",
        ] {
            assert!(errors.contains(want), "missing {want:?} in:\n{errors}");
        }
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
