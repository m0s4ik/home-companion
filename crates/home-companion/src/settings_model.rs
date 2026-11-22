// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The user's settings as one table: each one's key, kind, limits, words and group, how it is
//! read and how it is set. Setting one checks the value, writes `settings.toml` and has it
//! applied at once ([`Host::apply`]); the screen's own switches and steppers go through
//! [`set`], and so does the browser panel (crate `panel`, via the body's socket): there is
//! one logic, not two.
//!
//! Each item says whether the browser panel may show it (`panel`). The flag is off unless an
//! item says otherwise, so a setting added later never ends up on the web by forgetting: only
//! what is in [`PANEL_KEYS`] passes, and a test fails when the two lists part.

use serde_json::{Value, json};

use crate::settings::{Settings, WakeSensitivity};
use crate::ui::widgets::{EVERY_CHOICES, Page as WidgetPage};

/// What kind of value an item holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    /// A whole number in `min..=max`, in steps of `step` from `min`.
    Int {
        min: i64,
        max: i64,
        step: i64,
    },
    /// One of the item's options (a string).
    Choice,
    /// Some of the item's options (strings, in the options' order); for the lists and the
    /// calendars an empty choice means all of them.
    Multi,
}

/// One setting.
#[derive(Debug, Clone, Copy)]
pub struct Item {
    /// Its key, as the panel's API names it.
    pub key: &'static str,
    /// Its label: a key of `locales/*.toml` `[text]`.
    pub label: &'static str,
    /// The group it is shown in: `panel_group_<group>` is the group's title.
    pub group: &'static str,
    pub kind: Kind,
    /// The browser panel may read and set it.
    pub panel: bool,
}

const fn item(key: &'static str, label: &'static str, group: &'static str, kind: Kind) -> Item {
    Item {
        key,
        label,
        group,
        kind,
        panel: false,
    }
}

const fn on_panel(mut i: Item) -> Item {
    i.panel = true;
    i
}

/// Every setting, in the order the panel shows them.
pub const ITEMS: &[Item] = &[
    on_panel(item("widgets", "widgets_pages", "widgets", Kind::Multi)),
    on_panel(item(
        "widgets_every",
        "widgets_every",
        "widgets",
        Kind::Choice,
    )),
    on_panel(item(
        "widgets_currency",
        "widgets_currency",
        "widgets",
        Kind::Choice,
    )),
    on_panel(item(
        "check_openings",
        "check_openings",
        "widgets",
        Kind::Bool,
    )),
    on_panel(item(
        "check_batteries",
        "check_batteries",
        "widgets",
        Kind::Bool,
    )),
    on_panel(item("night_on", "night_mode", "screen", Kind::Bool)),
    on_panel(item(
        "night_start",
        "night_from",
        "screen",
        Kind::Int {
            min: 0,
            max: 23,
            step: 1,
        },
    )),
    on_panel(item(
        "night_end",
        "night_to",
        "screen",
        Kind::Int {
            min: 0,
            max: 23,
            step: 1,
        },
    )),
    on_panel(item("home_compact", "home_compact", "screen", Kind::Bool)),
    on_panel(item("language", "language", "voice", Kind::Choice)),
    on_panel(item(
        "volume",
        "volume",
        "voice",
        Kind::Int {
            min: 0,
            max: 100,
            step: 5,
        },
    )),
    on_panel(item(
        "wake_sensitivity",
        "wake_sensitivity",
        "voice",
        Kind::Choice,
    )),
    on_panel(item(
        "keep_conversations",
        "save_conversations",
        "voice",
        Kind::Bool,
    )),
    // Like the wake word's sensitivity: how readily the microphone opens, also from the panel.
    on_panel(item(
        "conversation_mode",
        "conversation_mode",
        "voice",
        Kind::Bool,
    )),
    on_panel(item(
        "conversation_max",
        "conversation_max",
        "voice",
        Kind::Int {
            min: crate::settings::CONVERSATION_MAX_MIN as i64,
            max: crate::settings::CONVERSATION_MAX_MAX as i64,
            step: 1,
        },
    )),
    on_panel(item("lists", "lists_todo_label", "lists", Kind::Multi)),
    on_panel(item(
        "calendars",
        "lists_calendars_label",
        "lists",
        Kind::Multi,
    )),
    // Updates stay on the screen (what runs on the device is chosen in front of it).
    item(
        "update_prerelease",
        "update_prerelease",
        "updates",
        Kind::Bool,
    ),
    // The panel itself is switched on and off on the screen only.
    item("panel", "panel_access", "access", Kind::Bool),
    // So is the remote screen (crate::panel::remote): never from the panel itself.
    item("panel_screen", "panel_screen", "access", Kind::Bool),
];

/// The keys the browser panel may read and set: the list the security review looked at. A test
/// compares it with the `panel` flags of [`ITEMS`].
pub const PANEL_KEYS: &[&str] = &[
    "widgets",
    "widgets_every",
    "widgets_currency",
    "check_openings",
    "check_batteries",
    "night_on",
    "night_start",
    "night_end",
    "home_compact",
    "language",
    "volume",
    "wake_sensitivity",
    "keep_conversations",
    "conversation_mode",
    "conversation_max",
    "lists",
    "calendars",
];

/// What is to be done once a value is written.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Widgets(Vec<WidgetPage>, u32),
    /// Every page of the widget in the turn's order (the browser panel's arrows).
    WidgetOrder(Vec<WidgetPage>),
    /// Another page of a kind that repeats (settings::Settings::add_widget_page): at that
    /// place of the whole order (None: after the last page on), on or not.
    WidgetAdded(WidgetPage, Option<usize>, bool),
    /// A second (third...) page gone, with its list and its name.
    WidgetRemoved(WidgetPage),
    /// A page's name ("" none).
    WidgetName(WidgetPage, String),
    /// The coins' currency.
    WidgetCurrency(String),
    /// The markets, the other cities and the sensors of a page (its number), chosen from the
    /// browser panel.
    Quotes(u8, Vec<crate::feeds::QuoteChoice>),
    Cities(u8, Vec<crate::feeds::CityChoice>),
    Sensors(u8, Vec<String>),
    /// Home Assistant's entities for the widget (the player, the people) and what to check.
    WidgetHa(crate::ha::widgets::Want),
    Night(bool, u32, u32),
    HomeCompact(bool),
    Language(String),
    Volume(u8),
    WakeSensitivity(WakeSensitivity),
    KeepConversations(bool),
    ConversationMode(bool),
    ConversationMax(u32),
    Lists(Vec<String>),
    Calendars(Vec<String>),
    UpdatePrerelease(bool),
    Panel(bool),
    PanelScreen(bool),
}

impl Change {
    /// A later change replaces this one before it is written (the latest of each kind, of each
    /// page for a page's list or name); a page added or removed is never replaced.
    pub fn replaced_by(&self, later: &Change) -> bool {
        use Change::*;
        match (self, later) {
            (WidgetAdded(..), _) | (WidgetRemoved(_), _) => false,
            (Quotes(a, _), Quotes(b, _))
            | (Cities(a, _), Cities(b, _))
            | (Sensors(a, _), Sensors(b, _)) => a == b,
            (WidgetName(a, _), WidgetName(b, _)) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(later),
        }
    }
}

/// Why a value was not set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    /// No such key (or not on the panel, for the panel).
    Unknown,
    /// Not of the item's kind, out of its limits, not one of its options.
    Invalid,
}

/// Where the settings live and what applies them: the screen (crate::ui::screen::Ui), or a fake
/// in the tests.
pub trait Host {
    fn settings(&self) -> Option<&Settings>;
    /// The value as it is now, where the file is not the whole truth: `language` (the
    /// configuration's when none was chosen) and `volume` (the core's, saved a second later).
    fn live(&self, key: &str) -> Option<Value>;
    /// The options of a choice that depend on the device: `language` (the locales), `lists` and
    /// `calendars` (Home Assistant's): (value, label).
    fn dynamic_options(&self, key: &str) -> Vec<(String, String)>;
    /// Applies a value just written.
    fn apply(&self, change: Change);
    /// Writes a change to `settings.toml`: at once by default; the screen gathers them and
    /// writes once a second at most (a flood of sets from the panel is not a flood of writes
    /// on the device's flash), answering [`Host::live`] from what it holds meanwhile.
    fn save(&self, change: &Change) {
        if let Some(s) = self.settings() {
            save(s, change);
        }
    }
}

pub fn find(key: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|i| i.key == key)
}

/// An item's options: (value, label), labels in the words of `word`.
pub fn options(
    host: &dyn Host,
    item: &Item,
    word: &dyn Fn(&str) -> String,
) -> Vec<(String, String)> {
    match item.key {
        "widgets" => {
            let names = host
                .settings()
                .map(|s| s.widget_names())
                .unwrap_or_default();
            widget_order(host)
                .iter()
                .map(|p| (p.key(), page_label(*p, &names, word)))
                .collect()
        }
        "widgets_every" => EVERY_CHOICES
            .iter()
            .map(|e| {
                let label = match e {
                    0 => word("widgets_by_hand"),
                    s if s % 60 == 0 => format!("{} min", s / 60),
                    s => format!("{s} s"),
                };
                (e.to_string(), label)
            })
            .collect(),
        "widgets_currency" => crate::feeds::coingecko::CURRENCIES
            .iter()
            .map(|c| (c.to_string(), c.to_ascii_uppercase()))
            .collect(),
        "wake_sensitivity" => [
            WakeSensitivity::Less,
            WakeSensitivity::Normal,
            WakeSensitivity::More,
        ]
        .iter()
        .map(|s| (s.key().to_string(), word(&format!("wake_{}", s.key()))))
        .collect(),
        k => host.dynamic_options(k),
    }
}

/// A page's label in the words of `word`: its name, else the kind's word with its number from
/// the second on ("Markets 2").
pub fn page_label(
    p: WidgetPage,
    names: &[(WidgetPage, String)],
    word: &dyn Fn(&str) -> String,
) -> String {
    let name = names
        .iter()
        .find(|(x, _)| *x == p)
        .map_or("", |(_, n)| n.as_str());
    p.label(&word(&format!("widget_{}", p.kind.key())), name)
}

/// The order of the widget's pages now: what the host holds (`widget_order`), else the file,
/// else the default.
pub fn widget_order(host: &dyn Host) -> Vec<WidgetPage> {
    if let Some(v) = host.live("widget_order") {
        let keys = strings(&v);
        let mut out: Vec<WidgetPage> = Vec::new();
        for p in keys.iter().filter_map(|k| WidgetPage::from_key(k)) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
        for p in WidgetPage::all() {
            if !out.contains(&p) {
                out.push(p);
            }
        }
        return out;
    }
    host.settings()
        .map(|s| s.widget_order())
        .unwrap_or_else(WidgetPage::all)
}

/// What the widget asks of Home Assistant now: the file's, with the checks as the host holds
/// them.
pub fn widget_ha(host: &dyn Host) -> crate::ha::widgets::Want {
    let mut w = host.settings().map(|s| s.widget_ha()).unwrap_or_default();
    for (k, f) in [
        ("check_openings", &mut w.openings),
        ("check_batteries", &mut w.batteries),
    ] {
        if let Some(v) = host.live(k).and_then(|v| v.as_bool()) {
            *f = v;
        }
    }
    w
}

fn int(v: i64) -> Value {
    json!(v)
}

/// An item's value now.
pub fn get(host: &dyn Host, key: &str) -> Option<Value> {
    if let Some(v) = host.live(key) {
        return Some(v);
    }
    let s = host.settings()?;
    Some(match key {
        "widgets" => json!(s.widgets().0.iter().map(|k| k.key()).collect::<Vec<_>>()),
        "widgets_every" => json!(s.widgets().1.to_string()),
        "widgets_currency" => json!(s.widget_currency()),
        "check_openings" => json!(s.widget_ha().openings),
        "check_batteries" => json!(s.widget_ha().batteries),
        "night_on" => json!(s.night().0),
        "night_start" => int(s.night().1 as i64),
        "night_end" => int(s.night().2 as i64),
        "home_compact" => json!(s.home_compact()),
        "language" => json!(s.language()?),
        "volume" => int(s.volume()? as i64),
        "wake_sensitivity" => json!(s.wake_sensitivity().key()),
        "keep_conversations" => json!(s.keep_conversations()),
        "conversation_mode" => json!(s.conversation_mode()),
        "conversation_max" => int(s.conversation_max() as i64),
        "lists" => json!(s.lists()),
        "calendars" => json!(s.calendars()),
        "update_prerelease" => json!(s.update_prerelease().unwrap_or(false)),
        "panel" => json!(s.panel_on()),
        "panel_screen" => json!(s.panel_screen()),
        _ => return None,
    })
}

/// Checks `value` against the item: Some(the value, normalised) when it fits.
fn check(host: &dyn Host, item: &Item, value: &Value) -> Option<Value> {
    let word = |k: &str| k.to_string();
    match item.kind {
        Kind::Bool => value.as_bool().map(Value::Bool),
        Kind::Int { min, max, step } => {
            let v = value.as_i64()?;
            ((min..=max).contains(&v) && (v - min) % step == 0).then(|| int(v))
        }
        Kind::Choice => {
            let v = value.as_str()?;
            options(host, item, &word)
                .iter()
                .any(|(o, _)| o == v)
                .then(|| json!(v))
        }
        Kind::Multi => {
            let list = value.as_array()?;
            if list.len() > 64 {
                return None;
            }
            let opts = options(host, item, &word);
            let mut out: Vec<String> = Vec::new();
            for v in list {
                let v = v.as_str()?;
                if !opts.iter().any(|(o, _)| o == v) || out.iter().any(|o| o == v) {
                    return None;
                }
                out.push(v.to_string());
            }
            // In the options' order.
            let ordered: Vec<String> = opts
                .iter()
                .filter(|(o, _)| out.contains(o))
                .map(|(o, _)| o.clone())
                .collect();
            Some(json!(ordered))
        }
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// The change `value` makes to an item: checked, and completed with the item's neighbours as
/// they are now (night's hours, the widgets' interval).
pub fn change_for(host: &dyn Host, key: &str, value: &Value) -> Result<Change, SetError> {
    let item = find(key).ok_or(SetError::Unknown)?;
    let v = check(host, item, value).ok_or(SetError::Invalid)?;
    // The neighbours as they are now (what the host holds, else the file).
    let widgets = || {
        let on: Vec<WidgetPage> = get(host, "widgets")
            .map(|v| {
                let keys = strings(&v);
                widget_order(host)
                    .into_iter()
                    .filter(|k| keys.contains(&k.key()))
                    .collect()
            })
            .unwrap_or_else(|| {
                widget_order(host)
                    .into_iter()
                    .filter(|k| k.n == 1 && k.kind.default_on())
                    .collect()
            });
        let every = get(host, "widgets_every")
            .and_then(|v| v.as_str().and_then(|e| e.parse().ok()))
            .unwrap_or(crate::ui::widgets::DEFAULT_EVERY.as_secs() as u32);
        (on, every)
    };
    let night = || {
        let n = |k: &str, d: i64| get(host, k).and_then(|v| v.as_i64()).unwrap_or(d) as u32;
        let on = get(host, "night_on")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        (on, n("night_start", 23), n("night_end", 7))
    };
    Ok(match key {
        "widgets" => {
            let keys = strings(&v);
            let on: Vec<WidgetPage> = widget_order(host)
                .into_iter()
                .filter(|k| keys.contains(&k.key()))
                .collect();
            Change::Widgets(on, widgets().1)
        }
        "widgets_every" => {
            let every: u32 = v
                .as_str()
                .and_then(|e| e.parse().ok())
                .ok_or(SetError::Invalid)?;
            Change::Widgets(widgets().0, every)
        }
        "widgets_currency" => Change::WidgetCurrency(v.as_str().unwrap_or("usd").to_string()),
        "check_openings" | "check_batteries" => {
            let mut w = widget_ha(host);
            let on = v.as_bool().unwrap_or(true);
            if key == "check_openings" {
                w.openings = on;
            } else {
                w.batteries = on;
            }
            Change::WidgetHa(w)
        }
        "night_on" | "night_start" | "night_end" => {
            let (mut on, mut start, mut end) = night();
            match key {
                "night_on" => on = v.as_bool().unwrap_or(on),
                "night_start" => start = v.as_i64().unwrap_or(0) as u32,
                _ => end = v.as_i64().unwrap_or(0) as u32,
            }
            Change::Night(on, start, end)
        }
        "home_compact" => Change::HomeCompact(v.as_bool().unwrap_or(false)),
        "language" => Change::Language(v.as_str().unwrap_or_default().to_string()),
        "volume" => Change::Volume(v.as_i64().unwrap_or(0) as u8),
        "wake_sensitivity" => Change::WakeSensitivity(
            v.as_str()
                .and_then(WakeSensitivity::from_key)
                .ok_or(SetError::Invalid)?,
        ),
        "keep_conversations" => Change::KeepConversations(v.as_bool().unwrap_or(true)),
        "conversation_mode" => Change::ConversationMode(v.as_bool().unwrap_or(false)),
        "conversation_max" => Change::ConversationMax(
            v.as_i64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(SetError::Invalid)?,
        ),
        "lists" | "calendars" => {
            let mut chosen = strings(&v);
            // All of them is "all": the lists Home Assistant adds later show too.
            if chosen.len() == host.dynamic_options(key).len() {
                chosen.clear();
            }
            if key == "lists" {
                Change::Lists(chosen)
            } else {
                Change::Calendars(chosen)
            }
        }
        "update_prerelease" => Change::UpdatePrerelease(v.as_bool().unwrap_or(false)),
        "panel" => Change::Panel(v.as_bool().unwrap_or(false)),
        "panel_screen" => Change::PanelScreen(v.as_bool().unwrap_or(false)),
        _ => return Err(SetError::Unknown),
    })
}

/// Writes a change to `settings.toml`.
pub fn save(s: &Settings, change: &Change) {
    match change {
        Change::Widgets(on, every) => s.save_widgets(on, *every),
        Change::WidgetOrder(order) => s.save_widget_order(order),
        Change::WidgetCurrency(c) => s.save_widget_currency(c),
        Change::WidgetAdded(p, at, on) => s.add_widget_page(*p, *at, *on),
        Change::WidgetRemoved(p) => s.remove_widget_page(*p),
        Change::WidgetName(p, name) => s.save_widget_name(*p, name),
        Change::Quotes(n, q) => s.save_quotes(*n, q),
        Change::Cities(n, c) => s.save_cities(*n, c),
        Change::Sensors(n, ids) => s.save_sensors(*n, ids),
        Change::WidgetHa(w) => s.save_widget_ha(w),
        Change::Night(on, start, end) => s.save_night(*on, *start, *end),
        Change::HomeCompact(on) => s.save_home_compact(*on),
        Change::Language(code) => s.save_language(code),
        Change::Volume(p) => s.save_volume(*p),
        Change::WakeSensitivity(w) => s.save_wake_sensitivity(*w),
        Change::KeepConversations(k) => s.save_keep_conversations(*k),
        Change::ConversationMode(on) => s.save_conversation_mode(*on),
        Change::ConversationMax(n) => s.save_conversation_max(*n),
        Change::Lists(l) => s.save_lists(l),
        Change::Calendars(c) => s.save_calendars(c),
        Change::UpdatePrerelease(on) => s.save_update_prerelease(*on),
        Change::Panel(on) => s.save_panel_on(*on),
        Change::PanelScreen(on) => s.save_panel_screen(*on),
    }
}

/// A change written (when there is a settings file: not in the demos) and applied. The screen's
/// switches that already know the whole change come here directly.
pub fn commit(host: &dyn Host, change: Change) {
    host.save(&change);
    host.apply(change);
}

/// Sets an item: checked, written to `settings.toml`, applied ([`Host::apply`]).
pub fn set(host: &dyn Host, key: &str, value: &Value) -> Result<(), SetError> {
    let change = change_for(host, key, value)?;
    commit(host, change);
    Ok(())
}

/// The panel's items with their values: `{key: value}`.
pub fn panel_state(host: &dyn Host) -> Value {
    let mut out = serde_json::Map::new();
    for i in ITEMS.iter().filter(|i| i.panel) {
        if let Some(v) = get(host, i.key) {
            out.insert(i.key.into(), v);
        }
    }
    Value::Object(out)
}

/// What the panel draws its forms from: the groups in order, each with its items (key, kind,
/// limits, label, options), the words already in the reader's language.
pub fn panel_schema(host: &dyn Host, word: &dyn Fn(&str) -> String) -> Value {
    let mut groups: Vec<Value> = Vec::new();
    for i in ITEMS.iter().filter(|i| i.panel) {
        let mut v = json!({"key": i.key, "label": word(i.label)});
        // The line that explains it, where the words have one (`<label>_help`).
        let help_key = format!("{}_help", i.label);
        let help = word(&help_key);
        if !help.is_empty() && help != help_key {
            v["help"] = json!(help);
        }
        match i.kind {
            Kind::Bool => v["kind"] = json!("bool"),
            Kind::Int { min, max, step } => {
                v["kind"] = json!("int");
                v["min"] = json!(min);
                v["max"] = json!(max);
                v["step"] = json!(step);
            }
            Kind::Choice | Kind::Multi => {
                v["kind"] = json!(if i.kind == Kind::Choice {
                    "choice"
                } else {
                    "multi"
                });
                v["options"] = Value::Array(
                    options(host, i, word)
                        .into_iter()
                        .map(|(value, label)| json!({"value": value, "label": label}))
                        .collect(),
                );
            }
        }
        match groups.last_mut() {
            Some(g) if g["key"] == i.group => {
                if let Some(items) = g["items"].as_array_mut() {
                    items.push(v);
                }
            }
            _ => groups.push(json!({
                "key": i.group,
                "label": word(&format!("panel_group_{}", i.group)),
                "items": [v],
            })),
        }
    }
    json!({"groups": groups})
}

/// Sets an item from the panel: only the items with `panel` on exist for it.
pub fn panel_set(host: &dyn Host, key: &str, value: &Value) -> Result<(), SetError> {
    match find(key) {
        Some(i) if i.panel && PANEL_KEYS.contains(&key) => set(host, key, value),
        _ => Err(SetError::Unknown),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ui::widgets::Kind as WidgetKind;
    use std::cell::RefCell;
    use std::path::PathBuf;

    /// A host on a settings file in a temporary directory; it records what it was asked to apply.
    pub struct FakeHost {
        pub settings: Settings,
        pub dir: PathBuf,
        pub applied: RefCell<Vec<Change>>,
        pub volume: RefCell<u8>,
    }

    impl FakeHost {
        pub fn new(name: &str) -> FakeHost {
            let dir = std::env::temp_dir().join(format!(
                "home-companion-model-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            FakeHost {
                settings: Settings::in_dir(&dir),
                dir,
                applied: RefCell::new(Vec::new()),
                volume: RefCell::new(40),
            }
        }
    }

    impl Drop for FakeHost {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Host for FakeHost {
        fn settings(&self) -> Option<&Settings> {
            Some(&self.settings)
        }
        fn live(&self, key: &str) -> Option<Value> {
            match key {
                "volume" => Some(json!(*self.volume.borrow())),
                "language" if self.settings.language().is_none() => Some(json!("en")),
                _ => None,
            }
        }
        fn dynamic_options(&self, key: &str) -> Vec<(String, String)> {
            let v: &[(&str, &str)] = match key {
                "language" => &[("en", "English"), ("it", "Italiano")],
                "lists" => &[("todo.shopping", "Shopping"), ("todo.hardware", "Hardware")],
                "calendars" => &[("calendar.family", "Family")],
                _ => &[],
            };
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        }
        fn apply(&self, change: Change) {
            if let Change::Volume(p) = change {
                *self.volume.borrow_mut() = p;
            }
            self.applied.borrow_mut().push(change);
        }
    }

    /// Every item: a value set reads back the same, is in the file (read again from disk) and
    /// was applied.
    #[test]
    fn a_pages_list_replaces_only_its_own() {
        use crate::ui::widgets::Page;
        let q2 = Page::new(WidgetKind::Quotes, 2);
        assert!(Change::Quotes(1, vec![]).replaced_by(&Change::Quotes(1, vec![])));
        assert!(!Change::Quotes(1, vec![]).replaced_by(&Change::Quotes(2, vec![])));
        assert!(!Change::Sensors(2, vec![]).replaced_by(&Change::Cities(2, vec![])));
        assert!(
            Change::WidgetName(q2, "a".into()).replaced_by(&Change::WidgetName(q2, "b".into()))
        );
        assert!(
            !Change::WidgetAdded(q2, None, true).replaced_by(&Change::WidgetAdded(q2, None, true))
        );
        assert!(!Change::WidgetRemoved(q2).replaced_by(&Change::WidgetRemoved(q2)));
        assert!(Change::WidgetOrder(vec![]).replaced_by(&Change::WidgetOrder(vec![q2])));
    }

    #[test]
    fn set_then_get_for_every_item() {
        let h = FakeHost::new("every");
        let cases: Vec<(&str, Value, Change)> = vec![
            (
                "widgets",
                json!(["shopping", "next"]),
                Change::Widgets(
                    vec![WidgetKind::Next.into(), WidgetKind::Shopping.into()],
                    20,
                ),
            ),
            (
                "widgets_every",
                json!("60"),
                Change::Widgets(
                    vec![WidgetKind::Next.into(), WidgetKind::Shopping.into()],
                    60,
                ),
            ),
            (
                "widgets_currency",
                json!("eur"),
                Change::WidgetCurrency("eur".into()),
            ),
            (
                "check_openings",
                json!(false),
                Change::WidgetHa(crate::ha::widgets::Want {
                    people: Some(vec![]),
                    openings: false,
                    batteries: true,
                    ..Default::default()
                }),
            ),
            (
                "check_batteries",
                json!(false),
                Change::WidgetHa(crate::ha::widgets::Want {
                    people: Some(vec![]),
                    openings: false,
                    batteries: false,
                    ..Default::default()
                }),
            ),
            ("night_on", json!(false), Change::Night(false, 23, 7)),
            ("night_start", json!(22), Change::Night(false, 22, 7)),
            ("night_end", json!(6), Change::Night(false, 22, 6)),
            ("home_compact", json!(true), Change::HomeCompact(true)),
            ("language", json!("it"), Change::Language("it".into())),
            ("volume", json!(35), Change::Volume(35)),
            (
                "wake_sensitivity",
                json!("more"),
                Change::WakeSensitivity(WakeSensitivity::More),
            ),
            (
                "keep_conversations",
                json!(false),
                Change::KeepConversations(false),
            ),
            (
                "conversation_mode",
                json!(true),
                Change::ConversationMode(true),
            ),
            ("conversation_max", json!(5), Change::ConversationMax(5)),
            (
                "lists",
                json!(["todo.hardware"]),
                Change::Lists(vec!["todo.hardware".into()]),
            ),
            (
                "calendars",
                json!(["calendar.family"]),
                // All of them: stored as "all".
                Change::Calendars(vec![]),
            ),
            (
                "update_prerelease",
                json!(true),
                Change::UpdatePrerelease(true),
            ),
            ("panel", json!(true), Change::Panel(true)),
            ("panel_screen", json!(true), Change::PanelScreen(true)),
        ];
        assert_eq!(cases.len(), ITEMS.len(), "a case for every item");
        for (key, value, change) in cases {
            set(&h, key, &value).unwrap_or_else(|e| panic!("{key}: {e:?}"));
            assert_eq!(h.applied.borrow().last(), Some(&change), "{key}");
            let back = get(&h, key).unwrap();
            let expect = match key {
                "widgets" => json!(["next", "shopping"]),
                "calendars" => json!([]),
                _ => value.clone(),
            };
            assert_eq!(back, expect, "{key}");
        }
        // On disk (a fresh reader).
        let s = Settings::in_dir(&h.dir);
        assert_eq!(
            s.widgets(),
            (
                vec![WidgetKind::Next.into(), WidgetKind::Shopping.into()],
                60
            )
        );
        assert_eq!(s.night(), (false, 22, 6));
        assert_eq!(s.widget_currency(), "eur");
        assert!(s.home_compact());
        assert_eq!(s.language().as_deref(), Some("it"));
        assert_eq!(s.volume(), Some(35));
        assert_eq!(s.wake_sensitivity(), WakeSensitivity::More);
        assert!(!s.keep_conversations());
        assert!(s.conversation_mode());
        assert_eq!(s.conversation_max(), 5);
        assert_eq!(s.lists(), ["todo.hardware"]);
        assert!(s.calendars().is_empty());
        assert_eq!(s.update_prerelease(), Some(true));
        assert!(s.panel_on());
        // The two keys of [panel] are kept apart.
        assert!(s.panel_screen());
        s.save_panel_on(false);
        assert!(s.panel_screen());
    }

    #[test]
    fn bad_values_are_refused_and_nothing_written() {
        let h = FakeHost::new("bad");
        for (key, value) in [
            ("night_start", json!(24)),
            ("night_start", json!(-1)),
            ("night_start", json!("3")),
            ("volume", json!(33)),
            ("volume", json!(105)),
            ("home_compact", json!(1)),
            ("conversation_mode", json!("on")),
            ("conversation_max", json!(0)),
            ("conversation_max", json!(11)),
            ("language", json!("xx")),
            ("wake_sensitivity", json!("loud")),
            ("widgets", json!(["next", "next"])),
            ("widgets", json!(["stocks"])),
            ("widgets_every", json!(20)),
            ("widgets_every", json!("45")),
            ("widgets_currency", json!("btc")),
            ("widgets_currency", json!("EUR")),
            ("lists", json!(["todo.unknown"])),
            ("lists", json!("todo.shopping")),
        ] {
            assert_eq!(
                set(&h, key, &value),
                Err(SetError::Invalid),
                "{key} {value}"
            );
        }
        assert_eq!(set(&h, "ha_token", &json!("x")), Err(SetError::Unknown));
        assert!(h.applied.borrow().is_empty());
        assert!(!h.settings.path.exists());
    }

    /// The panel's list is the reviewed one: changing a `panel` flag without updating
    /// [`PANEL_KEYS`] (or the other way round) fails here.
    #[test]
    fn panel_items_are_the_reviewed_list() {
        let flagged: Vec<&str> = ITEMS.iter().filter(|i| i.panel).map(|i| i.key).collect();
        assert_eq!(flagged, PANEL_KEYS);
        for k in PANEL_KEYS {
            let k = k.to_ascii_lowercase();
            for bad in [
                "token", "password", "passwd", "secret", "key", "url", "auth", "wifi",
            ] {
                assert!(!k.contains(bad), "{k} looks like a secret ({bad})");
            }
        }
        // Not on the panel: refused there, even though it exists.
        let h = FakeHost::new("panel");
        for k in ["update_prerelease", "panel"] {
            assert_eq!(panel_set(&h, k, &json!(true)), Err(SetError::Unknown));
        }
        assert!(panel_set(&h, "home_compact", &json!(true)).is_ok());
        let state = panel_state(&h);
        assert!(state.get("update_prerelease").is_none());
        assert_eq!(state["home_compact"], json!(true));
        assert_eq!(state["volume"], json!(40));
    }

    #[test]
    fn schema_groups_and_options() {
        let h = FakeHost::new("schema");
        let s = panel_schema(&h, &|k| format!("<{k}>"));
        let groups = s["groups"].as_array().unwrap();
        let keys: Vec<&str> = groups.iter().map(|g| g["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["widgets", "screen", "voice", "lists"]);
        assert_eq!(groups[0]["label"], "<panel_group_widgets>");
        let every = &groups[0]["items"][1];
        assert_eq!(every["kind"], "choice");
        assert_eq!(
            every["options"],
            json!([{"value": "20", "label": "20 s"}, {"value": "30", "label": "30 s"},
                   {"value": "60", "label": "1 min"}, {"value": "0", "label": "<widgets_by_hand>"}])
        );
        let volume = &groups[2]["items"][1];
        assert_eq!(volume["key"], "volume");
        assert_eq!(
            (volume["min"].as_i64(), volume["max"].as_i64()),
            (Some(0), Some(100))
        );
        assert_eq!(groups[3]["items"][0]["options"][1]["label"], "Hardware");
    }
}
