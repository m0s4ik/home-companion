// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The companion screen: crates/spike-ui/ui/companion.slint compiled once by build.rs, with
//! fonts and icons embedded as files, for both renderers (`gpu` is the same module as `sw`).
//! [`Screen`] is what the body uses of the generated `CompanionWindow`; every other piece of UI
//! code is generic over it.

pub mod anim;
pub mod backlight;
pub mod cards;
pub mod control;
pub mod house;
pub mod keycombo;
pub mod light;
pub mod lists;
pub mod mirror;
pub mod mirror_video;
pub mod next;
pub mod platform_fb;
pub mod renderer;
pub mod screen;
pub mod sensors;
pub mod touch;
pub mod weather;
pub mod webview;

use slint::{Color, ComponentHandle, Image, Model, ModelRc, VecModel};

pub mod sw {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/companion.rs"));
}

/// The GPU path uses the same generated code (femtovg reads the embedded fonts and icons).
#[cfg(feature = "gpu")]
pub mod gpu {
    pub use super::sw::*;
}

/// A card of the home's middle band (cards.rs picks them).
#[derive(Debug, Clone, PartialEq)]
pub struct CardView {
    /// "status", "reminder" (due, with OK), "timer", "more", "alarm", "upcoming" (the next
    /// reminder), "shopping", "music", "answer".
    pub kind: &'static str,
    pub title: String,
    pub text: String,
    pub left: String,
    pub remaining: f32,
    pub paused: bool,
}

/// A timer of the timers page.
#[derive(Debug, Clone, PartialEq)]
pub struct TimerLineView {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub left: String,
    pub remaining: f32,
    pub paused: bool,
}

/// An alarm of the timers page.
#[derive(Debug, Clone, PartialEq)]
pub struct AlarmLineView {
    pub id: u32,
    pub time: String,
    pub days: String,
    pub on: bool,
}

/// The values of the settings page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsView {
    pub language_name: String,
    pub brains: String,
    pub ha_status: String,
    pub version: String,
    pub screen_status: String,
    pub save_chat: bool,
    pub night_on: bool,
    pub night_start: u32,
    pub night_end: u32,
}

/// A check of Settings → Home Assistant, in the screen's words.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HaLine {
    pub id: String,
    pub label: String,
    pub detail: String,
    /// 0 ok, 1 warn, 2 bad, 3 skipped, 4 not checked.
    pub level: i32,
    pub repair: bool,
}

/// The cloud services' use on screen (usage.rs): the row's line and the page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageView {
    pub label: String,
    /// The Home Assistant page's row: today only.
    pub line: String,
    /// The page: "Today" (or "Today · estimated") over today's amount, this month's line and
    /// one quiet line of details.
    pub today_label: String,
    pub today: String,
    pub month: String,
    pub detail: String,
    /// Over the chart ("Last 30 days").
    pub chart_label: String,
    /// The tallest bar's amount (the chart's top line; empty without any use).
    pub max: String,
    /// The last days, oldest first: (label, height 0..1, amount).
    pub bars: Vec<(String, f32, String)>,
    /// At the bottom: where precise numbers come from, or that they are precise.
    pub note: String,
    pub precise: bool,
}

/// What Settings → Home Assistant shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HaPageView {
    pub rows: Vec<HaLine>,
    /// The line over the rows ("" = the introduction).
    pub status: String,
    pub busy: bool,
    pub configured: bool,
    /// Ended well after the user asked: "All set" and an OK.
    pub done: bool,
    /// The choices (kind, label, the one in use): the AI, speech-to-text, voice.
    pub picks: Vec<(String, String, String)>,
    /// What the setup is doing right now (the pop-up while busy; "" = "a few seconds").
    pub step: String,
    /// The working pop-up is up: a run the user asked for.
    pub popup: bool,
}

/// What plays (AirPlay later; nothing yet).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MusicView {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub source: String,
}

/// The setters, getters and callbacks of `CompanionWindow` the body uses, with plain Rust
/// types.
pub trait Screen: Sized + 'static + webview::CasaScreen {
    fn create() -> Result<Self, slint::PlatformError>;
    fn window(&self) -> &slint::Window;
    fn run(&self) -> Result<(), slint::PlatformError>;

    fn set_state(&self, s: &str);
    fn get_state(&self) -> String;
    fn set_page(&self, s: &str);
    fn get_page(&self) -> String;
    fn get_dialog(&self) -> String;
    fn set_dialog(&self, s: &str);
    fn set_pose(&self, i: Image);
    fn set_error_pose(&self, i: Image);
    fn set_dnd_prop(&self, i: Image);
    /// An announcement plays (a due reminder stays on the screen under its voice).
    fn set_announcing(&self, on: bool);
    fn set_music_prop(&self, i: Image);
    /// An animation frame drawn instead of the pose (`Image::default()` = the pose).
    fn set_frame(&self, i: Image);
    fn set_frame_rect(&self, x: f32, y: f32, w: f32, h: f32);
    fn set_character_name(&self, s: &str);
    fn set_character_background(&self, c: Color);
    fn set_color_listening(&self, c: Color);
    fn set_color_thinking(&self, c: Color);
    fn set_color_speaking(&self, c: Color);
    fn set_color_error(&self, c: Color);
    fn set_phrase_listening(&self, s: &str);
    fn set_phrase_thinking(&self, s: &str);
    fn set_phrase_error(&self, s: &str);
    fn set_phrase_idle(&self, s: &str);
    fn set_phrase_starting(&self, s: &str);
    /// The body's words: `word(key)` for each field of `Words` (locales' `[text]` keys).
    fn set_words(&self, word: &dyn Fn(&str) -> String);
    /// Monday first.
    fn set_day_letters(&self, letters: &[String]);
    /// (code, name, selected)
    fn set_languages(&self, rows: &[(String, String, bool)]);
    fn set_mouth(&self, x: f32, y: f32, w: f32, h: f32);
    fn set_clock(&self, s: &str);
    fn get_clock(&self) -> String;
    fn set_date(&self, s: &str);
    /// The weather on the date line: (Home Assistant's condition, its words, "14°"); `None`
    /// removes it.
    fn set_weather(&self, w: Option<(&str, &str, &str)>);
    fn set_cards(&self, cards: &[CardView]);
    /// The weather page: the condition's icon (MDI path data), today's high and low, the
    /// daily forecast.
    fn set_weather_page(&self, icon: &str, high: &str, low: &str, rows: &[weather::ForecastRow]);
    /// Home Assistant → Lists and calendars: (entity id, name, on) of the lists and of the
    /// calendars.
    fn set_choices(&self, todo: &[(String, String, bool)], calendars: &[(String, String, bool)]);
    fn on_choice_toggled(&self, f: impl Fn(String) + 'static);
    fn set_offline(&self, offline: bool);
    fn set_starting(&self, starting: bool);
    /// Nothing moves by itself (demo screenshots): the start-up line, the ringing ring.
    fn set_still(&self, still: bool);
    /// Night mode: on, its one line ("" none), whether that line is an alarm.
    fn set_night(&self, on: bool, line: &str, alarm: bool);
    fn set_heard(&self, s: &str);
    fn set_answer(&self, s: &str);
    fn set_wave(&self, bars: &[f32]);
    fn set_ringing(&self, name: &str, since: &str, alarm: bool);
    /// The due reminder full screen (shown, title, its words, when it was for, "in 10 min").
    fn set_notice(&self, shown: bool, title: &str, text: &str, when: &str, later: &str);
    /// (kind, text): "sep", "user", "character".
    fn set_chat(&self, rows: &[(String, String)]);
    fn set_timer_rows(&self, rows: &[TimerLineView]);
    fn set_alarm_rows(&self, rows: &[AlarmLineView]);
    fn set_undo(&self, text: &str);
    fn set_tiles(&self, chat: &str, timers: &str, settings: &str);
    fn set_settings_values(&self, v: &SettingsView);
    fn set_setup(&self, wifi: &str, ha: &str);
    /// The link page: its QR code (None = leave it), a status line, linked.
    fn set_setup_link(&self, qr: Option<Image>, status: &str, linked: bool);
    fn set_music(&self, m: Option<&MusicView>);
    /// The control center: open or not, its tiles, the brightness (percent) and "Auto".
    fn set_control(
        &self,
        open: bool,
        tiles: &[control::ControlTileView],
        brightness: u8,
        auto: bool,
        auto_label: &str,
    );
    fn set_control_open(&self, open: bool);
    fn get_control_open(&self) -> bool;
    /// AirPlay's cover art (None: an empty square) and paused (the play glyph).
    fn set_music_cover(&self, cover: Option<Image>, paused: bool);
    fn on_control_tile(&self, f: impl Fn(String) + 'static);
    fn on_control_opened(&self, f: impl Fn() + 'static);
    fn on_control_volume(&self, f: impl Fn(i32) + 'static);
    fn on_control_brightness(&self, f: impl Fn(i32) + 'static);
    fn on_control_brightness_auto(&self, f: impl Fn() + 'static);
    /// Screen mirroring: a session is on, the sender's name, the video frame's size (pixels).
    fn set_mirror(&self, active: bool, source: &str, width: u32, height: u32);
    /// The mirrored video (the transport) or the demos' picture.
    fn set_mirror_frame(&self, frame: Image);
    /// The demos: the page's opening frozen at this point (0..1; below 0 = animated).
    fn set_mirror_freeze(&self, t: f32);
    /// The controls over a mirrored screen that fills the panel are shown.
    fn set_mirror_chrome(&self, shown: bool);
    /// A touch on the mirroring page (its controls stay a while longer).
    fn on_mirror_poked(&self, f: impl Fn() + 'static);
    /// The character's face (the conversation bar over the mirrored video).
    fn set_face(&self, face: Image);
    fn on_music_toggle(&self, f: impl Fn() + 'static);
    fn on_music_next(&self, f: impl Fn() + 'static);
    /// "Interrompi" on the mirroring page: the sender's session is closed.
    fn on_mirror_stop(&self, f: impl Fn() + 'static);
    /// The X of the home's "setup complete" card.
    fn on_ready_dismissed(&self, f: impl Fn() + 'static);
    fn on_music_previous(&self, f: impl Fn() + 'static);
    fn set_settings_open(&self, open: bool);
    fn set_notifications_hidden(&self, hidden: bool);
    /// The volume HUD: percentage and whether it is shown.
    fn set_volume(&self, percent: u8, shown: bool);
    fn get_character_cx(&self) -> f32;
    fn get_character_top(&self) -> f32;
    fn get_character_height(&self) -> f32;

    fn on_state_changed(&self, f: impl Fn(&Self, String) + 'static);
    fn on_page_changed(&self, f: impl Fn(String) + 'static);
    fn on_talk_clicked(&self, f: impl Fn() + 'static);
    fn on_character_touched(&self, f: impl Fn() + 'static);
    fn on_stop_clicked(&self, f: impl Fn() + 'static);
    fn on_false_wake_clicked(&self, f: impl Fn() + 'static);
    fn on_message_ok(&self, f: impl Fn(usize) + 'static);
    fn on_language_chosen(&self, f: impl Fn(&Self, String) + 'static);
    fn on_home_assistant_clicked(&self, f: impl Fn() + 'static);
    /// Settings → Suspend, confirmed.
    fn on_suspend_confirmed(&self, f: impl Fn() + 'static);
    fn on_screenshot_requested(&self, f: impl Fn(&Self) + 'static);
    /// A control of the screen was pressed (the tap sound).
    fn on_tap(&self, f: impl Fn() + 'static);
    /// The back of a page was pressed (before the page changes).
    fn on_back_pressed(&self, f: impl Fn() + 'static);
    /// Keys + and - of the simulator (+1, -1).
    fn on_volume_key(&self, f: impl Fn(i32) + 'static);
    /// Minutes.
    fn on_timer_start(&self, f: impl Fn(u32) + 'static);
    fn on_timer_pause(&self, f: impl Fn(String) + 'static);
    fn on_timer_cancel(&self, f: impl Fn(String) + 'static);
    fn on_undo_clicked(&self, f: impl Fn() + 'static);
    /// Hour, minute, days (bit 0 = Monday).
    fn on_alarm_save(&self, f: impl Fn(u8, u8, u8) + 'static);
    fn on_alarm_toggled(&self, f: impl Fn(u32, bool) + 'static);
    fn on_alarm_delete(&self, f: impl Fn(u32) + 'static);
    fn on_snooze(&self, f: impl Fn() + 'static);
    fn on_notice_ok(&self, f: impl Fn() + 'static);
    fn on_notice_snooze(&self, f: impl Fn() + 'static);
    fn on_chat_new(&self, f: impl Fn() + 'static);
    fn on_chat_clear(&self, f: impl Fn() + 'static);
    fn on_save_chat_toggled(&self, f: impl Fn(bool) + 'static);
    /// On, start hour, end hour.
    fn on_night_changed(&self, f: impl Fn(bool, u32, u32) + 'static);
    fn on_setup_later(&self, f: impl Fn() + 'static);
    /// The first start's welcome page: "Welcome" in each language, (code, name in itself).
    fn set_welcome(&self, title: &str, rows: &[(String, String)]);
    /// The first start's Home Assistant page: "searching", "found", "none" or "saving"; the
    /// instances (name, URL); a line of explanation ("" none).
    fn set_find(&self, state: &str, rows: &[(String, String)], line: &str);
    fn on_find_search(&self, f: impl Fn() + 'static);
    fn on_find_pick(&self, f: impl Fn(String) + 'static);
    /// The "Ricordi" page (home-companion-brain): present, the tile/row status, the page's first line,
    /// profile, notes, facts (id, text, date), reminders (id, when, text).
    fn set_memories(&self, m: &MemoriesView);
    /// The shopping list: configured, open items (uid, text), the tile's line.
    fn set_shopping(&self, on: bool, items: &[(String, String)], tile: &str);
    fn on_memory_delete(&self, f: impl Fn(i64) + 'static);
    /// "Right" on a fact to confirm.
    fn on_memory_confirm(&self, f: impl Fn(i64) + 'static);
    fn on_memories_forget(&self, f: impl Fn() + 'static);
    fn on_job_cancel(&self, f: impl Fn(i64) + 'static);
    fn on_shop_done(&self, f: impl Fn(String) + 'static);
    /// Settings → Home Assistant.
    fn set_ha_page(&self, v: &HaPageView);
    /// Linked: the Home Assistant row of Settings opens its page, else the QR code.
    fn set_ha_linked(&self, linked: bool);
    fn on_ha_check(&self, f: impl Fn() + 'static);
    fn on_ha_repair(&self, f: impl Fn(String) + 'static);
    fn on_ha_unlink(&self, f: impl Fn() + 'static);
    /// A choice of the Home Assistant page: its title, a line, (id, name, in use) of each option.
    fn set_ha_pick(&self, title: &str, hint: &str, rows: &[(String, String, bool)]);
    /// A choice opened (its kind) / one picked (its id).
    fn on_ha_pick(&self, f: impl Fn(String) + 'static);
    fn on_ha_picked(&self, f: impl Fn(String) + 'static);
    /// The OK after "All set".
    fn on_ha_ok(&self, f: impl Fn() + 'static);
    fn on_ha_dismiss(&self, f: impl Fn() + 'static);
    /// v0.2: a house command went through (the "done" card: shown, its line, with Undo).
    fn set_done(&self, shown: bool, line: &str, undo: bool);
    /// The big Undo pop-up (before the card): up, its time left (1..0).
    fn set_done_popup(&self, popup: bool, left: f32);
    /// The pop-up's two lines: the thing and its state.
    fn set_done_parts(&self, name: &str, state: &str);
    /// A house command waits for a yes: shown, the question, the line under it (the devices'
    /// names, or ""), the time left (1..0).
    fn set_ask(&self, shown: bool, title: &str, text: &str, left: f32);
    fn set_ask_left(&self, left: f32);
    /// A yes came (spoken or touched): the question's window says "Doing it…" under the
    /// devices' names (`title`), Cancel only (gone once touched: `cancelled`), until its done
    /// goes on in the same window.
    fn set_ask_working(&self, on: bool, title: &str, cancelled: bool);
    /// The done pop-up goes on from the question's window (its size and place).
    fn set_done_from_ask(&self, on: bool);
    /// Do not disturb: on, its line.
    fn set_dnd(&self, on: bool, line: &str);
    /// The Wi-Fi signal on every page header and in the control center: 0 not connected,
    /// 1..4 bars, -1 not known.
    fn set_wifi_level(&self, level: i32);
    /// The music page's spectrum: band levels and their peaks, 0..1, low bands first.
    fn set_music_spectrum(&self, bands: &[f32], peaks: &[f32]);
    /// Trying layouts of the music page (0..3).
    fn set_music_layout(&self, layout: i32);
    /// The compact home (the clock higher, four cards) or the normal one.
    fn set_home_compact(&self, on: bool);
    fn on_home_compact_changed(&self, f: impl Fn(bool) + 'static);
    /// Settings → how readily the character answers to his wake word (-1 less, 0, 1 more).
    fn set_wake_step(&self, step: i32);
    fn on_wake_step_changed(&self, f: impl Fn(i32) + 'static);
    /// The cloud services' use: the Home Assistant page's row ("" hides it) and its page.
    fn set_usage(&self, v: &UsageView);
    /// The microphone off: on, its line over the clock.
    fn set_mic_off(&self, on: bool, line: &str);
    /// The card of the microphone's switch: shown, on or off, its text.
    fn set_mic_hud(&self, shown: bool, on: bool, text: &str);
    /// "What to say": (icon, title, examples) per group.
    fn set_what(&self, rows: &[(String, String, String)]);
    fn on_done_undo(&self, f: impl Fn() + 'static);
    /// The pop-up's OK (or a tap around it).
    fn on_done_ok(&self, f: impl Fn() + 'static);
    fn on_ask_answered(&self, f: impl Fn(bool) + 'static);
    /// Cancel in the question's window (after the yes).
    fn on_ask_cancel(&self, f: impl Fn() + 'static);
}

/// A fact on the "Ricordi" page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryLine {
    pub id: i64,
    pub text: String,
    /// The day it was last written ("4 nov").
    pub date: String,
    /// Not in what the assistant reads at every conversation (over the budget).
    pub dim: bool,
}

/// What the "Ricordi" page shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoriesView {
    pub present: bool,
    pub tile: String,
    /// The "Promemoria" tile in Altro: how many are coming, or none.
    pub reminders_tile: String,
    pub status: String,
    /// The line on what reaches the assistant, and the note on the facts over the budget ("" =
    /// none).
    pub sent: String,
    pub more: String,
    /// What Alfred knows, in the order the assistant reads it.
    pub facts: Vec<MemoryLine>,
    /// What was heard once, to confirm on the screen.
    pub confirm: Vec<MemoryLine>,
    pub jobs: Vec<(i64, String, String)>,
    /// Calendar events of today and tomorrow: (when, what), after the reminders.
    pub events: Vec<(String, String)>,
}

/// Words of `Words` (companion.slint) and their keys in locales/*.toml (`[text]`).
macro_rules! words {
    ($w:ident, $f:ident, $($k:ident),* $(,)?) => {
        $w { $($k: $f(stringify!($k)).into(),)* }
    };
}

macro_rules! model {
    ($v:expr) => {
        ModelRc::new(VecModel::from($v))
    };
}

macro_rules! impl_screen {
    ($m:ident) => {
        impl Screen for $m::CompanionWindow {
            fn create() -> Result<Self, slint::PlatformError> {
                $m::CompanionWindow::new()
            }
            fn window(&self) -> &slint::Window {
                ComponentHandle::window(self)
            }
            fn run(&self) -> Result<(), slint::PlatformError> {
                ComponentHandle::run(self)
            }
            fn set_state(&self, s: &str) {
                $m::CompanionWindow::set_state(self, s.into())
            }
            fn get_state(&self) -> String {
                $m::CompanionWindow::get_state(self).into()
            }
            fn set_page(&self, s: &str) {
                $m::CompanionWindow::set_page(self, s.into())
            }
            fn get_page(&self) -> String {
                $m::CompanionWindow::get_page(self).into()
            }
            fn get_dialog(&self) -> String {
                $m::CompanionWindow::get_dialog(self).into()
            }
            fn set_dialog(&self, s: &str) {
                $m::CompanionWindow::set_dialog(self, s.into())
            }
            fn set_pose(&self, i: Image) {
                $m::CompanionWindow::set_pose(self, i)
            }
            fn set_error_pose(&self, i: Image) {
                $m::CompanionWindow::set_error_pose(self, i)
            }
            fn set_dnd_prop(&self, i: Image) {
                $m::CompanionWindow::set_dnd_prop(self, i)
            }
            fn set_announcing(&self, on: bool) {
                $m::CompanionWindow::set_announcing(self, on)
            }
            fn set_music_prop(&self, i: Image) {
                $m::CompanionWindow::set_music_prop(self, i)
            }
            fn set_frame(&self, i: Image) {
                $m::CompanionWindow::set_frame(self, i)
            }
            fn set_frame_rect(&self, x: f32, y: f32, w: f32, h: f32) {
                $m::CompanionWindow::set_frame_x(self, x);
                $m::CompanionWindow::set_frame_y(self, y);
                $m::CompanionWindow::set_frame_width(self, w);
                $m::CompanionWindow::set_frame_height(self, h);
            }
            fn set_character_name(&self, s: &str) {
                $m::CompanionWindow::set_character_name(self, s.into())
            }
            fn set_character_background(&self, c: Color) {
                $m::CompanionWindow::set_character_background(self, c)
            }
            fn set_color_listening(&self, c: Color) {
                $m::CompanionWindow::set_color_listening(self, c)
            }
            fn set_color_thinking(&self, c: Color) {
                $m::CompanionWindow::set_color_thinking(self, c)
            }
            fn set_color_speaking(&self, c: Color) {
                $m::CompanionWindow::set_color_speaking(self, c)
            }
            fn set_color_error(&self, c: Color) {
                $m::CompanionWindow::set_color_error(self, c)
            }
            fn set_phrase_listening(&self, s: &str) {
                $m::CompanionWindow::set_phrase_listening(self, s.into())
            }
            fn set_phrase_thinking(&self, s: &str) {
                $m::CompanionWindow::set_phrase_thinking(self, s.into())
            }
            fn set_phrase_error(&self, s: &str) {
                $m::CompanionWindow::set_phrase_error(self, s.into())
            }
            fn set_phrase_idle(&self, s: &str) {
                $m::CompanionWindow::set_phrase_idle(self, s.into())
            }
            fn set_phrase_starting(&self, s: &str) {
                $m::CompanionWindow::set_phrase_starting(self, s.into())
            }
            fn set_words(&self, f: &dyn Fn(&str) -> String) {
                use $m::Words;
                let w = words!(
                    Words,
                    f,
                    ha_short,
                    talk,
                    stop,
                    ok,
                    more,
                    conversations,
                    music,
                    timers_alarms,
                    settings,
                    language,
                    new_timer,
                    new_alarm,
                    start,
                    save,
                    cancel,
                    close,
                    retry,
                    later,
                    start_over,
                    clear,
                    clear_question,
                    clear_text,
                    chat_empty,
                    timers_section,
                    alarms_section,
                    timers_empty,
                    alarms_empty,
                    minutes,
                    undo,
                    plus_minute,
                    timer_finished,
                    touch_to_stop,
                    section_device,
                    brains,
                    save_conversations,
                    volume,
                    screen,
                    info,
                    suspend,
                    suspend_row,
                    suspend_hint,
                    suspend_question,
                    suspend_text,
                    night_mode,
                    night_from,
                    night_to,
                    night_hint,
                    home_compact,
                    false_wake,
                    home_compact_off,
                    home_compact_on,
                    wake_sensitivity,
                    wake_less,
                    wake_normal,
                    wake_more,
                    setup_title,
                    setup_subtitle,
                    setup_step1,
                    setup_step2,
                    setup_step3,
                    ha_unavailable,
                    ha_unavailable_text,
                    error_hint,
                    alarm,
                    memories,
                    memories_about,
                    memories_confirm,
                    memories_right,
                    memories_wrong,
                    memories_empty,
                    reminders_section,
                    reminders,
                    reminders_empty,
                    delete_question,
                    forget_all,
                    forget_question,
                    forget_text,
                    shopping,
                    shopping_empty,
                    calendar_events,
                    weather_title,
                    weather_forecast,
                    weather_no_forecast,
                    ha_lists,
                    lists_title,
                    lists_todo,
                    lists_calendars,
                    lists_empty,
                    lists_hint,
                    ha_intro,
                    ha_configure,
                    ha_check,
                    ha_repair,
                    ha_unlink,
                    ha_unlink_question,
                    ha_working_short,
                    ha_working,
                    ha_working_wait,
                    mirror,
                    mirror_back,
                    mirror_waiting,
                    mirror_stop,
                    first_welcome,
                    find_title,
                    find_searching,
                    find_found,
                    find_none,
                    find_again,
                    yes,
                    no,
                    ask_executing,
                    what_short,
                    what_title,
                    what_intro,
                    dnd,
                );
                $m::CompanionWindow::set_t(self, w)
            }
            fn set_day_letters(&self, letters: &[String]) {
                let v: Vec<slint::SharedString> = letters.iter().map(|s| s.into()).collect();
                $m::CompanionWindow::set_day_letters(self, model!(v))
            }
            fn set_languages(&self, rows: &[(String, String, bool)]) {
                let v: Vec<$m::LanguageRow> = rows
                    .iter()
                    .map(|(c, n, s)| $m::LanguageRow {
                        code: c.into(),
                        name: n.into(),
                        selected: *s,
                    })
                    .collect();
                $m::CompanionWindow::set_languages(self, model!(v))
            }
            fn set_mouth(&self, x: f32, y: f32, w: f32, h: f32) {
                $m::CompanionWindow::set_mouth_x(self, x);
                $m::CompanionWindow::set_mouth_y(self, y);
                $m::CompanionWindow::set_mouth_width(self, w);
                $m::CompanionWindow::set_mouth_height(self, h);
            }
            fn set_clock(&self, s: &str) {
                $m::CompanionWindow::set_clock(self, s.into())
            }
            fn get_clock(&self) -> String {
                $m::CompanionWindow::get_clock(self).into()
            }
            fn set_date(&self, s: &str) {
                $m::CompanionWindow::set_date(self, s.into())
            }
            fn set_weather(&self, w: Option<(&str, &str, &str)>) {
                $m::CompanionWindow::set_weather_shown(self, w.is_some());
                if let Some((kind, condition, temperature)) = w {
                    $m::CompanionWindow::set_weather_kind(self, kind.into());
                    $m::CompanionWindow::set_weather_condition(self, condition.into());
                    $m::CompanionWindow::set_temperature(self, temperature.into());
                }
            }
            fn set_weather_page(
                &self,
                icon: &str,
                high: &str,
                low: &str,
                rows: &[weather::ForecastRow],
            ) {
                $m::CompanionWindow::set_weather_icon(self, icon.into());
                $m::CompanionWindow::set_weather_high(self, high.into());
                $m::CompanionWindow::set_weather_low(self, low.into());
                let v: Vec<$m::ForecastRow> = rows
                    .iter()
                    .map(|r| $m::ForecastRow {
                        day: r.day.as_str().into(),
                        kind: r.kind.as_str().into(),
                        condition: r.condition.as_str().into(),
                        high: r.high.as_str().into(),
                        low: r.low.as_str().into(),
                        rain: r.rain.as_str().into(),
                    })
                    .collect();
                $m::CompanionWindow::set_forecast_rows(self, model!(v))
            }
            fn set_choices(
                &self,
                todo: &[(String, String, bool)],
                calendars: &[(String, String, bool)],
            ) {
                let rows = |v: &[(String, String, bool)]| -> Vec<$m::ChoiceRow> {
                    v.iter()
                        .map(|(i, n, on)| $m::ChoiceRow {
                            id: i.as_str().into(),
                            name: n.as_str().into(),
                            on: *on,
                        })
                        .collect()
                };
                $m::CompanionWindow::set_choice_todo(self, model!(rows(todo)));
                $m::CompanionWindow::set_choice_calendars(self, model!(rows(calendars)))
            }
            fn on_choice_toggled(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_choice_toggled(self, move |id| f(id.into()))
            }
            fn set_cards(&self, cards: &[CardView]) {
                let v: Vec<$m::CardRow> = cards
                    .iter()
                    .map(|c| $m::CardRow {
                        kind: c.kind.into(),
                        title: c.title.as_str().into(),
                        text: c.text.as_str().into(),
                        left: c.left.as_str().into(),
                        remaining: c.remaining,
                        paused: c.paused,
                    })
                    .collect();
                $m::CompanionWindow::set_cards(self, model!(v))
            }
            fn set_offline(&self, offline: bool) {
                $m::CompanionWindow::set_offline(self, offline)
            }
            fn set_starting(&self, starting: bool) {
                $m::CompanionWindow::set_starting(self, starting)
            }
            fn set_still(&self, still: bool) {
                $m::CompanionWindow::set_still(self, still)
            }
            fn set_night(&self, on: bool, line: &str, alarm: bool) {
                $m::CompanionWindow::set_night(self, on);
                $m::CompanionWindow::set_night_line(self, line.into());
                $m::CompanionWindow::set_night_alarm(self, alarm);
            }
            fn set_heard(&self, s: &str) {
                $m::CompanionWindow::set_heard(self, s.into())
            }
            fn set_answer(&self, s: &str) {
                $m::CompanionWindow::set_answer(self, s.into())
            }
            fn set_wave(&self, bars: &[f32]) {
                // Update the existing model row by row (only the bars redraw); replace it when
                // its length differs.
                let current = $m::CompanionWindow::get_wave(self);
                if let Some(vm) = current.as_any().downcast_ref::<VecModel<f32>>() {
                    if vm.row_count() == bars.len() {
                        for (i, v) in bars.iter().enumerate() {
                            if vm.row_data(i) != Some(*v) {
                                vm.set_row_data(i, *v);
                            }
                        }
                        return;
                    }
                }
                $m::CompanionWindow::set_wave(self, model!(bars.to_vec()))
            }
            fn set_notice(&self, shown: bool, title: &str, text: &str, when: &str, later: &str) {
                $m::CompanionWindow::set_notice_title(self, title.into());
                $m::CompanionWindow::set_notice_text(self, text.into());
                $m::CompanionWindow::set_notice_when(self, when.into());
                $m::CompanionWindow::set_notice_later(self, later.into());
                $m::CompanionWindow::set_notice_shown(self, shown);
            }
            fn set_ringing(&self, name: &str, since: &str, alarm: bool) {
                $m::CompanionWindow::set_ringing_name(self, name.into());
                $m::CompanionWindow::set_ringing_since(self, since.into());
                $m::CompanionWindow::set_ringing_alarm(self, alarm);
            }
            fn set_chat(&self, rows: &[(String, String)]) {
                let v: Vec<$m::ChatRow> = rows
                    .iter()
                    .map(|(k, t)| $m::ChatRow {
                        kind: k.into(),
                        text: t.into(),
                    })
                    .collect();
                $m::CompanionWindow::set_chat(self, model!(v))
            }
            fn set_timer_rows(&self, rows: &[TimerLineView]) {
                let v: Vec<$m::TimerRow> = rows
                    .iter()
                    .map(|r| $m::TimerRow {
                        id: r.id.as_str().into(),
                        name: r.name.as_str().into(),
                        detail: r.detail.as_str().into(),
                        left: r.left.as_str().into(),
                        remaining: r.remaining,
                        paused: r.paused,
                    })
                    .collect();
                $m::CompanionWindow::set_timer_rows(self, model!(v))
            }
            fn set_alarm_rows(&self, rows: &[AlarmLineView]) {
                let v: Vec<$m::AlarmRow> = rows
                    .iter()
                    .map(|r| $m::AlarmRow {
                        id: r.id as i32,
                        time: r.time.as_str().into(),
                        days: r.days.as_str().into(),
                        on: r.on,
                    })
                    .collect();
                $m::CompanionWindow::set_alarm_rows(self, model!(v))
            }
            fn set_undo(&self, text: &str) {
                $m::CompanionWindow::set_undo_text(self, text.into())
            }
            fn set_tiles(&self, chat: &str, timers: &str, settings: &str) {
                $m::CompanionWindow::set_tile_chat(self, chat.into());
                $m::CompanionWindow::set_tile_timers(self, timers.into());
                $m::CompanionWindow::set_tile_settings(self, settings.into());
            }
            fn set_settings_values(&self, v: &SettingsView) {
                $m::CompanionWindow::set_language_name(self, v.language_name.as_str().into());
                $m::CompanionWindow::set_brains(self, v.brains.as_str().into());
                $m::CompanionWindow::set_ha_status(self, v.ha_status.as_str().into());
                $m::CompanionWindow::set_version(self, v.version.as_str().into());
                $m::CompanionWindow::set_screen_status(self, v.screen_status.as_str().into());
                $m::CompanionWindow::set_save_chat(self, v.save_chat);
                $m::CompanionWindow::set_night_on(self, v.night_on);
                $m::CompanionWindow::set_night_start(self, v.night_start as i32);
                $m::CompanionWindow::set_night_end(self, v.night_end as i32);
            }
            fn set_setup(&self, wifi: &str, ha: &str) {
                $m::CompanionWindow::set_setup_wifi(self, wifi.into());
                $m::CompanionWindow::set_setup_ha(self, ha.into());
            }
            fn set_setup_link(&self, qr: Option<Image>, status: &str, linked: bool) {
                if let Some(q) = qr {
                    $m::CompanionWindow::set_setup_qr(self, q);
                }
                $m::CompanionWindow::set_setup_status(self, status.into());
                $m::CompanionWindow::set_setup_linked(self, linked);
            }
            fn set_music(&self, m: Option<&MusicView>) {
                $m::CompanionWindow::set_music_playing(self, m.is_some());
                if let Some(m) = m {
                    $m::CompanionWindow::set_music_title(self, m.title.as_str().into());
                    $m::CompanionWindow::set_music_artist(self, m.artist.as_str().into());
                    $m::CompanionWindow::set_music_album(self, m.album.as_str().into());
                    $m::CompanionWindow::set_music_source(self, m.source.as_str().into());
                }
            }
            fn set_settings_open(&self, open: bool) {
                $m::CompanionWindow::set_settings_open(self, open);
                if !open {
                    $m::CompanionWindow::set_dialog(self, "".into());
                }
            }
            fn set_notifications_hidden(&self, hidden: bool) {
                $m::CompanionWindow::set_notifications_hidden(self, hidden)
            }
            fn set_control(
                &self,
                open: bool,
                tiles: &[control::ControlTileView],
                brightness: u8,
                auto: bool,
                auto_label: &str,
            ) {
                use $m::ControlRow;
                let rows: Vec<ControlRow> = tiles
                    .iter()
                    .map(|t| ControlRow {
                        key: t.key.into(),
                        label: t.label.as_str().into(),
                        status: t.status.as_str().into(),
                        on: t.on,
                        enabled: t.enabled,
                        tappable: t.tappable,
                    })
                    .collect();
                $m::CompanionWindow::set_control_tiles(self, model!(rows));
                $m::CompanionWindow::set_brightness(self, brightness as i32);
                $m::CompanionWindow::set_brightness_auto(self, auto);
                $m::CompanionWindow::set_brightness_label(self, auto_label.into());
                $m::CompanionWindow::set_control_open(self, open);
            }
            fn set_control_open(&self, open: bool) {
                $m::CompanionWindow::set_control_open(self, open)
            }
            fn get_control_open(&self) -> bool {
                $m::CompanionWindow::get_control_open(self)
            }
            fn set_music_cover(&self, cover: Option<Image>, paused: bool) {
                $m::CompanionWindow::set_music_cover(self, cover.unwrap_or_default());
                $m::CompanionWindow::set_music_paused(self, paused);
            }
            fn on_control_tile(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_control_tile(self, move |k| f(k.into()))
            }
            fn on_control_opened(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_control_opened(self, f)
            }
            fn on_control_volume(&self, f: impl Fn(i32) + 'static) {
                $m::CompanionWindow::on_control_volume(self, f)
            }
            fn on_control_brightness(&self, f: impl Fn(i32) + 'static) {
                $m::CompanionWindow::on_control_brightness(self, f)
            }
            fn on_control_brightness_auto(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_control_brightness_auto(self, f)
            }
            fn set_mirror(&self, active: bool, source: &str, width: u32, height: u32) {
                $m::CompanionWindow::set_mirror_active(self, active);
                $m::CompanionWindow::set_mirror_source(self, source.into());
                if width > 0 && height > 0 {
                    $m::CompanionWindow::set_mirror_frame_width(self, width as f32);
                    $m::CompanionWindow::set_mirror_frame_height(self, height as f32);
                }
            }
            fn set_mirror_frame(&self, frame: Image) {
                $m::CompanionWindow::set_mirror_frame(self, frame)
            }
            fn set_mirror_freeze(&self, t: f32) {
                $m::CompanionWindow::set_mirror_freeze(self, t)
            }
            fn set_mirror_chrome(&self, shown: bool) {
                $m::CompanionWindow::set_mirror_chrome(self, shown)
            }
            fn on_mirror_poked(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_mirror_poked(self, f)
            }
            fn set_face(&self, face: Image) {
                $m::CompanionWindow::set_face(self, face)
            }
            fn on_music_toggle(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_music_toggle(self, f)
            }
            fn on_music_next(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_music_next(self, f)
            }
            fn on_ready_dismissed(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_ready_dismissed(self, f)
            }
            fn on_mirror_stop(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_mirror_stop(self, f)
            }
            fn on_music_previous(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_music_previous(self, f)
            }
            fn set_volume(&self, percent: u8, shown: bool) {
                $m::CompanionWindow::set_volume(self, percent as i32);
                $m::CompanionWindow::set_volume_shown(self, shown);
            }
            fn get_character_cx(&self) -> f32 {
                $m::CompanionWindow::get_character_cx(self)
            }
            fn get_character_top(&self) -> f32 {
                $m::CompanionWindow::get_character_top(self)
            }
            fn get_character_height(&self) -> f32 {
                $m::CompanionWindow::get_character_height(self)
            }
            fn on_state_changed(&self, f: impl Fn(&Self, String) + 'static) {
                let weak = self.as_weak();
                $m::CompanionWindow::on_state_changed(self, move |s| {
                    if let Some(app) = weak.upgrade() {
                        f(&app, s.into());
                    }
                })
            }
            fn on_page_changed(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_page_changed(self, move |s| f(s.into()))
            }
            fn on_talk_clicked(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_talk_clicked(self, f)
            }
            fn on_character_touched(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_character_touched(self, f)
            }
            fn on_stop_clicked(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_stop_clicked(self, f)
            }
            fn on_false_wake_clicked(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_false_wake_clicked(self, f)
            }
            fn on_message_ok(&self, f: impl Fn(usize) + 'static) {
                $m::CompanionWindow::on_message_ok(self, move |i| f(i.max(0) as usize))
            }
            fn on_language_chosen(&self, f: impl Fn(&Self, String) + 'static) {
                let weak = self.as_weak();
                $m::CompanionWindow::on_language_chosen(self, move |s| {
                    if let Some(app) = weak.upgrade() {
                        f(&app, s.into());
                    }
                })
            }
            fn on_home_assistant_clicked(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_home_assistant_clicked(self, f)
            }
            fn on_suspend_confirmed(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_suspend_confirmed(self, f)
            }
            fn on_volume_key(&self, f: impl Fn(i32) + 'static) {
                $m::CompanionWindow::on_volume_key(self, f)
            }
            fn on_tap(&self, f: impl Fn() + 'static) {
                self.global::<$m::Feedback>().on_tap(f)
            }
            fn on_back_pressed(&self, f: impl Fn() + 'static) {
                self.global::<$m::Feedback>().on_back(f)
            }
            fn on_screenshot_requested(&self, f: impl Fn(&Self) + 'static) {
                let weak = self.as_weak();
                $m::CompanionWindow::on_screenshot_requested(self, move || {
                    if let Some(app) = weak.upgrade() {
                        f(&app);
                    }
                })
            }
            fn on_timer_start(&self, f: impl Fn(u32) + 'static) {
                $m::CompanionWindow::on_timer_start(self, move |m| f(m.max(1) as u32))
            }
            fn on_timer_pause(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_timer_pause(self, move |id| f(id.into()))
            }
            fn on_timer_cancel(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_timer_cancel(self, move |id| f(id.into()))
            }
            fn on_undo_clicked(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_undo_clicked(self, f)
            }
            fn on_alarm_save(&self, f: impl Fn(u8, u8, u8) + 'static) {
                $m::CompanionWindow::on_alarm_save(self, move |h, m, d| {
                    f(
                        h.clamp(0, 23) as u8,
                        m.clamp(0, 59) as u8,
                        d.clamp(0, 127) as u8,
                    )
                })
            }
            fn on_alarm_toggled(&self, f: impl Fn(u32, bool) + 'static) {
                $m::CompanionWindow::on_alarm_toggled(self, move |id, on| f(id.max(0) as u32, on))
            }
            fn on_alarm_delete(&self, f: impl Fn(u32) + 'static) {
                $m::CompanionWindow::on_alarm_delete(self, move |id| f(id.max(0) as u32))
            }
            fn on_snooze(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_snooze(self, f)
            }
            fn on_notice_ok(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_notice_ok(self, f)
            }
            fn on_notice_snooze(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_notice_snooze(self, f)
            }
            fn on_chat_new(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_chat_new(self, f)
            }
            fn on_chat_clear(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_chat_clear(self, f)
            }
            fn on_save_chat_toggled(&self, f: impl Fn(bool) + 'static) {
                $m::CompanionWindow::on_save_chat_toggled(self, f)
            }
            fn on_night_changed(&self, f: impl Fn(bool, u32, u32) + 'static) {
                $m::CompanionWindow::on_night_changed(self, move |on, a, b| {
                    f(on, a.rem_euclid(24) as u32, b.rem_euclid(24) as u32)
                })
            }
            fn set_memories(&self, m: &MemoriesView) {
                $m::CompanionWindow::set_brain_present(self, m.present);
                $m::CompanionWindow::set_tile_memories(self, m.tile.as_str().into());
                $m::CompanionWindow::set_tile_reminders(self, m.reminders_tile.as_str().into());
                $m::CompanionWindow::set_memories_status(self, m.status.as_str().into());
                $m::CompanionWindow::set_memories_sent(self, m.sent.as_str().into());
                $m::CompanionWindow::set_memories_more(self, m.more.as_str().into());
                let rows = |v: &[MemoryLine]| -> Vec<$m::MemoryRow> {
                    v.iter()
                        .map(|f| $m::MemoryRow {
                            id: f.id as i32,
                            text: f.text.as_str().into(),
                            date: f.date.as_str().into(),
                            dim: f.dim,
                        })
                        .collect()
                };
                $m::CompanionWindow::set_memory_rows(self, model!(rows(&m.facts)));
                $m::CompanionWindow::set_confirm_rows(self, model!(rows(&m.confirm)));
                let j: Vec<$m::JobRow> = m
                    .jobs
                    .iter()
                    .map(|(id, w, t)| $m::JobRow {
                        id: *id as i32,
                        when: w.into(),
                        text: t.into(),
                    })
                    .collect();
                $m::CompanionWindow::set_job_rows(self, model!(j));
                let e: Vec<$m::JobRow> = m
                    .events
                    .iter()
                    .enumerate()
                    .map(|(i, (w, t))| $m::JobRow {
                        id: -(i as i32) - 1,
                        when: w.into(),
                        text: t.into(),
                    })
                    .collect();
                $m::CompanionWindow::set_event_rows(self, model!(e));
            }
            fn set_shopping(&self, on: bool, items: &[(String, String)], tile: &str) {
                $m::CompanionWindow::set_shopping_on(self, on);
                $m::CompanionWindow::set_tile_shopping(self, tile.into());
                let v: Vec<$m::ShopRow> = items
                    .iter()
                    .map(|(u, t)| $m::ShopRow {
                        uid: u.into(),
                        text: t.into(),
                    })
                    .collect();
                $m::CompanionWindow::set_shop_rows(self, model!(v));
            }
            fn on_memory_delete(&self, f: impl Fn(i64) + 'static) {
                $m::CompanionWindow::on_memory_delete(self, move |id| f(id as i64))
            }
            fn on_memory_confirm(&self, f: impl Fn(i64) + 'static) {
                $m::CompanionWindow::on_memory_confirm(self, move |id| f(id as i64))
            }
            fn on_memories_forget(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_memories_forget(self, f)
            }
            fn on_job_cancel(&self, f: impl Fn(i64) + 'static) {
                $m::CompanionWindow::on_job_cancel(self, move |id| f(id as i64))
            }
            fn on_shop_done(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_shop_done(self, move |u| f(u.into()))
            }
            fn on_setup_later(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_setup_later(self, f)
            }
            fn set_welcome(&self, title: &str, rows: &[(String, String)]) {
                let v: Vec<$m::WelcomeRow> = rows
                    .iter()
                    .map(|(c, n)| $m::WelcomeRow {
                        code: c.into(),
                        name: n.into(),
                    })
                    .collect();
                $m::CompanionWindow::set_welcome_title(self, title.into());
                $m::CompanionWindow::set_welcome_rows(self, model!(v));
            }
            fn set_find(&self, state: &str, rows: &[(String, String)], line: &str) {
                let v: Vec<$m::FindRow> = rows
                    .iter()
                    .map(|(n, u)| $m::FindRow {
                        name: n.into(),
                        url: u.into(),
                    })
                    .collect();
                $m::CompanionWindow::set_find_state(self, state.into());
                $m::CompanionWindow::set_find_rows(self, model!(v));
                $m::CompanionWindow::set_find_line(self, line.into());
            }
            fn on_find_search(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_find_search(self, f)
            }
            fn on_find_pick(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_find_pick(self, move |u| f(u.into()))
            }
            fn set_ha_page(&self, v: &HaPageView) {
                let rows: Vec<$m::HaRow> = v
                    .rows
                    .iter()
                    .map(|r| $m::HaRow {
                        id: r.id.as_str().into(),
                        label: r.label.as_str().into(),
                        detail: r.detail.as_str().into(),
                        level: r.level,
                        repair: r.repair,
                    })
                    .collect();
                $m::CompanionWindow::set_ha_rows(self, model!(rows));
                $m::CompanionWindow::set_ha_line(self, v.status.as_str().into());
                $m::CompanionWindow::set_ha_busy(self, v.busy);
                $m::CompanionWindow::set_ha_step(self, v.step.as_str().into());
                $m::CompanionWindow::set_ha_popup(self, v.popup);
                $m::CompanionWindow::set_ha_configured(self, v.configured);
                $m::CompanionWindow::set_ha_done(self, v.done);
                let picks: Vec<$m::PickRow> = v
                    .picks
                    .iter()
                    .map(|(id, label, value)| $m::PickRow {
                        id: id.as_str().into(),
                        label: label.as_str().into(),
                        value: value.as_str().into(),
                    })
                    .collect();
                $m::CompanionWindow::set_ha_picks(self, model!(picks));
            }
            fn set_ha_pick(&self, title: &str, hint: &str, rows: &[(String, String, bool)]) {
                let v: Vec<$m::ChoiceRow> = rows
                    .iter()
                    .map(|(i, n, on)| $m::ChoiceRow {
                        id: i.as_str().into(),
                        name: n.as_str().into(),
                        on: *on,
                    })
                    .collect();
                $m::CompanionWindow::set_ha_pick_title(self, title.into());
                $m::CompanionWindow::set_ha_pick_hint(self, hint.into());
                $m::CompanionWindow::set_ha_pick_rows(self, model!(v));
            }
            fn on_ha_pick(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_ha_pick(self, move |id| f(id.into()))
            }
            fn on_ha_picked(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_ha_picked(self, move |id| f(id.into()))
            }
            fn set_ha_linked(&self, linked: bool) {
                $m::CompanionWindow::set_ha_linked(self, linked)
            }
            fn on_ha_check(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_ha_check(self, f)
            }
            fn on_ha_repair(&self, f: impl Fn(String) + 'static) {
                $m::CompanionWindow::on_ha_repair(self, move |id| f(id.into()))
            }
            fn on_ha_unlink(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_ha_unlink(self, f)
            }
            fn set_done(&self, shown: bool, line: &str, undo: bool) {
                $m::CompanionWindow::set_done_line(self, line.into());
                $m::CompanionWindow::set_done_undo(self, undo);
                $m::CompanionWindow::set_done_shown(self, shown);
            }
            fn set_done_popup(&self, popup: bool, left: f32) {
                $m::CompanionWindow::set_done_left(self, left);
                $m::CompanionWindow::set_done_popup(self, popup);
            }
            fn set_done_parts(&self, name: &str, state: &str) {
                $m::CompanionWindow::set_done_name(self, name.into());
                $m::CompanionWindow::set_done_state(self, state.into());
            }
            fn set_ask(&self, shown: bool, title: &str, text: &str, left: f32) {
                $m::CompanionWindow::set_ask_text(self, title.into());
                $m::CompanionWindow::set_ask_names(self, text.into());
                $m::CompanionWindow::set_ask_left(self, left);
                $m::CompanionWindow::set_ask_shown(self, shown);
            }
            fn set_ask_left(&self, left: f32) {
                $m::CompanionWindow::set_ask_left(self, left)
            }
            fn set_ask_working(&self, on: bool, title: &str, cancelled: bool) {
                $m::CompanionWindow::set_ask_working_title(self, title.into());
                $m::CompanionWindow::set_ask_cancelled(self, cancelled);
                $m::CompanionWindow::set_ask_working(self, on)
            }
            fn set_done_from_ask(&self, on: bool) {
                $m::CompanionWindow::set_done_from_ask(self, on)
            }
            fn set_dnd(&self, on: bool, line: &str) {
                $m::CompanionWindow::set_dnd_line(self, line.into());
                $m::CompanionWindow::set_dnd(self, on);
            }
            fn set_wifi_level(&self, level: i32) {
                self.global::<$m::Status>().set_wifi_level(level)
            }
            fn set_music_spectrum(&self, bands: &[f32], peaks: &[f32]) {
                $m::CompanionWindow::set_music_bands(self, model!(bands.to_vec()));
                $m::CompanionWindow::set_music_peaks(self, model!(peaks.to_vec()));
            }
            fn set_music_layout(&self, layout: i32) {
                $m::CompanionWindow::set_music_layout(self, layout);
            }
            fn set_home_compact(&self, on: bool) {
                self.global::<$m::Grid>().set_compact(on);
                $m::CompanionWindow::set_home_compact(self, on);
            }
            fn on_home_compact_changed(&self, f: impl Fn(bool) + 'static) {
                $m::CompanionWindow::on_home_compact_changed(self, f)
            }
            fn set_wake_step(&self, step: i32) {
                $m::CompanionWindow::set_wake_step(self, step);
            }
            fn on_wake_step_changed(&self, f: impl Fn(i32) + 'static) {
                $m::CompanionWindow::on_wake_step_changed(self, f)
            }
            fn set_usage(&self, v: &UsageView) {
                $m::CompanionWindow::set_usage_label(self, v.label.as_str().into());
                $m::CompanionWindow::set_usage_line(self, v.line.as_str().into());
                $m::CompanionWindow::set_usage_today_label(self, v.today_label.as_str().into());
                $m::CompanionWindow::set_usage_today(self, v.today.as_str().into());
                $m::CompanionWindow::set_usage_month(self, v.month.as_str().into());
                $m::CompanionWindow::set_usage_detail(self, v.detail.as_str().into());
                $m::CompanionWindow::set_usage_chart_label(self, v.chart_label.as_str().into());
                $m::CompanionWindow::set_usage_max(self, v.max.as_str().into());
                $m::CompanionWindow::set_usage_note(self, v.note.as_str().into());
                $m::CompanionWindow::set_usage_precise(self, v.precise);
                let bars: Vec<$m::UsageBar> = v
                    .bars
                    .iter()
                    .map(|(label, h, cost)| $m::UsageBar {
                        label: label.as_str().into(),
                        height: *h,
                        cost: cost.as_str().into(),
                    })
                    .collect();
                $m::CompanionWindow::set_usage_bars(self, model!(bars));
            }
            fn set_mic_off(&self, on: bool, line: &str) {
                $m::CompanionWindow::set_mic_off_line(self, line.into());
                $m::CompanionWindow::set_mic_off(self, on);
            }
            fn set_mic_hud(&self, shown: bool, on: bool, text: &str) {
                $m::CompanionWindow::set_mic_hud_text(self, text.into());
                $m::CompanionWindow::set_mic_hud_on(self, on);
                $m::CompanionWindow::set_mic_hud_shown(self, shown);
            }
            fn set_what(&self, rows: &[(String, String, String)]) {
                let v: Vec<$m::WhatRow> = rows
                    .iter()
                    .map(|(i, t, e)| $m::WhatRow {
                        icon: i.into(),
                        title: t.into(),
                        examples: e.into(),
                    })
                    .collect();
                $m::CompanionWindow::set_what_rows(self, model!(v))
            }
            fn on_done_undo(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_done_undo_clicked(self, f)
            }
            fn on_done_ok(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_done_ok_clicked(self, f)
            }
            fn on_ask_answered(&self, f: impl Fn(bool) + 'static) {
                $m::CompanionWindow::on_ask_answered(self, f)
            }
            fn on_ask_cancel(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_ask_cancel(self, f)
            }
            fn on_ha_dismiss(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_ha_dismiss(self, f)
            }
            fn on_ha_ok(&self, f: impl Fn() + 'static) {
                $m::CompanionWindow::on_ha_ok(self, f)
            }
        }
    };
}

impl_screen!(sw);
